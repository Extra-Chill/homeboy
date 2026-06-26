use std::collections::BTreeMap;

use homeboy::core::api_jobs::{JobEvent, JobStatus};
use homeboy::core::runner::{RunnerActiveJobSource, RunnerActiveJobState};
use homeboy::core::runners::{self as runner, RunnerSession, RunnerStatusReport, RunnerTunnelMode};

use super::super::jobs::format_job_event;
use super::super::status::{
    runner_artifact_feature_diagnostics, runner_status_operator_commands,
    wp_codebox_runtime_diagnostics, wp_codebox_runtime_output, wp_codebox_tool_diagnostics,
};

#[test]
fn runner_job_event_format_includes_sequence_kind_message_and_data() {
    let event = JobEvent {
        sequence: 7,
        job_id: uuid::Uuid::nil(),
        kind: homeboy::core::api_jobs::JobEventKind::Progress,
        timestamp_ms: 123,
        message: Some("cell started".to_string()),
        data: Some(serde_json::json!({ "cell": "audit" })),
    };

    assert_eq!(
        format_job_event(&event),
        "#0007 progress cell started {\"cell\":\"audit\"}"
    );
}

#[test]
fn reverse_runner_status_commands_include_lifecycle_operations() {
    let report = RunnerStatusReport {
        runner_id: "homeboy-lab".to_string(),
        connected: true,
        state: runner::RunnerSessionState::Connected,
        session: Some(RunnerSession {
            runner_id: "homeboy-lab".to_string(),
            mode: RunnerTunnelMode::Reverse,
            role: runner::RunnerSessionRole::Controller,
            server_id: None,
            controller_id: Some("controller".to_string()),
            broker_url: Some("https://broker.example.test/".to_string()),
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: None,
            connected_at: "2026-06-19T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: Some("2026-06-19T00:00:01Z".to_string()),
        }),
        stale_daemon: None,
        active_jobs: vec![homeboy::core::api_jobs::ActiveRunnerJobSummary {
            runner_id: "homeboy-lab".to_string(),
            job_id: "job-123".to_string(),
            operation: "runner.exec".to_string(),
            source: "broker".to_string(),
            kind: "runner.exec".to_string(),
            status: JobStatus::Running,
            command: "true".to_string(),
            cwd: None,
            started_at_ms: 1000,
            updated_at_ms: 1500,
            elapsed_ms: 500,
            heartbeat_age_ms: 0,
            claim_id: Some("claim-123".to_string()),
            claimed_by_runner_id: Some("homeboy-lab".to_string()),
            claimed_at_ms: Some(1000),
            claim_expires_at_ms: Some(31_000),
            claim_expires_in_ms: Some(29_500),
            durable_run_id: Some("run-123".to_string()),
            stale_reason: None,
            lifecycle_state: Some("active".to_string()),
            retryable: Some(false),
            active_child_count: None,
            active_cell_count: None,
        }],
        active_runner_jobs: vec![homeboy::core::runners::RunnerJob {
            runner_id: "homeboy-lab".to_string(),
            job_id: "job-123".to_string(),
            operation: "runner.exec".to_string(),
            status: JobStatus::Running,
            command: "true".to_string(),
            cwd: None,
            source: "broker".to_string(),
            lifecycle_owner: homeboy::core::runners::RunnerLifecycleOwner::Broker,
            started_at_ms: Some(1000),
            updated_at_ms: Some(1500),
            elapsed_ms: Some(500),
            heartbeat_age_ms: Some(0),
            claim_id: Some("claim-123".to_string()),
            claimed_by_runner_id: Some("homeboy-lab".to_string()),
            claimed_at_ms: Some(1000),
            claim_expires_at_ms: Some(31_000),
            claim_expires_in_ms: Some(29_500),
            durable_run_id: Some("run-123".to_string()),
            stale_reason: None,
            lifecycle_state: Some("active".to_string()),
            retryable: Some(false),
            artifact_refs: Vec::new(),
        }],
        active_job_count: 1,
        stale_runner_jobs: Vec::new(),
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::Available,
        active_job_source: Some(RunnerActiveJobSource::ReverseBroker),
        active_job_error: None,
        session_path: "/tmp/session.json".to_string(),
    };

    let commands = runner_status_operator_commands(&report);
    let serialized = serde_json::to_string(&commands).expect("serialize commands");

    assert!(serialized.contains("homeboy runner job logs homeboy-lab job-123 --follow"));
    assert!(serialized.contains("homeboy runner job cancel homeboy-lab job-123"));
    assert!(serialized.contains("homeboy runs artifact get run-123 <artifact-id> -o <path>"));
    assert!(serialized.contains("homeboy runner job reconcile homeboy-lab"));
    assert!(serialized.contains("homeboy runner job artifacts homeboy-lab job-123 <artifact-id>"));
    assert!(!serialized.contains("curl -fsS"));
}

