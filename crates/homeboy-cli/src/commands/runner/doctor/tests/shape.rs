use super::super::*;
use std::collections::BTreeMap;
use types::RunnerDoctorStatus;

#[test]
fn local_alias_report_has_stable_top_level_shape() {
    let (report, exit_code) = run("local").expect("local doctor report");
    assert_eq!(exit_code, 0);
    let value = serde_json::to_value(report).expect("serialize report");
    assert_eq!(value["command"], "runner.doctor");
    assert_eq!(value["runner_id"], "local");
    assert!(value.get("status").is_some());
    assert!(value.get("capabilities").is_some());
    assert!(value.get("resources").is_some());
    assert!(value
        .get("checks")
        .and_then(|checks| checks.as_array())
        .is_some());
}

#[test]
fn doctor_options_default_to_general_read_only_scope() {
    let options = RunnerDoctorOptions::default();

    assert_eq!(options.scope, RunnerDoctorScope::General);
    assert!(!options.repair);
}

#[test]
fn bare_repair_deterministically_selects_the_lab_offload_scope() {
    assert_eq!(
        repair_scope(RunnerDoctorScope::General, true),
        RunnerDoctorScope::LabOffload
    );
    assert_eq!(
        repair_scope(RunnerDoctorScope::General, false),
        RunnerDoctorScope::General
    );
    assert_eq!(
        repair_scope(RunnerDoctorScope::SecretEnv, true),
        RunnerDoctorScope::SecretEnv
    );
}

#[test]
fn doctor_output_omits_empty_repairs() {
    let (report, _) = run("local").expect("local doctor report");
    let value = serde_json::to_value(report).expect("serialize report");

    assert!(value.get("repairs").is_none());
}

#[test]
fn compact_doctor_projection_bounds_evidence_and_renders_action() {
    let (mut report, _) = run("local").expect("local doctor report");
    report.checks = (0..(COMPACT_CHECK_LIMIT + 5))
        .map(|index| types::RunnerCheck {
            id: format!("check-{index}"),
            status: RunnerDoctorStatus::Warning,
            message: "m".repeat(4_000),
            remediation: Some("homeboy runner doctor local --repair".to_string()),
            details: BTreeMap::from([("large".to_string(), "d".repeat(4_000))]),
        })
        .collect();

    let compact = output_projection(report, false);
    let rendered = serde_json::to_string(&compact).expect("compact JSON");
    assert_eq!(
        compact["checks"].as_array().expect("checks").len(),
        COMPACT_CHECK_LIMIT
    );
    assert_eq!(compact["truncation"]["checks"]["omitted"], 5);
    assert!(rendered.len() < 16 * 1024, "{rendered}");
    assert_eq!(
        render_summary(&compact).as_deref(),
        Some("Runner doctor\nStatus: degraded\nChecks shown: 12\nNext: homeboy runner doctor local --full")
    );
}

#[test]
fn capabilities_are_runner_substrate_only() {
    let (report, _) = run("local").expect("local doctor report");
    let value = serde_json::to_value(report).expect("serialize report");
    let capabilities = value["capabilities"]
        .as_object()
        .expect("capabilities object");

    assert!(capabilities.contains_key("local_execution"));
    assert!(capabilities.contains_key("homeboy_available"));
    assert!(!capabilities.contains_key("github_cli"));
    assert!(!capabilities.contains_key("node"));
    assert!(!capabilities.contains_key("npm"));
    assert!(!capabilities.contains_key("php"));
    assert!(!capabilities.contains_key("docker"));
}

#[test]
fn overall_status_promotes_errors_over_warnings() {
    let checks = vec![
        checks::warning("optional", "optional missing".to_string(), None),
        checks::error(
            "required",
            "required missing".to_string(),
            None,
            BTreeMap::new(),
        ),
    ];
    assert_eq!(checks::overall_status(&checks), RunnerDoctorStatus::Error);
}

#[test]
fn lab_offload_readiness_keeps_a_healthy_eligible_provider_ready() {
    let checks = vec![
        provider_check("selected.provider", RunnerDoctorStatus::Ok),
        provider_check("optional.provider", RunnerDoctorStatus::Error),
    ];
    let eligible = vec![
        "selected.provider".to_string(),
        "optional.provider".to_string(),
    ];

    let (status, readiness) = checks::lab_offload_status(&checks, &eligible);

    assert_eq!(status, RunnerDoctorStatus::Ok);
    assert_eq!(readiness.ready_for, vec!["selected.provider"]);
    assert_eq!(readiness.blocked_for, vec!["optional.provider"]);
}

#[test]
fn lab_offload_readiness_blocks_a_failed_selected_provider() {
    let checks = vec![
        provider_check("selected.provider", RunnerDoctorStatus::Error),
        provider_check("optional.provider", RunnerDoctorStatus::Ok),
    ];
    let eligible = vec!["selected.provider".to_string()];

    let (status, readiness) = checks::lab_offload_status(&checks, &eligible);

    assert_eq!(status, RunnerDoctorStatus::Error);
    assert!(readiness.ready_for.is_empty());
    assert_eq!(readiness.blocked_for, vec!["selected.provider"]);
}

fn provider_check(provider_id: &str, status: RunnerDoctorStatus) -> types::RunnerCheck {
    types::RunnerCheck {
        id: format!("provider.{provider_id}"),
        status,
        message: "provider readiness".to_string(),
        remediation: None,
        details: BTreeMap::from([("provider_id".to_string(), provider_id.to_string())]),
    }
}

#[test]
fn operational_exit_code_matches_the_doctor_readiness_verdict() {
    for (scenario, status, expected_exit_code) in [
        ("healthy", RunnerDoctorStatus::Ok, 0),
        ("degraded", RunnerDoctorStatus::Warning, 0),
        // A disconnected runner with a recoverable daemon is still not ready
        // until `--repair` has rerun its probe successfully.
        ("disconnected_recoverable", RunnerDoctorStatus::Error, 1),
        ("terminal_error", RunnerDoctorStatus::Error, 1),
    ] {
        assert_eq!(
            status.operational_exit_code(),
            expected_exit_code,
            "{scenario}"
        );
    }
}
