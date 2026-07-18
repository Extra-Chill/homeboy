use super::*;
use crate::{
    RunnerActiveJobSource, RunnerActiveJobState, RunnerSession, RunnerSessionRole,
    RunnerSessionState, RunnerStaleDaemonWarning, RunnerStatusReport, RunnerTunnelMode,
};
use homeboy_core::runner_execution_envelope::{
    PATH_MATERIALIZATION_MODE_GIT, PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT,
    PATH_MATERIALIZATION_STATUS_MATERIALIZED,
};
use serde_json::json;

#[test]
fn daemon_submission_recovers_a_lost_tunnel_before_resending_to_the_same_lease() {
    let accepted = direct_daemon_session("lease-live", "http://127.0.0.1:1");
    let submitted = std::cell::RefCell::new(Vec::new());
    let recovered = std::cell::Cell::new(false);

    let response = submit_daemon_exec_with_session_recovery(
        "http://127.0.0.1:1",
        Some(&accepted),
        |endpoint| {
            submitted.borrow_mut().push(endpoint.to_string());
            if submitted.borrow().len() == 1 {
                return Err(connect_error());
            }
            Ok(DaemonHttpTextResponse {
                status_code: 200,
                body: "{}".to_string(),
            })
        },
        |session| {
            recovered.set(true);
            assert_eq!(
                session.remote_daemon_lease_id.as_deref(),
                Some("lease-live")
            );
            Ok("http://127.0.0.1:2".to_string())
        },
    )
    .expect("recovered tunnel submits once to the proven daemon");

    assert!(recovered.get());
    assert_eq!(response.status_code, 200);
    assert_eq!(
        submitted.into_inner(),
        ["http://127.0.0.1:1", "http://127.0.0.1:2"]
    );
}

#[test]
fn daemon_submission_refuses_recovery_when_the_lease_changes() {
    let accepted = direct_daemon_session("lease-old", "http://127.0.0.1:1");
    let submissions = std::cell::Cell::new(0);
    let result = submit_daemon_exec_with_session_recovery(
        "http://127.0.0.1:1",
        Some(&accepted),
        |_| {
            submissions.set(submissions.get() + 1);
            Err(connect_error())
        },
        |session| {
            assert_eq!(session.remote_daemon_lease_id.as_deref(), Some("lease-old"));
            Err(Error::new(
                homeboy_core::error::ErrorCode::InternalUnexpected,
                "runner `lab` recovered a different daemon lease; refusing to submit a request proven for lease `lease-old`",
                json!({}),
            ))
        },
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("a replacement daemon cannot receive the old session's submission"),
    };

    assert_eq!(submissions.get(), 1);
    assert!(error.message.contains("different daemon lease"));
}

