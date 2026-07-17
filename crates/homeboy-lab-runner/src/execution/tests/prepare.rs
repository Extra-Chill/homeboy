use super::*;

#[test]
fn daemon_job_context_error_preserves_in_flight_job_details() {
    let source = Error::internal_unexpected(
        "query runner daemon: error sending request for url (http://127.0.0.1:63203/jobs/job-123)",
    )
    .with_hint("original hint");

    let err = daemon_job_context_error("homeboy-lab", "job-123", None, source);

    assert_eq!(err.code, ErrorCode::RunnerControllerDisconnected);
    assert_eq!(err.retryable, Some(true));
    assert_eq!(err.details["runner_id"], "homeboy-lab");
    assert_eq!(err.details["job_id"], "job-123");
    assert_eq!(err.hints[0].message, "original hint");
    assert!(err.message.contains("query runner daemon"));
}

#[test]
fn daemon_job_context_error_preserves_persisted_run_retrieval() {
    let source = Error::internal_json(
        "EOF while parsing a value",
        Some("parse daemon response".to_string()),
    );

    let err = daemon_job_context_error(
        "homeboy-lab",
        "job-123",
        Some("runner-exec-lab-job-123"),
        source,
    );

    assert_eq!(err.code, ErrorCode::RunnerControllerDisconnected);
    assert_eq!(err.details["runner_id"], "homeboy-lab");
    assert_eq!(err.details["job_id"], "job-123");
    assert_eq!(err.details["persisted_run_id"], "runner-exec-lab-job-123");
    assert_eq!(
        err.details["recovery"]["persisted_run_show"],
        "homeboy runs show runner-exec-lab-job-123"
    );
    assert_eq!(
        err.details["recovery"]["persisted_run_evidence"],
        "homeboy runs evidence runner-exec-lab-job-123"
    );
    assert!(err.hints.iter().any(|hint| hint
        .message
        .contains("Persisted run id: `runner-exec-lab-job-123`")));
}

#[test]
fn malformed_daemon_response_json_is_structured_and_retryable() {
    let err =
        super::super::super::daemon_http_get::parse_daemon_response_json::<serde_json::Value>(
            "{\"success\": true, \"data\":",
            200,
            "/jobs/job-123",
            "parse daemon response",
        )
        .expect_err("malformed daemon JSON");

    assert_eq!(err.code, ErrorCode::InternalJsonError);
    assert_eq!(err.message, "Malformed runner daemon JSON response");
    assert_eq!(err.retryable, Some(true));
    assert_eq!(err.details["context"], "parse daemon response");
    assert_eq!(err.details["http_status"], 200);
    assert_eq!(err.details["path"], "/jobs/job-123");
    assert_eq!(err.details["likely_truncated"], true);
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("known job/run")));
}

#[test]
fn test_resolve_cwd_defaults_ssh_runner_to_workspace_root() {
    let cwd = resolve_cwd(&ssh_runner(), None).expect("cwd");
    assert_eq!(cwd, "/srv/homeboy");
}

#[test]
fn test_resolve_cwd_rejects_ssh_cwd_outside_workspace_root() {
    let err = resolve_cwd(&ssh_runner(), Some("/tmp/project")).expect_err("reject cwd");
    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err.message.contains("workspace_root"));
}

#[test]
fn prepare_runner_process_uses_embedded_runner_snapshot() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(ssh_runner()),
            cwd: Some("/srv/homeboy/project".to_string()),
            project_id: None,
            command: vec!["homeboy".to_string(), "--version".to_string()],
            env: Default::default(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("prepare from runner snapshot");

        assert_eq!(plan.runner.id, "lab");
        assert_eq!(plan.cwd, "/srv/homeboy/project");
    });
}

#[test]
fn ssh_runner_prep_leaves_default_path_to_runner_side() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(ssh_runner()),
            cwd: Some("/srv/homeboy/project".to_string()),
            project_id: None,
            command: vec!["node".to_string(), "--version".to_string()],
            env: Default::default(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("prepare ssh runner process");

        assert!(
            !plan.env.contains_key("PATH"),
            "controller must not freeze PATH before daemon-side runner normalization"
        );
    });
}

#[test]
fn ssh_runner_prep_preserves_explicit_path() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut runner = ssh_runner();
        runner
            .env
            .insert("PATH".to_string(), "$HOME/custom/bin:$PATH".to_string());

        let plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(runner),
            cwd: Some("/srv/homeboy/project".to_string()),
            project_id: None,
            command: vec!["node".to_string(), "--version".to_string()],
            env: Default::default(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("prepare ssh runner process with explicit path");

        assert_eq!(
            plan.env.get("PATH").map(String::as_str),
            Some("$HOME/custom/bin:$PATH")
        );
    });
}

