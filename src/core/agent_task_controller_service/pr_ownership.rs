//! Split from `agent_task_controller_service` god file (#5208). Structural move only.
#![allow(unused_imports)]
use super::*;

pub(super) fn execute_own_pr_until_green_action(
    record: &mut AgentTaskLoopControllerRecord,
    ownership: &AgentTaskPrOwnershipRequest,
    entity_id: Option<&str>,
) -> Result<(Value, i32)> {
    let pr_number = match ownership.pr_number {
        Some(number) => Some(number),
        None => find_pr_number(ownership)?,
    };
    let Some(pr_number) = pr_number else {
        let update = AgentTaskPrOwnershipStatusUpdate {
            missing_pr: true,
            ..AgentTaskPrOwnershipStatusUpdate::default()
        };
        let status =
            record.record_pr_ownership_status(ownership, entity_id.map(str::to_string), update);
        return Ok((
            serde_json::json!({
                "mode": "own_pr_until_green",
                "ownership": status,
                "action": "stop",
                "reason": "no open pull request matched the owned branch"
            }),
            1,
        ));
    };

    let view = pr_view(
        ownership.component_id.as_deref(),
        pr_number,
        ownership.path.clone(),
    )?;
    let previous_retry_count = existing_pr_retry_count(record, &ownership.ownership_id);
    let retry_count = if pr_needs_retry(&view.ci_state, view.review_decision.as_deref()) {
        previous_retry_count.saturating_add(1)
    } else {
        previous_retry_count
    };
    let update = AgentTaskPrOwnershipStatusUpdate {
        pr_number: Some(view.number),
        pr_url: Some(format!(
            "https://github.com/{}/{}/pull/{}",
            view.owner, view.repo, view.number
        )),
        head_sha: view.head_sha.clone(),
        ci_state: Some(view.ci_state.clone()),
        ci_summary: Some(view.ci_summary.clone()),
        review_decision: view.review_decision.clone(),
        merge_state: view
            .merge_state
            .clone()
            .or_else(|| Some(view.state.clone())),
        retry_count,
        evidence: vec![AgentTaskLoopArtifactRef {
            uri: format!("github://{}/{}/pull/{}", view.owner, view.repo, view.number),
            kind: Some("github.pull_request".to_string()),
            role: None,
            label: Some("Owned pull request".to_string()),
            semantic_key: None,
        }],
        missing_pr: false,
    };
    let status =
        record.record_pr_ownership_status(ownership, entity_id.map(str::to_string), update);
    queue_pr_ownership_follow_up(record, ownership, entity_id, &status);
    Ok((
        serde_json::json!({
            "mode": "own_pr_until_green",
            "ownership": status,
        }),
        0,
    ))
}

pub(super) fn pr_needs_retry(ci_state: &str, review_decision: Option<&str>) -> bool {
    ci_state == "terminal_failed"
        || ci_state == "stale"
        || review_decision == Some("CHANGES_REQUESTED")
}

pub(super) fn find_pr_number(ownership: &AgentTaskPrOwnershipRequest) -> Result<Option<u64>> {
    let output = pr_find(
        ownership.component_id.as_deref(),
        PrFindOptions {
            base: Some(ownership.base.clone()),
            head: Some(ownership.head.clone()),
            state: PrState::Open,
            limit: 10,
            path: ownership.path.clone(),
        },
    )?;
    Ok(output.items.into_iter().next().map(|item| item.number))
}

pub(super) fn existing_pr_retry_count(
    record: &AgentTaskLoopControllerRecord,
    ownership_id: &str,
) -> u32 {
    record
        .pr_ownerships
        .iter()
        .find(|ownership| ownership.ownership_id == ownership_id)
        .map(|ownership| ownership.retry_count)
        .unwrap_or_default()
}

pub(super) fn queue_pr_ownership_follow_up(
    record: &mut AgentTaskLoopControllerRecord,
    ownership: &AgentTaskPrOwnershipRequest,
    entity_id: Option<&str>,
    status: &crate::core::agent_task_loop_controller::AgentTaskPrOwnershipRecord,
) {
    match status.state {
        AgentTaskPrOwnershipState::ChangesRequested => {
            let feedback_id = format!(
                "pr-ownership:{}:retry-{}",
                ownership.ownership_id,
                status.retry_count + 1
            );
            record.record_action(
                AgentTaskLoopPolicyAction::RequestChanges {
                    target_run_id: ownership.ownership_id.clone(),
                    feedback_id: Some(feedback_id),
                },
                "owned PR has red checks or requested changes",
            );
        }
        AgentTaskPrOwnershipState::RetryLimitReached => {
            if let Some(entity_id) = entity_id {
                record.record_action(
                    AgentTaskLoopPolicyAction::MarkHumanReady {
                        entity_id: entity_id.to_string(),
                        reason: Some(
                            "owned PR reached retry limit with red checks or requested changes"
                                .to_string(),
                        ),
                    },
                    "owned PR retry limit reached",
                );
            }
        }
        AgentTaskPrOwnershipState::WaitingForChecks => {
            record.record_action(
                AgentTaskLoopPolicyAction::WaitForEvent(
                    crate::core::agent_task_loop_controller::AgentTaskLoopWait {
                        wait_key: format!("pr-ownership:{}:checks", ownership.ownership_id),
                        event_type: "github.pr.checks_changed".to_string(),
                        entity_id: entity_id.map(str::to_string),
                        external_ref: status
                            .pr_number
                            .map(|number| format!("{}#{}", ownership.head, number)),
                        timeout_at: None,
                        escalation_policy: Some("reinspect_pr".to_string()),
                        status:
                            crate::core::agent_task_loop_controller::AgentTaskLoopWaitStatus::Open,
                        satisfied_by_event_id: None,
                    },
                ),
                "owned PR is waiting for checks",
            );
        }
        AgentTaskPrOwnershipState::GreenReady => {
            if let Some(entity_id) = entity_id {
                record.record_action(
                    AgentTaskLoopPolicyAction::MarkHumanReady {
                        entity_id: entity_id.to_string(),
                        reason: Some(
                            "owned PR checks are green and merge is allowed by policy".to_string(),
                        ),
                    },
                    "owned PR is green",
                );
            }
        }
        AgentTaskPrOwnershipState::WaitingForMerge => {
            record.record_action(
                AgentTaskLoopPolicyAction::WaitForEvent(
                    crate::core::agent_task_loop_controller::AgentTaskLoopWait {
                        wait_key: format!("pr-ownership:{}:merged", ownership.ownership_id),
                        event_type: "github.pr.merged".to_string(),
                        entity_id: entity_id.map(str::to_string),
                        external_ref: status
                            .pr_number
                            .map(|number| format!("{}#{}", ownership.head, number)),
                        timeout_at: None,
                        escalation_policy: Some("wait_for_merge".to_string()),
                        status:
                            crate::core::agent_task_loop_controller::AgentTaskLoopWaitStatus::Open,
                        satisfied_by_event_id: None,
                    },
                ),
                "owned PR is green and waiting for merge",
            );
        }
        AgentTaskPrOwnershipState::Merged => {
            record.record_action(
                AgentTaskLoopPolicyAction::Complete {
                    reason: Some("owned PR merged".to_string()),
                },
                "owned PR lifecycle completed",
            );
        }
        AgentTaskPrOwnershipState::MissingPr
        | AgentTaskPrOwnershipState::Stopped
        | AgentTaskPrOwnershipState::Tracking => {}
    }
}
