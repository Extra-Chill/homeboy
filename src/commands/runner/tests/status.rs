use std::collections::BTreeMap;

use homeboy::core::agent_runtime_manifest::{
    AgentRuntimeExecutableRequirement, AgentRuntimePackageDiagnosticDeclaration,
    AgentRuntimeProbeDiagnosticDeclaration, AgentRuntimeRuntimeDiagnosticDeclaration,
    AgentRuntimeSourceConsistencyDiagnostic, AgentRuntimeToolDiagnosticDeclaration,
};
use homeboy::core::api_jobs::{JobEvent, JobStatus};
use homeboy::core::runner::{RunnerActiveJobSource, RunnerActiveJobState};
use homeboy::core::runners::{self as runner, RunnerSession, RunnerStatusReport, RunnerTunnelMode};

use super::super::jobs::format_job_event;
use super::super::status::{
    declared_executable_requirement_diagnostics, declared_run_followups_for_legacy,
    declared_runtime_diagnostics, declared_runtime_diagnostics_for_legacy,
    declared_runtime_source_diagnostics, declared_tool_diagnostics, lab_runner_homeboy_output,
    runner_artifact_feature_diagnostics, runner_status_operator_commands,
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
            lifecycle: None,
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
            lifecycle: None,
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
fn selected_runtime_diagnostics_distinguish_configured_managed_and_effective() {
    let declaration = selected_runtime_tool_declaration();
    let diagnostics = declared_tool_diagnostics(
        &declaration,
        Some("homeboy-lab"),
        &BTreeMap::from([
            (
                "HOMEBOY_SAMPLE_RUNTIME_BIN".to_string(),
                "/stale/sample-runtime/packages/cli/dist/index.js".to_string(),
            ),
            (
                "HOMEBOY_SAMPLE_RUNTIME_INSTALL_DIR".to_string(),
                "/home/chubes/.cache/homeboy/sample-runtime".to_string(),
            ),
        ]),
    );

    assert_eq!(diagnostics.tool, "sample-runtime");
    assert_eq!(
        diagnostics.configured_binary.as_deref(),
        Some("/stale/sample-runtime/packages/cli/dist/index.js")
    );
    assert_eq!(
        diagnostics.configured_binary_source,
        "HOMEBOY_SAMPLE_RUNTIME_BIN"
    );
    assert_eq!(
        diagnostics.managed_cache_source,
        "/home/chubes/.cache/homeboy/sample-runtime/source"
    );
    assert_eq!(
        diagnostics.managed_cache_binary,
        "/home/chubes/.cache/homeboy/sample-runtime/source/packages/cli/dist/index.js"
    );
    assert!(diagnostics
        .effective_binary_rule
        .contains("managed cache binary wins"));
    assert!(diagnostics
        .diagnostic_command
        .contains("effective_source=%s"));
}

#[test]
fn declared_runtime_reports_generic_package_paths_probe_and_mixed_source_warnings() {
    let declaration = sample_runtime_declaration();
    let runtime = declared_runtime_diagnostics(
        &declaration,
        Some("homeboy-lab"),
        &BTreeMap::from([
            (
                "HOMEBOY_SAMPLE_RUNTIME_BIN".to_string(),
                "/stale/sample-runtime/packages/cli/dist/index.js".to_string(),
            ),
            (
                "HOMEBOY_SAMPLE_RUNTIME_INSTALL_DIR".to_string(),
                "/home/chubes/.cache/homeboy/sample-runtime".to_string(),
            ),
            (
                "HOMEBOY_SAMPLE_RUNTIME_CORE_MODULE".to_string(),
                "/other/sample-runtime/packages/core/dist/index.js".to_string(),
            ),
        ]),
    );

    assert_eq!(runtime.runtime, "sample-runtime");
    assert_eq!(runtime.legacy_output.as_deref(), Some("sample_runtime"));
    assert_eq!(
        runtime.managed_cache_source,
        "/home/chubes/.cache/homeboy/sample-runtime/source"
    );
    let playground_package = runtime
        .packages
        .iter()
        .find(|package| package.field == "playground_package")
        .expect("playground package diagnostics");
    assert_eq!(
        playground_package.package,
        "@automattic/sample-runtime-playground"
    );
    assert_eq!(
        playground_package.expected_path,
        "/home/chubes/.cache/homeboy/sample-runtime/source/packages/playground"
    );
    let core_package = runtime
        .packages
        .iter()
        .find(|package| package.field == "core_package")
        .expect("core package diagnostics");
    assert_eq!(core_package.package, "@automattic/sample-runtime-core");
    assert_eq!(
        core_package.expected_path,
        "/other/sample-runtime/packages/core/dist/index.js"
    );
    assert_eq!(
        runtime
            .probes
            .get("source_git_sha")
            .expect("source git sha probe")
            .source,
        "runtime_probe_command"
    );
    assert_eq!(
        runtime
            .probes
            .get("dist_build_freshness")
            .expect("dist freshness probe")
            .source,
        "runtime_probe_command"
    );
    assert!(runtime
        .runtime_probe_command
        .contains("@automattic/sample-runtime-playground"));
    assert!(runtime
        .runtime_probe_command
        .contains("dist_build_freshness=%s"));
    assert!(runtime
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.id == "selected_runtime.mixed_cli_source"));
    assert!(runtime
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.id == "selected_runtime.mixed_core_source"));
}

