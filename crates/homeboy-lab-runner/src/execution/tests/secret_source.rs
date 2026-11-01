use super::*;

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
}
