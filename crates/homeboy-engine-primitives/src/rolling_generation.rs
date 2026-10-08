use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Durable ownership for a blue-green endpoint handoff.
///
/// Work is pinned at admission; moving the admission owner never moves an
/// existing job, run, or artifact to the newer endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(bound(deserialize = "E: Deserialize<'de>"))]
pub struct RollingGenerations<E> {
    pub admission_owner: String,
    pub generations: BTreeMap<String, RollingGeneration<E>>,
    #[serde(default)]
    pub job_owners: BTreeMap<String, String>,
    #[serde(default)]
    pub run_owners: BTreeMap<String, String>,
    #[serde(default)]
    pub artifact_owners: BTreeMap<String, String>,
    /// Producing endpoints whose evidence is now controller-owned. These are
    /// immutable provenance, never live execution/admission endpoints.
    #[serde(default)]
    pub retired_evidence: BTreeMap<String, E>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollingGeneration<E> {
    pub endpoint: E,
    /// Write-only compatibility counter: no production path reads it, but
    /// older pinned binaries still read it from disk to decide retirement,
    /// so it must never be written below the generation's real live job
    /// count. Writers refresh it with `sync_compat_active_jobs`, whose
    /// owner-derived count retains terminal-job routes and is therefore
    /// always at least the live count.
    #[serde(default)]
    pub active_jobs: usize,
    #[serde(default)]
    pub observed_active_jobs: Option<usize>,
    pub drain_state: RollingDrainState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RollingDrainState {
    Admitting,
    Draining,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollingStart {
    Start,
    AlreadyActive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollingResultOwnerRetirement<'a> {
    Run(&'a str),
    Artifact(&'a str),
}

impl<E> RollingGenerations<E> {
    pub fn new(generation: impl Into<String>, endpoint: E) -> Self {
        let generation = generation.into();
        Self {
            admission_owner: generation.clone(),
            generations: BTreeMap::from([(
                generation,
                RollingGeneration {
                    endpoint,
                    active_jobs: 0,
                    observed_active_jobs: None,
                    drain_state: RollingDrainState::Admitting,
                },
            )]),
            job_owners: BTreeMap::new(),
            run_owners: BTreeMap::new(),
            artifact_owners: BTreeMap::new(),
            retired_evidence: BTreeMap::new(),
        }
    }

    pub fn begin(&mut self, generation: impl Into<String>, endpoint: E) -> RollingStart {
        let generation = generation.into();
        if self.generations.contains_key(&generation) {
            return RollingStart::AlreadyActive;
        }
        self.generations.insert(
            generation,
            RollingGeneration {
                endpoint,
                active_jobs: 0,
                observed_active_jobs: None,
                drain_state: RollingDrainState::Draining,
            },
        );
        RollingStart::Start
    }

    /// Activate a generation while leaving empty draining generations available
    /// to a caller that must finish an external retirement protocol first.
    pub fn activate_preserving_drained(&mut self, generation: &str) -> bool {
        if !self.generations.contains_key(generation) {
            return false;
        }
        if self.admission_owner == generation {
            return true;
        }
        if let Some(previous) = self.generations.get_mut(&self.admission_owner) {
            previous.drain_state = RollingDrainState::Draining;
        }
        self.admission_owner = generation.to_string();
        self.generations
            .get_mut(generation)
            .expect("generation was checked")
            .drain_state = RollingDrainState::Admitting;
        true
    }

    pub fn rollback(&mut self, generation: &str) -> bool {
        if self.admission_owner == generation {
            return false;
        }
        self.generations.remove(generation).is_some()
    }

    pub fn admit_job_for(&mut self, generation: &str, job_id: impl Into<String>) -> bool {
        let job_id = job_id.into();
        if self.job_owners.contains_key(&job_id) || !self.generations.contains_key(generation) {
            return false;
        }
        self.job_owners.insert(job_id, generation.to_string());
        true
    }

    pub fn job_owner(&self, job_id: &str) -> Option<&str> {
        self.job_owners.get(job_id).map(String::as_str)
    }

    pub fn record_run(&mut self, job_id: &str, run_id: impl Into<String>) -> bool {
        let Some(owner) = self.job_owner(job_id).map(str::to_string) else {
            return false;
        };
        self.run_owners.insert(run_id.into(), owner);
        true
    }

    pub fn record_artifact(&mut self, job_id: &str, artifact_id: impl Into<String>) -> bool {
        let Some(owner) = self.job_owner(job_id).map(str::to_string) else {
            return false;
        };
        self.artifact_owners.insert(artifact_id.into(), owner);
        true
    }

    pub fn endpoint_owner(
        &self,
        job_id: Option<&str>,
        run_id: Option<&str>,
        artifact_id: Option<&str>,
    ) -> Option<&str> {
        job_id
            .and_then(|id| self.job_owner(id))
            .or_else(|| run_id.and_then(|id| self.run_owners.get(id).map(String::as_str)))
            .or_else(|| artifact_id.and_then(|id| self.artifact_owners.get(id).map(String::as_str)))
    }

    /// External process owners retain the endpoint until stop is proven.
    pub fn complete_job_preserving_drained(&mut self, job_id: &str) -> bool {
        self.job_owners.remove(job_id).is_some()
    }

    /// Rewrite each generation's write-only `active_jobs` compatibility
    /// counter to its `job_owners` count. Registry writers call this before
    /// persisting: `job_owners` keeps routes for terminal jobs, so the value
    /// is always at least the generation's live job count, which is the
    /// invariant older pinned binaries rely on when they read the field.
    pub fn sync_compat_active_jobs(&mut self) {
        for (generation, entry) in self.generations.iter_mut() {
            entry.active_jobs = self
                .job_owners
                .values()
                .filter(|owner| owner.as_str() == generation.as_str())
                .count();
        }
    }

    pub fn retire_result_owner(&mut self, retirement: RollingResultOwnerRetirement<'_>) -> bool {
        let removed = match retirement {
            RollingResultOwnerRetirement::Run(id) => self.run_owners.remove(id),
            RollingResultOwnerRetirement::Artifact(id) => self.artifact_owners.remove(id),
        };
        removed.is_some()
    }

    pub fn reconcile_result_owners(
        &mut self,
        retained_run_ids: &BTreeSet<String>,
        retained_artifact_ids: &BTreeSet<String>,
    ) -> bool {
        let before = (self.run_owners.len(), self.artifact_owners.len());
        self.run_owners
            .retain(|id, _| retained_run_ids.contains(id));
        self.artifact_owners
            .retain(|id, _| retained_artifact_ids.contains(id));
        before != (self.run_owners.len(), self.artifact_owners.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_preserves_job_and_result_owners_until_their_lifecycle_releases_them() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        assert!(generations.admit_job_for("A", "job-a"));
        assert!(generations.record_run("job-a", "run-a"));
        assert!(generations.record_artifact("job-a", "artifact-a"));
        assert_eq!(generations.begin("B", "endpoint-b"), RollingStart::Start);
        assert!(generations.activate_preserving_drained("B"));
        assert!(generations.admit_job_for("B", "job-b"));
        assert_eq!(
            generations.endpoint_owner(Some("job-a"), None, None),
            Some("A")
        );
        assert!(generations.complete_job_preserving_drained("job-a"));
        assert!(
            generations.generations.contains_key("A"),
            "retained result owners keep the drained generation routable"
        );
    }

    #[test]
    fn failed_candidate_rollback_does_not_move_admission() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        assert!(generations.admit_job_for("A", "job-a"));
        generations.begin("B", "endpoint-b");
        assert!(generations.rollback("B"));
        assert_eq!(generations.admission_owner, "A");
        assert_eq!(generations.job_owner("job-a"), Some("A"));
    }

    #[test]
    fn sync_compat_active_jobs_writes_each_generation_owner_count() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        assert!(generations.admit_job_for("A", "job-a"));
        assert!(generations.admit_job_for("A", "job-a2"));
        assert_eq!(generations.begin("B", "endpoint-b"), RollingStart::Start);
        assert!(generations.activate_preserving_drained("B"));
        assert!(generations.admit_job_for("B", "job-b"));
        assert!(generations.complete_job_preserving_drained("job-a"));
        generations.sync_compat_active_jobs();
        assert_eq!(generations.generations["A"].active_jobs, 1);
        assert_eq!(generations.generations["B"].active_jobs, 1);
    }

    #[test]
    fn a_registry_without_active_jobs_still_loads() {
        let registry: RollingGenerations<&str> = serde_json::from_str(
            r#"{
                "admission_owner": "A",
                "generations": {
                    "A": {"endpoint": "endpoint-a", "drain_state": "admitting"}
                },
                "job_owners": {"job-a": "A"}
            }"#,
        )
        .expect("a missing active_jobs field defaults to zero");
        assert_eq!(registry.generations["A"].active_jobs, 0);
        assert_eq!(registry.job_owner("job-a"), Some("A"));
    }
}