#[test]
fn declared_runtime_legacy_projection_preserves_selected_runtime_status_shape() {
    let runtime = declared_runtime_diagnostics_for_legacy(
        "sample_runtime",
        Some("homeboy-lab"),
        &BTreeMap::from([(
            "HOMEBOY_SAMPLE_RUNTIME_INSTALL_DIR".to_string(),
            "/home/chubes/.cache/homeboy/sample-runtime".to_string(),
        )]),
    );

    let runtime = runtime.expect("catalog declares legacy sample runtime projection");
    assert_eq!(runtime.tool, "sample-runtime");
    assert_eq!(
        runtime.playground_package.package,
        "@automattic/sample-runtime-playground"
    );
    assert_eq!(runtime.source_git_sha.source, "runtime_probe_command");

    let generic = declared_runtime_diagnostics(
        &sample_runtime_declaration(),
        Some("homeboy-lab"),
        &BTreeMap::from([(
            "HOMEBOY_SAMPLE_RUNTIME_INSTALL_DIR".to_string(),
            "/home/chubes/.cache/homeboy/sample-runtime".to_string(),
        )]),
    );
    let serialized = serde_json::to_string(&generic).expect("serialize generic diagnostics");

    assert!(serialized.contains("\"runtime\":\"sample-runtime\""));
    assert!(serialized.contains("\"packages\""));
    assert!(!serialized.contains("\"playground_package\":{"));
}

#[test]
fn sample_runtime_diagnostics_accept_single_managed_checkout() {
    let diagnostics = declared_runtime_source_diagnostics(
        &sample_runtime_declaration().source_consistency,
        &BTreeMap::from([(
            "HOMEBOY_SAMPLE_RUNTIME_CORE_MODULE".to_string(),
            "/cache/sample-runtime/source/packages/core/dist/index.js".to_string(),
        )]),
        Some("/cache/sample-runtime/source/packages/cli/dist/index.js"),
        "/cache/sample-runtime",
        "/cache/sample-runtime/source",
    );

    assert!(diagnostics.is_empty());
}

#[test]
fn unknown_runtime_declaration_does_not_emit_wp_specific_guidance() {
    let declaration = AgentRuntimeToolDiagnosticDeclaration {
        tool: "other-runtime".to_string(),
        legacy_output: None,
        configured_binary_env: vec!["OTHER_RUNTIME_BIN".to_string()],
        install_dir_env: Some("OTHER_RUNTIME_INSTALL_DIR".to_string()),
        default_install_dir: Some("${HOME}/.cache/homeboy/other-runtime".to_string()),
        managed_cache_source: "${install_dir}/source".to_string(),
        managed_cache_binary: "${managed_cache_source}/bin/other-runtime".to_string(),
        effective_binary_rule: "declared runtime rule".to_string(),
        diagnostic_script: "printf other_runtime".to_string(),
        extra: BTreeMap::new(),
    };

    let diagnostics =
        declared_tool_diagnostics(&declaration, Some("homeboy-lab"), &BTreeMap::new());
    let serialized = serde_json::to_string(&diagnostics).expect("serialize diagnostics");

    assert!(serialized.contains("other-runtime"));
    assert!(!serialized.contains("sample-runtime"));
    assert!(!serialized.contains("HOMEBOY_SAMPLE_RUNTIME"));
}

