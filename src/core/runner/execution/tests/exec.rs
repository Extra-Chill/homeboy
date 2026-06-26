use super::*;
use serde_json::json;

#[test]
fn remote_daemon_secret_env_refs_forward_controller_secrets_and_keep_runner_refs_local() {
    crate::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let secret_file = temp.path().join("runner-secret");
        std::fs::write(&secret_file, "dummy-runner-secret\n").expect("secret file");
        crate::core::agent_task_secrets::set_config_secret(
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
fn daemon_read_only_runner_exec_ignores_unrelated_missing_secret_env_refs() {
    crate::test_support::with_isolated_home(|_| {
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
    crate::test_support::with_isolated_home(|_| {
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
fn runner_exec_secret_env_names_include_runtime_provider_defaults() {
    let names = runner_exec_secret_env_names(
        &["node".to_string(), "run-headless-loop.cjs".to_string()],
        None,
        &[],
        &HashMap::from([(
            "HOMEBOY_AGENT_RUNTIME_PROVIDER".to_string(),
            "codex".to_string(),
        )]),
    );

    assert_eq!(
        names,
        vec![
            "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_FEDRAMP".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN".to_string(),
        ]
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
    crate::test_support::with_isolated_home(|_| {
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

        let plan = crate::core::plan::HomeboyPlan::builder_for_description(
            crate::core::plan::PlanKind::LabOffload,
            "test",
        )
        .build();
        let command_contract = crate::core::runner::LabOffloadCommand {
            hot_label: "tunnel preview-client start",
            portable: true,
            unsupported_reason: None,
            source_path_mode: crate::core::runner::LabOffloadSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy:
                crate::core::runner::LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            required_extensions: Vec::new(),
            requires_playwright: false,
            routing_policy: crate::command_contract::LabRoutingPolicy::default(),
        };
        let workload = crate::core::runner::workload::build_runner_workload(
            crate::core::runner::workload::RunnerWorkloadBuildInput {
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
                command,
                env,
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: Vec::new(),
                runner_workload: Some(workload),
                run_id: None,
                detach_after_handoff: false,
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
    crate::test_support::with_isolated_home(|_| {
        super::super::super::create(r#"{"id":"lab-local","kind":"local"}"#, false)
            .expect("create local runner");

        let (output, exit_code) = exec(
            "lab-local",
            RunnerExecOptions {
                cwd: None,
                project_id: None,
                allow_diagnostic_ssh: false,
                command: vec!["sh".to_string(), "-c".to_string(), "printf ok".to_string()],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: Vec::new(),
                runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
            },
        )
        .expect("exec local runner");

        assert_eq!(exit_code, 0);
        assert_eq!(output.runner_id, "lab-local");
        assert_eq!(output.mode, RunnerExecMode::Local);
        assert_eq!(output.stdout, "ok");
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
        assert_eq!(source_snapshot.sync_mode, "existing_remote");
        assert!(source_snapshot.snapshot_hash.starts_with("sha256:"));
        assert!(output.job_id.is_none());
    });
}

#[test]
fn test_exec_does_not_leak_ambient_process_env() {
    crate::test_support::with_isolated_home(|_| {
        let _guard = EnvVarGuard::set("HOMEBOY_TEST_AMBIENT_ONLY", "leaked");
        super::super::super::create(r#"{"id":"lab-local","kind":"local"}"#, false)
            .expect("create local runner");

        let (output, exit_code) = exec(
            "lab-local",
            RunnerExecOptions {
                cwd: None,
                project_id: None,
                allow_diagnostic_ssh: false,
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "test -z \"${HOMEBOY_TEST_AMBIENT_ONLY+x}\" && printf isolated".to_string(),
                ],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: Vec::new(),
                runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
            },
        )
        .expect("exec local runner");

        assert_eq!(exit_code, 0);
        assert_eq!(output.stdout, "isolated");
    });
}

#[test]
fn test_exec_preserves_explicit_request_env() {
    crate::test_support::with_isolated_home(|_| {
        super::super::super::create(r#"{"id":"lab-local","kind":"local"}"#, false)
            .expect("create local runner");

        let (output, exit_code) = exec(
            "lab-local",
            RunnerExecOptions {
                cwd: None,
                project_id: None,
                allow_diagnostic_ssh: false,
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf %s \"$HOMEBOY_TEST_EXPLICIT\"".to_string(),
                ],
                env: HashMap::from([("HOMEBOY_TEST_EXPLICIT".to_string(), "planned".to_string())]),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: Vec::new(),
                runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
            },
        )
        .expect("exec local runner");

        assert_eq!(exit_code, 0);
        assert_eq!(output.stdout, "planned");
    });
}

#[test]
fn runner_exec_explicit_run_id_overrides_conflicting_run_id_env() {
    crate::test_support::with_isolated_home(|_| {
        super::super::super::create(r#"{"id":"lab-local","kind":"local"}"#, false)
            .expect("create local runner");

        let (output, exit_code) = exec(
            "lab-local",
            RunnerExecOptions {
                cwd: None,
                project_id: None,
                allow_diagnostic_ssh: false,
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
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: Vec::new(),
                runner_workload: None,
                run_id: Some("explicit-run".to_string()),
                detach_after_handoff: false,
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
    crate::test_support::with_isolated_home(|_| {
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
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf nope".to_string(),
                ],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: vec![missing.display().to_string()],
                runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
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
    crate::test_support::with_isolated_home(|_| {
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
                command: vec!["sh".to_string(), "-c".to_string(), "printf ok".to_string()],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: vec![required_path.display().to_string()],
                runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
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
    crate::test_support::with_isolated_home(|_| {
        server::create(
            r#"{"id":"lab-server","host":"192.168.86.63","user":"user"}"#,
            false,
        )
        .expect("create server");

        super::super::super::create(
            r#"{"id":"lab-server","kind":"ssh","server_id":"lab-server","workspace_root":"/srv/homeboy"}"#,
            false,
        )
        .expect("create ssh runner");

        let err = exec(
            "lab-server",
            RunnerExecOptions {
                cwd: Some("/srv/homeboy/project".to_string()),
                project_id: None,
                allow_diagnostic_ssh: false,
                command: vec!["homeboy".to_string(), "test".to_string()],
                env: Default::default(),
                secret_env_names: Vec::new(),
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: Vec::new(),
                runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
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
        command: vec!["homeboy".to_string(), "--version".to_string()],
        env: Default::default(),
        secret_env_names: Vec::new(),
        capture_patch: false,
        raw_exec: true,
        source_snapshot: None,
        capability_preflight: None,
        required_extensions: Vec::new(),
        require_paths: Vec::new(),
        runner_workload: None,
        run_id: None,
        detach_after_handoff: false,
    };

    assert!(should_force_diagnostic_ssh(&ssh_runner(), &options));
    options.allow_diagnostic_ssh = false;
    assert!(!should_force_diagnostic_ssh(&ssh_runner(), &options));
}
