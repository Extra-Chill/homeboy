//! Cook attempt lineage: its one durable record and its one reader (#15567).
//!
//! Every recipe attempt appended since #15567 carries its own
//! [`CookAttemptLineage`], written in the same recipe write that appends the
//! attempt and validated with the recipe. That record is authoritative.
//!
//! Provider artifact provenance and generic lifecycle retry reservations retain
//! their separate evidence and admission roles. They do not manufacture a Cook
//! lineage edge when its recipe record is absent.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_task_lifecycle::AgentTaskLifecycleStore;
use crate::agent_task_scheduler::AgentTaskPlan;
use homeboy_core::Result;

/// How an attempt came to continue its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookLineageKind {
    /// A lifecycle retry of the source attempt.
    Retry,
    /// A remediation built from the source's promoted candidate to make
    /// failed gates pass.
    GateFix,
    /// A follow-up that only fills in the review form for the source's
    /// candidate.
    ReviewForm,
    /// A replacement execution or pre-provider correction of the source.
    Replacement,
}

/// One attempt's edge back to the attempt it continues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CookAttemptLineage {
    pub source_run_id: String,
    pub kind: CookLineageKind,
    /// The candidate patch a remediation was built from. A retry has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_patch_sha256: Option<String>,
}

impl CookAttemptLineage {
    pub(crate) fn retry(source_run_id: &str) -> Self {
        Self {
            source_run_id: source_run_id.to_string(),
            kind: CookLineageKind::Retry,
            source_patch_sha256: None,
        }
    }

    pub(crate) fn replacement(replaced_run_id: &str) -> Self {
        Self {
            source_run_id: replaced_run_id.to_string(),
            kind: CookLineageKind::Replacement,
            source_patch_sha256: None,
        }
    }
}

/// The immutable Cook lineage authority persisted with a recipe attempt.
pub(crate) fn recipe_attempt_lineage(
    attempt: &super::cook_recipe::AgentTaskCookRecipeAttempt,
) -> Option<CookAttemptLineage> {
    attempt.lineage.clone()
}

/// Replacement executions retain their bound remediation's execution purpose.
/// Resolve that purpose through persisted edges, never through plan fields.
pub(crate) fn execution_lineage(
    attempt: &super::cook_recipe::AgentTaskCookRecipeAttempt,
    attempts: &[super::cook_recipe::AgentTaskCookRecipeAttempt],
) -> Option<CookAttemptLineage> {
    let mut current = attempt;
    for _ in 0..=attempts.len() {
        let edge = recipe_attempt_lineage(current)?;
        if edge.kind != CookLineageKind::Replacement {
            return Some(edge);
        }
        current = attempts
            .iter()
            .find(|parent| parent.run_id == edge.source_run_id)?;
    }
    None
}

/// Construct a remediation edge at the current plan-producer boundary.
/// Persisted Cook decisions read the resulting recipe edge, not this payload.
pub(crate) fn plan_lineage(plan: &AgentTaskPlan) -> Option<CookAttemptLineage> {
    let [task] = plan.tasks.as_slice() else {
        return None;
    };
    let cook_loop = &task.inputs["cook_loop"];
    let provenance = &cook_loop["artifact_provenance"];
    let source = provenance["source_run_id"]
        .as_str()
        .filter(|source| !source.is_empty())?;
    Some(CookAttemptLineage {
        source_run_id: source.to_string(),
        kind: if cook_loop["review_form_required"] == true {
            CookLineageKind::ReviewForm
        } else {
            CookLineageKind::GateFix
        },
        source_patch_sha256: provenance["source_patch_artifact_sha256"]
            .as_str()
            .map(str::to_string),
    })
}