#[test]
fn declared_executable_requirement_status_projection_is_generic() {
    let diagnostics = declared_executable_requirement_diagnostics(
        "example-runtime",
        AgentRuntimeExecutableRequirement {
            id: "example-runtime-cli".to_string(),
            label: Some("Example Runtime CLI".to_string()),
            env: vec!["EXAMPLE_RUNTIME_BIN".to_string()],
            candidates: vec!["example-runtime".to_string()],
            version_command: vec!["--version".to_string()],
            install_hint: Some("Install example-runtime on the runner PATH.".to_string()),
            extra: BTreeMap::new(),
        },
    );

    assert_eq!(diagnostics.runtime, "example-runtime");
    assert_eq!(diagnostics.id, "example-runtime-cli");
    assert_eq!(diagnostics.label.as_deref(), Some("Example Runtime CLI"));
    assert_eq!(diagnostics.env, vec!["EXAMPLE_RUNTIME_BIN".to_string()]);
    assert_eq!(diagnostics.candidates, vec!["example-runtime".to_string()]);
    assert_eq!(diagnostics.version_command, vec!["--version".to_string()]);
    assert_eq!(diagnostics.diagnostic_state, "declared");
    let serialized = serde_json::to_string(&diagnostics).expect("serialize diagnostics");
    assert!(serialized.contains("example-runtime"));
    assert!(!serialized.contains("sample-runtime"));
}

#[test]
fn declared_bench_followups_preserve_existing_status_guidance() {
    let followups = declared_run_followups_for_legacy("managed_followups", Some("bench"), None);
    let serialized = serde_json::to_string(&followups).expect("serialize followups");

    assert!(serialized.contains("latest_bench_run"));
    assert!(serialized.contains("homeboy runs latest-run --kind bench"));
    assert!(serialized.contains("homeboy runs refs --kind bench --limit 10"));
    assert!(serialized.contains("homeboy runs artifacts <run-id>"));
    assert!(!serialized.contains("latest_fuzz_run"));
    assert!(!serialized.contains("homeboy runs refs --kind fuzz --limit 10"));
}

#[test]
fn declared_fuzz_followups_preserve_existing_status_guidance() {
    let followups = declared_run_followups_for_legacy("managed_followups", Some("fuzz"), None);
    let serialized = serde_json::to_string(&followups).expect("serialize followups");

    assert!(serialized.contains("latest_fuzz_run"));
    assert!(serialized.contains("homeboy runs latest-run --kind fuzz"));
    assert!(serialized.contains("homeboy runs refs --kind fuzz --limit 10"));
    assert!(serialized.contains("homeboy runs evidence <run-id>"));
    assert!(!serialized.contains("latest_bench_run"));
    assert!(!serialized.contains("homeboy runs refs --kind bench --limit 10"));
}