fn direct_daemon_session(lease: &str, local_url: &str) -> RunnerSession {
    RunnerSession {
        runner_id: "lab".to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some("lab".to_string()),
        controller_id: Some("test".to_string()),
        broker_url: None,
        remote_daemon_address: Some("127.0.0.1:4545".to_string()),
        local_port: Some(4545),
        local_url: Some(local_url.to_string()),
        tunnel_pid: None,
        remote_daemon_pid: Some(42),
        remote_daemon_lease_id: Some(lease.to_string()),
        homeboy_version: "test".to_string(),
        homeboy_build_identity: Some("homeboy test+live".to_string()),
        connected_at: "2026-07-17T00:00:00Z".to_string(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    }
}

fn connect_error() -> Error {
    Error::new(
        homeboy_core::error::ErrorCode::InternalUnexpected,
        "connection refused",
        json!({ "daemon_transport_error": { "kind": "connect" } }),
    )
}

#[test]
fn explicit_refresh_allows_an_idle_stale_daemon_after_reconnect() {
    let mut status = stale_direct_daemon_status();
    status.active_job_state = RunnerActiveJobState::Unavailable;
    status.active_job_source = None;
    status.active_job_error = Some(crate::RunnerActiveJobError {
        code: "jobs_unavailable".to_string(),
        message: "typed jobs endpoint belongs to the stale daemon".to_string(),
    });
    status.daemon_freshness = Some(authoritative_drained_freshness());

    assert!(allows_idle_stale_daemon_refresh(
        &explicit_refresh_options(),
        &status,
    ));
}

#[test]
fn explicit_refresh_keeps_active_or_uncertain_stale_daemons_protected() {
    let options = explicit_refresh_options();
    let mut active = stale_direct_daemon_status();
    active.active_job_count = 1;
    active.daemon_freshness = Some(homeboy_core::daemon::DaemonFreshnessReport {
        active_jobs: 1,
        ..authoritative_drained_freshness()
    });
    let mut unavailable = stale_direct_daemon_status();
    unavailable.active_job_state = RunnerActiveJobState::Unavailable;

    assert!(!allows_idle_stale_daemon_refresh(&options, &active));
    assert!(!allows_idle_stale_daemon_refresh(&options, &unavailable));
}

#[test]
fn runner_exec_accepts_a_fresh_daemon_when_a_scoped_session_projection_is_stale() {
    let mut status = stale_direct_daemon_status();
    status.daemon_freshness = Some(homeboy_core::daemon::DaemonFreshnessReport {
        fresh: true,
        stale_reason_code: None,
        ..authoritative_drained_freshness()
    });

    assert!(!refuses_stale_daemon_execution(
        &RunnerExecOptions::raw_command(vec!["cargo".to_string()]),
        &status,
    ));
}

#[test]
fn runner_exec_keeps_stale_session_projections_fail_closed_without_fresh_daemon_evidence() {
    assert!(refuses_stale_daemon_execution(
        &RunnerExecOptions::raw_command(vec!["cargo".to_string()]),
        &stale_direct_daemon_status(),
    ));
}

#[test]
fn refresh_execution_uses_the_connected_preflight_session_without_a_second_lookup() {
    let mut status = stale_direct_daemon_status();
    status.session = Some(RunnerSession {
        runner_id: "homeboy-lab".to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some("homeboy-lab".to_string()),
        controller_id: Some("controller".to_string()),
        broker_url: None,
        remote_daemon_address: Some("127.0.0.1:4242".to_string()),
        local_port: Some(4242),
        local_url: Some("http://127.0.0.1:4242".to_string()),
        tunnel_pid: Some(1),
        remote_daemon_pid: Some(2),
        remote_daemon_lease_id: Some("idempotently-reattached-lease".to_string()),
        homeboy_version: "0.288.10".to_string(),
        homeboy_build_identity: None,
        connected_at: "2026-07-17T00:00:00Z".to_string(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    });
    let resolved = execution_status("unresolvable-runner", Some(status.clone()))
        .expect("the refresh transaction keeps its authoritative preflight session");

    assert_eq!(resolved.runner_id, "homeboy-lab");
    assert!(resolved.connected);
    assert_eq!(resolved.session_path, "/tmp/homeboy-lab.json");
    assert_eq!(
        resolved
            .session
            .as_ref()
            .and_then(|session| session.remote_daemon_lease_id.as_deref()),
        Some("idempotently-reattached-lease")
    );
}

fn authoritative_drained_freshness() -> homeboy_core::daemon::DaemonFreshnessReport {
    homeboy_core::daemon::DaemonFreshnessReport {
        fresh: false,
        stale_reason_code: Some(homeboy_core::daemon::DaemonStaleReasonCode::VersionMismatch),
        restartable: true,
        lease_id: Some("lease-stale".to_string()),
        pid: Some(4242),
        recovery_evidence: None,
        ownership_evidence: None,
        adoption_command: None,
        binary_hash: None,
        daemon_version: Some("0.288.8".to_string()),
        daemon_build_identity: Some("homeboy 0.288.8+stale".to_string()),
        runtime_paths: None,
        active_jobs: 0,
        termination_evidence: None,
        repair_plan: Vec::new(),
    }
}

fn explicit_refresh_options() -> RunnerExecOptions {
    RunnerExecOptions::raw_command(vec!["bash".to_string()]).with_capability_preflight(
        RunnerCapabilityPreflight {
            command: "runner.refresh-homeboy".to_string(),
            ..Default::default()
        },
    )
}

fn stale_direct_daemon_status() -> RunnerStatusReport {
    RunnerStatusReport {
        runner_id: "homeboy-lab".to_string(),
        connected: true,
        state: RunnerSessionState::Connected,
        session: None,
        stale_daemon: Some(RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "homeboy 0.288.8".to_string(),
            "homeboy 0.288.9".to_string(),
            None,
            None,
        )),
        daemon_freshness: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        stale_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::Available,
        active_job_source: Some(RunnerActiveJobSource::DirectDaemon),
        active_job_error: None,
        active_job_recovery_evidence: None,
        session_path: "/tmp/homeboy-lab.json".to_string(),
    }
}

#[test]
fn runner_execution_record_uses_dispatched_path_materialization_plan() {
    let dispatched_plan = PathMaterializationPlan::new([PathMaterializationEntry::new(
        "primary_workspace",
        PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT,
        Some("/controller/worktree".to_string()),
        "/runner/context-plan",
        PATH_MATERIALIZATION_MODE_GIT,
        PATH_MATERIALIZATION_STATUS_MATERIALIZED,
    )]);
    let snapshot = SourceSnapshot::existing_remote("lab", "/runner/source-snapshot", Some("/srv"));
    let (output, _) = exec_output(
        &ssh_runner(),
        RunnerExecMode::Daemon,
        "/runner/context-plan".to_string(),
        vec!["homeboy".to_string(), "test".to_string()],
        ProcessOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            metrics: None,
            capture: None,
        },
        Some(snapshot),
        Some(dispatched_plan.clone()),
        vec!["/runner/required-path".to_string()],
        &Default::default(),
        &[],
    );

    assert_eq!(
        output
            .execution_record
            .expect("execution record")
            .path_materialization_plan,
        Some(dispatched_plan)
    );
}

