use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum CandidateState {
    Finalized,
    ApplyReady,
    PatchAvailable,
    NoChangesProduced,
    Empty,
    Missing,
    Unreadable,
    Conflicting,
    RetainedOnly,
    NotAvailable,
    #[default]
    Unknown,
}

impl CandidateState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Finalized => "finalized",
            Self::ApplyReady => "apply_ready",
            Self::PatchAvailable => "patch_available",
            Self::NoChangesProduced => "no_changes_produced",
            Self::Empty => "empty",
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
            Self::Conflicting => "conflicting",
            Self::RetainedOnly => "retained_only",
            Self::NotAvailable => "not_available",
            Self::Unknown => "unknown",
        }
    }

    pub(crate) fn is_available(self) -> bool {
        matches!(
            self,
            Self::Finalized | Self::ApplyReady | Self::PatchAvailable
        )
    }
}

/// The canonical terminal result for a Cook's candidate evidence. Evidence is
/// monotonic: a finalized PR, retained/apply-ready candidate, or typed candidate
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
    pub(crate) changed_files: usize,
    pub(crate) changed_files_unknown_patches: usize,
    pub(crate) provider_executions: Option<usize>,
    pub(crate) finalized: bool,
    pub(crate) retained: bool,
    pub(crate) attempts_omitted: usize,
    pub(crate) outcomes_omitted: usize,
    pub(crate) artifacts_omitted: usize,
    no_changes_produced: bool,
    not_available: bool,
}

impl CandidateResult {
    pub(crate) fn state(self) -> CandidateState {
        if self.finalized {
            CandidateState::Finalized
        } else if self.retained {
            CandidateState::ApplyReady
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
        } else if self.not_available {
            CandidateState::NotAvailable
        } else if self.no_changes_produced {
            CandidateState::NoChangesProduced
        } else {
            CandidateState::Unknown
        }
    }

    pub(crate) fn is_degraded(self) -> bool {
        self.attempts_omitted > 0 || self.outcomes_omitted > 0 || self.artifacts_omitted > 0
    }
}

/// Classify only the agents-layer canonical candidate projection.
pub(crate) fn classify_candidates(payload: &Value) -> CandidateResult {
    payload
        .get("canonical_candidate")
        .or_else(|| payload.pointer("/durable_candidate/canonical_candidate"))
        .and_then(candidate_result_from_projection)
        .unwrap_or_default()
}

fn candidate_result_from_projection(value: &Value) -> Option<CandidateResult> {
    if value.get("schema").and_then(Value::as_str) != Some("homeboy/agent-task-candidate/v1") {
        return None;
    }
    let state = match value.get("state").and_then(Value::as_str)? {
        "finalized" => CandidateState::Finalized,
        "apply_ready" => CandidateState::ApplyReady,
        "patch_available" => CandidateState::PatchAvailable,
        "no_changes_produced" => CandidateState::NoChangesProduced,
        "empty" => CandidateState::Empty,
        "missing" => CandidateState::Missing,
        "unreadable" => CandidateState::Unreadable,
        "conflicting" => CandidateState::Conflicting,
        "retained_only" => CandidateState::RetainedOnly,
        "unknown" => CandidateState::Unknown,
        "not_available" => CandidateState::NotAvailable,
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
        available: count("patch_available")
            .max(usize::from(state == CandidateState::PatchAvailable)),
        empty: count("empty").max(usize::from(state == CandidateState::Empty)),
        missing: count("missing").max(usize::from(state == CandidateState::Missing)),
        unreadable: count("unreadable").max(usize::from(state == CandidateState::Unreadable)),
        conflicting: count("conflicting").max(usize::from(state == CandidateState::Conflicting)),
        retained_only: count("retained_only")
            .max(usize::from(state == CandidateState::RetainedOnly)),
        unknown: count("unknown"),
        diff_bytes: value
            .get("diff_bytes")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        changed_files: value
            .get("changed_files")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or_default(),
        changed_files_unknown_patches: value
            .get("changed_files_unknown_patches")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or_default(),
        provider_executions: value
            .get("provider_executions")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok()),
        finalized: state == CandidateState::Finalized,
        retained: state == CandidateState::ApplyReady,
        attempts_omitted: omitted("attempts_omitted"),
        outcomes_omitted: omitted("outcomes_omitted"),
        artifacts_omitted: omitted("artifacts_omitted"),
        no_changes_produced: state == CandidateState::NoChangesProduced,
        not_available: state == CandidateState::NotAvailable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_projection_is_the_only_candidate_source() {
        for state in [
            "finalized",
            "apply_ready",
            "patch_available",
            "no_changes_produced",
            "retained_only",
            "not_available",
        ] {
            let projection = json!({
                "schema": "homeboy/agent-task-candidate/v1", "state": state,
            });
            for payload in [
                json!({ "canonical_candidate": projection }),
                json!({ "durable_candidate": { "canonical_candidate": projection } }),
            ] {
                assert_eq!(classify_candidates(&payload).state().as_str(), state);
            }
        }
        for payload in [
            json!({}),
            json!({ "artifact_refs": [{ "kind": "patch", "size_bytes": 100 }] }),
            json!({ "aggregate": { "outcomes": [{ "status": "no_op" }] } }),
            json!({ "finalization": { "status": "review_ready", "pr_url": "https://example.test/pr/1" } }),
            json!({ "canonical_candidate": { "schema": "homeboy/agent-task-candidate/v1", "state": "promoted" } }),
        ] {
            assert_eq!(
                classify_candidates(&payload).state(),
                CandidateState::Unknown
            );
        }
    }

    #[test]
    fn omitted_scan_cannot_prove_no_changes() {
        let result = classify_candidates(&json!({ "canonical_candidate": {
            "schema": "homeboy/agent-task-candidate/v1", "state": "no_changes_produced",
            "scan": { "attempts_omitted": 1, "outcomes_omitted": 2, "artifacts_omitted": 3 },
        }}));
        assert!(result.is_degraded());
        assert_eq!(result.state(), CandidateState::Unknown);
    }
}