#[test]
fn unknown_workload_does_not_emit_declared_bench_or_fuzz_followups() {
    let followups = declared_run_followups_for_legacy("managed_followups", Some("unknown"), None);
    let serialized = serde_json::to_string(&followups).expect("serialize followups");

    assert!(serialized.contains("recent_runs"));
    assert!(serialized.contains("run_artifacts"));
    assert!(!serialized.contains("latest_bench_run"));
    assert!(!serialized.contains("latest_fuzz_run"));
    assert!(!serialized.contains("--kind bench"));
    assert!(!serialized.contains("--kind fuzz"));
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

#[test]
fn runner_homeboy_status_distinguishes_daemon_and_job_binary_roles() {
    let report = RunnerStatusReport {
        runner_id: "homeboy-lab".to_string(),
        connected: true,
        state: runner::RunnerSessionState::Connected,
        session: Some(RunnerSession {
            runner_id: "homeboy-lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: runner::RunnerSessionRole::Controller,
            server_id: Some("homeboy-lab".to_string()),
            controller_id: None,
            broker_url: None,
            remote_daemon_address: Some("127.0.0.1:7357".to_string()),
            local_port: Some(7357),
            local_url: Some("http://127.0.0.1:7357".to_string()),
            tunnel_pid: Some(123),
            remote_daemon_pid: Some(456),
            homeboy_version: "0.262.0".to_string(),
            homeboy_build_identity: Some("0.262.0 old-build".to_string()),
            connected_at: "2026-06-19T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
        }),
        stale_daemon: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_jobs: Vec::new(),
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::Available,
        active_job_source: Some(RunnerActiveJobSource::DirectDaemon),
        active_job_error: None,
        session_path: "/tmp/session.json".to_string(),
    };

    let output = lab_runner_homeboy_output("homeboy-lab", "/opt/homeboy/bin/homeboy", &report);
    let serialized = serde_json::to_string(&output).expect("serialize runner homeboy output");

    assert!(serialized.contains("controller_cli"));
    assert!(serialized.contains("active_daemon"));
    assert!(serialized.contains("configured_job_binary"));
    assert!(serialized.contains("runner_config.settings.homeboy_path"));
    assert!(serialized.contains("/opt/homeboy/bin/homeboy tunnel artifact-origin dom-boxes --help"));
    assert!(serialized.contains("Recent or already-queued runner workflows"));
    assert_eq!(output.controller_cli.role, "controller_cli");
    assert_eq!(output.active_daemon.role, "active_daemon");
    assert_eq!(output.configured_job_binary.role, "configured_job_binary");
    assert_eq!(
        output.configured_job_binary.path.as_deref(),
        Some("/opt/homeboy/bin/homeboy")
    );
    assert_eq!(output.active_daemon_version.as_deref(), Some("0.262.0"));
}

fn selected_runtime_tool_declaration() -> AgentRuntimeToolDiagnosticDeclaration {
    AgentRuntimeToolDiagnosticDeclaration {
        tool: "sample-runtime".to_string(),
        legacy_output: Some("selected_runtime".to_string()),
        configured_binary_env: vec![
            "HOMEBOY_SAMPLE_RUNTIME_BIN".to_string(),
            "HOMEBOY_SETTINGS_SAMPLE_RUNTIME_BIN".to_string(),
        ],
        install_dir_env: Some("HOMEBOY_SAMPLE_RUNTIME_INSTALL_DIR".to_string()),
        default_install_dir: Some("${HOME}/.cache/homeboy/sample-runtime".to_string()),
        managed_cache_source: "${install_dir}/source".to_string(),
        managed_cache_binary: "${managed_cache_source}/packages/cli/dist/index.js".to_string(),
        effective_binary_rule:
            "managed cache binary wins when executable; otherwise configured binary, then PATH"
                .to_string(),
        diagnostic_script: "configured=${HOMEBOY_SAMPLE_RUNTIME_BIN:-${HOMEBOY_SETTINGS_SAMPLE_RUNTIME_BIN:-}}; install_dir=${HOMEBOY_SAMPLE_RUNTIME_INSTALL_DIR:-$HOME/.cache/homeboy/sample-runtime}; managed_source=$install_dir/source; managed_binary=$managed_source/packages/cli/dist/index.js; if [ -x \"$managed_binary\" ]; then effective=$managed_binary; source=managed_cache; elif [ -n \"$configured\" ]; then effective=$configured; source=configured; else effective=$(command -v sample-runtime 2>/dev/null || true); source=path; fi; revision=$(git -C \"$managed_source\" rev-parse --short HEAD 2>/dev/null || true); printf 'configured_binary=%s\nmanaged_cache_source=%s\nmanaged_cache_binary=%s\neffective_binary=%s\neffective_source=%s\nmanaged_cache_revision=%s\n' \"${configured:-}\" \"$managed_source\" \"$managed_binary\" \"${effective:-}\" \"$source\" \"${revision:-}\"".to_string(),
        extra: BTreeMap::new(),
    }
}

fn sample_runtime_declaration() -> AgentRuntimeRuntimeDiagnosticDeclaration {
    AgentRuntimeRuntimeDiagnosticDeclaration {
        tool: "sample-runtime".to_string(),
        legacy_output: Some("sample_runtime".to_string()),
        configured_binary_env: vec![
            "HOMEBOY_SAMPLE_RUNTIME_BIN".to_string(),
            "HOMEBOY_SETTINGS_SAMPLE_RUNTIME_BIN".to_string(),
        ],
        install_dir_env: Some("HOMEBOY_SAMPLE_RUNTIME_INSTALL_DIR".to_string()),
        default_install_dir: Some("${HOME}/.cache/homeboy/sample-runtime".to_string()),
        managed_cache_source: "${install_dir}/source".to_string(),
        managed_cache_binary: "${managed_cache_source}/packages/cli/dist/index.js".to_string(),
        effective_binary_rule:
            "managed cache binary wins when executable; otherwise configured binary, then PATH"
                .to_string(),
        packages: vec![
            AgentRuntimePackageDiagnosticDeclaration {
                field: "playground_package".to_string(),
                package: "@automattic/sample-runtime-playground".to_string(),
                expected_path: "${managed_cache_source}/packages/playground".to_string(),
                env_override: None,
            },
            AgentRuntimePackageDiagnosticDeclaration {
                field: "core_package".to_string(),
                package: "@automattic/sample-runtime-core".to_string(),
                expected_path: "${managed_cache_source}/packages/core".to_string(),
                env_override: Some("HOMEBOY_SAMPLE_RUNTIME_CORE_MODULE".to_string()),
            },
        ],
        probes: vec![
            AgentRuntimeProbeDiagnosticDeclaration {
                field: "source_git_sha".to_string(),
                source: "runtime_probe_command".to_string(),
            },
            AgentRuntimeProbeDiagnosticDeclaration {
                field: "dist_build_freshness".to_string(),
                source: "runtime_probe_command".to_string(),
            },
        ],
        runtime_probe_script: "configured=${HOMEBOY_SAMPLE_RUNTIME_BIN:-${HOMEBOY_SETTINGS_SAMPLE_RUNTIME_BIN:-}}; install_dir=${HOMEBOY_SAMPLE_RUNTIME_INSTALL_DIR:-$HOME/.cache/homeboy/sample-runtime}; managed_source=$install_dir/source; managed_binary=$managed_source/packages/cli/dist/index.js; if [ -x \"$managed_binary\" ]; then effective=$managed_binary; source=managed_cache; elif [ -n \"$configured\" ]; then effective=$configured; source=configured; else effective=$(command -v sample-runtime 2>/dev/null || true); source=path; fi; resolve_pkg() { node -e 'const path=require(\"path\"); try { const p=require.resolve(process.argv[2] + \"/package.json\", { paths: [process.argv[1]] }); process.stdout.write(path.dirname(p)); } catch (error) {}' \"$managed_source\" \"$1\" 2>/dev/null || true; }; playground=$(resolve_pkg @automattic/sample-runtime-playground); core=$(resolve_pkg @automattic/sample-runtime-core); if [ -z \"$core\" ] && [ -n \"${HOMEBOY_SAMPLE_RUNTIME_CORE_MODULE:-}\" ]; then core=$HOMEBOY_SAMPLE_RUNTIME_CORE_MODULE; fi; revision=$(git -C \"$managed_source\" rev-parse HEAD 2>/dev/null || true); if [ ! -e \"$managed_binary\" ]; then dist_state=missing; elif find \"$managed_source/packages/cli/src\" -type f -newer \"$managed_binary\" 2>/dev/null | read newer; then dist_state=stale; else dist_state=fresh; fi; printf 'cli_binary=%s\ncli_binary_source=%s\nmanaged_cache_source=%s\nmanaged_cache_binary=%s\nplayground_path=%s\ncore_path=%s\nsource_git_sha=%s\ndist_build_freshness=%s\n' \"${effective:-}\" \"$source\" \"$managed_source\" \"$managed_binary\" \"${playground:-}\" \"${core:-}\" \"${revision:-}\" \"$dist_state\"".to_string(),
        source_consistency: vec![
            AgentRuntimeSourceConsistencyDiagnostic {
                id: "selected_runtime.mixed_cli_source".to_string(),
                severity: "warning".to_string(),
                path: "configured_binary".to_string(),
                root: "${managed_cache_source}".to_string(),
                message: "Configured Managed Sandbox CLI `${path}` is outside managed cache `${root}`; runner jobs may mix a stale CLI with managed package sources.".to_string(),
                remediation: "Unset HOMEBOY_SAMPLE_RUNTIME_BIN/HOMEBOY_SETTINGS_SAMPLE_RUNTIME_BIN or point it at the managed cache binary reported here.".to_string(),
            },
            AgentRuntimeSourceConsistencyDiagnostic {
                id: "selected_runtime.mixed_core_source".to_string(),
                severity: "warning".to_string(),
                path: "HOMEBOY_SAMPLE_RUNTIME_CORE_MODULE".to_string(),
                root: "${managed_cache_source}".to_string(),
                message: "Resolved Managed Sandbox core path `${path}` is outside managed cache `${root}`; runner jobs may mix core and playground builds.".to_string(),
                remediation: "Refresh the extension setup/runtime env so HOMEBOY_SAMPLE_RUNTIME_CORE_MODULE resolves from the same managed sandbox checkout used by the CLI.".to_string(),
            },
        ],
        extra: BTreeMap::new(),
    }
}
