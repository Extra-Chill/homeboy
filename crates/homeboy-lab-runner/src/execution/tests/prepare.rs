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
fn prepare_runner_process_defers_runner_owned_secret_plan_entries() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut runner = ssh_runner();
        runner.id = "homeboy-lab".to_string();
        runner.secret_env.insert(
            "ACCESS_TOKEN".to_string(),
            RunnerSecretEnvRef {
                env: Some("ACCESS_TOKEN".to_string()),
                file: None,
                secret: None,
            },
        );
        let mut secret_env_plan =
            SecretEnvPlan::from_secret_env_names(["ACCESS_TOKEN".to_string()]);
        secret_env_plan.env_materialization = Some(
            homeboy_core::env_materialization_plan::EnvMaterializationPlan {
                secret_refs: vec![homeboy_core::env_materialization_plan::EnvSecretRef {
                    name: "ACCESS_TOKEN".to_string(),
                    owner: Some("runner".to_string()),
                }],
                ..Default::default()
            },
        );

        let prepared = prepare_runner_process(RunnerProcessRequest {
            runner_id: "homeboy-lab".to_string(),
            runner: Some(runner),
            cwd: Some("/srv/homeboy/project".to_string()),
            project_id: None,
            command: vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "providers".to_string(),
            ],
            env: Default::default(),
            secret_env_names: vec!["ACCESS_TOKEN".to_string()],
            secret_env_plan: Some(secret_env_plan),
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: false,
        })
        .expect("runner-owned secret must not be resolved by the controller");

        assert!(!prepared.env.contains_key("ACCESS_TOKEN"));
    });
}