#[test]
fn wp_codebox_diagnostics_distinguish_configured_managed_and_effective() {
    let diagnostics = wp_codebox_tool_diagnostics(
        Some("homeboy-lab"),
        &BTreeMap::from([
            (
                "HOMEBOY_WP_CODEBOX_BIN".to_string(),
                "/stale/wp-codebox/packages/cli/dist/index.js".to_string(),
            ),
            (
                "HOMEBOY_WP_CODEBOX_INSTALL_DIR".to_string(),
                "/home/chubes/.cache/homeboy/wp-codebox".to_string(),
            ),
        ]),
    );

    assert_eq!(diagnostics.tool, "wp-codebox");
    assert_eq!(
        diagnostics.configured_binary.as_deref(),
        Some("/stale/wp-codebox/packages/cli/dist/index.js")
    );
    assert_eq!(
        diagnostics.configured_binary_source,
        "HOMEBOY_WP_CODEBOX_BIN"
    );
    assert_eq!(
        diagnostics.managed_cache_source,
        "/home/chubes/.cache/homeboy/wp-codebox/source"
    );
    assert_eq!(
        diagnostics.managed_cache_binary,
        "/home/chubes/.cache/homeboy/wp-codebox/source/packages/cli/dist/index.js"
    );
    assert!(diagnostics
        .effective_binary_rule
        .contains("managed cache binary wins"));
    assert!(diagnostics
        .diagnostic_command
        .contains("effective_source=%s"));
}

#[test]
fn wp_codebox_runtime_reports_package_paths_probe_and_mixed_source_warnings() {
    let runtime = wp_codebox_runtime_output(
        Some("homeboy-lab"),
        &BTreeMap::from([
            (
                "HOMEBOY_WP_CODEBOX_BIN".to_string(),
                "/stale/wp-codebox/packages/cli/dist/index.js".to_string(),
            ),
            (
                "HOMEBOY_WP_CODEBOX_INSTALL_DIR".to_string(),
                "/home/chubes/.cache/homeboy/wp-codebox".to_string(),
            ),
            (
                "HOMEBOY_WP_CODEBOX_CORE_MODULE".to_string(),
                "/other/wp-codebox/packages/core/dist/index.js".to_string(),
            ),
        ]),
    );

    assert_eq!(runtime.tool, "wp-codebox");
    assert_eq!(
        runtime.managed_cache_source,
        "/home/chubes/.cache/homeboy/wp-codebox/source"
    );
    assert_eq!(
        runtime.playground_package.package,
        "@automattic/wp-codebox-playground"
    );
    assert_eq!(
        runtime.playground_package.expected_path,
        "/home/chubes/.cache/homeboy/wp-codebox/source/packages/playground"
    );
    assert_eq!(runtime.core_package.package, "@automattic/wp-codebox-core");
    assert_eq!(
        runtime.core_package.expected_path,
        "/other/wp-codebox/packages/core/dist/index.js"
    );
    assert_eq!(runtime.source_git_sha.source, "runtime_probe_command");
    assert_eq!(runtime.dist_build_freshness.source, "runtime_probe_command");
    assert!(runtime
        .runtime_probe_command
        .contains("@automattic/wp-codebox-playground"));
    assert!(runtime
        .runtime_probe_command
        .contains("dist_build_freshness=%s"));
    assert!(runtime
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.id == "wp_codebox.mixed_cli_source"));
    assert!(runtime
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.id == "wp_codebox.mixed_core_source"));
}

#[test]
fn wp_codebox_runtime_diagnostics_accept_single_managed_checkout() {
    let diagnostics = wp_codebox_runtime_diagnostics(
        Some("/cache/wp-codebox/source/packages/cli/dist/index.js"),
        "/cache/wp-codebox/source",
        "/cache/wp-codebox/source/packages/core/dist/index.js",
    );

    assert!(diagnostics.is_empty());
}

#[test]
fn runner_status_artifact_diagnostics_surface_controller_runner_checks_and_drift_hint() {
    let report = RunnerStatusReport {
        runner_id: "homeboy-lab".to_string(),
        connected: true,
        state: runner::RunnerSessionState::Connected,
        session: Some(RunnerSession {
            runner_id: "homeboy-lab".to_string(),
            mode: RunnerTunnelMode::Reverse,
            role: runner::RunnerSessionRole::Controller,
            server_id: None,
            controller_id: Some("controller".to_string()),
            broker_url: Some("https://broker.example.test/".to_string()),
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            homeboy_version: "old".to_string(),
            homeboy_build_identity: None,
            connected_at: "2026-06-19T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: Some("2026-06-19T00:00:01Z".to_string()),
        }),
        stale_daemon: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_jobs: Vec::new(),
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::Available,
        active_job_source: Some(RunnerActiveJobSource::ReverseBroker),
        active_job_error: None,
        session_path: "/tmp/session.json".to_string(),
    };

    let diagnostics = runner_artifact_feature_diagnostics("homeboy-lab", "homeboy", &report, true);
    let serialized = serde_json::to_string(&diagnostics).expect("serialize diagnostics");

    assert!(diagnostics
        .required_features
        .contains(&"runner_exec_artifact_output"));
    assert!(diagnostics
        .required_features
        .contains(&"runs_artifact_attach"));
    assert!(serialized.contains(
        "homeboy runner exec <runner-id> --run-id <run-id> --artifact <path> -- <command>"
    ));
    assert!(serialized.contains(
        "homeboy runs artifact attach <run-id> --runner <runner-id> --path <path> --name <name>"
    ));
    assert!(serialized.contains("homeboy runner exec homeboy-lab -- homeboy runner exec --help"));
    assert!(serialized
        .contains("homeboy runner exec homeboy-lab -- homeboy runs artifact attach --help"));
    assert!(serialized.contains("version/build drift"));
}
