//! Versioned command-provider contract for worktree retention.
//!
//! One JSON stdin/stdout command carries plan, apply, status, and evidence.
//! Apply is bound to an explicit reviewed `run_id` and `plan_id`; it cannot
//! start a fresh sweep. Core owns authorization and deadlines; the provider
//! owns inventory, plan persistence, mutation, and apply-time revalidation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const WORKTREE_RETENTION_SCHEMA: &str = "homeboy/worktree-retention/v1";
pub const DEFAULT_WORKTREE_RETENTION_TIMEOUT_MS: u64 = 30_000;
/// Hard protocol ceilings protect stdin delivery and stdout capture before
/// provider-specific policy is available. The aggregate may choose stricter
/// limits.
pub const MAX_WORKTREE_RETENTION_REQUEST_BYTES: usize = 256 * 1024;
pub const MAX_WORKTREE_RETENTION_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeRetentionOperation {
    Plan,
    Apply,
    Status,
    Evidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeRetentionState {
    Planned,
    Applying,
    Continuing,
    Completed,
    Blocked,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeRetentionInventoryCompleteness {
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRetentionBounds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRetentionRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRetentionContinuation {
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_operation: Option<WorktreeRetentionOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRetentionEffects {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees_removed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locks_pruned: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_reconciled: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_reclaimed: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRetentionBlockers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_reason: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRetentionRequest {
    pub schema: String,
    pub provider_id: String,
    pub operation: WorktreeRetentionOperation,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<WorktreeRetentionBounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRetentionResponse {
    pub schema: String,
    pub provider_id: String,
    pub run_id: String,
    pub plan_id: String,
    pub state: WorktreeRetentionState,
    pub inventory_completeness: WorktreeRetentionInventoryCompleteness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<WorktreeRetentionContinuation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_ref: Option<WorktreeRetentionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<WorktreeRetentionRef>,
    #[serde(default)]
    pub effects: WorktreeRetentionEffects,
    #[serde(default)]
    pub blockers: WorktreeRetentionBlockers,
}

impl WorktreeRetentionRequest {
    pub fn protocol_bytes(&self) -> Result<Vec<u8>, String> {
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_WORKTREE_RETENTION_REQUEST_BYTES {
            return Err("worktree retention request exceeds the protocol byte ceiling".to_string());
        }
        Ok(bytes)
    }

    pub fn reviewed_apply_identity(&self) -> Result<(&str, &str), String> {
        if self.operation != WorktreeRetentionOperation::Apply {
            return Err("apply identity is only defined for apply".to_string());
        }
        match (
            nonempty(self.run_id.as_deref()),
            nonempty(self.plan_id.as_deref()),
        ) {
            (Some(run_id), Some(plan_id)) => Ok((run_id, plan_id)),
            _ => Err("apply requires explicit reviewed run_id and plan_id".to_string()),
        }
    }
}

impl WorktreeRetentionResponse {
    pub fn validate_identity(&self, request: &WorktreeRetentionRequest) -> Result<(), String> {
        if self.schema != WORKTREE_RETENTION_SCHEMA {
            return Err("provider returned an unexpected schema".to_string());
        }
        if self.provider_id != request.provider_id {
            return Err("provider returned an unexpected provider id".to_string());
        }
        if nonempty(Some(self.run_id.as_str())).is_none()
            || nonempty(Some(self.plan_id.as_str())).is_none()
        {
            return Err("provider response is missing run_id or plan_id".to_string());
        }
        if request.operation == WorktreeRetentionOperation::Apply {
            let (run_id, plan_id) = request.reviewed_apply_identity()?;
            if self.run_id != run_id || self.plan_id != plan_id {
                return Err(
                    "provider response identity does not match the reviewed plan".to_string(),
                );
            }
        }
        Ok(())
    }

    pub fn bounded_continuation(&self) -> bool {
        matches!(self.state, WorktreeRetentionState::Continuing)
            || self
                .continuation
                .as_ref()
                .is_some_and(|continuation| !continuation.complete)
    }

    pub fn reviewable_planned_identity(&self) -> Option<(&str, &str)> {
        if self.state != WorktreeRetentionState::Planned {
            return None;
        }
        Some((
            nonempty(Some(self.run_id.as_str()))?,
            nonempty(Some(self.plan_id.as_str()))?,
        ))
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_request() -> WorktreeRetentionRequest {
        WorktreeRetentionRequest {
            schema: WORKTREE_RETENTION_SCHEMA.to_string(),
            provider_id: "fixture".to_string(),
            operation: WorktreeRetentionOperation::Plan,
            request_id: "req-1".to_string(),
            idempotency_key: Some("req-1".to_string()),
            run_id: None,
            plan_id: None,
            bounds: Some(WorktreeRetentionBounds {
                max_items: Some(8),
                timeout_ms: Some(1_000),
            }),
            deadline_unix_ms: Some(1),
        }
    }

    fn planned_response() -> WorktreeRetentionResponse {
        WorktreeRetentionResponse {
            schema: WORKTREE_RETENTION_SCHEMA.to_string(),
            provider_id: "fixture".to_string(),
            run_id: "run-1".to_string(),
            plan_id: "plan-1".to_string(),
            state: WorktreeRetentionState::Planned,
            inventory_completeness: WorktreeRetentionInventoryCompleteness::Complete,
            continuation: None,
            status_ref: Some(WorktreeRetentionRef {
                command: Some("provider status run-1".to_string()),
                locator: None,
            }),
            evidence_ref: Some(WorktreeRetentionRef {
                command: None,
                locator: Some("provider://run-1/evidence".to_string()),
            }),
            effects: WorktreeRetentionEffects {
                worktrees_removed: None,
                locks_pruned: None,
                metadata_reconciled: None,
                bytes_reclaimed: None,
            },
            blockers: WorktreeRetentionBlockers {
                count: Some(2),
                by_reason: BTreeMap::from([("dirty".to_string(), 2)]),
            },
        }
    }

    #[test]
    fn request_and_response_round_trip_with_strict_serde() {
        let request = plan_request();
        let request_json = serde_json::to_value(&request).expect("request serializes");
        let parsed_request: WorktreeRetentionRequest =
            serde_json::from_value(request_json).expect("request parses");
        assert_eq!(parsed_request, request);

        let response = planned_response();
        let response_json = serde_json::to_value(&response).expect("response serializes");
        let parsed_response: WorktreeRetentionResponse =
            serde_json::from_value(response_json).expect("response parses");
        assert_eq!(parsed_response, response);
        parsed_response
            .validate_identity(&request)
            .expect("plan identity is valid");
    }

    #[test]
    fn unknown_fields_and_missing_required_fields_are_rejected() {
        let error = serde_json::from_value::<WorktreeRetentionRequest>(serde_json::json!({
            "schema": WORKTREE_RETENTION_SCHEMA,
            "provider_id": "fixture",
            "operation": "plan",
            "request_id": "req-1",
            "unexpected": true
        }))
        .expect_err("unknown request fields are rejected");
        assert!(error.to_string().contains("unexpected"));

        let error = serde_json::from_value::<WorktreeRetentionResponse>(serde_json::json!({
            "schema": WORKTREE_RETENTION_SCHEMA,
            "provider_id": "fixture",
            "run_id": "run-1",
            "plan_id": "plan-1",
            "state": "planned",
            "inventory_completeness": "complete",
            "extra": 1
        }))
        .expect_err("unknown response fields are rejected");
        assert!(error.to_string().contains("extra"));

        serde_json::from_value::<WorktreeRetentionRequest>(serde_json::json!({
            "provider_id": "fixture",
            "operation": "plan",
            "request_id": "req-1"
        }))
        .expect_err("schema is required");
    }

    #[test]
    fn apply_requires_explicit_reviewed_identity() {
        let mut request = plan_request();
        request.operation = WorktreeRetentionOperation::Apply;
        assert_eq!(
            request
                .reviewed_apply_identity()
                .expect_err("missing identity"),
            "apply requires explicit reviewed run_id and plan_id"
        );
        request.run_id = Some("run-1".to_string());
        request.plan_id = Some("plan-1".to_string());
        assert_eq!(
            request
                .reviewed_apply_identity()
                .expect("reviewed identity"),
            ("run-1", "plan-1")
        );
    }

    #[test]
    fn mismatched_apply_response_identity_is_rejected() {
        let mut request = plan_request();
        request.operation = WorktreeRetentionOperation::Apply;
        request.run_id = Some("run-1".to_string());
        request.plan_id = Some("plan-1".to_string());
        let mut response = planned_response();
        response.state = WorktreeRetentionState::Applying;
        response.plan_id = "other-plan".to_string();
        assert!(response
            .validate_identity(&request)
            .expect_err("mismatch")
            .contains("does not match the reviewed plan"));
    }

    #[test]
    fn bounded_continuation_is_not_a_terminal_failure_signal() {
        let mut response = planned_response();
        response.state = WorktreeRetentionState::Continuing;
        response.inventory_completeness = WorktreeRetentionInventoryCompleteness::Partial;
        response.continuation = Some(WorktreeRetentionContinuation {
            complete: false,
            resume_operation: Some(WorktreeRetentionOperation::Apply),
            reason: Some("deadline".to_string()),
        });
        assert!(response.bounded_continuation());
        assert_eq!(response.reviewable_planned_identity(), None);
    }

    #[test]
    fn complete_planned_identity_is_reviewable_for_apply() {
        let response = planned_response();
        assert_eq!(
            response.reviewable_planned_identity(),
            Some(("run-1", "plan-1"))
        );
        let mut partial = response.clone();
        partial.inventory_completeness = WorktreeRetentionInventoryCompleteness::Partial;
        assert_eq!(
            partial.reviewable_planned_identity(),
            Some(("run-1", "plan-1"))
        );

        let mut blocked = response;
        blocked.state = WorktreeRetentionState::Blocked;
        assert_eq!(blocked.reviewable_planned_identity(), None);
    }

    #[test]
    fn dmc_bounded_plan_shape_is_strict_and_reviewable() {
        let response: WorktreeRetentionResponse = serde_json::from_value(serde_json::json!({
            "schema": WORKTREE_RETENTION_SCHEMA,
            "provider_id": "data-machine-code",
            "run_id": "cleanup-run-1",
            "plan_id": "cleanup-plan-1",
            "state": "planned",
            "inventory_completeness": "partial",
            "continuation": {
                "complete": false,
                "resume_operation": "plan",
                "reason": "inventory_page"
            },
            "status_ref": {"command": "studio wp datamachine-code workspace cleanup status cleanup-run-1 --format=json"},
            "evidence_ref": {"command": "studio wp datamachine-code workspace cleanup evidence cleanup-run-1 --format=json"},
            "effects": {},
            "blockers": {"count": 0, "by_reason": {}}
        }))
        .expect("DMC provider shape parses");

        assert_eq!(
            response.reviewable_planned_identity(),
            Some(("cleanup-run-1", "cleanup-plan-1"))
        );
    }

    #[test]
    fn protocol_request_ceiling_rejects_oversized_payloads() {
        let mut request = plan_request();
        request.request_id = "x".repeat(MAX_WORKTREE_RETENTION_REQUEST_BYTES);
        assert!(request
            .protocol_bytes()
            .expect_err("oversized")
            .contains("protocol byte ceiling"));
    }
}