#[test]
fn ssh_runner_prep_marks_remote_placement_as_resolved() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(ssh_runner()),
            cwd: Some("/srv/homeboy/project".to_string()),
            project_id: None,
            command: vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
            ],
            env: std::collections::HashMap::from([(
                RUNNER_PLACEMENT_RESOLVED_ENV.to_string(),
                "1".to_string(),
            )]),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("prepare ssh runner process");

        assert_eq!(
            plan.env.get(RUNNER_HOSTED_EXEC_ENV).map(String::as_str),
            Some("1")
        );
        assert_eq!(
            plan.env
                .get(RUNNER_PLACEMENT_RESOLVED_ENV)
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(plan.env.get(RUNNER_ID_ENV).map(String::as_str), Some("lab"));
    });
}

#[test]
fn local_runner_prep_marks_placement_as_resolved() {
    // Regression for #8115: a local runner exec must stamp the dispatch-only
    // placement-resolved markers so nested Homeboy subprocesses (parity
    // preflight `extension show`, extension materialization, ready_check
    // chains) recognize that placement is already resolved and short-circuit
    // routing instead of re-dispatching. Without these markers a local exec
    // carrying an explicit `--placement` recursively spawns
    // `homeboy component show` / `extension show` plus the extension
    // ready_check and saturates the host.
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "local".to_string(),
            runner: Some(local_runner(workspace.display().to_string())),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
            ],
            env: Default::default(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("prepare local runner process");

        assert_eq!(
            plan.env.get(RUNNER_HOSTED_EXEC_ENV).map(String::as_str),
            Some("1")
        );
        assert_eq!(
            plan.env
                .get(RUNNER_PLACEMENT_RESOLVED_ENV)
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            plan.env.get(RUNNER_ID_ENV).map(String::as_str),
            Some("local")
        );
    });
}

#[test]
fn daemon_worker_marks_nested_cook_as_runner_hosted() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut runner = local_runner(workspace.display().to_string());
        runner.id = "homeboy-lab".to_string();

        let plan = prepare_daemon_local_process(RunnerProcessRequest {
            runner_id: "homeboy-lab".to_string(),
            runner: Some(runner),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
            ],
            env: Default::default(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: true,
        })
        .expect("prepare daemon worker process");

        assert_eq!(
            plan.env.get(RUNNER_HOSTED_EXEC_ENV).map(String::as_str),
            Some("1")
        );
        assert_eq!(
            plan.env.get(RUNNER_ID_ENV).map(String::as_str),
            Some("homeboy-lab")
        );
    });
}

#[test]
fn runner_prep_drops_undeclared_sensitive_env() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let mut runner = local_runner(workspace.display().to_string());
        runner.env.insert(
            "UNDECLARED_API_TOKEN".to_string(),
            "secret-value".to_string(),
        );

        let plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "local".to_string(),
            runner: Some(runner),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec!["env".to_string()],
            env: HashMap::new(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("undeclared sensitive env is dropped before execution");

        assert!(!plan.env.contains_key("UNDECLARED_API_TOKEN"));
        assert!(!format!("{plan:?}").contains("secret-value"));
    });
}

#[test]
fn runner_prep_allows_declared_sensitive_env() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "local".to_string(),
            runner: Some(local_runner(workspace.display().to_string())),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec!["env".to_string()],
            env: HashMap::from([("DECLARED_API_TOKEN".to_string(), "secret-value".to_string())]),
            secret_env_names: vec!["DECLARED_API_TOKEN".to_string()],
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("declared sensitive env is allowed");

        assert_eq!(
            plan.env.get("DECLARED_API_TOKEN").map(String::as_str),
            Some("secret-value")
        );
    });
}

#[test]
fn daemon_prep_preserves_placement_marker_and_unrelated_runner_side_secret() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let mut runner = ssh_runner();
        runner.workspace_root = Some(workspace.display().to_string());
        runner.env.insert(
            "OPENAI_API_KEY".to_string(),
            "runner-side-secret".to_string(),
        );

        let plan = prepare_daemon_local_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(runner),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec![
                "homeboy".to_string(),
                "refactor".to_string(),
                "--help".to_string(),
            ],
            env: std::collections::HashMap::from([(
                RUNNER_PLACEMENT_RESOLVED_ENV.to_string(),
                "1".to_string(),
            )]),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("unrelated runner-side secret should not block non-secret command");

        assert!(!plan.env.contains_key("OPENAI_API_KEY"));
        assert_eq!(
            plan.env
                .get(RUNNER_PLACEMENT_RESOLVED_ENV)
                .map(String::as_str),
            Some("1")
        );
    });
}

