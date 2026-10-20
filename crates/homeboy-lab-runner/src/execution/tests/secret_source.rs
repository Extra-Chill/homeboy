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
                fallback_fields: Vec::new(),
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
    assert_eq!(
        provisions[0].fields,
        vec![
            "tokens.access_token".to_string(),
            "tokens.refresh_token".to_string(),
        ]
    );
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
fn runner_resolved_provider_source_is_refreshed_from_controller() {
    // Runner-side resolution still reads the declared file, so the controller
    // must keep that file current before every run instead of leaving a stale copy.
    let sources = HashMap::from([(
        "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
        json_file_source("~/.codex/auth.json", "tokens.access_token"),
    )]);

    let provisions = super::super::provider_file_secret_source_provisions(
        &["AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string()],
        &sources,
    );

    assert_eq!(provisions.len(), 1);
    assert_eq!(provisions[0].path, "~/.codex/auth.json");
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
            "pi.agent-task-executor".to_string(),
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
                        fallback_fields: Vec::new(),
                        fallback_value: None,
                    },
                )]
                .into_iter()
                .collect(),
            },
        );

        let err = resolve_runner_secret_env_for_plan_with_sources(
            &HashMap::new(),
            &plan,
            &HashMap::new(),
            &HashMap::new(),
        )
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
    // Extra-Chill/homeboy#14382: a durable runner job carries secret
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
fn provider_file_secret_source_error_is_early_clear_and_redacted() {
    let provision = ProviderFileSecretSourceProvision {
        path: "~/.provider/auth.json".to_string(),
        env_names: vec![
            "PROVIDER_ACCESS_TOKEN".to_string(),
            "PROVIDER_REFRESH_TOKEN".to_string(),
        ],
        fields: vec!["tokens.access_token".to_string()],
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

fn projection(controller: serde_json::Value, runner: &str, fields: &[&str]) -> serde_json::Value {
    let fields = fields
        .iter()
        .map(|field| field.to_string())
        .collect::<Vec<_>>();
    let projected =
        super::super::project_provider_file_secret_source(&controller.to_string(), runner, &fields)
            .expect("projection");
    serde_json::from_str(&projected).expect("projected json")
}

#[test]
fn projection_withholds_an_undeclared_refresh_token() {
    // #15783: the runner gets a usable access token but never the single-use
    // refresh token, so it cannot rotate the controller's chain.
    let projected = projection(
        serde_json::json!({
            "anthropic": {"type": "oauth", "access": "acc", "refresh": "controller-refresh", "expires": 42}
        }),
        "",
        &["anthropic.access", "anthropic.expires"],
    );

    assert_eq!(
        projected["anthropic"],
        serde_json::json!({"type": "oauth", "access": "acc", "refresh": "", "expires": 42})
    );
    assert!(!projected.to_string().contains("controller-refresh"));
}

#[test]
fn projection_delivers_a_declared_refresh_token() {
    let projected = projection(
        serde_json::json!({"tokens": {"access_token": "acc", "refresh_token": "rt"}}),
        "",
        &["tokens.access_token", "tokens.refresh_token"],
    );

    assert_eq!(projected["tokens"]["refresh_token"], "rt");
}

#[test]
fn projection_keeps_runner_owned_entries_and_drops_undeclared_controller_entries() {
    // The old whole-file upload overwrote every runner login. Only the
    // declared account crosses now; the runner's own accounts survive.
    let projected = projection(
        serde_json::json!({
            "anthropic": {"type": "oauth", "access": "new", "refresh": "r", "expires": 2},
            "openai": {"type": "oauth", "access": "controller-openai", "refresh": "co", "expires": 3}
        }),
        r#"{"anthropic": {"type": "oauth", "access": "dead", "refresh": "dead", "expires": 1},
            "openai": {"type": "oauth", "access": "runner-openai", "refresh": "ro", "expires": 9}}"#,
        &["anthropic.access", "anthropic.expires"],
    );

    assert_eq!(projected["anthropic"]["access"], "new");
    assert_eq!(projected["anthropic"]["refresh"], "");
    assert_eq!(projected["openai"]["access"], "runner-openai");
    assert_eq!(projected["openai"]["refresh"], "ro");
    assert!(!projected.to_string().contains("controller-openai"));
}

#[test]
fn projection_refuses_to_clobber_an_unparseable_runner_file() {
    let err = super::super::project_provider_file_secret_source(
        r#"{"anthropic": {"access": "a"}}"#,
        "not json",
        &["anthropic.access".to_string()],
    )
    .expect_err("unparseable runner file must not be overwritten");

    assert!(err.contains("not a JSON object"), "{err}");
}