#[test]
fn daemon_prepare_resolves_sealed_provider_source_without_runner_secret_map() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let auth_dir = home.path().join(".codex");
        std::fs::create_dir_all(&auth_dir).expect("auth directory");
        std::fs::write(
            auth_dir.join("auth.json"),
            serde_json::json!({
                "tokens": {
                    "access_token": "eyJhbGciOiJub25lIn0.eyJleHAiOjQxMDI0NDQ4MDB9.signature",
                    "refresh_token": "runner-refresh-secret",
                    "account_id": "runner-account"
                }
            })
            .to_string(),
        )
        .expect("auth file");
        let workspace = tempfile::tempdir().expect("workspace");
        let names = vec![
            "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_FEDRAMP".to_string(),
        ];
        let mut secret_env_plan = SecretEnvPlan::from_secret_env_names(names.clone());
        secret_env_plan.provider_credentials.insert(
            "opencode.agent-task-executor".to_string(),
            homeboy_core::secret_env_plan::SecretEnvProviderCredentialMapping {
                secret_env: names.clone(),
                sources: [
                    (
                        "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
                        homeboy_core::secret_env_plan::SecretEnvCredentialSource {
                            source: "json-file".to_string(),
                            env_var: None,
                            path: Some("~/.codex/auth.json".to_string()),
                            scope: None,
                            name: None,
                            field: Some("tokens.access_token".to_string()),
                            fallback_fields: Vec::new(),
                            fallback_value: None,
                        },
                    ),
                    (
                        "AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN".to_string(),
                        homeboy_core::secret_env_plan::SecretEnvCredentialSource {
                            source: "json-file".to_string(),
                            env_var: None,
                            path: Some("~/.codex/auth.json".to_string()),
                            scope: None,
                            name: None,
                            field: Some("tokens.refresh_token".to_string()),
                            fallback_fields: Vec::new(),
                            fallback_value: None,
                        },
                    ),
                    (
                        "AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT".to_string(),
                        homeboy_core::secret_env_plan::SecretEnvCredentialSource {
                            source: "json-file-jwt-expiration".to_string(),
                            env_var: None,
                            path: Some("~/.codex/auth.json".to_string()),
                            scope: None,
                            name: None,
                            field: Some("tokens.access_token".to_string()),
                            fallback_fields: vec![
                                "tokens.expires_at".to_string(),
                                "tokens.expiresAt".to_string(),
                            ],
                            fallback_value: None,
                        },
                    ),
                    (
                        "AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID".to_string(),
                        homeboy_core::secret_env_plan::SecretEnvCredentialSource {
                            source: "json-file".to_string(),
                            env_var: None,
                            path: Some("~/.codex/auth.json".to_string()),
                            scope: None,
                            name: None,
                            field: Some("tokens.account_id".to_string()),
                            fallback_fields: Vec::new(),
                            fallback_value: None,
                        },
                    ),
                    (
                        "AI_PROVIDER_OPENAI_CODEX_FEDRAMP".to_string(),
                        homeboy_core::secret_env_plan::SecretEnvCredentialSource {
                            source: "json-file".to_string(),
                            env_var: None,
                            path: Some("~/.codex/auth.json".to_string()),
                            scope: None,
                            name: None,
                            field: Some("tokens.fedramp".to_string()),
                            fallback_fields: Vec::new(),
                            fallback_value: Some(false),
                        },
                    ),
                ]
                .into_iter()
                .collect(),
            },
        );
        let serialized = serde_json::to_string(&secret_env_plan).expect("serialize sealed plan");
        let secret_env_plan: SecretEnvPlan =
            serde_json::from_str(&serialized).expect("deserialize sealed plan");
        assert!(!serialized.contains("runner-refresh-secret"));
        assert!(serialized.contains("tokens.expires_at"));
        let mapping = secret_env_plan
            .provider_credentials
            .get("opencode.agent-task-executor")
            .expect("selected provider credential mapping");
        assert_eq!(
            mapping.sources["AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN"]
                .path
                .as_deref(),
            Some("~/.codex/auth.json")
        );
        assert_eq!(
            mapping.sources["AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN"]
                .field
                .as_deref(),
            Some("tokens.access_token")
        );
        assert_eq!(
            mapping.sources["AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT"].fallback_fields,
            vec!["tokens.expires_at", "tokens.expiresAt"]
        );
        assert_eq!(
            mapping.sources["AI_PROVIDER_OPENAI_CODEX_FEDRAMP"].fallback_value,
            Some(false)
        );

        let prepared = prepare_daemon_local_process(RunnerProcessRequest {
            runner_id: "homeboy-lab".to_string(),
            runner: Some(local_runner(workspace.path().display().to_string())),
            cwd: Some(workspace.path().display().to_string()),
            project_id: None,
            command: vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "providers".to_string(),
            ],
            env: Default::default(),
            secret_env_names: names,
            secret_env_plan: Some(secret_env_plan),
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            require_paths: Vec::new(),
            validate_require_paths_on_host: true,
        })
        .expect("daemon preparation resolves the sealed runner source");

        assert_eq!(
            prepared.env.get("AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN"),
            Some(&"runner-refresh-secret".to_string())
        );
        assert_eq!(
            prepared.env.get("AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT"),
            Some(&"4102444800".to_string())
        );
        assert_eq!(
            prepared.env.get("AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID"),
            Some(&"runner-account".to_string())
        );
        assert_eq!(
            prepared.env.get("AI_PROVIDER_OPENAI_CODEX_FEDRAMP"),
            Some(&"false".to_string())
        );
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
        assert_eq!(
            plan.env
                .get(homeboy_core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV)
                .map(String::as_str),
            Some("homeboy-lab"),
            "daemon-local execution retains the runner provenance needed by nested run-plan"
        );
    });
}

#[test]
fn daemon_prep_keeps_resource_guard_request_separate_from_runner_env() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut runner = local_runner(workspace.display().to_string());
        runner.env.insert(
            "HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT".to_string(),
            "160".to_string(),
        );
        runner
            .env
            .insert("RUNNER_ONLY".to_string(), "1".to_string());

        let plan = prepare_daemon_local_process(RunnerProcessRequest {
            runner_id: "homeboy-lab".to_string(),
            runner: Some(runner),
            cwd: Some(workspace.display().to_string()),
            project_id: None,
            command: vec!["true".to_string()],
            env: HashMap::from([(
                "HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT".to_string(),
                "200".to_string(),
            )]),
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
            plan.env
                .get("HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT")
                .map(String::as_str),
            Some("200")
        );
        assert_eq!(
            plan.resource_guard_env
                .get("HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT")
                .map(String::as_str),
            Some("200")
        );
        assert_eq!(plan.resource_guard_env.len(), 1);
        assert!(!plan.resource_guard_env.contains_key("RUNNER_ONLY"));
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
