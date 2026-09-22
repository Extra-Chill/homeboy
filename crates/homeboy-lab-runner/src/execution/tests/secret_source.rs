use super::*;

#[test]
fn provider_file_secret_source_provisions_group_json_file_sources_without_values() {
    let sources = HashMap::from([
        (
            "PROVIDER_ACCESS_TOKEN".to_string(),
            json_file_source("~/.provider/auth.json", "tokens.access_token"),
        ),
        (
            "PROVIDER_REFRESH_TOKEN".to_string(),
            json_file_source("~/.provider/auth.json", "tokens.refresh_token"),
        ),
        (
            "UNRELATED_SECRET".to_string(),
            AgentTaskSecretSource {
                source: "env".to_string(),
                env_var: Some("UNRELATED_SECRET".to_string()),
                path: None,
                scope: None,
                name: None,
                field: None,
                value: None,
            },
        ),
    ]);

    let provisions = super::super::provider_file_secret_source_provisions(
        &[
            "PROVIDER_REFRESH_TOKEN".to_string(),
            "PROVIDER_ACCESS_TOKEN".to_string(),
            "UNRELATED_SECRET".to_string(),
        ],
        &sources,
        None,
    );

    // Production groups the two json-file sources that share one path into a
    // single provision and DROPS the `env`-source secret entirely. A single
    // provision proves both the grouping and the non-json-file filtering.
    assert_eq!(provisions.len(), 1);
    assert_eq!(provisions[0].path, "~/.provider/auth.json");
    // The grouped env names are sorted and deduped by production even though the
    // required-names input was deliberately supplied REFRESH-before-ACCESS and
    // included the unrelated env secret. The output order proves the sort, and
    // the absence of `UNRELATED_SECRET` proves the source-kind filter.
    assert_eq!(
        provisions[0].env_names,
        vec![
            "PROVIDER_ACCESS_TOKEN".to_string(),
            "PROVIDER_REFRESH_TOKEN".to_string(),
        ]
    );
    assert!(!provisions[0]
        .env_names
        .contains(&"UNRELATED_SECRET".to_string()));
    // Provisions carry only the path + env-name routing, never the resolved
    // secret values, so the credential bytes cannot leak through this grouping.
    let rendered = format!("{:?}", provisions);
    assert!(!rendered.contains("access-secret"));
    assert!(!rendered.contains("refresh-secret"));
}

#[test]
fn provider_file_secret_source_provisions_include_json_file_jwt_expiration_sources() {
    let mut expires_at = json_file_source("~/.codex/auth.json", "tokens.access_token");
    expires_at.source = "json-file-jwt-expiration".to_string();
    // Negative control: an unsupported source kind on the SAME path must be
    // filtered out by production so it never widens the provision's env list.
    let mut unsupported = json_file_source("~/.codex/auth.json", "tokens.access_token");
    unsupported.source = "vault".to_string();
    let sources = HashMap::from([
        (
            "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
            json_file_source("~/.codex/auth.json", "tokens.access_token"),
        ),
        (
            "AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT".to_string(),
            expires_at,
        ),
        (
            "AI_PROVIDER_OPENAI_CODEX_VAULT_TOKEN".to_string(),
            unsupported,
        ),
    ]);

    let provisions = super::super::provider_file_secret_source_provisions(
        &[
            "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_VAULT_TOKEN".to_string(),
        ],
        &sources,
        None,
    );

    // Production accepts BOTH `json-file` and `json-file-jwt-expiration` kinds,
    // grouping them under their shared path into one provision.
    assert_eq!(provisions.len(), 1);
    assert_eq!(provisions[0].path, "~/.codex/auth.json");
    // The access-token (json-file) and expires-at (json-file-jwt-expiration)
    // names are both retained and sorted; the unsupported `vault` kind is
    // dropped even though it pointed at the same path.
    assert_eq!(
        provisions[0].env_names,
        vec![
            "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT".to_string(),
        ]
    );
    assert!(!provisions[0]
        .env_names
        .contains(&"AI_PROVIDER_OPENAI_CODEX_VAULT_TOKEN".to_string()));
}