#[test]
fn remote_daemon_secret_env_refs_forward_controller_secrets_and_keep_runner_refs_local() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let secret_file = temp.path().join("runner-secret");
        std::fs::write(&secret_file, "dummy-runner-secret\n").expect("secret file");
        homeboy_agents::agent_task_secrets::set_config_secret(
            "HOMEBOY_CONTROLLER_SECRET_TEST_KEY",
            "dummy-controller-secret",
        )
        .expect("configure controller secret");

        let mut controller_runner = ssh_runner();
        controller_runner.workspace_root = Some(workspace.display().to_string());
        controller_runner.secret_env.insert(
            "CONTROLLER_API_KEY".to_string(),
            RunnerSecretEnvRef {
                env: None,
                file: None,
                secret: Some("HOMEBOY_CONTROLLER_SECRET_TEST_KEY".to_string()),
            },
        );
        controller_runner.secret_env.insert(
            "RUNNER_API_KEY".to_string(),
            RunnerSecretEnvRef {
                env: None,
                file: Some(secret_file.display().to_string()),
                secret: None,
            },
        );

        let controller_plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(controller_runner),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec!["true".to_string()],
            env: Default::default(),
            secret_env_names: vec![
                "CONTROLLER_API_KEY".to_string(),
                "RUNNER_API_KEY".to_string(),
            ],
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: Some(SourceSnapshot::existing_remote(
                "lab",
                &workspace.display().to_string(),
                Some(&workspace.display().to_string()),
            )),
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("controller prep forwards configured secret refs for SSH runner");

        assert_eq!(
            controller_plan
                .env
                .get("CONTROLLER_API_KEY")
                .map(String::as_str),
            Some("dummy-controller-secret")
        );
        assert!(!controller_plan.env.contains_key("RUNNER_API_KEY"));

        let mut daemon_runner = ssh_runner();
        daemon_runner.workspace_root = Some(workspace.display().to_string());
        daemon_runner.secret_env.insert(
            "RUNNER_API_KEY".to_string(),
            RunnerSecretEnvRef {
                env: None,
                file: Some(secret_file.display().to_string()),
                secret: None,
            },
        );

        let daemon_plan = prepare_daemon_local_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(daemon_runner),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec![
                "homeboy".to_string(),
                "trace".to_string(),
                "compare".to_string(),
                "demo".to_string(),
                "scenario".to_string(),
                "--secret-env=RUNNER_API_KEY".to_string(),
            ],
            env: Default::default(),
            secret_env_names: vec!["RUNNER_API_KEY".to_string()],
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: Some(SourceSnapshot::existing_remote(
                "lab",
                &workspace.display().to_string(),
                Some(&workspace.display().to_string()),
            )),
            require_paths: Vec::new(),
            validate_require_paths_on_host: true,
        })
        .expect("daemon prep resolves secret refs on runner side");

        assert_eq!(
            daemon_plan.env.get("RUNNER_API_KEY").map(String::as_str),
            Some("dummy-runner-secret")
        );
    });
}

#[test]
fn remote_daemon_secret_env_refs_require_missing_controller_refs() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let mut runner = ssh_runner();
        runner.workspace_root = Some(workspace.display().to_string());

        let err = prepare_runner_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(runner),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec!["true".to_string()],
            env: Default::default(),
            secret_env_names: vec!["MISSING_CONTROLLER_SECRET".to_string()],
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: Some(SourceSnapshot::existing_remote(
                "lab",
                &workspace.display().to_string(),
                Some(&workspace.display().to_string()),
            )),
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect_err("missing controller secret ref should fail during controller prep");

        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(err.details["field"], "secret_env");
        assert!(err.message.contains("MISSING_CONTROLLER_SECRET"));
    });
}

