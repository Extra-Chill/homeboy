//! Cook attempt lineage: its one durable record and its one reader (#15567).
//!
//! Every recipe attempt appended since #15567 carries its own
//! [`CookAttemptLineage`], written in the same recipe write that appends the
//! attempt and validated with the recipe. That record is authoritative.
//!
//! Attempts recorded before it have no such record. For those, lineage is
//! derived from the two places it used to be written by two different
//! producers:
//!
//! - `metadata.retry_of` on the lifecycle record, written by the lifecycle
//!   retry reserver;
//! - `inputs.cook_loop.artifact_provenance` on the attempt's plan, written by
//!   gate-fix and review-form remediation.
//!
//! Each decision site used to read only the representation its own producer
//! writes. So a gate-fix successor, which carries provenance and no
//! `retry_of`, was rejected by retry admission as "not the durable retry of its
//! source attempt", and was invisible to the retry-lineage walk. Every lineage
//! decision now reads through [`attempt_lineage`], which understands both.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_task_lifecycle::{self, AgentTaskLifecycleStore};
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
    /// Another execution of the same attempt number, replacing the source.
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

/// The lineage of a recipe attempt: its durable record when it has one,
/// otherwise derived from its lifecycle `metadata` (when the caller has the
/// record) and its plan. Every lineage decision reads through this.
pub(crate) fn recipe_attempt_lineage(
    attempt: &super::cook_recipe::AgentTaskCookRecipeAttempt,
    metadata: Option<&Value>,
) -> Option<CookAttemptLineage> {
    attempt
        .lineage
        .clone()
        .or_else(|| attempt_lineage(metadata.unwrap_or(&Value::Null), Some(&attempt.plan)))
}

/// Legacy derivation, for attempts recorded before lineage was persisted:
/// the lineage edge of one attempt, from its lifecycle metadata and plan.
///
/// `retry_of` wins when both are present: it is written by the lifecycle
/// reserver with the record itself, so it is the stronger evidence.
pub(crate) fn attempt_lineage(
    metadata: &Value,
    plan: Option<&AgentTaskPlan>,
) -> Option<CookAttemptLineage> {
    if let Some(source) = metadata
        .get("retry_of")
        .and_then(Value::as_str)
        .filter(|source| !source.is_empty())
    {
        return Some(CookAttemptLineage {
            source_run_id: source.to_string(),
            kind: CookLineageKind::Retry,
            source_patch_sha256: None,
        });
    }
    plan.and_then(plan_lineage)
}

/// The remediation edge carried by a plan alone. Recipe attempts carry their
/// plan before any lifecycle record exists, so this is also what recipe-only
/// decisions read.
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

/// The run `run_id` continues, read from its durable record and plan.
pub(crate) fn lineage_parent_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<String>> {
    let record = lifecycle_store.read_record(run_id)?;
    let plan = agent_task_lifecycle::load_plan_in_store(lifecycle_store, run_id).ok();
    Ok(attempt_lineage(&record.metadata, plan.as_ref()).map(|lineage| lineage.source_run_id))
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
        let lineage = attempt_lineage(&json!({}), Some(&gate_fix_plan("run-1", "abc"))).unwrap();
        assert_eq!(lineage.source_run_id, "run-1");
        assert_eq!(lineage.kind, CookLineageKind::GateFix);
        assert_eq!(lineage.source_patch_sha256.as_deref(), Some("abc"));
    }

    #[test]
    fn retry_of_is_a_retry_and_wins_over_provenance() {
        let lineage = attempt_lineage(
            &json!({ "retry_of": "run-0" }),
            Some(&gate_fix_plan("run-1", "abc")),
        )
        .unwrap();
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
        let lineage =
            recipe_attempt_lineage(&attempt, Some(&json!({ "retry_of": "run-9" }))).unwrap();
        assert_eq!(lineage, CookAttemptLineage::retry("run-0"));
    }

    /// Attempts recorded before #15567 still resolve, from the legacy forms.
    #[test]
    fn a_legacy_attempt_falls_back_to_derivation() {
        let attempt = recipe_attempt(gate_fix_plan("run-1", "abc"), None);
        assert_eq!(
            recipe_attempt_lineage(&attempt, None).unwrap().kind,
            CookLineageKind::GateFix
        );
        assert_eq!(
            recipe_attempt_lineage(&attempt, Some(&json!({ "retry_of": "run-0" })))
                .unwrap()
                .source_run_id,
            "run-0"
        );
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
        assert_eq!(
            attempt_lineage(&json!({}), Some(&plan_with(json!({})))),
            None
        );
        assert_eq!(attempt_lineage(&json!({ "retry_of": "" }), None), None);
    }
}