/// Admission provenance of an unbound generic lifecycle retry. This is not a
/// replacement for a recipe-owned Cook edge.
pub(crate) fn lifecycle_retry_parent_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<String>> {
    let record = lifecycle_store.read_record(run_id)?;
    Ok(record
        .metadata
        .get("retry_of")
        .and_then(Value::as_str)
        .filter(|source| !source.is_empty())
        .map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::AgentTaskRequest;
    use serde_json::json;

    fn plan_with(cook_loop: Value) -> AgentTaskPlan {
        let mut task: AgentTaskRequest = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-request/v1",
            "task_id": "cook-x-gate-fix-2",
            "group_key": "x",
            "executor": { "backend": "opencode" },
            "instructions": "fix",
        }))
        .unwrap();
        task.inputs = json!({ "cook_loop": cook_loop });
        AgentTaskPlan::new("plan", vec![task])
    }

    fn gate_fix_plan(source: &str, sha: &str) -> AgentTaskPlan {
        plan_with(json!({
            "review_form_required": false,
            "artifact_provenance": {
                "source_run_id": source,
                "source_patch_artifact_sha256": sha,
            },
        }))
    }

    /// The incident: a gate-fix successor has provenance and no `retry_of`.
    /// It must still be recognized as continuing its source.
    #[test]
    fn a_gate_fix_without_retry_of_still_continues_its_source() {
        let lineage = plan_lineage(&gate_fix_plan("run-1", "abc")).unwrap();
        assert_eq!(lineage.source_run_id, "run-1");
        assert_eq!(lineage.kind, CookLineageKind::GateFix);
        assert_eq!(lineage.source_patch_sha256.as_deref(), Some("abc"));
    }

    #[test]
    fn explicit_retry_edge_does_not_reinterpret_artifact_provenance() {
        let attempt = recipe_attempt(
            gate_fix_plan("run-1", "abc"),
            Some(CookAttemptLineage::retry("run-0")),
        );
        let lineage = recipe_attempt_lineage(&attempt).unwrap();
        assert_eq!(lineage.source_run_id, "run-0");
        assert_eq!(lineage.kind, CookLineageKind::Retry);
        assert_eq!(lineage.source_patch_sha256, None);
    }

    #[test]
    fn review_form_follow_ups_are_marked() {
        let plan = plan_with(json!({
            "review_form_required": true,
            "artifact_provenance": { "source_run_id": "run-1" },
        }));
        assert_eq!(
            plan_lineage(&plan).unwrap().kind,
            CookLineageKind::ReviewForm
        );
    }

    fn recipe_attempt(
        plan: AgentTaskPlan,
        lineage: Option<CookAttemptLineage>,
    ) -> crate::agent_task_service::AgentTaskCookRecipeAttempt {
        crate::agent_task_service::AgentTaskCookRecipeAttempt {
            attempt: 2,
            run_id: "run-2".to_string(),
            plan,
            lineage,
        }
    }

    /// The durable record is authoritative over anything derivable.
    #[test]
    fn a_persisted_lineage_wins_over_derivation() {
        let attempt = recipe_attempt(
            gate_fix_plan("run-1", "abc"),
            Some(CookAttemptLineage::retry("run-0")),
        );
        let lineage = recipe_attempt_lineage(&attempt).unwrap();
        assert_eq!(lineage, CookAttemptLineage::retry("run-0"));
    }

    /// Artifact evidence cannot grant absent recipe lineage authority.
    #[test]
    fn an_absent_edge_is_not_derived_from_artifact_provenance() {
        let attempt = recipe_attempt(gate_fix_plan("run-1", "abc"), None);
        assert_eq!(recipe_attempt_lineage(&attempt), None);
    }

    #[test]
    fn the_record_round_trips_and_omits_an_absent_patch() {
        let value = serde_json::to_value(CookAttemptLineage::retry("run-1")).unwrap();
        assert_eq!(value, json!({ "source_run_id": "run-1", "kind": "retry" }));
        assert_eq!(
            serde_json::from_value::<CookAttemptLineage>(value).unwrap(),
            CookAttemptLineage::retry("run-1")
        );
    }

    #[test]
    fn an_initial_attempt_has_no_lineage() {
        assert_eq!(plan_lineage(&plan_with(json!({}))), None);
    }
}
