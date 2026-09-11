//! Versioned, non-mutating durable-run review resource.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ControlPlaneError, ControlPlaneRun, RunId};

pub const CONTROL_PLANE_RUN_REVIEW_SCHEMA: &str = "homeboy/control-plane-run-review/v1";

/// Optional read context used to describe a promotion handoff without changing
/// the durable run or materializing a candidate.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneRunReviewRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_argv: Vec<String>,
}

impl ControlPlaneRunReviewRequest {
    pub fn validate(&self) -> Result<(), ControlPlaneError> {
        if self.provider_command.is_some() && !self.provider_argv.is_empty() {
            return Err(ControlPlaneError::invalid_argument(
                "provider_command and provider_argv are mutually exclusive",
            ));
        }
        Ok(())
    }
}

/// Canonical durable review. `evidence` retains the complete bounded durable
/// read so transports can render summaries without inventing a parallel schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneRunReview {
    pub schema: String,
    pub run: RunId,
    pub resource: ControlPlaneRun,
    pub evidence: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_resource_is_versioned_and_round_trips() {
        let run = RunId::new("run-1").expect("run");
        let review = ControlPlaneRunReview {
            schema: CONTROL_PLANE_RUN_REVIEW_SCHEMA.to_string(),
            run: run.clone(),
            resource: ControlPlaneRun::new(run),
            evidence: serde_json::json!({"aggregate": null}),
        };
        assert_eq!(
            serde_json::from_value::<ControlPlaneRunReview>(
                serde_json::to_value(&review).expect("serialize"),
            )
            .expect("deserialize"),
            review
        );
    }

    #[test]
    fn review_request_rejects_conflicting_provider_inputs() {
        let request = ControlPlaneRunReviewRequest {
            to_worktree: None,
            provider_command: Some("provider".to_string()),
            provider_argv: vec!["provider".to_string()],
        };

        assert!(request.validate().is_err());
    }
}
