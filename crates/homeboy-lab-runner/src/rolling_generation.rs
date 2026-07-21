use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Generation ownership is independent of the workload running behind it.
/// Its serializable state can be persisted alongside endpoint records so jobs
/// remain addressable after admission moves to a newer generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollingGenerations<E> {
    pub admission_owner: String,
    pub generations: BTreeMap<String, RollingGeneration<E>>,
    /// A job is pinned at admission time. This mapping is deliberately separate
    /// from the active count: completed jobs can still need their original
    /// daemon for logs, cancellation acknowledgement, and artifact lookup.
    #[serde(default)]
    pub job_owners: BTreeMap<String, String>,
    /// Run and artifact reads are pinned with the job that created them. These
    /// maps survive controller restarts while the producing generation drains.
    #[serde(default)]
    pub run_owners: BTreeMap<String, String>,
    #[serde(default)]
    pub artifact_owners: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollingGeneration<E> {
    pub endpoint: E,
    pub active_jobs: usize,
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

/// A durable result lifecycle has explicitly released its routing claim.
/// These events are intentionally separate from job completion: terminal jobs
/// can retain their producing generation until run and artifact retention ends.
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
                    drain_state: RollingDrainState::Admitting,
                },
            )]),
            job_owners: BTreeMap::new(),
            run_owners: BTreeMap::new(),
            artifact_owners: BTreeMap::new(),
        }
    }

    /// Records a validated candidate without changing admissions. Calling this
    /// again for the active candidate is idempotent; callers must start it only
    /// when this returns `Start` and then call `activate` after health checks.
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
                drain_state: RollingDrainState::Draining,
            },
        );
        RollingStart::Start
    }

    /// Atomically moves future admissions after the candidate endpoint has
    /// started and passed its identity/health verification.
    pub fn activate(&mut self, generation: &str) -> bool {
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
            .expect("generation checked above")
            .drain_state = RollingDrainState::Admitting;
        self.retire_drained();
        true
    }

    /// Removes an unactivated candidate. The current admission owner and all
    /// jobs it owns are unchanged, which provides startup rollback.
    pub fn rollback(&mut self, generation: &str) -> bool {
        if self.admission_owner == generation {
            return false;
        }
        self.generations.remove(generation).is_some()
    }

    pub fn admit(&mut self) -> &str {
        self.generations
            .get_mut(&self.admission_owner)
            .expect("admission owner is always a generation")
            .active_jobs += 1;
        &self.admission_owner
    }

    pub fn admit_job(&mut self, job_id: impl Into<String>) -> &str {
        let owner = self.admit().to_string();
        self.job_owners.insert(job_id.into(), owner);
        &self.admission_owner
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

    /// Release one durable result owner only after its owning lifecycle has
    /// consumed, finalized, or pruned it. The registry reconciler decides whether
    /// the now-unowned endpoint can safely stop and retire.
    pub fn retire_result_owner(&mut self, retirement: RollingResultOwnerRetirement<'_>) -> bool {
        let removed = match retirement {
            RollingResultOwnerRetirement::Run(id) => self.run_owners.remove(id),
            RollingResultOwnerRetirement::Artifact(id) => self.artifact_owners.remove(id),
        };
        removed.is_some()
    }

    /// Bounded stale-retention reconciliation only removes result owners whose
    /// durable records are absent from the supplied authoritative retention set.
    /// A caller controls the bound by passing at most one retention page.
    pub fn reconcile_result_owners(
        &mut self,
        retained_run_ids: &BTreeSet<String>,
        retained_artifact_ids: &BTreeSet<String>,
    ) -> bool {
        let runs_before = self.run_owners.len();
        let artifacts_before = self.artifact_owners.len();
        self.run_owners
            .retain(|id, _| retained_run_ids.contains(id));
        self.artifact_owners
            .retain(|id, _| retained_artifact_ids.contains(id));
        let released =
            runs_before != self.run_owners.len() || artifacts_before != self.artifact_owners.len();
        released
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

    /// A drained daemon remains routable while durable run or artifact evidence
    /// still identifies it as the producing generation.
    pub fn has_result_owners(&self, generation: &str) -> bool {
        self.run_owners.values().any(|owner| owner == generation)
            || self
                .artifact_owners
                .values()
                .any(|owner| owner == generation)
    }

    /// Returns true if the completion retired a drained generation.
    pub fn complete(&mut self, generation: &str) -> bool {
        let Some(entry) = self.generations.get_mut(generation) else {
            return false;
        };
        entry.active_jobs = entry.active_jobs.saturating_sub(1);
        self.retire_drained()
    }

    pub fn complete_job(&mut self, job_id: &str) -> bool {
        let Some(generation) = self.job_owners.remove(job_id) else {
            return false;
        };
        self.complete(&generation)
    }

    /// Recovery preserves a recorded admission owner when it is valid, repairs
    /// older records that predate the field, and retires only authoritative zero
    /// drained generations.
    pub fn recover(&mut self) {
        if !self.generations.contains_key(&self.admission_owner) {
            if let Some((generation, _)) = self
                .generations
                .iter()
                .find(|(_, entry)| entry.drain_state == RollingDrainState::Admitting)
                .or_else(|| self.generations.iter().next_back())
            {
                self.admission_owner = generation.clone();
            }
        }
        if let Some(entry) = self.generations.get_mut(&self.admission_owner) {
            entry.drain_state = RollingDrainState::Admitting;
        }
        self.retire_drained();
    }

    fn retire_drained(&mut self) -> bool {
        let before = self.generations.len();
        let admission_owner = self.admission_owner.clone();
        let result_owners = self
            .run_owners
            .values()
            .chain(self.artifact_owners.values())
            .collect::<BTreeSet<_>>();
        self.generations.retain(|generation, entry| {
            generation == &admission_owner
                || entry.drain_state != RollingDrainState::Draining
                || entry.active_jobs != 0
                || result_owners.contains(generation)
        });
        self.generations.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_a_keeps_its_jobs_while_new_admissions_use_b() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        assert_eq!(generations.admit(), "A");
        assert_eq!(generations.begin("B", "endpoint-b"), RollingStart::Start);
        assert!(generations.activate("B"));

        assert_eq!(generations.admit(), "B");
        assert_eq!(generations.admission_owner, "B");
        assert_eq!(generations.generations["A"].active_jobs, 1);
        assert_eq!(
            generations.generations["A"].drain_state,
            RollingDrainState::Draining
        );
    }

    #[test]
    fn drained_generation_retires_at_authoritative_zero() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        generations.admit();
        generations.begin("B", "endpoint-b");
        generations.activate("B");

        assert!(generations.complete("A"));
        assert!(!generations.generations.contains_key("A"));
        assert!(generations.generations.contains_key("B"));
    }

    #[test]
    fn startup_rollback_preserves_a_admission_and_jobs() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        generations.admit();
        generations.begin("B", "endpoint-b");

        assert!(generations.rollback("B"));
        assert_eq!(generations.admit(), "A");
        assert_eq!(generations.generations["A"].active_jobs, 2);
    }

    #[test]
    fn repeated_refresh_of_the_active_generation_is_idempotent() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        assert_eq!(generations.begin("B", "endpoint-b"), RollingStart::Start);
        assert!(generations.activate("B"));
        assert_eq!(
            generations.begin("B", "other-endpoint"),
            RollingStart::AlreadyActive
        );
        assert_eq!(generations.generations.len(), 1);
        assert_eq!(generations.admission_owner, "B");
    }

    #[test]
    fn recovery_preserves_owner_and_retires_completed_drains() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        generations.admit();
        generations.begin("B", "endpoint-b");
        generations.activate("B");
        generations.complete("A");
        generations.recover();

        assert_eq!(generations.admission_owner, "B");
        assert_eq!(
            generations.generations.keys().cloned().collect::<Vec<_>>(),
            vec!["B".to_string()]
        );
    }

    #[test]
    fn persisted_job_ownership_survives_controller_recovery() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        generations.admit_job("job-a");
        generations.begin("B", "endpoint-b");
        generations.activate("B");
        generations.admit_job("job-b");

        let serialized = serde_json::to_string(&generations).expect("serialize ledger");
        let mut recovered: RollingGenerations<&str> =
            serde_json::from_str(&serialized).expect("deserialize ledger");
        recovered.recover();

        assert_eq!(recovered.job_owner("job-a"), Some("A"));
        assert_eq!(recovered.job_owner("job-b"), Some("B"));
        assert!(recovered.complete_job("job-a"));
        assert!(!recovered.generations.contains_key("A"));
    }

    #[test]
    fn persisted_run_and_artifact_ownership_keep_using_draining_a() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        generations.admit_job("job-a");
        assert!(generations.record_run("job-a", "run-a"));
        assert!(generations.record_artifact("job-a", "artifact-a"));
        generations.begin("B", "endpoint-b");
        generations.activate("B");

        let serialized = serde_json::to_string(&generations).expect("serialize");
        let restored: RollingGenerations<&str> =
            serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(
            restored.endpoint_owner(None, Some("run-a"), None),
            Some("A")
        );
        assert_eq!(
            restored.endpoint_owner(None, None, Some("artifact-a")),
            Some("A")
        );
    }

    #[test]
    fn result_owners_retain_until_each_explicit_lifecycle_retirement() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        generations.admit_job("job-a");
        assert!(generations.record_run("job-a", "run-a"));
        assert!(generations.record_artifact("job-a", "artifact-a"));
        generations.begin("B", "endpoint-b");
        generations.activate("B");
        assert!(!generations.complete_job("job-a"));
        assert!(generations.generations.contains_key("A"));

        assert!(generations.retire_result_owner(RollingResultOwnerRetirement::Run("run-a")));
        assert!(generations.generations.contains_key("A"));
        assert!(
            generations.retire_result_owner(RollingResultOwnerRetirement::Artifact("artifact-a"))
        );
        assert!(generations.generations.contains_key("A"));
    }

    #[test]
    fn bounded_retention_reconciliation_releases_only_missing_owners() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        generations.admit_job("job-a");
        assert!(generations.record_run("job-a", "run-a"));
        assert!(generations.record_artifact("job-a", "artifact-a"));
        generations.begin("B", "endpoint-b");
        generations.activate("B");
        assert!(!generations.complete_job("job-a"));

        assert!(generations
            .reconcile_result_owners(&BTreeSet::from(["run-a".to_string()]), &BTreeSet::new(),));
        assert!(generations.artifact_owners.is_empty());
        assert!(generations.generations.contains_key("A"));
        assert!(generations.reconcile_result_owners(&BTreeSet::new(), &BTreeSet::new()));
        assert!(generations.generations.contains_key("A"));
    }

    #[test]
    fn older_records_without_job_ownership_deserialize() {
        let recovered: RollingGenerations<&str> = serde_json::from_str(
            r#"{"admission_owner":"A","generations":{"A":{"endpoint":"endpoint-a","active_jobs":0,"drain_state":"admitting"}}}"#,
        )
        .expect("legacy ledger deserializes");

        assert_eq!(recovered.admission_owner, "A");
        assert!(recovered.job_owners.is_empty());
        assert_eq!(recovered.generations["A"].endpoint, "endpoint-a");
    }
}