#[test]
fn runner_owned_provider_source_is_not_copied_from_controller() {
    let sources = HashMap::from([(
        "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
        json_file_source("~/.codex/auth.json", "tokens.access_token"),
    )]);
    let mut plan =
        SecretEnvPlan::from_secret_env_names(["AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string()]);
    plan.env_materialization = Some(
        homeboy_core::env_materialization_plan::EnvMaterializationPlan {
            secret_refs: vec![homeboy_core::env_materialization_plan::EnvSecretRef {
                owner: Some("runner".to_string()),
                name: "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
            }],
            ..Default::default()
        },
    );

    let provisions = super::super::provider_file_secret_source_provisions(
        &["AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string()],
        &sources,
        Some(&plan),
    );

    assert!(provisions.is_empty());
}

#[test]
fn runner_secret_env_resolution_uses_provider_json_file_source_values() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let provider_dir = home.path().join(".provider");
        std::fs::create_dir_all(&provider_dir).expect("provider dir");
        std::fs::write(
            provider_dir.join("auth.json"),
            serde_json::json!({
                "tokens": {
                    "access_token": "access-secret-value",
                    "refresh_token": "refresh-secret-value"
                }
            })
            .to_string(),
        )
        .expect("auth json");
        let sources = HashMap::from([
            (
                "PROVIDER_ACCESS_TOKEN".to_string(),
                json_file_source("~/.provider/auth.json", "tokens.access_token"),
            ),
            (
                "PROVIDER_REFRESH_TOKEN".to_string(),
                json_file_source("~/.provider/auth.json", "tokens.refresh_token"),
            ),
        ]);

        let resolved = resolve_runner_secret_env_for_command_with_fallbacks(
            &HashMap::new(),
            &[
                "PROVIDER_ACCESS_TOKEN".to_string(),
                "PROVIDER_REFRESH_TOKEN".to_string(),
            ],
            &HashMap::new(),
            &sources,
        )
        .expect("provider sources resolve on runner");

        assert_eq!(
            resolved.get("PROVIDER_ACCESS_TOKEN"),
            Some(&"access-secret-value".to_string())
        );
        assert_eq!(
            resolved.get("PROVIDER_REFRESH_TOKEN"),
            Some(&"refresh-secret-value".to_string())
        );
    });
}

#[test]
fn controller_secret_env_resolution_errors_when_required_name_has_no_ref() {
    let err = resolve_controller_secret_env_for_command_with_fallbacks(
        &HashMap::new(),
        &["MISSING_CONTROLLER_SECRET".to_string()],
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect_err("missing controller secret ref should fail closed");

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert_eq!(err.details["field"], "secret_env");
    assert!(err.message.contains("MISSING_CONTROLLER_SECRET"));
    assert!(err
        .message
        .contains("missing runner secret env ref for MISSING_CONTROLLER_SECRET"));
}

#[test]
fn runner_secret_env_resolution_preserves_fallback_source_errors() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let sources = HashMap::from([(
            "ACCESS_TOKEN".to_string(),
            json_file_source("~/.missing-provider/auth.json", "tokens.access_token"),
        )]);

        let err = resolve_runner_secret_env_for_command_with_fallbacks(
            &HashMap::new(),
            &["ACCESS_TOKEN".to_string()],
            &HashMap::new(),
            &sources,
        )
        .expect_err("an unreadable configured source must fail closed");

        assert!(err.message.contains("ACCESS_TOKEN"));
        assert!(err.message.contains("failed") || err.message.contains("missing"));
        assert!(!err.message.contains("missing runner secret env ref"));
    });
}

