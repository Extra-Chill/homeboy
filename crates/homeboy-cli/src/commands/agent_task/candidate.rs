use std::collections::BTreeMap;

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateState {
    Finalized,
    Promoted,
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
            Self::Finalized => "finalized",
            Self::Promoted => "promoted",
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

    pub(crate) fn is_available(self) -> bool {
        matches!(
            self,
            Self::Finalized | Self::Promoted | Self::PatchAvailable
        )
    }
}

const MAX_ATTEMPTS_SCANNED: usize = 64;
const MAX_OUTCOMES_SCANNED: usize = 256;
const MAX_ARTIFACTS_SCANNED: usize = 256;

/// The canonical terminal result for a Cook's candidate evidence. Evidence is
/// monotonic: a finalized PR, promoted/adopted candidate, or durable patch
/// remains authoritative even when a later provider attempt is empty or fails.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CandidateResult {
    pub(crate) available: usize,
    pub(crate) empty: usize,
    pub(crate) missing: usize,
    pub(crate) unreadable: usize,
    pub(crate) conflicting: usize,
    pub(crate) retained_only: usize,
    pub(crate) unknown: usize,
    pub(crate) diff_bytes: u64,
    pub(crate) finalized: bool,
    pub(crate) promoted: bool,
    pub(crate) attempts_omitted: usize,
    pub(crate) outcomes_omitted: usize,
    pub(crate) artifacts_omitted: usize,
    no_changes_produced: bool,
}