#[test]
fn daemon_read_only_runner_exec_ignores_unrelated_missing_secret_env_refs() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let mut runner = ssh_runner();
        runner.workspace_root = Some(workspace.display().to_string());
        runner.secret_env.insert(
            "HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string(),
            RunnerSecretEnvRef {
                env: Some("HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()),
                file: None,
                secret: None,
            },
        );
        std::env::remove_var("HOMEBOY_PREVIEW_TUNNEL_TOKEN");

        let plan = prepare_daemon_local_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(runner),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec![
                "/bin/ps".to_string(),
                "-eo".to_string(),
                "pid,ppid,etime,stat,pcpu,pmem,cmd".to_string(),
            ],
            env: Default::default(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: true,
            source_snapshot: Some(SourceSnapshot::existing_remote(
                "lab",
                &workspace.display().to_string(),
                Some(&workspace.display().to_string()),
            )),
            require_paths: Vec::new(),
            validate_require_paths_on_host: true,
        })
        .expect("read-only runner exec ignores unrelated optional secret refs");

        assert!(!plan.env.contains_key("HOMEBOY_PREVIEW_TUNNEL_TOKEN"));
    });
}

#[test]
fn daemon_runner_exec_requires_declared_missing_secret_env_refs() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::env::remove_var("HOMEBOY_REQUIRED_SECRET_TEST_KEY");

        let mut runner = ssh_runner();
        runner.workspace_root = Some(workspace.display().to_string());
        runner.secret_env.insert(
            "HOMEBOY_REQUIRED_SECRET_TEST_KEY".to_string(),
            RunnerSecretEnvRef {
                env: Some("HOMEBOY_REQUIRED_SECRET_TEST_KEY".to_string()),
                file: None,
                secret: None,
            },
        );

        let err = prepare_daemon_local_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(runner),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec![
                "homeboy".to_string(),
                "trace".to_string(),
                "compare".to_string(),
                "demo".to_string(),
                "scenario".to_string(),
                "--secret-env=HOMEBOY_REQUIRED_SECRET_TEST_KEY".to_string(),
            ],
            env: Default::default(),
            secret_env_names: vec!["HOMEBOY_REQUIRED_SECRET_TEST_KEY".to_string()],
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: Some(SourceSnapshot::existing_remote(
                "lab",
                &workspace.display().to_string(),
                Some(&workspace.display().to_string()),
            )),
            require_paths: Vec::new(),
            validate_require_paths_on_host: true,
        })
        .expect_err("declared missing command secret should fail validation");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert_eq!(err.details["field"], "secret_env");
        assert!(err.message.contains("HOMEBOY_REQUIRED_SECRET_TEST_KEY"));
    });
}