#[test]
fn runner_secret_env_plan_source_failure_stays_fail_closed_without_runner_ref() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let mut plan = SecretEnvPlan::from_secret_env_names(["ACCESS_TOKEN".to_string()]);
        plan.provider_credentials.insert(
            "opencode.agent-task-executor".to_string(),
            homeboy_core::secret_env_plan::SecretEnvProviderCredentialMapping {
                secret_env: vec!["ACCESS_TOKEN".to_string()],
                sources: [(
                    "ACCESS_TOKEN".to_string(),
                    homeboy_core::secret_env_plan::SecretEnvCredentialSource {
                        source: "json-file".to_string(),
                        env_var: None,
                        path: Some("~/.missing-provider/auth.json".to_string()),
                        scope: None,
                        name: None,
                        field: Some("tokens.access_token".to_string()),
                    },
                )]
                .into_iter()
                .collect(),
            },
        );

        let err = resolve_runner_secret_env_for_plan(&HashMap::new(), &plan, &HashMap::new())
            .expect_err("sealed source failure must stop before provider execution");

        assert!(err
            .message
            .contains("runner secret source for ACCESS_TOKEN failed"));
        assert!(!err.message.contains("missing runner secret env ref"));
    });
}

#[test]
fn runner_secret_env_resolution_stays_fail_closed_without_runner_ref_or_source() {
    let err = resolve_runner_secret_env_for_command_with_fallbacks(
        &HashMap::new(),
        &["ACCESS_TOKEN".to_string()],
        &HashMap::new(),
        &HashMap::new(),
    )
    .expect_err("missing runner secret ref must fail closed");

    assert!(err
        .message
        .contains("missing runner secret env ref for ACCESS_TOKEN"));
}

#[test]
fn controller_secret_env_resolution_uses_fallback_sources() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let provider_dir = home.path().join(".provider");
        std::fs::create_dir_all(&provider_dir).expect("provider dir");
        std::fs::write(
            provider_dir.join("auth.json"),
            serde_json::json!({
                "tokens": {
                    "access_token": "controller-access-secret"
                }
            })
            .to_string(),
        )
        .expect("auth json");
        let sources = HashMap::from([(
            "PROVIDER_ACCESS_TOKEN".to_string(),
            json_file_source("~/.provider/auth.json", "tokens.access_token"),
        )]);

        let resolved = resolve_controller_secret_env_for_command_with_fallbacks(
            &HashMap::new(),
            &["PROVIDER_ACCESS_TOKEN".to_string()],
            &HashMap::new(),
            &sources,
        )
        .expect("controller fallback source resolves");

        assert_eq!(
            resolved.get("PROVIDER_ACCESS_TOKEN"),
            Some(&"controller-access-secret".to_string())
        );
    });
}

#[test]
fn controller_secret_env_resolution_keeps_controller_owned_plan_entries_local() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let provider_dir = home.path().join(".provider");
        std::fs::create_dir_all(&provider_dir).expect("provider dir");
        std::fs::write(
            provider_dir.join("auth.json"),
            serde_json::json!({
                "tokens": { "access_token": "controller-access-secret" }
            })
            .to_string(),
        )
        .expect("auth json");
        let sources = HashMap::from([(
            "ACCESS_TOKEN".to_string(),
            json_file_source("~/.provider/auth.json", "tokens.access_token"),
        )]);
        let mut plan = SecretEnvPlan::from_secret_env_names(["ACCESS_TOKEN".to_string()]);
        plan.env_materialization = Some(
            homeboy_core::env_materialization_plan::EnvMaterializationPlan {
                secret_refs: vec![homeboy_core::env_materialization_plan::EnvSecretRef {
                    name: "ACCESS_TOKEN".to_string(),
                    owner: Some("controller".to_string()),
                }],
                ..Default::default()
            },
        );

        let resolved = super::super::resolve_controller_secret_env_for_plan_with_fallbacks(
            &HashMap::new(),
            &plan,
            &HashMap::new(),
            &sources,
        )
        .expect("controller-owned source resolves locally");

        assert_eq!(
            resolved.get("ACCESS_TOKEN"),
            Some(&"controller-access-secret".to_string())
        );
    });
}

