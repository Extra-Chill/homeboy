use homeboy_lifecycle_contract::RunExecutionState;
use serde_json::Value;

use super::AgentTaskRunRecord;

/// A detached Cook root's admission phase, derived once from its run state and
/// `detached_cook_handoff` (#18908). `handoff.state` is not stored; a stored
/// label is ignored in favor of the derived phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookAdmissionPhase<'a> {
    /// Live and awaiting its first attempt at admission `step`.
    Pending { step: Option<&'a str> },
    /// Live with a reserved first attempt that is not yet published.
    Materializing { attempt_run_id: &'a str },
    /// The first attempt took custody (stored as `redirected`).
    Materialized,
    /// The launch failed before any attempt took custody.
    ExitedBeforeHandoff,
    /// Terminal without an attempt; run state wins over any stale step.
    Ended { state: RunExecutionState },
}

impl<'a> CookAdmissionPhase<'a> {
    pub fn of(record: &'a AgentTaskRunRecord) -> Option<Self> {
        let handoff = &record.metadata["detached_cook_handoff"];
        handoff.as_object()?;
        Some(if handoff["admission_state"] == "failed" {
            Self::ExitedBeforeHandoff
        } else if handoff_materialized(handoff) {
            Self::Materialized
        } else if record.state().is_terminal() {
            Self::Ended {
                state: record.state(),
            }
        } else if let Some(attempt_run_id) = handoff["materializing_attempt_run_id"].as_str() {
            Self::Materializing { attempt_run_id }
        } else {
            Self::Pending {
                step: handoff["admission_state"].as_str(),
            }
        })
    }

    /// The label older records stored as `detached_cook_handoff.state`.
    pub fn handoff_label(self) -> &'static str {
        match self {
            Self::Materialized => "redirected",
            Self::ExitedBeforeHandoff => "exited_before_handoff",
            _ => "pending",
        }
    }
}

/// [`CookAdmissionPhase::Materialized`] for a projected row's handoff object.
pub(crate) fn handoff_materialized(handoff: &Value) -> bool {
    handoff["admission_state"] != "failed"
        && (handoff["admission_state"] == "materialized" || handoff["attempt_run_id"].is_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use CookAdmissionPhase::{Ended, ExitedBeforeHandoff, Materialized, Pending};
    use RunExecutionState::{Cancelled, Failed, Queued, Running, Succeeded};

    fn root(state: RunExecutionState, handoff: Value, admission: Value) -> AgentTaskRunRecord {
        serde_json::from_value(json!({
            "schema": "homeboy/agent-task-run/v1", "run_id": "cook", "plan_id": "plan",
            "state": state, "submitted_at": "2026-01-01T00:00:00Z", "plan_path": "/plan",
            "metadata": {
                "detached_cook_handoff": handoff,
                "unmaterialized_cook_admission": admission,
            },
        }))
        .expect("record")
    }

    /// Every row shape of the #18908 evidence table, plus the live phases,
    /// classifies identically with or without its stored label, and the
    /// derived label is the one it stored.
    #[test]
    fn evidence_row_shapes_derive_their_stored_handoff_label() {
        let (queued, blocked) = (
            json!({"state": "queued"}),
            json!({"state": "blocked_runner_unavailable"}),
        );
        let rows = [
            (
                Succeeded,
                "redirected",
                "materialized",
                Some("a1"),
                json!({"state": "materialized"}),
                Materialized,
            ),
            (
                Succeeded,
                "redirected",
                "materialized",
                Some("a1"),
                Value::Null,
                Materialized,
            ),
            (
                Failed,
                "pending",
                "unmaterialized",
                None,
                queued.clone(),
                Ended { state: Failed },
            ),
            (
                Failed,
                "pending",
                "unmaterialized",
                None,
                blocked.clone(),
                Ended { state: Failed },
            ),
            (
                Cancelled,
                "pending",
                "unmaterialized",
                None,
                queued.clone(),
                Ended { state: Cancelled },
            ),
            (
                Cancelled,
                "pending",
                "unmaterialized",
                None,
                blocked.clone(),
                Ended { state: Cancelled },
            ),
            (
                Failed,
                "exited_before_handoff",
                "failed",
                None,
                Value::Null,
                ExitedBeforeHandoff,
            ),
            (
                Failed,
                "exited_before_handoff",
                "failed",
                None,
                queued.clone(),
                ExitedBeforeHandoff,
            ),
            (
                Failed,
                "exited_before_handoff",
                "failed",
                None,
                blocked,
                ExitedBeforeHandoff,
            ),
            // A redirected root whose Cook index went missing is failed in place.
            (
                Failed,
                "exited_before_handoff",
                "failed",
                Some("a1"),
                Value::Null,
                ExitedBeforeHandoff,
            ),
            (
                Failed,
                "pending",
                "supervising",
                None,
                Value::Null,
                Ended { state: Failed },
            ),
            (
                Cancelled,
                "pending",
                "pre_supervisor",
                None,
                Value::Null,
                Ended { state: Cancelled },
            ),
            (
                Queued,
                "pending",
                "unmaterialized",
                None,
                queued,
                Pending {
                    step: Some("unmaterialized"),
                },
            ),
            (
                Running,
                "pending",
                "supervising",
                None,
                Value::Null,
                Pending {
                    step: Some("supervising"),
                },
            ),
        ];
        for (state, stored_label, step, attempt, admission, expected) in rows {
            let mut handoff =
                json!({"cook_id": "cook", "state": stored_label, "admission_state": step});
            if let Some(attempt) = attempt {
                handoff["attempt_run_id"] = json!(attempt);
            }
            let legacy = root(state, handoff.clone(), admission.clone());
            assert_eq!(CookAdmissionPhase::of(&legacy), Some(expected), "{handoff}");
            assert_eq!(expected.handoff_label(), stored_label, "{handoff}");
            handoff.as_object_mut().unwrap().remove("state");
            let current = root(state, handoff.clone(), admission);
            assert_eq!(
                CookAdmissionPhase::of(&current),
                Some(expected),
                "{handoff}"
            );
        }

        let materializing = root(
            Running,
            json!({"admission_state": "supervising", "materializing_attempt_run_id": "a1"}),
            Value::Null,
        );
        assert_eq!(
            CookAdmissionPhase::of(&materializing),
            Some(CookAdmissionPhase::Materializing {
                attempt_run_id: "a1"
            })
        );
        assert_eq!(
            CookAdmissionPhase::of(&root(Queued, Value::Null, Value::Null)),
            None
        );
    }
}
