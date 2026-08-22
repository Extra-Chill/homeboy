//! Hash-bound evidence used to decide whether a completed benchmark stage can
//! be reused by a later invocation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BenchStageEvidence {
    pub id: String,
    pub producer: String,
    pub compatibility_key: String,
    pub input_hashes: BTreeMap<String, String>,
    pub artifacts: Vec<BenchStageArtifact>,
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BenchStageArtifact {
    pub name: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BenchStageReuse {
    Reusable,
    Invalidated {
        reasons: Vec<BenchStageInvalidation>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BenchStageInvalidation {
    Incomplete,
    StageId {
        expected: String,
        actual: String,
    },
    Producer {
        expected: String,
        actual: String,
    },
    CompatibilityKey {
        expected: String,
        actual: String,
    },
    InputHash {
        input: String,
        expected: Option<String>,
        actual: Option<String>,
    },
    ArtifactHash {
        artifact: String,
        expected: String,
        actual: Option<String>,
    },
}

/// Verify a persisted stage against the exact requested inputs and the bytes
/// currently available for its declared outputs. Missing evidence is unsafe.
pub fn verify_stage_reuse(
    prior: &BenchStageEvidence,
    requested: &BenchStageEvidence,
    artifact_hashes: &BTreeMap<String, String>,
) -> BenchStageReuse {
    let mut reasons = Vec::new();
    if !prior.completed {
        reasons.push(BenchStageInvalidation::Incomplete);
    }
    if prior.id != requested.id {
        reasons.push(BenchStageInvalidation::StageId {
            expected: requested.id.clone(),
            actual: prior.id.clone(),
        });
    }
    if prior.producer != requested.producer {
        reasons.push(BenchStageInvalidation::Producer {
            expected: requested.producer.clone(),
            actual: prior.producer.clone(),
        });
    }
    if prior.compatibility_key != requested.compatibility_key {
        reasons.push(BenchStageInvalidation::CompatibilityKey {
            expected: requested.compatibility_key.clone(),
            actual: prior.compatibility_key.clone(),
        });
    }
    for input in prior
        .input_hashes
        .keys()
        .chain(requested.input_hashes.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let actual = prior.input_hashes.get(input).cloned();
        let expected = requested.input_hashes.get(input).cloned();
        if actual != expected {
            reasons.push(BenchStageInvalidation::InputHash {
                input: input.clone(),
                expected,
                actual,
            });
        }
    }
    for artifact in &prior.artifacts {
        let actual = artifact_hashes.get(&artifact.name).cloned();
        if actual.as_deref() != Some(&artifact.sha256) {
            reasons.push(BenchStageInvalidation::ArtifactHash {
                artifact: artifact.name.clone(),
                expected: artifact.sha256.clone(),
                actual,
            });
        }
    }
    if reasons.is_empty() {
        BenchStageReuse::Reusable
    } else {
        BenchStageReuse::Invalidated { reasons }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> BenchStageEvidence {
        BenchStageEvidence {
            id: "generate-site".to_string(),
            producer: "fixture-generator@abc".to_string(),
            compatibility_key: "site-v1".to_string(),
            input_hashes: BTreeMap::from([("fixture".to_string(), "input-sha".to_string())]),
            artifacts: vec![BenchStageArtifact {
                name: "site".to_string(),
                sha256: "artifact-sha".to_string(),
            }],
            completed: true,
        }
    }

    #[test]
    fn verified_stage_reuses_without_running_its_producer() {
        let stage = evidence();
        assert_eq!(
            verify_stage_reuse(
                &stage,
                &stage,
                &BTreeMap::from([("site".to_string(), "artifact-sha".to_string())]),
            ),
            BenchStageReuse::Reusable
        );
    }

    #[test]
    fn changed_downstream_input_does_not_invalidate_upstream_stage() {
        let prior = evidence();
        let requested = evidence();
        assert_eq!(
            verify_stage_reuse(
                &prior,
                &requested,
                &BTreeMap::from([("site".to_string(), "artifact-sha".to_string())]),
            ),
            BenchStageReuse::Reusable
        );
    }

    #[test]
    fn changed_declared_input_or_artifact_bytes_invalidates_reuse() {
        let prior = evidence();
        let mut requested = evidence();
        requested
            .input_hashes
            .insert("fixture".to_string(), "new-input-sha".to_string());
        let BenchStageReuse::Invalidated { reasons } = verify_stage_reuse(
            &prior,
            &requested,
            &BTreeMap::from([("site".to_string(), "changed-artifact-sha".to_string())]),
        ) else {
            panic!("changed evidence must invalidate reuse");
        };
        assert_eq!(reasons.len(), 2);
        assert!(matches!(
            reasons[0],
            BenchStageInvalidation::InputHash { .. }
        ));
        assert!(matches!(
            reasons[1],
            BenchStageInvalidation::ArtifactHash { .. }
        ));
    }
}