impl CandidateResult {
    pub(crate) fn state(self) -> CandidateState {
        if self.finalized {
            CandidateState::Finalized
        } else if self.promoted {
            CandidateState::Promoted
        } else if self.available > 0 {
            CandidateState::PatchAvailable
        } else if self.is_degraded() {
            // An incomplete scan cannot honestly prove that no candidate exists.
            CandidateState::Unknown
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

    pub(crate) fn is_degraded(self) -> bool {
        self.attempts_omitted > 0 || self.outcomes_omitted > 0 || self.artifacts_omitted > 0
    }

    fn record(&mut self, state: CandidateState, size: Option<u64>) {
        match state {
            CandidateState::Finalized | CandidateState::Promoted => {
                unreachable!("terminal facts are recorded directly")
            }
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
pub(crate) fn classify_candidates(payload: &Value) -> CandidateResult {
    if let Some(projected) = payload
        .get("canonical_candidate")
        .and_then(candidate_result_from_projection)
    {
        return projected;
    }
    let (artifacts, attempts_omitted, outcomes_omitted, artifacts_omitted) =
        aggregate_artifacts(payload);
    let canonical: Vec<&Value> = artifacts
        .iter()
        .copied()
        .filter(|artifact| {
            is_patch(artifact)
                && is_display_apply_artifact(artifact)
                && is_canonical_mirror(artifact)
        })
        .collect();
    let using_canonical = !canonical.is_empty();
    let artifacts = if !using_canonical {
        artifacts
            .iter()
            .copied()
            .filter(|artifact| is_patch(artifact) && is_display_apply_artifact(artifact))
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

    let mut counts = CandidateResult::default();
    counts.attempts_omitted = attempts_omitted;
    counts.outcomes_omitted = outcomes_omitted;
    counts.artifacts_omitted = artifacts_omitted;
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
    counts.finalized =
        has_finalized_pr(payload) || payload.get("record").is_some_and(has_finalized_pr);
    counts.promoted = !counts.finalized
        && (has_promoted_candidate(payload)
            || payload.get("record").is_some_and(has_promoted_candidate));
    counts
}

/// The bounded candidate contract carried by compact status responses.
pub(crate) fn canonical_candidate_projection(result: CandidateResult) -> Value {
    json!({
        "schema": "homeboy/agent-task-candidate/v1",
        "state": result.state().as_str(),
        "diff_bytes": result.diff_bytes,
        "counts": {
            "patch_available": result.available,
            "empty": result.empty,
            "missing": result.missing,
            "unreadable": result.unreadable,
            "conflicting": result.conflicting,
            "retained_only": result.retained_only,
            "unknown": result.unknown,
        },
        "scan": {
            "attempts_omitted": result.attempts_omitted,
            "outcomes_omitted": result.outcomes_omitted,
            "artifacts_omitted": result.artifacts_omitted,
            "degraded": result.is_degraded(),
        },
    })
}

fn candidate_result_from_projection(value: &Value) -> Option<CandidateResult> {
    if value.get("schema").and_then(Value::as_str) != Some("homeboy/agent-task-candidate/v1") {
        return None;
    }
    let state = match value.get("state").and_then(Value::as_str)? {
        "finalized" => CandidateState::Finalized,
        "promoted" => CandidateState::Promoted,
        "patch_available" => CandidateState::PatchAvailable,
        "no_changes_produced" => CandidateState::NoChangesProduced,
        "empty" => CandidateState::Empty,
        "missing" => CandidateState::Missing,
        "unreadable" => CandidateState::Unreadable,
        "conflicting" => CandidateState::Conflicting,
        "retained_only" => CandidateState::RetainedOnly,
        "unknown" => CandidateState::Unknown,
        _ => return None,
    };
    let count = |key| {
        value
            .pointer(&format!("/counts/{key}"))
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or_default()
    };
    let omitted = |key| {
        value
            .pointer(&format!("/scan/{key}"))
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or_default()
    };
    Some(CandidateResult {
        available: count("patch_available"),
        empty: count("empty"),
        missing: count("missing"),
        unreadable: count("unreadable"),
        conflicting: count("conflicting"),
        retained_only: count("retained_only"),
        unknown: count("unknown"),
        diff_bytes: value
            .get("diff_bytes")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        finalized: state == CandidateState::Finalized,
        promoted: state == CandidateState::Promoted,
        attempts_omitted: omitted("attempts_omitted"),
        outcomes_omitted: omitted("outcomes_omitted"),
        artifacts_omitted: omitted("artifacts_omitted"),
        no_changes_produced: state == CandidateState::NoChangesProduced,
        ..Default::default()
    })
}

fn aggregate_artifacts(payload: &Value) -> (Vec<&Value>, usize, usize, usize) {
    let mut artifacts = Vec::new();
    let mut attempts_omitted = 0;
    let mut outcomes_omitted = 0;
    let mut artifacts_omitted = 0;
    for aggregate in [
        payload.get("aggregate"),
        payload
            .get("record")
            .and_then(|record| record.get("aggregate")),
    ]
    .into_iter()
    .flatten()
    {
        scan_aggregate_artifacts(
            aggregate,
            true,
            &mut artifacts,
            &mut outcomes_omitted,
            &mut artifacts_omitted,
        );
    }
    for (index, attempt) in payload
        .get("attempts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let scanned = index < MAX_ATTEMPTS_SCANNED;
        if !scanned {
            attempts_omitted += 1;
        }
        if let Some(aggregate) = attempt.get("aggregate") {
            scan_aggregate_artifacts(
                aggregate,
                scanned,
                &mut artifacts,
                &mut outcomes_omitted,
                &mut artifacts_omitted,
            );
        }
    }
    (
        artifacts,
        attempts_omitted,
        outcomes_omitted,
        artifacts_omitted,
    )
}

/// Count omitted entries from array lengths while only visiting individual
/// artifacts that remain within the classification budget.
fn scan_aggregate_artifacts<'a>(
    aggregate: &'a Value,
    scanned: bool,
    artifacts: &mut Vec<&'a Value>,
    outcomes_omitted: &mut usize,
    artifacts_omitted: &mut usize,
) {
    let outcomes = aggregate
        .get("outcomes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if !scanned {
        *outcomes_omitted += outcomes.len();
        *artifacts_omitted += outcomes.iter().map(outcome_artifact_count).sum::<usize>();
        return;
    }
    for (index, outcome) in outcomes.iter().enumerate() {
        if index >= MAX_OUTCOMES_SCANNED {
            *outcomes_omitted += 1;
            *artifacts_omitted += outcome_artifact_count(outcome);
            continue;
        }
        let outcome_artifacts = outcome
            .get("artifacts")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let remaining = MAX_ARTIFACTS_SCANNED.saturating_sub(artifacts.len());
        let scanned_count = outcome_artifacts.len().min(remaining);
        artifacts.extend(outcome_artifacts.iter().take(scanned_count));
        *artifacts_omitted += outcome_artifacts.len().saturating_sub(scanned_count);
    }
}

fn outcome_artifact_count(outcome: &Value) -> usize {
    outcome
        .get("artifacts")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

fn has_promoted_candidate(payload: &Value) -> bool {
    promotion_has_candidate(payload.get("latest_promotion"))
        || promotion_has_candidate(payload.pointer("/metadata/latest_promotion"))
        || payload
            .get("attempts")
            .and_then(Value::as_array)
            .is_some_and(|attempts| {
                attempts
                    .iter()
                    .take(MAX_ATTEMPTS_SCANNED)
                    .any(|attempt| promotion_has_candidate(attempt.get("promotion")))
            })
        || successful_adoption(payload)
}

fn successful_adoption(payload: &Value) -> bool {
    let Some(adoption) = payload.get("candidate_adoption") else {
        return false;
    };
    adoption.get("state").and_then(Value::as_str) == Some("completed")
        && adoption.get("terminal_error").is_none_or(Value::is_null)
        && matches!(
            adoption.pointer("/result/status").and_then(Value::as_str),
            Some("review_ready" | "green_no_finalize")
        )
}

fn has_finalized_pr(payload: &Value) -> bool {
    [
        payload.get("finalization"),
        payload.get("cook_finalization"),
        payload.pointer("/metadata/cook_finalization"),
    ]
    .into_iter()
    .flatten()
    .any(|finalization| {
        finalization.get("status").and_then(Value::as_str) == Some("review_ready")
            && finalization
                .get("pr_url")
                .or_else(|| finalization.get("pull_request_url"))
                .and_then(Value::as_str)
                .is_some_and(|url| !url.trim().is_empty())
    })
}

fn promotion_has_candidate(promotion: Option<&Value>) -> bool {
    let Some(promotion) = promotion else {
        return false;
    };
    matches!(
        promotion.get("status").and_then(Value::as_str),
        Some("verification_pending" | "applied" | "gate_failed")
    ) && (promotion
        .get("patch_artifact")
        .or_else(|| promotion.get("patch"))
        .and_then(|artifact| {
            artifact
                .get("id")
                .or_else(|| artifact.get("artifact_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| promotion.get("patch_artifact_id").and_then(Value::as_str))
        .or_else(|| promotion.get("artifact_id").and_then(Value::as_str))
        .is_some_and(|id| !id.trim().is_empty()))
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

fn is_display_apply_artifact(artifact: &Value) -> bool {
    !["rejected", "false_positive"].into_iter().any(|key| {
        artifact
            .pointer(&format!("/metadata/{key}"))
            .and_then(Value::as_bool)
            == Some(true)
    })
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

    #[test]
    fn terminal_candidate_precedence_preserves_authoritative_evidence_across_retries() {
        let scenarios = [
            (
                "successful candidate plus empty retry",
                json!({
                    "attempts": [
                        { "aggregate": { "outcomes": [{ "artifacts": [mirrored_patch("candidate", 32_318)] }] } },
                        { "aggregate": { "outcomes": [{ "artifacts": [{ "id": "retry", "kind": "patch", "size_bytes": 0 }] }] } }
                    ]
                }),
                CandidateState::PatchAvailable,
            ),
            (
                "promoted PR",
                json!({ "finalization": { "status": "review_ready", "pr_url": "https://example.test/pull/1" } }),
                CandidateState::Finalized,
            ),
            (
                "adoption",
                json!({ "candidate_adoption": { "candidate_sha": "abc123", "state": "completed", "result": { "status": "review_ready" } } }),
                CandidateState::Promoted,
            ),
            (
                "failed final attempt",
                json!({
                    "attempts": [
                        { "promotion": { "status": "applied", "patch_artifact": { "id": "candidate" } } },
                        { "aggregate": { "outcomes": [{ "status": "failed", "artifacts": [] }] } }
                    ]
                }),
                CandidateState::Promoted,
            ),
        ];

        for (name, payload, expected) in scenarios {
            assert_eq!(classify_candidates(&payload).state(), expected, "{name}");
        }

        let no_candidate = json!({
            "attempts": [
                { "aggregate": { "outcomes": [{ "status": "succeeded", "artifacts": [{ "id": "empty", "kind": "patch", "size_bytes": 0 }] }] } },
                { "aggregate": { "outcomes": [{ "status": "failed", "artifacts": [] }] } }
            ]
        });
        assert_eq!(
            classify_candidates(&no_candidate).state(),
            CandidateState::Empty
        );
    }

    #[test]
    fn active_or_unsuccessful_adoption_never_becomes_candidate_evidence() {
        for state in ["verification_running", "failed", "cancelled"] {
            let result = classify_candidates(&json!({
                "candidate_adoption": {
                    "candidate_sha": "claimed-but-not-adopted",
                    "state": state,
                    "result": { "status": "review_ready" }
                }
            }));
            assert_eq!(result.state(), CandidateState::Unknown, "{state}");
        }

        let unsuccessful = classify_candidates(&json!({
            "candidate_adoption": {
                "candidate_sha": "blocked",
                "state": "completed",
                "result": { "status": "execution_budget_exhausted" }
            }
        }));
        assert_eq!(unsuccessful.state(), CandidateState::Unknown);

        let unsuccessful_finalization = classify_candidates(&json!({
            "finalization": {
                "status": "failed",
                "pr_url": "https://example.test/pull/1"
            }
        }));
        assert_eq!(unsuccessful_finalization.state(), CandidateState::Unknown);
    }

    #[test]
    fn rejected_or_false_positive_patches_never_become_candidates() {
        for flag in ["rejected", "false_positive"] {
            let mut artifact = json!({
                "id": flag,
                "kind": "patch",
                "size_bytes": 32_318,
                "url": "homeboy://agent-task/run/rejected/artifacts#patch",
                "metadata": { "executor_artifact_finalized": true }
            });
            artifact["metadata"][flag] = Value::Bool(true);
            let result = classify_candidates(&lab_fixture(vec![artifact]));
            assert_eq!(result.state(), CandidateState::Unknown, "{flag}");
            assert_eq!(result.available, 0, "{flag}");
        }
    }

    #[test]
    fn bounded_history_preserves_candidate_precedence_and_marks_incomplete_scans() {
        let attempts = (0..=MAX_ATTEMPTS_SCANNED)
            .map(|attempt| {
                json!({
                    "aggregate": { "outcomes": [{ "artifacts": [
                        if attempt == 0 { mirrored_patch("canonical", 32_318) }
                        else { json!({ "id": format!("empty-{attempt}"), "kind": "patch", "size_bytes": 0 }) }
                    ] }] }
                })
            })
            .collect::<Vec<_>>();
        let result = classify_candidates(&json!({ "attempts": attempts }));
        assert_eq!(result.state(), CandidateState::PatchAvailable);
        assert_eq!(result.attempts_omitted, 1);
        assert!(result.is_degraded());

        let artifacts = (0..=MAX_ARTIFACTS_SCANNED)
            .map(
                |index| json!({ "id": format!("empty-{index}"), "kind": "patch", "size_bytes": 0 }),
            )
            .collect::<Vec<_>>();
        let result = classify_candidates(&json!({
            "aggregate": { "outcomes": [{ "artifacts": artifacts }] }
        }));
        assert_eq!(result.state(), CandidateState::Unknown);
        assert_eq!(result.artifacts_omitted, 1);
        assert!(result.is_degraded());

        let outcomes = (0..=MAX_OUTCOMES_SCANNED)
            .map(|_| json!({ "artifacts": [] }))
            .collect::<Vec<_>>();
        let result = classify_candidates(&json!({
            "aggregate": { "outcomes": outcomes }
        }));
        assert_eq!(result.state(), CandidateState::Unknown);
        assert_eq!(result.outcomes_omitted, 1);
        assert!(result.is_degraded());
    }

    #[test]
    fn bounded_history_counts_all_multi_extra_omissions_from_structure() {
        let root_outcomes = std::iter::once(json!({
            "artifacts": (0..MAX_ARTIFACTS_SCANNED + 3)
                .map(|index| json!({ "id": format!("root-{index}"), "kind": "patch" }))
                .collect::<Vec<_>>()
        }))
        .chain((0..MAX_OUTCOMES_SCANNED + 2).map(|index| {
            json!({
                "artifacts": [{ "id": format!("outcome-{index}"), "kind": "patch" }]
            })
        }))
        .collect::<Vec<_>>();
        let attempts = (0..MAX_ATTEMPTS_SCANNED + 3)
            .map(|index| {
                let outcomes = if index < MAX_ATTEMPTS_SCANNED {
                    vec![]
                } else {
                    vec![json!({
                        "artifacts": [
                            { "id": format!("attempt-{index}-a"), "kind": "patch" },
                            { "id": format!("attempt-{index}-b"), "kind": "patch" }
                        ]
                    })]
                };
                json!({ "aggregate": { "outcomes": outcomes } })
            })
            .collect::<Vec<_>>();
        let result = classify_candidates(&json!({
            "aggregate": { "outcomes": root_outcomes },
            "attempts": attempts,
        }));

        assert_eq!(result.attempts_omitted, 3);
        assert_eq!(result.outcomes_omitted, 6);
        assert_eq!(result.artifacts_omitted, 267);
        assert!(result.is_degraded());
    }

    #[test]
    fn review_envelope_reads_terminal_facts_from_its_record() {
        let promoted = classify_candidates(&json!({
            "record": { "metadata": { "latest_promotion": {
                "status": "applied", "patch_artifact": { "id": "patch" }
            }}}
        }));
        assert_eq!(promoted.state(), CandidateState::Promoted);

        let finalized = classify_candidates(&json!({
            "record": { "metadata": { "cook_finalization": {
                "status": "review_ready", "pr_url": "https://example.test/pull/1"
            }}}
        }));
        assert_eq!(finalized.state(), CandidateState::Finalized);

        let adopted = classify_candidates(&json!({
            "record": { "candidate_adoption": {
                "state": "completed", "result": { "status": "review_ready" }
            }}
        }));
        assert_eq!(adopted.state(), CandidateState::Promoted);
    }
}
