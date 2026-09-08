use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Durable ownership for a blue-green endpoint handoff.
///
/// Work is pinned at admission; moving the admission owner never moves an
/// existing job, run, or artifact to the newer endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollingGenerations<E> {
    pub admission_owner: String,
    pub generations: BTreeMap<String, RollingGeneration<E>>,
    #[serde(default)]
    pub job_owners: BTreeMap<String, String>,
    #[serde(default)]
    pub run_owners: BTreeMap<String, String>,
    #[serde(default)]
    pub artifact_owners: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollingGeneration<E> {
    pub endpoint: E,
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
            .expect("generation was checked")
            .drain_state = RollingDrainState::Admitting;
        self.retire_drained();
        true
    }

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
        let job_id = job_id.into();
        if self.job_owners.contains_key(&job_id) {
            return &self.admission_owner;
        }
        let owner = self.admission_owner.clone();
        let entry = self
            .generations
            .get_mut(&owner)
            .expect("admission owner is always a generation");
        entry.active_jobs += 1;
        self.job_owners.insert(job_id, owner);
        &self.admission_owner
    }

    pub fn admit_job_for(&mut self, generation: &str, job_id: impl Into<String>) -> bool {
        let job_id = job_id.into();
        if self.job_owners.contains_key(&job_id) || !self.generations.contains_key(generation) {
            return false;
        }
        self.generations
            .get_mut(generation)
            .expect("generation was checked")
            .active_jobs += 1;
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

    pub fn complete_job(&mut self, job_id: &str) -> bool {
        let Some(generation) = self.job_owners.remove(job_id) else {
            return false;
        };
        self.complete(&generation)
    }

    pub fn complete(&mut self, generation: &str) -> bool {
        let Some(entry) = self.generations.get_mut(generation) else {
            return false;
        };
        entry.active_jobs = entry.active_jobs.saturating_sub(1);
        self.retire_drained()
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
    fn rotation_preserves_job_and_result_owners_until_their_lifecycle_releases_them() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        assert_eq!(generations.admit_job("job-a"), "A");
        assert!(generations.record_run("job-a", "run-a"));
        assert!(generations.record_artifact("job-a", "artifact-a"));
        assert_eq!(generations.begin("B", "endpoint-b"), RollingStart::Start);
        assert!(generations.activate("B"));
        assert_eq!(generations.admit_job("job-b"), "B");
        assert_eq!(
            generations.endpoint_owner(Some("job-a"), None, None),
            Some("A")
        );
        assert!(!generations.complete_job("job-a"));
        assert!(generations.generations.contains_key("A"));
    }

    #[test]
    fn failed_candidate_and_recovery_do_not_move_admission() {
        let mut generations = RollingGenerations::new("A", "endpoint-a");
        generations.admit_job("job-a");
        generations.begin("B", "endpoint-b");
        assert!(generations.rollback("B"));
        generations.recover();
        assert_eq!(generations.admission_owner, "A");
        assert_eq!(generations.job_owner("job-a"), Some("A"));
    }
}