#[test]
fn runner_exec_secret_env_names_include_tunnel_preview_client_token() {
    let names = runner_exec_secret_env_names(
        &[
            "homeboy".to_string(),
            "tunnel".to_string(),
            "preview-client".to_string(),
            "start".to_string(),
            "--ingress".to_string(),
            "https://preview-broker.example.test".to_string(),
            "--public-host".to_string(),
            "preview.example.test".to_string(),
            "--local-origin".to_string(),
            "http://127.0.0.1:8888".to_string(),
        ],
        None,
        &[],
        &HashMap::new(),
    );

    assert_eq!(names, vec!["HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()]);
}

#[test]
fn runner_exec_secret_env_names_carry_no_hardcoded_provider_literals() {
    // Generic core must not embed provider-specific token names. A bare
    // provider hint without an explicit allowlist contributes no secret env
    // names; the runtime/extension is the authority and declares them itself
    // (generic-core rule, #6676).
    let names = runner_exec_secret_env_names(
        &["node".to_string(), "run-headless-loop.cjs".to_string()],
        None,
        &[],
        &HashMap::from([(
            "HOMEBOY_AGENT_RUNTIME_PROVIDER".to_string(),
            "codex".to_string(),
        )]),
    );

    assert!(
        names.is_empty(),
        "provider hint alone must not imply provider token names: {names:?}"
    );
}

#[test]
fn runner_exec_secret_env_names_use_runtime_declared_allowlist() {
    // A codex-style runtime keeps working by declaring the exact names it owns
    // through the generic allowlist — no core change per provider.
    let names = runner_exec_secret_env_names(
        &["node".to_string(), "run-headless-loop.cjs".to_string()],
        None,
        &[],
        &HashMap::from([
            (
                "HOMEBOY_AGENT_RUNTIME_PROVIDER".to_string(),
                "codex".to_string(),
            ),
            (
                "HOMEBOY_AGENT_RUNTIME_SECRET_ENV".to_string(),
                "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN,AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN"
                    .to_string(),
            ),
        ]),
    );

    assert_eq!(
        names,
        vec![
            "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN".to_string(),
        ]
    );
}

#[test]
fn runner_exec_secret_env_plan_preserves_explicit_plan_fields() {
    let plan = runner_exec_secret_env_plan(
        &["node".to_string(), "run-headless-loop.cjs".to_string()],
        Some(&RunnerCapabilityPreflight {
            command: "runner.exec".to_string(),
            required_env: vec!["PREFLIGHT_SECRET".to_string()],
            ..Default::default()
        }),
        &["CLI_SECRET".to_string()],
        &HashMap::from([(
            "HOMEBOY_AGENT_RUNTIME_SECRET_ENV".to_string(),
            "RUNTIME_SECRET".to_string(),
        )]),
        Some(homeboy_core::secret_env_plan::SecretEnvPlan {
            public_env: std::collections::BTreeMap::from([(
                "PUBLIC_FLAG".to_string(),
                "1".to_string(),
            )]),
            requirements: vec![homeboy_core::secret_env_plan::SecretEnvRequirement {
                name: "PLAN_SECRET".to_string(),
                required: true,
                source_env_names: vec!["PLAN_SOURCE_SECRET".to_string()],
                refresh: Some(homeboy_core::secret_env_plan::SecretEnvRefreshHint {
                    provider: "provider-auth".to_string(),
                    metadata: Default::default(),
                }),
            }],
            ..Default::default()
        }),
    );

    assert_eq!(plan.public_env.get("PUBLIC_FLAG"), Some(&"1".to_string()));
    assert_eq!(
        plan.secret_env_names(),
        vec![
            "CLI_SECRET".to_string(),
            "PLAN_SECRET".to_string(),
            "PLAN_SOURCE_SECRET".to_string(),
            "PREFLIGHT_SECRET".to_string(),
            "RUNTIME_SECRET".to_string()
        ]
    );
    assert_eq!(
        plan.requirements[0]
            .refresh
            .as_ref()
            .map(|hint| hint.provider.as_str()),
        Some("provider-auth")
    );
}

#[test]
fn runner_exec_secret_env_names_prefer_explicit_runtime_secret_env() {
    let names = runner_exec_secret_env_names(
        &["node".to_string(), "run-headless-loop.cjs".to_string()],
        None,
        &[],
        &HashMap::from([
            (
                "HOMEBOY_AGENT_RUNTIME_PROVIDER".to_string(),
                "codex".to_string(),
            ),
            (
                "HOMEBOY_AGENT_RUNTIME_SECRET_ENV".to_string(),
                "CUSTOM_REFRESH,CUSTOM_ACCESS".to_string(),
            ),
        ]),
    );

    assert_eq!(
        names,
        vec!["CUSTOM_ACCESS".to_string(), "CUSTOM_REFRESH".to_string()]
    );
}

#[test]
fn worker_local_workload_validation_uses_implicit_command_secret_names() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        super::super::super::create(
            &serde_json::json!({
                "id": "lab-local",
                "kind": "local",
                "workspace_root": workspace.display().to_string(),
            })
            .to_string(),
            false,
        )
        .expect("create local runner");

        let plan = homeboy_core::plan::HomeboyPlan::builder_for_description(
            homeboy_core::plan::PlanKind::LabOffload,
            "test",
        )
        .build();
        let command_contract = crate::LabOffloadCommand {
            command: homeboy_core::lab_contract::LabCommandContract::portable(
                "tunnel preview-client start",
                None,
                false,
                &[],
            ),
            required_extensions: Vec::new(),
            required_capabilities: Vec::new(),
            workload: None,
        };
        let workload = crate::workload::build_lab_runner_workload(
            crate::workload::LabRunnerWorkloadBuildInput {
                plan: &plan,
                command: &command_contract,
                capture_patch: false,
                mutation_flag: None,
                allow_dirty_lab_workspace: false,
                runner_id: "lab-local",
                runner_mode: "worker_local",
                assignment_source: "worker",
                status: "claimed",
                remote_workspace: Some(&workspace.display().to_string()),
                fallback_reason: None,
                workspace_mapping_ref: None,
                proof_id: None,
            },
        );

        let command = vec![
            "homeboy".to_string(),
            "tunnel".to_string(),
            "preview-client".to_string(),
            "start".to_string(),
            "--ingress".to_string(),
            "https://preview-broker.example.test".to_string(),
            "--public-host".to_string(),
            "preview.example.test".to_string(),
            "--local-origin".to_string(),
            "http://127.0.0.1:8888".to_string(),
        ];
        let mut env = std::collections::HashMap::new();
        env.insert(
            "HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string(),
            "dummy-token".to_string(),
        );

        let (_output, exit_code) = super::super::worker::exec_worker_local_with_process_output(
            "lab-local",
            RunnerExecOptions {
                cwd: Some(workspace.display().to_string()),
                project_id: None,
                allow_diagnostic_ssh: false,
                diagnostic_ssh_timeout: None,
                command,
                env,
                secret_env_names: Vec::new(),
                secret_env_plan: None,
                env_materialization: None,
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                path_materialization_plan: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                accepted_extension_settings: Vec::new(),
                require_paths: Vec::new(),
                lab_runner_workload: Some(workload),
                run_id: None,
                detach_after_handoff: false,
                mirror_evidence: true,
                print_handoff: true,
            },
            |_plan| {
                Ok(ProcessOutput {
                    stdout: "ok".to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                    metrics: None,
                    capture: None,
                })
            },
        )
        .expect("worker-local validation accepts implicit command secret names");

        assert_eq!(exit_code, 0);
    });
}

