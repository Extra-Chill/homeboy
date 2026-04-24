//! Runner report shape tests for `src/core/rig/runner.rs`.
//!
//! The `run_up` / `run_check` / `run_down` / `run_status` functions integrate
//! real services + filesystem state and are validated by the manual smoke
//! in #1468. This module covers the report serialization contract consumers
//! (CLI JSON envelope, scheduled jobs) rely on.

use crate::rig::pipeline::PipelineOutcome;
use crate::rig::runner::{CheckReport, RigStatusReport, ServiceStatusReport, UpReport};

fn empty_pipeline(name: &str) -> PipelineOutcome {
    PipelineOutcome {
        name: name.to_string(),
        steps: Vec::new(),
        passed: 0,
        failed: 0,
    }
}

#[test]
fn test_up_report_serializes_success_flag() {
    let report = UpReport {
        rig_id: "test".to_string(),
        pipeline: empty_pipeline("up"),
        success: true,
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"rig_id\":\"test\""));
    assert!(json.contains("\"success\":true"));
}

#[test]
fn test_check_report_serializes_success_flag() {
    let report = CheckReport {
        rig_id: "test".to_string(),
        pipeline: empty_pipeline("check"),
        success: false,
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"success\":false"));
}

#[test]
fn test_status_report_empty_services_serializes() {
    let report = RigStatusReport {
        rig_id: "test".to_string(),
        description: "empty rig".to_string(),
        services: Vec::new(),
        last_up: None,
        last_check: None,
        last_check_result: None,
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"services\":[]"));
    // last_up is None -> serialized as null (not skipped, to aid tooling).
    assert!(json.contains("\"last_up\":null"));
}

#[test]
fn test_service_status_report_omits_optional_fields_when_stopped() {
    let report = ServiceStatusReport {
        id: "svc".to_string(),
        status: "stopped".to_string(),
        pid: None,
        started_at: None,
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(!json.contains("\"pid\""));
    assert!(!json.contains("\"started_at\""));
}

#[test]
fn test_service_status_report_emits_pid_when_running() {
    let report = ServiceStatusReport {
        id: "svc".to_string(),
        status: "running".to_string(),
        pid: Some(4321),
        started_at: Some("2026-04-24T13:00:00Z".to_string()),
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"pid\":4321"));
    assert!(json.contains("\"started_at\":\"2026-04-24T13:00:00Z\""));
}
