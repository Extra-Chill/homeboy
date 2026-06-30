use super::*;

#[test]
fn daemon_job_context_error_preserves_in_flight_job_details() {
    let source = Error::internal_unexpected(
        "query runner daemon: error sending request for url (http://127.0.0.1:63203/jobs/job-123)",
    )
    .with_hint("original hint");

    let err = daemon_job_context_error("homeboy-lab", "job-123", source);

    assert_eq!(err.code, ErrorCode::InternalUnexpected);
    assert_eq!(err.retryable, Some(true));
    assert_eq!(err.details["runner_id"], "homeboy-lab");
    assert_eq!(err.details["job_id"], "job-123");
    assert_eq!(err.hints[0].message, "original hint");
    assert!(err.message.contains("query runner daemon"));
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
    crate::test_support::with_isolated_home(|_| {
        let plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(ssh_runner()),
            cwd: Some("/srv/homeboy/project".to_string()),
            project_id: None,
            command: vec!["homeboy".to_string(), "--version".to_string()],
            env: Default::default(),
            secret_env_names: Vec::new(),
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
    crate::test_support::with_isolated_home(|_| {
        let plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "lab".to_string(),
            runner: Some(ssh_runner()),
            cwd: Some("/srv/homeboy/project".to_string()),
            project_id: None,
            command: vec!["node".to_string(), "--version".to_string()],
            env: Default::default(),
            secret_env_names: Vec::new(),
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
    crate::test_support::with_isolated_home(|_| {
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
fn ssh_runner_prep_marks_commands_as_runner_hosted() {
    crate::test_support::with_isolated_home(|_| {
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
            env: Default::default(),
            secret_env_names: Vec::new(),
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
        assert_eq!(plan.env.get(RUNNER_ID_ENV).map(String::as_str), Some("lab"));
    });
}

#[test]
fn local_runner_prep_does_not_mark_commands_as_runner_hosted() {
    crate::test_support::with_isolated_home(|_| {
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
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("prepare local runner process");

        assert!(!plan.env.contains_key(RUNNER_HOSTED_EXEC_ENV));
        assert!(!plan.env.contains_key(RUNNER_ID_ENV));
    });
}

#[test]
fn runner_prep_drops_undeclared_sensitive_env() {
    crate::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");

        let plan = prepare_runner_process(RunnerProcessRequest {
            runner_id: "local".to_string(),
            runner: Some(local_runner(workspace.display().to_string())),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec!["env".to_string()],
            env: HashMap::from([(
                "UNDECLARED_API_TOKEN".to_string(),
                "secret-value".to_string(),
            )]),
            secret_env_names: Vec::new(),
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
    crate::test_support::with_isolated_home(|_| {
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
fn daemon_local_prep_normalizes_default_path_on_runner_side() {
    crate::test_support::with_isolated_home(|_| {
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
    });
}