#[test]
fn test_exec_runs_local_runner_command() {
    homeboy_core::test_support::with_isolated_home(|_| {
        super::super::super::create(r#"{"id":"lab-local","kind":"local"}"#, false)
            .expect("create local runner");

        let (output, exit_code) = exec(
            "lab-local",
            RunnerExecOptions {
                cwd: None,
                project_id: None,
                allow_diagnostic_ssh: false,
                diagnostic_ssh_timeout: None,
                command: vec!["sh".to_string(), "-c".to_string(), "printf ok".to_string()],
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: None,
                env_materialization: None,
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                path_materialization_plan: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                accepted_extension_settings: Vec::new(),
                require_paths: Vec::new(),
                lab_runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
                mirror_evidence: true,
                print_handoff: true,
            },
        )
        .expect("exec local runner");

        assert_eq!(exit_code, 0);
        assert_eq!(output.runner_id, "lab-local");
        assert_eq!(output.mode, RunnerExecMode::Local);
        assert_eq!(output.stdout, "ok");
        let provenance = output
            .execution_record
            .as_ref()
            .and_then(|record| record.orchestration_provenance.as_ref())
            .expect("orchestration provenance");
        assert_eq!(provenance.selected_runner_id, "lab-local");
        assert_eq!(provenance.controller_binary.owner, "operator_command");
        assert_eq!(provenance.runner_daemon_binary.owner, "runner_session");
        assert_eq!(
            provenance.runner_command_binary.owner,
            "runner_config.settings.homeboy_path"
        );
        assert!(provenance.source_snapshot_identity.is_some());
        let metrics = output.metrics.expect("local exec metrics");
        assert!(metrics.duration_ms < 60_000);
        if cfg!(target_os = "linux") {
            assert_eq!(metrics.source, "linux_procfs_process_tree");
            if metrics.sample_count > 0 {
                assert!(metrics.peak_rss_bytes.is_some());
                assert!(metrics.child_process_count_peak.is_some());
            }
        } else {
            assert_eq!(metrics.source, "duration_only");
            assert_eq!(metrics.sample_count, 0);
        }
        let source_snapshot = output.source_snapshot.expect("source snapshot");
        assert_eq!(source_snapshot.runner_id, "lab-local");
        assert_eq!(
            source_snapshot.sync_mode,
            homeboy_core::runner_execution_envelope::PATH_MATERIALIZATION_MODE_EXISTING_REMOTE
        );
        assert!(source_snapshot.snapshot_hash.starts_with("sha256:"));
        assert!(output.job_id.is_none());
    });
}

#[test]
fn test_exec_does_not_leak_ambient_process_env() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let _guard = EnvVarGuard::set("HOMEBOY_TEST_AMBIENT_ONLY", "leaked");
        super::super::super::create(r#"{"id":"lab-local","kind":"local"}"#, false)
            .expect("create local runner");

        let (output, exit_code) = exec(
            "lab-local",
            RunnerExecOptions {
                cwd: None,
                project_id: None,
                allow_diagnostic_ssh: false,
                diagnostic_ssh_timeout: None,
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "test -z \"${HOMEBOY_TEST_AMBIENT_ONLY+x}\" && printf isolated".to_string(),
                ],
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: None,
                env_materialization: None,
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                path_materialization_plan: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                accepted_extension_settings: Vec::new(),
                require_paths: Vec::new(),
                lab_runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
                mirror_evidence: true,
                print_handoff: true,
            },
        )
        .expect("exec local runner");

        assert_eq!(exit_code, 0);
        assert_eq!(output.stdout, "isolated");
    });
}

