use super::{AgentTaskRunRecord, AgentTaskRunState, LocalOwnerLiveness};
use crate::agent_task_schedule::AgentTaskPlan;
use homeboy_control_plane_contract::{
    ControlPlaneAction, ControlPlaneActionAvailability, ControlPlaneActionConfirmation,
    ControlPlaneActionEligibility, ControlPlaneActionEligibilityReport, RunId,
    CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA,
};

/// Canonical non-mutating lifecycle action policy for CLI, daemon, and API projections.
/// Mutation commands revalidate these snapshot decisions under their own locks.
pub fn lifecycle_action_eligibility(
    record: &AgentTaskRunRecord,
    plan: Option<&AgentTaskPlan>,
) -> ControlPlaneActionEligibilityReport {
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

    ControlPlaneActionEligibilityReport {
        schema: CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA.to_string(),
        run: RunId::new(&record.run_id).expect("durable run IDs are validated before persistence"),
        actions: vec![
            action(
                ControlPlaneAction::Cancel,
                cancellation,
                ControlPlaneActionConfirmation::Required,
                Vec::new(),
                true,
                "agent_task_run",
            ),
            action(
                ControlPlaneAction::Resume,
                resume,
                ControlPlaneActionConfirmation::None,
                Vec::new(),
                true,
                "agent_task_run",
            ),
            action(
                ControlPlaneAction::Retry,
                retry,
                ControlPlaneActionConfirmation::Required,
                Vec::new(),
                true,
                "agent_task_run",
            ),
            action(
                ControlPlaneAction::Review,
                available("review is a non-mutating read available for every durable run"),
                ControlPlaneActionConfirmation::None,
                Vec::new(),
                true,
                "agent_task_review",
            ),
            action(
                ControlPlaneAction::Promote,
                promotion,
                ControlPlaneActionConfirmation::Required,
                vec!["to_worktree"],
                true,
                "agent_task_promotion",
            ),
            action(
                ControlPlaneAction::Reconcile,
                reconcile,
                ControlPlaneActionConfirmation::None,
                Vec::new(),
                true,
                "agent_task_run",
            ),
        ],
    }
}

fn action(
    action: ControlPlaneAction,
    decision: (ControlPlaneActionAvailability, String),
    confirmation: ControlPlaneActionConfirmation,
    required_inputs: Vec<&str>,
    idempotent: bool,
    result_resource_type: &str,
) -> ControlPlaneActionEligibility {
    ControlPlaneActionEligibility {
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

fn available(reason: impl Into<String>) -> (ControlPlaneActionAvailability, String) {
    (ControlPlaneActionAvailability::Available, reason.into())
}

fn unavailable(reason: impl Into<String>) -> (ControlPlaneActionAvailability, String) {
    (ControlPlaneActionAvailability::Unavailable, reason.into())
}

fn indeterminate(reason: impl Into<String>) -> (ControlPlaneActionAvailability, String) {
    (ControlPlaneActionAvailability::Indeterminate, reason.into())
}

fn resume_availability(record: &AgentTaskRunRecord) -> (ControlPlaneActionAvailability, String) {
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
) -> (ControlPlaneActionAvailability, String) {
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
    if record.metadata["cook_id"].is_string() {
        match crate::agent_task_service::retry_admission_for_projection(&record.run_id) {
            Ok(crate::agent_task_service::RetryProjectionAdmission::DurableCook) => {
                return available(
                "durable Cook retry is admitted; runtime admission will be revalidated before execution",
            )
            }
            Ok(crate::agent_task_service::RetryProjectionAdmission::GenericLifecycle) => {
                return available(
                    "generic lifecycle retry is admitted; runtime admission will be revalidated before execution",
                )
            }
            Err(error) => return unavailable(format!(
                "durable Cook retry is unavailable: {}; inspect this exact attempt with: homeboy agent-task status {}",
                error.message, record.run_id
            )),
        }
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
        report: &ControlPlaneActionEligibilityReport,
        action: ControlPlaneAction,
    ) -> ControlPlaneActionAvailability {
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
            assert_eq!(report.schema, CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA);
            assert_eq!(report.actions.len(), 6);
            assert_eq!(
                decision(&report, ControlPlaneAction::Review),
                ControlPlaneActionAvailability::Available
            );
            if state.is_terminal() {
                assert_eq!(
                    decision(&report, ControlPlaneAction::Cancel),
                    ControlPlaneActionAvailability::Unavailable
                );
            }
            if matches!(
                state,
                AgentTaskRunState::Succeeded
                    | AgentTaskRunState::CandidateRecoverable
                    | AgentTaskRunState::PartialRecoverable
            ) {
                assert_eq!(
                    decision(&report, ControlPlaneAction::Promote),
                    ControlPlaneActionAvailability::Indeterminate
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
            decision(&report, ControlPlaneAction::Resume),
            ControlPlaneActionAvailability::Unavailable
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
            .find(|action| action.action == ControlPlaneAction::Resume)
            .expect("resume action");
        assert!(resume.idempotent);
        assert_eq!(
            resume.availability,
            ControlPlaneActionAvailability::Available
        );
    }
}
