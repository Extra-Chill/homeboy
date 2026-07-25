use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateState {
    PatchAvailable,
    NoChangesProduced,
    Empty,
    Missing,
    Unreadable,
    Conflicting,
    RetainedOnly,
    Unknown,
}

impl Default for CandidateState {
    fn default() -> Self {
        Self::Unknown
    }
}

impl CandidateState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PatchAvailable => "patch_available",
            Self::NoChangesProduced => "no_changes_produced",
            Self::Empty => "empty",
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
            Self::Conflicting => "conflicting",
            Self::RetainedOnly => "retained_only",
            Self::Unknown => "unknown",
        }
    }

    pub(crate) fn is_recoverable(self) -> bool {
        self == Self::PatchAvailable
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CandidateCounts {
    pub(crate) available: usize,
    pub(crate) empty: usize,
    pub(crate) missing: usize,
    pub(crate) unreadable: usize,
    pub(crate) conflicting: usize,
    pub(crate) retained_only: usize,
    pub(crate) unknown: usize,
    pub(crate) diff_bytes: u64,
    no_changes_produced: bool,
}

impl CandidateCounts {
    pub(crate) fn state(self) -> CandidateState {
        if self.available > 0 {
            CandidateState::PatchAvailable
        } else if self.conflicting > 0 {
            CandidateState::Conflicting
        } else if self.unreadable > 0 {
            CandidateState::Unreadable
        } else if self.missing > 0 {
            CandidateState::Missing
        } else if self.retained_only > 0 {
            CandidateState::RetainedOnly
        } else if self.empty > 0 {
            CandidateState::Empty
        } else if self.no_changes_produced {
            CandidateState::NoChangesProduced
        } else {
            CandidateState::Unknown
        }
    }

    fn record(&mut self, state: CandidateState, size: Option<u64>) {
        match state {
            CandidateState::PatchAvailable => {
                self.available += 1;
                self.diff_bytes += size.unwrap_or_default();
            }
            CandidateState::NoChangesProduced => self.no_changes_produced = true,
            CandidateState::Empty => self.empty += 1,
            CandidateState::Missing => self.missing += 1,
            CandidateState::Unreadable => self.unreadable += 1,
            CandidateState::Conflicting => self.conflicting += 1,
            CandidateState::RetainedOnly => self.retained_only += 1,
            CandidateState::Unknown => self.unknown += 1,
        }
    }
}

/// Classify promotion candidates from immutable aggregate artifacts. Finalized
/// mirrored artifacts are authoritative; runner-local refs are only a fallback
/// when no such artifact exists, so stale aliases cannot downgrade recovery.
pub(crate) fn classify_candidates(payload: &Value) -> CandidateCounts {
    let artifacts = aggregate_artifacts(payload);
    let canonical: Vec<&Value> = artifacts
        .iter()
        .copied()
        .filter(|artifact| is_patch(artifact) && is_canonical_mirror(artifact))
        .collect();
    let using_canonical = !canonical.is_empty();
    let artifacts = if !using_canonical {
        artifacts
            .iter()
            .copied()
            .filter(|artifact| is_patch(artifact))
            .collect()
    } else {
        canonical
    };

    let mut by_identity: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for artifact in artifacts {
        let identity = artifact
            .get("id")
            .or_else(|| artifact.get("artifact_id"))
            .and_then(Value::as_str)
            .unwrap_or("unnamed")
            .to_string();
        by_identity.entry(identity).or_default().push(artifact);
    }

    let mut counts = CandidateCounts::default();
    for artifacts in by_identity.into_values() {
        let artifact = artifacts[0];
        let size = artifact.get("size_bytes").and_then(Value::as_u64);
        let conflicting = artifacts.iter().skip(1).any(|other| {
            other.get("size_bytes") != artifact.get("size_bytes")
                || other.get("sha256") != artifact.get("sha256")
        });
        let state = if conflicting {
            CandidateState::Conflicting
        } else if artifact
            .pointer("/metadata/review_only")
            .and_then(Value::as_bool)
            == Some(true)
        {
            CandidateState::RetainedOnly
        } else {
            match size {
                Some(0) => CandidateState::Empty,
                // Finalization records the immutable aggregate only after Homeboy
                // owns the artifact. Status must trust that durable fact rather
                // than probing a possibly remote or evicted path.
                Some(_)
                    if !using_canonical
                        || (is_canonical_mirror(artifact) && declared_location(artifact)) =>
                {
                    CandidateState::PatchAvailable
                }
                Some(_) if declared_location(artifact) => CandidateState::Unreadable,
                Some(_) => CandidateState::Missing,
                None => CandidateState::Unknown,
            }
        };
        counts.record(state, size);
    }
    counts.no_changes_produced = counts.available == 0
        && counts.empty == 0
        && counts.missing == 0
        && counts.unreadable == 0
        && counts.conflicting == 0
        && counts.retained_only == 0
        && aggregate_outcomes_are_no_op(payload);
    counts
}

fn aggregate_artifacts(payload: &Value) -> Vec<&Value> {
    payload
        .pointer("/aggregate/outcomes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|outcome| {
            outcome
                .get("artifacts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .collect()
}

fn aggregate_outcomes_are_no_op(payload: &Value) -> bool {
    let Some(outcomes) = payload
        .pointer("/aggregate/outcomes")
        .and_then(Value::as_array)
    else {
        return false;
    };
    !outcomes.is_empty()
        && outcomes
            .iter()
            .all(|outcome| outcome.get("status").and_then(Value::as_str) == Some("no_op"))
}

fn is_patch(artifact: &Value) -> bool {
    matches!(
        artifact.get("kind").and_then(Value::as_str),
        Some("patch" | "diff" | "change_artifact" | "workspace_patch" | "artifact")
    )
}

fn is_canonical_mirror(artifact: &Value) -> bool {
    artifact
        .pointer("/metadata/executor_artifact_finalized")
        .and_then(Value::as_bool)
        == Some(true)
        || artifact
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|url| url.starts_with("homeboy://agent-task/run/"))
}

fn declared_location(artifact: &Value) -> bool {
    artifact
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| !path.trim().is_empty())
        || artifact
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|url| !url.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lab_fixture(artifacts: Vec<Value>) -> Value {
        json!({
            "aggregate": { "outcomes": [{ "task_id": "cook-intelligence", "artifacts": artifacts }] },
            "artifact_refs": [{ "task_id": "cook-intelligence", "kind": "patch", "uri": "runner-artifact://stale-alias" }]
        })
    }

    fn mirrored_patch(id: &str, size_bytes: u64) -> Value {
        json!({
            "id": id,
            "kind": "patch",
            "size_bytes": size_bytes,
            "sha256": "f".repeat(64),
            "url": format!("homeboy://agent-task/run/lab-restarted/artifacts#task=cook-intelligence&artifact={id}"),
            "metadata": { "executor_artifact_finalized": true, "source_provenance": { "runner_id": "homeboy-lab" } }
        })
    }

    #[test]
    fn lab_fixtures_keep_canonical_mirror_authoritative_across_restart_and_eviction() {
        let available = classify_candidates(&lab_fixture(vec![mirrored_patch("patch", 32_318)]));
        assert_eq!(available.state(), CandidateState::PatchAvailable);
        assert_eq!(available.available, 1);
        assert_eq!(available.diff_bytes, 32_318);

        let fixtures = [
            ("empty", mirrored_patch("empty", 0), CandidateState::Empty),
            (
                "missing",
                json!({ "id": "missing", "kind": "patch", "size_bytes": 1, "metadata": { "executor_artifact_finalized": true } }),
                CandidateState::Missing,
            ),
            (
                "artifact_evicted",
                json!({ "id": "evicted", "kind": "patch", "size_bytes": 1, "path": "/definitely-evicted-lab-patch", "metadata": { "executor_artifact_finalized": true } }),
                CandidateState::PatchAvailable,
            ),
            (
                "retained_only",
                json!({ "id": "retained", "kind": "patch", "size_bytes": 1, "url": "homeboy://agent-task/run/lab/artifacts#task=cook&artifact=retained", "metadata": { "executor_artifact_finalized": true, "review_only": true } }),
                CandidateState::RetainedOnly,
            ),
        ];
        for (name, artifact, expected) in fixtures {
            assert_eq!(
                classify_candidates(&lab_fixture(vec![artifact])).state(),
                expected,
                "{name}"
            );
        }

        let first = mirrored_patch("conflict", 32_318);
        let second = mirrored_patch("conflict", 1);
        assert_eq!(
            classify_candidates(&lab_fixture(vec![first, second])).state(),
            CandidateState::Conflicting
        );
    }
}
