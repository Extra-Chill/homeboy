use super::{AgentTaskRunRecord, AgentTaskRunState, LocalOwnerLiveness};
use crate::agent_task_schedule::AgentTaskPlan;
use serde::{Deserialize, Serialize};

pub const AGENT_TASK_LIFECYCLE_ACTION_ELIGIBILITY_SCHEMA: &str =
    "homeboy/agent-task-lifecycle-action-eligibility/v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLifecycleAction {
    Cancel,
    Resume,
    Retry,
    Review,
    Promote,
    Reconcile,
}

impl AgentTaskLifecycleAction {
    /// Review is a non-mutating read. Every other lifecycle action mutates the
    /// durable run or a related resource.
    pub const fn is_mutating(self) -> bool {
        !matches!(self, Self::Review)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskActionConfirmation {
    None,
    Required,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskActionAvailability {
    Available,
    Unavailable,
    Indeterminate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLifecycleActionEligibility {
    pub action: AgentTaskLifecycleAction,
    pub availability: AgentTaskActionAvailability,
    pub reason: String,
    pub confirmation: AgentTaskActionConfirmation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_inputs: Vec<String>,
    pub idempotent: bool,
    pub requires_revalidation: bool,
    pub result_resource_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskLifecycleActionEligibilityReport {
    pub schema: String,
    pub run_id: String,
    pub state: AgentTaskRunState,
    pub location: String,
    pub actions: Vec<AgentTaskLifecycleActionEligibility>,
}

/// Canonical non-mutating lifecycle action policy for CLI, daemon, and API projections.
/// Mutation commands revalidate these snapshot decisions under their own locks.
pub fn lifecycle_action_eligibility(
    record: &AgentTaskRunRecord,
    plan: Option<&AgentTaskPlan>,
) -> AgentTaskLifecycleActionEligibilityReport {
    let cancellation = match super::cancellation::ensure_rooted_exact_cancellation_supported(record)
    {
        Ok(()) => available("run is non-terminal and its ownership transport is supported"),
        Err(error) => unavailable(error.message),
    };
    let resume = resume_availability(record);
    let retry = retry_availability(record, plan);
    let promotion = if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::CandidateRecoverable
            | AgentTaskRunState::PartialRecoverable
    ) {
        indeterminate("promotion requires a target worktree and candidate preflight")
    } else {
        unavailable("run does not retain a potentially promotable candidate")
    };
    let reconcile = if record.state == AgentTaskRunState::Running
        && !matches!(record.local_owner_liveness(), LocalOwnerLiveness::Live)
    {
        available("running record has no authoritative live local owner")
    } else {
        unavailable("reconciliation is offered only for running records without a live local owner")
    };

    AgentTaskLifecycleActionEligibilityReport {
        schema: AGENT_TASK_LIFECYCLE_ACTION_ELIGIBILITY_SCHEMA.to_string(),
        run_id: record.run_id.clone(),
        state: record.state,
        location: record
            .runner_id()
            .map(|runner| format!("runner:{runner}"))
            .unwrap_or_else(|| "local_controller".to_string()),
        actions: vec![
            action(
                AgentTaskLifecycleAction::Cancel,
                cancellation,
                AgentTaskActionConfirmation::Required,
                Vec::new(),
                false,
                "agent_task_run",
            ),
            action(
                AgentTaskLifecycleAction::Resume,
                resume,
                AgentTaskActionConfirmation::None,
                Vec::new(),
                record
                    .metadata
                    .get("unmaterialized_cook_admission")
                    .is_some_and(serde_json::Value::is_object),
                "agent_task_run",
            ),
            action(
                AgentTaskLifecycleAction::Retry,
                retry,
                AgentTaskActionConfirmation::Required,
                Vec::new(),
                false,
                "agent_task_run",
            ),
            action(
                AgentTaskLifecycleAction::Review,
                available("review is a non-mutating read available for every durable run"),
                AgentTaskActionConfirmation::None,
                Vec::new(),
                true,
                "agent_task_review",
            ),
            action(
                AgentTaskLifecycleAction::Promote,
                promotion,
                AgentTaskActionConfirmation::Required,
                vec!["to_worktree"],
                false,
                "agent_task_promotion",
            ),
            action(
                AgentTaskLifecycleAction::Reconcile,
                reconcile,
                AgentTaskActionConfirmation::None,
                Vec::new(),
                true,
                "agent_task_run",
            ),
        ],
    }
}

fn action(
    action: AgentTaskLifecycleAction,
    decision: (AgentTaskActionAvailability, String),
    confirmation: AgentTaskActionConfirmation,
    required_inputs: Vec<&str>,
    idempotent: bool,
    result_resource_type: &str,
) -> AgentTaskLifecycleActionEligibility {
    AgentTaskLifecycleActionEligibility {
        action,
        availability: decision.0,
        reason: decision.1,
        confirmation,
        required_inputs: required_inputs.into_iter().map(str::to_string).collect(),
        idempotent,
        requires_revalidation: true,
        result_resource_type: result_resource_type.to_string(),
    }
}

fn available(reason: impl Into<String>) -> (AgentTaskActionAvailability, String) {
    (AgentTaskActionAvailability::Available, reason.into())
}

fn unavailable(reason: impl Into<String>) -> (AgentTaskActionAvailability, String) {
    (AgentTaskActionAvailability::Unavailable, reason.into())
}

fn indeterminate(reason: impl Into<String>) -> (AgentTaskActionAvailability, String) {
    (AgentTaskActionAvailability::Indeterminate, reason.into())
}

fn resume_availability(record: &AgentTaskRunRecord) -> (AgentTaskActionAvailability, String) {
    if record.metadata.get("queue_quarantine").is_some() {
        return unavailable("run is quarantined and must be re-armed before resume");
    }
    match record.state {
        AgentTaskRunState::Queued => available("queued run can re-enter execution"),
        AgentTaskRunState::Running => match record.local_owner_liveness() {
            LocalOwnerLiveness::Live => unavailable("run has an authoritative live local owner"),
            LocalOwnerLiveness::Unverifiable => {
                indeterminate("local owner identity cannot be verified without reconciliation")
            }
            LocalOwnerLiveness::Dead | LocalOwnerLiveness::Absent => {
                available("running record has no live local owner")
            }
        },
        _ => unavailable("terminal runs cannot be resumed"),
    }
}

fn retry_availability(
    record: &AgentTaskRunRecord,
    plan: Option<&AgentTaskPlan>,
) -> (AgentTaskActionAvailability, String) {
    if !matches!(
        record.state,
        AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
            | AgentTaskRunState::PartialFailure
    ) {
        return unavailable("retry requires a failed, cancelled, or partial-failure run");
    }
    if record.acceptance.as_ref().is_some_and(|acceptance| {
        acceptance.repair_attempts > 1
            || (acceptance.repair_attempts > 0
                && acceptance.verdict != super::AgentTaskAcceptanceVerdict::Rejected)
    }) {
        return unavailable("acceptance rejection repair budget is exhausted for this lineage");
    }
    match plan {
        Some(plan) if super::plan_has_retry_materialization_identity(plan) => {
            indeterminate(
                "durable plan is materializable; retry lineage and runtime admission require store preflight",
            )
        }
        Some(_) => unavailable("durable plan lacks retry materialization identity"),
        None => indeterminate("durable plan was unavailable to the eligibility projection"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(state: AgentTaskRunState, stale: bool) -> AgentTaskRunRecord {
        let mut record: AgentTaskRunRecord = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-run/v1", "run_id": "run", "plan_id": "plan",
            "state": state, "submitted_at": "2026-01-01T00:00:00Z", "plan_path": "/plan"
        }))
        .expect("record");
        record.metadata["stale_running"] = serde_json::json!(stale);
        record
    }

    fn decision(
        report: &AgentTaskLifecycleActionEligibilityReport,
        action: AgentTaskLifecycleAction,
    ) -> AgentTaskActionAvailability {
        report
            .actions
            .iter()
            .find(|candidate| candidate.action == action)
            .expect("action")
            .availability
    }

    #[test]
    fn lifecycle_action_contract_covers_every_durable_state_without_false_promotion() {
        for state in [
            AgentTaskRunState::Queued,
            AgentTaskRunState::Running,
            AgentTaskRunState::Succeeded,
            AgentTaskRunState::CandidateRecoverable,
            AgentTaskRunState::PartialRecoverable,
            AgentTaskRunState::PartialFailure,
            AgentTaskRunState::Failed,
            AgentTaskRunState::Cancelled,
        ] {
            let report = lifecycle_action_eligibility(&record(state, false), None);
            assert_eq!(report.actions.len(), 6);
            assert_eq!(
                decision(&report, AgentTaskLifecycleAction::Review),
                AgentTaskActionAvailability::Available
            );
            if state.is_terminal() {
                assert_eq!(
                    decision(&report, AgentTaskLifecycleAction::Cancel),
                    AgentTaskActionAvailability::Unavailable
                );
            }
            if matches!(
                state,
                AgentTaskRunState::Succeeded
                    | AgentTaskRunState::CandidateRecoverable
                    | AgentTaskRunState::PartialRecoverable
            ) {
                assert_eq!(
                    decision(&report, AgentTaskLifecycleAction::Promote),
                    AgentTaskActionAvailability::Indeterminate
                );
            }
        }
    }

    #[test]
    fn stale_metadata_does_not_override_a_quarantine_guard() {
        let mut record = record(AgentTaskRunState::Running, true);
        record.metadata["queue_quarantine"] = serde_json::json!({"reason": "fixture"});
        let report = lifecycle_action_eligibility(&record, None);

        assert_eq!(
            decision(&report, AgentTaskLifecycleAction::Resume),
            AgentTaskActionAvailability::Unavailable
        );
    }

    #[test]
    fn unmaterialized_cook_resume_is_projected_as_idempotent() {
        let mut record = record(AgentTaskRunState::Queued, false);
        record.metadata["unmaterialized_cook_admission"] =
            serde_json::json!({"state": "blocked_runner_unavailable"});
        let report = lifecycle_action_eligibility(&record, None);
        let resume = report
            .actions
            .iter()
            .find(|action| action.action == AgentTaskLifecycleAction::Resume)
            .expect("resume action");
        assert!(resume.idempotent);
        assert_eq!(resume.availability, AgentTaskActionAvailability::Available);
    }
}