#[test]
fn runner_secret_env_resolution_accepts_secret_env_plan_names() {
    std::env::set_var("HOMEBOY_PLAN_SECRET_ENV_TEST", "plan-secret-value");
    let plan = homeboy_core::secret_env_plan::SecretEnvPlan::from_secret_env_names([
        "HOMEBOY_PLAN_SECRET_ENV_TEST".to_string(),
    ]);
    let secret_env = HashMap::from([(
        "HOMEBOY_PLAN_SECRET_ENV_TEST".to_string(),
        RunnerSecretEnvRef {
            env: Some("HOMEBOY_PLAN_SECRET_ENV_TEST".to_string()),
            file: None,
            secret: None,
        },
    )]);

    let resolved = resolve_runner_secret_env_for_plan(&secret_env, &plan, &HashMap::new())
        .expect("plan-declared secret resolves through runner policy");

    assert_eq!(
        resolved.get("HOMEBOY_PLAN_SECRET_ENV_TEST"),
        Some(&"plan-secret-value".to_string())
    );
    assert!(!serde_json::to_string(&plan.redacted())
        .expect("redacted plan json")
        .contains("plan-secret-value"));
    std::env::remove_var("HOMEBOY_PLAN_SECRET_ENV_TEST");
}

#[test]
fn runner_secret_env_plan_resolution_fails_closed_before_provider_execution() {
    // Extra-Chill/homeboy#14382: a durable reverse-runner job carries secret
    // references only. When the runner cannot resolve a planned provider
    // credential from its own sources, the worker must fail with a clear
    // readiness error instead of crashing inside the provider.
    let plan = homeboy_core::secret_env_plan::SecretEnvPlan::from_secret_env_names([
        "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
    ]);

    let err = resolve_runner_secret_env_for_plan(&HashMap::new(), &plan, &HashMap::new())
        .expect_err("missing runner credential must fail closed pre-provider");

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert_eq!(err.details["field"], "secret_env");
    assert!(err
        .message
        .contains("missing runner secret env ref for AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN"));
    let rendered = format!("{} {:?} {:?}", err.message, err.details, err.hints);
    assert!(!rendered.contains("access-secret-value"));
}

#[test]
fn strip_durable_secret_env_values_removes_planned_names_and_keeps_public_env() {
    // Extra-Chill/homeboy#14382: the reverse-runner dispatch env must persist
    // no inline secret values. Planned names include provider credential
    // requirements contributed through `provider_credentials` mappings.
    let mut plan = homeboy_core::secret_env_plan::SecretEnvPlan::from_secret_env_names([
        "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
    ]);
    plan.provider_credentials.insert(
        "test.opencode-provider".to_string(),
        homeboy_core::secret_env_plan::SecretEnvProviderCredentialMapping {
            secret_env: vec!["AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN".to_string()],
            sources: Default::default(),
        },
    );

    let env = HashMap::from([
        (
            "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
            "access-secret-value".to_string(),
        ),
        (
            "AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN".to_string(),
            "refresh-secret-value".to_string(),
        ),
        ("PUBLIC_FLAG".to_string(), "1".to_string()),
    ]);

    let stripped = strip_durable_secret_env_values(env, &plan);

    assert_eq!(
        stripped,
        HashMap::from([("PUBLIC_FLAG".to_string(), "1".to_string())])
    );
}

#[test]
fn provider_file_secret_source_error_is_early_clear_and_redacted() {
    let provision = ProviderFileSecretSourceProvision {
        path: "~/.provider/auth.json".to_string(),
        env_names: vec![
            "PROVIDER_ACCESS_TOKEN".to_string(),
            "PROVIDER_REFRESH_TOKEN".to_string(),
        ],
    };

    let err = provider_file_secret_source_error(
        "homeboy-lab",
        &provision,
        "controller credential source is not readable".to_string(),
    );

    assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
    assert!(err.message.contains("homeboy-lab"));
    assert!(err.message.contains("PROVIDER_ACCESS_TOKEN"));
    assert!(err
        .message
        .contains("controller credential source is not readable"));
    assert!(err.details["tried"]
        .as_array()
        .is_some_and(|hints| hints.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Refresh the provider credentials")))));
    let rendered = format!("{} {:?} {:?}", err.message, err.details, err.hints);
    assert!(!rendered.contains("access-secret-value"));
    assert!(!rendered.contains("refresh-secret-value"));
}
