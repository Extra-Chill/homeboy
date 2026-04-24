//! Pipeline executor tests for `src/core/rig/pipeline.rs`.
//!
//! End-to-end pipeline runs exercise real services + filesystem mutations
//! and are covered by the manual smoke documented in #1468. Scope here is
//! the public outcome types — shape, serialization, `is_success` contract.

use crate::rig::pipeline::{PipelineOutcome, PipelineStepOutcome};

fn step(status: &str) -> PipelineStepOutcome {
    PipelineStepOutcome {
        kind: "command".to_string(),
        label: "noop".to_string(),
        status: status.to_string(),
        error: None,
    }
}

#[test]
fn test_pipeline_outcome_success_when_zero_failures() {
    let outcome = PipelineOutcome {
        name: "up".to_string(),
        steps: vec![step("pass"), step("pass")],
        passed: 2,
        failed: 0,
    };
    assert!(outcome.is_success());
}

#[test]
fn test_pipeline_outcome_failure_when_any_step_failed() {
    let outcome = PipelineOutcome {
        name: "up".to_string(),
        steps: vec![step("pass"), step("fail")],
        passed: 1,
        failed: 1,
    };
    assert!(!outcome.is_success());
}

#[test]
fn test_pipeline_step_outcome_serializes_error_when_present() {
    let outcome = PipelineStepOutcome {
        kind: "service".to_string(),
        label: "svc start".to_string(),
        status: "fail".to_string(),
        error: Some("boom".to_string()),
    };
    let json = serde_json::to_string(&outcome).expect("serialize");
    assert!(json.contains("\"error\":\"boom\""));
}

#[test]
fn test_pipeline_step_outcome_omits_error_when_absent() {
    let outcome = step("pass");
    let json = serde_json::to_string(&outcome).expect("serialize");
    assert!(!json.contains("\"error\""));
}
