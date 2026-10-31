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

impl<E> RollingGenerations<E> {
    pub fn new(generation: impl Into<String>, endpoint: E) -> Self {
        let generation = generation.into();
        Self {
            admission_owner: generation.clone(),
            generations: BTreeMap::from([(
                generation,
                RollingGeneration {
                    endpoint,
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

    /// External process owners retain the endpoint until stop is proven.
    pub fn complete_job_preserving_drained(&mut self, job_id: &str) -> bool {
        self.job_owners.remove(job_id).is_some()
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
        generations
            .run_owners
            .insert("run-a".to_string(), "A".to_string());
        assert_eq!(generations.begin("B", "endpoint-b"), RollingStart::Start);
        assert!(generations.activate_preserving_drained("B"));
        assert!(generations.admit_job_for("B", "job-b"));
        assert_eq!(generations.job_owner("job-a"), Some("A"));
        assert!(generations.complete_job_preserving_drained("job-a"));
        assert!(
            generations.generations.contains_key("A"),
            "retained result owners keep the drained generation routable"
        );
    }
}