#[test]
fn runner_prep_runtime_secret_env_allowlist_declares_sensitive_env() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "local".to_string(),
            runner: Some(local_runner(workspace.display().to_string())),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec!["node".to_string(), "run-headless-loop.cjs".to_string()],
            env: HashMap::from([
                (
                    "HOMEBOY_AGENT_RUNTIME_SECRET_ENV".to_string(),
                    "OPENAI_API_KEY".to_string(),
                ),
                ("OPENAI_API_KEY".to_string(), "secret-value".to_string()),
            ]),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("runtime secret env allowlist should satisfy preflight declaration");

        assert_eq!(
            plan.env.get("OPENAI_API_KEY").map(String::as_str),
            Some("secret-value")
        );
    });
}

#[test]
fn runner_prep_diagnostic_identifies_local_controller_secret_source() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let err = prepare_runner_process(RunnerProcessRequest {
            runner_id: "local".to_string(),
            runner: Some(local_runner(workspace.display().to_string())),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec!["env".to_string()],
            env: HashMap::from([("OPENAI_API_KEY".to_string(), "secret-value".to_string())]),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect_err("undeclared local controller secret should fail closed");

        assert!(err.message.contains("OPENAI_API_KEY"));
        assert!(err.message.contains("local controller env"));
    });
}

#[test]
fn daemon_prep_diagnostic_identifies_remote_runner_daemon_secret_source() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let mut runner = ssh_runner();
        runner.workspace_root = Some(workspace.display().to_string());
        runner.env.insert(
            "OPENAI_API_KEY".to_string(),
            "runner-side-secret".to_string(),
        );

        let err = prepare_daemon_local_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(runner),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec!["node".to_string(), "run-headless-loop.cjs".to_string()],
            env: HashMap::from([(
                "HOMEBOY_AGENT_RUNTIME_SECRET_ENV".to_string(),
                "AI_PROVIDER_TOKEN".to_string(),
            )]),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect_err("undeclared runner daemon secret should identify source");

        assert!(err.message.contains("OPENAI_API_KEY"));
        assert!(err.message.contains("remote runner daemon env"));
    });
}

#[test]
fn daemon_local_prep_normalizes_default_path_on_runner_side() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let workspace = workspace.display().to_string();

        let plan = prepare_daemon_local_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(ssh_runner()),
            cwd: Some(workspace),
            project_id: None,
            command: vec!["node".to_string(), "--version".to_string()],
            env: Default::default(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("prepare daemon-local runner process");

        assert!(
            plan.env.contains_key("PATH"),
            "daemon-side runner prep should build the default job PATH from the runner host"
        );
        assert_eq!(
            plan.env
                .get(RUNNER_PLACEMENT_RESOLVED_ENV)
                .map(String::as_str),
            Some("1"),
            "daemon jobs must identify their controller-resolved placement"
        );
        assert_eq!(
            plan.env.get(RUNNER_HOSTED_EXEC_ENV).map(String::as_str),
            Some("1")
        );
        assert_eq!(plan.env.get(RUNNER_ID_ENV).map(String::as_str), Some("lab"));
        assert!(!plan.command.iter().any(|arg| arg == "--placement"));
    });
}

#[test]
fn daemon_local_prep_prefers_configured_homeboy_path_for_nested_homeboy() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let workspace = workspace.display().to_string();
        let mut runner = ssh_runner();
        runner.settings.homeboy_path = Some("/opt/homeboy/current/homeboy".to_string());
        runner.env.insert(
            "PATH".to_string(),
            "/usr/local/bin:/usr/bin:/bin".to_string(),
        );

        let plan = prepare_daemon_local_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(runner),
            cwd: Some(workspace),
            project_id: None,
            command: vec!["homeboy".to_string(), "--version".to_string()],
            env: Default::default(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("prepare daemon-local runner process");

        assert_eq!(
            plan.env.get("PATH").map(String::as_str),
            Some("/opt/homeboy/current:/usr/local/bin:/usr/bin:/bin"),
            "daemon-side nested `homeboy` commands should resolve through configured homeboy_path first"
        );
    });
}