#[test]
fn test_exec_preserves_explicit_request_env() {
    homeboy_core::test_support::with_isolated_home(|_| {
        super::super::super::create(r#"{"id":"lab-local","kind":"local"}"#, false)
            .expect("create local runner");

        let (output, exit_code) = exec(
            "lab-local",
            RunnerExecOptions {
                cwd: None,
                project_id: None,
                allow_diagnostic_ssh: false,
                diagnostic_ssh_timeout: None,
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf %s \"$HOMEBOY_TEST_EXPLICIT\"".to_string(),
                ],
                env: HashMap::from([("HOMEBOY_TEST_EXPLICIT".to_string(), "planned".to_string())]),
                secret_env_names: Vec::new(),
                secret_env_plan: None,
                env_materialization: None,
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                path_materialization_plan: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                accepted_extension_settings: Vec::new(),
                require_paths: Vec::new(),
                lab_runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
                mirror_evidence: true,
                print_handoff: true,
            },
        )
        .expect("exec local runner");

        assert_eq!(exit_code, 0);
        assert_eq!(output.stdout, "planned");
    });
}

#[test]
fn runner_exec_explicit_run_id_overrides_conflicting_run_id_env() {
    homeboy_core::test_support::with_isolated_home(|_| {
        super::super::super::create(r#"{"id":"lab-local","kind":"local"}"#, false)
            .expect("create local runner");

        let (output, exit_code) = exec(
            "lab-local",
            RunnerExecOptions {
                cwd: None,
                project_id: None,
                allow_diagnostic_ssh: false,
                diagnostic_ssh_timeout: None,
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf '%s|%s|%s|%s' \"$HOMEBOY_ACTIVE_RUN_ID\" \"$HOMEBOY_RUN_ID\" \"$HOMEBOY_BENCH_RUN_ID\" \"${WORKFLOW_BENCH_RUN_ID-unset}\"".to_string(),
                ],
                env: HashMap::from([
                    (
                        "HOMEBOY_ACTIVE_RUN_ID".to_string(),
                        "ambient-active".to_string(),
                    ),
                    ("HOMEBOY_RUN_ID".to_string(), "ambient-homeboy".to_string()),
                    (
                        "HOMEBOY_BENCH_RUN_ID".to_string(),
                        "ambient-bench".to_string(),
                    ),
                    (
                        "WORKFLOW_BENCH_RUN_ID".to_string(),
                        "ambient-workflow".to_string(),
                    ),
                ]),
                secret_env_names: Vec::new(),
                secret_env_plan: None,
                env_materialization: None,
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
            path_materialization_plan: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                accepted_extension_settings: Vec::new(),
                require_paths: Vec::new(),
                lab_runner_workload: None,
                run_id: Some("explicit-run".to_string()),
                detach_after_handoff: false,
                mirror_evidence: true,
                print_handoff: true,
            },
        )
        .expect("exec local runner");

        assert_eq!(exit_code, 0);
        assert_eq!(
            output.stdout,
            "explicit-run|explicit-run|explicit-run|unset"
        );
        let hints = output
            .diagnostics
            .expect("run-id diagnostics")
            .hints
            .join("\n");
        assert!(hints.contains("runner exec --run-id took precedence"));
        assert!(hints.contains("HOMEBOY_ACTIVE_RUN_ID"));
        assert!(hints.contains("HOMEBOY_RUN_ID"));
        assert!(hints.contains("HOMEBOY_BENCH_RUN_ID"));
        assert!(hints.contains("WORKFLOW_BENCH_RUN_ID"));
    });
}

#[test]
fn test_exec_rejects_missing_required_local_runner_path() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        super::super::super::create(
            &format!(
                r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                workspace.path().display()
            ),
            false,
        )
        .expect("create local runner");
        let missing = workspace.path().join("missing-worktree");

        let err = exec(
            "lab-local",
            RunnerExecOptions {
                cwd: None,
                project_id: None,
                allow_diagnostic_ssh: false,
                diagnostic_ssh_timeout: None,
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf nope".to_string(),
                ],
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: None,
                env_materialization: None,
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                path_materialization_plan: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                accepted_extension_settings: Vec::new(),
                require_paths: vec![missing.display().to_string()],
                lab_runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
                mirror_evidence: true,
                print_handoff: true,
            },
        )
        .expect_err("missing required path rejects before command");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert_eq!(err.details["field"], "require_path");
        assert!(err.message.contains("required runner path"));
        assert!(err.details["tried"].to_string().contains("_lab_workspaces"));
    });
}

#[test]
fn test_exec_reports_required_path_diagnostics() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        let required_path = workspace.path().join("project");
        std::fs::create_dir(&required_path).expect("required path");
        super::super::super::create(
            &format!(
                r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                workspace.path().display()
            ),
            false,
        )
        .expect("create local runner");

        let (output, exit_code) = exec(
            "lab-local",
            RunnerExecOptions {
                cwd: None,
                project_id: None,
                allow_diagnostic_ssh: false,
                diagnostic_ssh_timeout: None,
                command: vec!["sh".to_string(), "-c".to_string(), "printf ok".to_string()],
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: None,
                env_materialization: None,
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                path_materialization_plan: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                accepted_extension_settings: Vec::new(),
                require_paths: vec![required_path.display().to_string()],
                lab_runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
                mirror_evidence: true,
                print_handoff: true,
            },
        )
        .expect("exec with required path");

        assert_eq!(exit_code, 0);
        let diagnostics = output.diagnostics.expect("diagnostics");
        assert_eq!(
            diagnostics.runner_workspace_root,
            Some(workspace.path().display().to_string())
        );
        assert_eq!(
            diagnostics.required_paths,
            vec![required_path.display().to_string()]
        );
        assert!(diagnostics.source_snapshot_remote_path.is_some());
        assert!(diagnostics
            .hints
            .iter()
            .any(|hint| hint.contains("_lab_workspaces")));
    });
}

#[test]
fn test_exec_rejects_disconnected_ssh_runner_without_diagnostic_fallback() {
    homeboy_core::test_support::with_isolated_home(|_| {
        server::create(
            r#"{"id":"lab-server","host":"192.168.86.63","user":"user"}"#,
            false,
        )
        .expect("create server");

        // Configure an explicit remote homeboy_path so the extension-parity
        // preflight (which now refuses a bare `homeboy` on a remote runner) is
        // satisfied and the test reaches the daemon-connection rejection it is
        // actually exercising.
        super::super::super::create(
            r#"{"id":"lab-server","kind":"ssh","server_id":"lab-server","workspace_root":"/srv/homeboy","homeboy_path":"/usr/local/bin/homeboy"}"#,
            false,
        )
        .expect("create ssh runner");

        let err = exec(
            "lab-server",
            RunnerExecOptions {
                cwd: Some("/srv/homeboy/project".to_string()),
                project_id: None,
                allow_diagnostic_ssh: false,
                diagnostic_ssh_timeout: None,
                command: vec!["homeboy".to_string(), "test".to_string()],
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: None,
                env_materialization: None,
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                path_materialization_plan: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                accepted_extension_settings: Vec::new(),
                require_paths: Vec::new(),
                lab_runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
                mirror_evidence: true,
                print_handoff: true,
            },
        )
        .expect_err("disconnected ssh runner needs daemon or diagnostic fallback");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("connected to a daemon"));
        let tried = err.details["tried"].as_array().expect("tried details");
        assert!(tried.iter().any(|detail| detail
            .as_str()
            .is_some_and(|detail| detail.contains("job metadata"))));
    });
}

#[test]
fn test_diagnostic_ssh_mode_serializes_as_diagnostic_ssh() {
    assert_eq!(
        serde_json::to_value(RunnerExecMode::DiagnosticSsh).expect("mode json"),
        json!("diagnostic_ssh")
    );
}

#[test]
fn explicit_diagnostic_ssh_wins_for_ssh_runners() {
    let mut options = RunnerExecOptions {
        cwd: Some("/srv/homeboy/project".to_string()),
        project_id: None,
        allow_diagnostic_ssh: true,
        diagnostic_ssh_timeout: None,
        command: vec!["homeboy".to_string(), "--version".to_string()],
        env: Default::default(),
        secret_env_names: Vec::new(),
        secret_env_plan: None,
        env_materialization: None,
        capture_patch: false,
        raw_exec: true,
        source_snapshot: None,
        path_materialization_plan: None,
        capability_preflight: None,
        required_extensions: Vec::new(),
        accepted_extension_settings: Vec::new(),
        require_paths: Vec::new(),
        lab_runner_workload: None,
        run_id: None,
        detach_after_handoff: false,
        mirror_evidence: true,
        print_handoff: true,
    };

    assert!(should_force_diagnostic_ssh(&ssh_runner(), &options));
    options.allow_diagnostic_ssh = false;
    assert!(!should_force_diagnostic_ssh(&ssh_runner(), &options));
}
