use super::resolution::select_provider;
use super::*;

pub(super) fn apply_provider_runner_secret_env_contracts_with_providers(
    plan: &mut AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) {
    for request in &mut plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        request.executor.secret_env =
            provider_secret_env_plan(provider, request).secret_env_names();
    }
}

pub(super) fn apply_provider_runner_secret_env_contract_for_request(
    request: &mut AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    base_secret_env: &[String],
) {
    request.executor.secret_env = base_secret_env.to_vec();
    request.executor.secret_env = provider_secret_env_plan(provider, request).secret_env_names();
}

pub(super) fn provider_request_secret_env_missing(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> Vec<String> {
    let names = provider_secret_env(provider, Some(request));
    secret_env_status_with_fallbacks(&names, &provider_secret_sources(provider, Some(request)))
        .into_iter()
        .filter(|status| !status.configured)
        .map(|status| status.name)
        .collect()
}

pub(super) fn provider_request_credential_env(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> std::result::Result<Vec<(String, String)>, AgentTaskSecretResolutionError> {
    // A readiness probe verifies provider authentication, not arbitrary task or
    // runtime-tool secrets that execution may need later.
    let names = provider_secret_env(provider, Some(request));
    let sources = provider_secret_sources(provider, Some(request));
    resolve_secret_env_with_fallbacks(&names, &sources)
}

pub(super) fn provider_declared_credential_env(
    provider: &AgentTaskExecutorProvider,
) -> std::result::Result<Vec<(String, String)>, AgentTaskSecretResolutionError> {
    let names = super::credential_readiness::provider_required_secret_env_names(provider);
    resolve_secret_env_with_fallbacks(&names, &provider_declared_secret_sources(provider))
}

pub fn provider_runner_secret_env_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> Vec<String> {
    let mut names = Vec::new();
    for request in &plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        names.extend(provider_secret_env_plan(provider, request).secret_env_names());
    }
    names.sort();
    names.dedup();
    names
}

pub fn provider_secret_sources_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = HashMap::new();
    let mut conflicted = BTreeSet::new();
    for request in &plan.tasks {
        for (candidate, _) in
            crate::agent_task_scheduler::AgentTaskScheduleSupport::provider_route_candidates(
                request,
                plan.options.rotation.as_ref(),
            )
        {
            let Some(provider) = select_provider(providers, &candidate) else {
                continue;
            };
            for (name, source) in provider_secret_sources(provider, Some(&candidate)) {
                if conflicted.contains(&name) {
                    continue;
                }
                match sources.get(&name) {
                    None => {
                        sources.insert(name, source);
                    }
                    Some(existing) if existing == &source => {}
                    Some(_) => {
                        sources.remove(&name);
                        conflicted.insert(name);
                    }
                }
            }
        }
    }
    sources
}

fn provider_secret_env(
    provider: &AgentTaskExecutorProvider,
    request: Option<&AgentTaskRequest>,
) -> Vec<String> {
    let mut names = Vec::new();
    names.extend(
        provider
            .invocation
            .env
            .iter()
            .filter(|env| {
                env.source.as_deref() == Some("secret_env") || env.redacted.unwrap_or(false)
            })
            .map(|env| env.name.clone()),
    );
    for readiness in &provider.runner_readiness {
        names.extend(readiness.secret_env.iter().cloned());
    }
    for requirement in &provider.secret_requirements {
        if requirement.required == Some(false) {
            continue;
        }
        if let Some(name) = &requirement.name {
            names.push(name.clone());
        }
        names.extend(requirement.env.iter().cloned());
    }
    for requirement in &provider.secret_env_requirements {
        if requirement_matches_request(requirement.when.as_ref(), request) {
            names.extend(requirement.env.iter().cloned());
        }
    }
    if let Some(defaults) =
        request.and_then(|request| effective_provider_default(provider, request))
    {
        names.extend(provider_config_secret_env(defaults));
    }
    names.sort();
    names.dedup();
    names
}

pub(super) fn provider_secret_env_plan(
    provider: &AgentTaskExecutorProvider,
    request: &AgentTaskRequest,
) -> SecretEnvPlan {
    let provider_names = provider_secret_env(provider, Some(request));
    let mut plan = SecretEnvPlan::from_secret_env_names(request.executor.secret_env.clone());
    plan.extend_secret_env_names(
        request
            .runtime_tools
            .iter()
            .flat_map(|tool| tool.secret_env.iter().cloned()),
    );
    plan.extend_secret_env_names(provider_names.clone());
    plan.map_env_names(provider.id.clone(), provider_names);
    plan
}

pub(super) fn provider_secret_env_plan_with_status(
    provider: &AgentTaskExecutorProvider,
    request: &AgentTaskRequest,
) -> SecretEnvPlan {
    let plan = provider_secret_env_plan(provider, request);
    let status = secret_env_status_with_fallbacks(
        &plan.secret_env_names(),
        &provider_secret_sources(provider, Some(request)),
    )
    .into_iter()
    .map(|status| SecretEnvStatus {
        name: status.name,
        configured: status.configured,
        source: status.source,
        source_env_name: None,
        missing_source_env_names: Vec::new(),
    });
    plan.with_status(status).redacted()
}

pub(super) fn provider_secret_sources(
    provider: &AgentTaskExecutorProvider,
    request: Option<&AgentTaskRequest>,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = HashMap::new();
    for requirement in &provider.secret_env_requirements {
        if requirement_matches_request(requirement.when.as_ref(), request) {
            sources.extend(secret_source_map_from_extra(&requirement.extra));
        }
    }
    if let Some(defaults) =
        request.and_then(|request| effective_provider_default(provider, request))
    {
        sources.extend(provider_config_secret_sources(defaults));
    }
    sources
}

/// Every secret source a provider declares for itself, independent of any
/// dispatch request: its unconditional `secret_env_requirements` sources plus
/// a sole provider default, which is the same implicit-default rule scheduling
/// uses. Multiple account defaults remain request-scoped; merging them here
/// would let map iteration choose an arbitrary source for a shared env name.
///
/// This is the single source-resolution path behind `agent-task auth status`,
/// `agent-task providers --secret-env`, and provider credential readiness
/// (`--validate-readiness`). Those three surfaces used to assemble this same
/// merge independently; a provider whose declared source resolved differently
/// under one assembly than another could report a secret as configured in one
/// place and missing in another for no reason a caller could see (#13629).
/// Routing all three through this one function makes that divergence
/// structurally impossible rather than something each call site has to keep
/// in sync by hand.
pub(super) fn provider_declared_secret_sources(
    provider: &AgentTaskExecutorProvider,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = provider_secret_sources(provider, None);
    if provider.provider_defaults.len() == 1 {
        if let Some(provider_default) = provider.provider_defaults.values().next() {
            sources.extend(provider_config_secret_sources(provider_default));
        }
    }
    sources
}

fn secret_source_map_from_extra(
    extra: &BTreeMap<String, Value>,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    for key in [
        "secret_env_sources",
        "secretEnvSources",
        "credential_sources",
        "credentialSources",
    ] {
        if let Some(value) = extra.get(key) {
            return secret_source_map(value);
        }
    }
    HashMap::new()
}

pub(super) fn provider_config_secret_sources(
    config: &Value,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let Some(config) = config.as_object() else {
        return HashMap::new();
    };
    for key in [
        "secret_env_sources",
        "secretEnvSources",
        "credential_sources",
        "credentialSources",
    ] {
        if let Some(value) = config.get(key) {
            return secret_source_map(value);
        }
    }
    HashMap::new()
}

fn secret_source_map(value: &Value) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let Some(entries) = value.as_object() else {
        return HashMap::new();
    };
    entries
        .iter()
        .filter_map(|(name, source)| {
            serde_json::from_value::<defaults::AgentTaskSecretSource>(source.clone())
                .ok()
                .map(|source| (name.clone(), source))
        })
        .collect()
}

fn provider_config_secret_env(config: &Value) -> Vec<String> {
    let Some(config) = config.as_object() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for key in [
        "secret_env",
        "secretEnv",
        "required_secret_env",
        "requiredSecretEnv",
    ] {
        match config.get(key) {
            Some(Value::String(name)) => names.push(name.clone()),
            Some(Value::Array(items)) => names.extend(
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string)),
            ),
            _ => {}
        }
    }
    names
}

fn effective_provider_default<'a>(
    provider: &'a AgentTaskExecutorProvider,
    request: &AgentTaskRequest,
) -> Option<&'a Value> {
    if let Some(name) = request
        .executor
        .config
        .get("provider")
        .and_then(Value::as_str)
    {
        return provider.provider_defaults.get(name);
    }
    (provider.provider_defaults.len() == 1)
        .then(|| provider.provider_defaults.values().next())
        .flatten()
}

fn requirement_matches_request(when: Option<&Value>, request: Option<&AgentTaskRequest>) -> bool {
    let Some(when) = when else {
        return true;
    };
    let Some(request) = request else {
        return false;
    };
    let Ok(request_value) = serde_json::to_value(request) else {
        return false;
    };
    condition_matches(when, &request_value)
}

fn condition_matches(condition: &Value, request: &Value) -> bool {
    if let Some(any) = condition.get("any").and_then(Value::as_array) {
        return any.iter().any(|item| condition_matches(item, request));
    }
    if let Some(all) = condition.get("all").and_then(Value::as_array) {
        return all.iter().all(|item| condition_matches(item, request));
    }
    let Some(path) = condition.get("path").and_then(Value::as_str) else {
        return false;
    };
    let actual = value_at_contract_path(request, path);
    match condition.get("equals") {
        Some(expected) => actual == Some(expected),
        None => actual.is_some(),
    }
}

fn value_at_contract_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path == "provider" {
        return value_at_contract_path(value, "executor.config.provider");
    }
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::agent_task_scheduler::{
        AgentTaskProviderRotationEntry, AgentTaskProviderRotationPolicy,
    };

    fn request(config: Value) -> AgentTaskRequest {
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "credential-default".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config,
            },
            instructions: "test".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
            metadata: Value::Null,
        }
    }

    #[test]
    fn sole_default_required_secret_is_effective_without_an_explicit_provider() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "only-account": { "required_secret_env": ["ONLY_ACCOUNT_TOKEN"] }
            }
        }))
        .expect("provider");

        assert_eq!(
            provider_secret_env(&provider, Some(&request(Value::Null))),
            vec!["ONLY_ACCOUNT_TOKEN"]
        );
    }

    #[test]
    fn explicit_default_includes_required_secret_env_with_multiple_accounts() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "first": { "required_secret_env": "FIRST_TOKEN" },
                "second": { "requiredSecretEnv": ["SECOND_TOKEN"] }
            }
        }))
        .expect("provider");

        assert_eq!(
            provider_secret_env(
                &provider,
                Some(&request(serde_json::json!({"provider":"second"})))
            ),
            vec!["SECOND_TOKEN"]
        );
    }

    #[test]
    fn multiple_defaults_with_the_same_env_keep_sources_account_scoped() {
        let root = tempfile::tempdir().expect("tempdir");
        let first = root.path().join("first.json");
        let second = root.path().join("second.json");
        std::fs::write(&first, r#"{"token":"first-value"}"#).expect("first credential");
        std::fs::write(&second, r#"{"token":"second-value"}"#).expect("second credential");
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "first": {
                    "required_secret_env": "ACCOUNT_TOKEN",
                    "secret_env_sources": {"ACCOUNT_TOKEN": {"source":"json-file", "path":first, "field":"token"}}
                },
                "second": {
                    "required_secret_env": "ACCOUNT_TOKEN",
                    "secret_env_sources": {"ACCOUNT_TOKEN": {"source":"json-file", "path":second, "field":"token"}}
                }
            }
        }))
        .expect("provider");

        assert!(provider_declared_credential_env(&provider)
            .expect("unscoped credentials")
            .is_empty());
        assert_eq!(
            provider_request_credential_env(
                &request(serde_json::json!({"provider":"first"})),
                &provider,
            )
            .expect("first account"),
            vec![("ACCOUNT_TOKEN".to_string(), "first-value".to_string())]
        );
        assert_eq!(
            provider_request_credential_env(
                &request(serde_json::json!({"provider":"second"})),
                &provider,
            )
            .expect("second account"),
            vec![("ACCOUNT_TOKEN".to_string(), "second-value".to_string())]
        );
    }

    #[test]
    fn plan_secret_sources_include_plan_and_task_local_rotation_fallbacks() {
        let providers: Vec<AgentTaskExecutorProvider> = [
            serde_json::json!({
                "id": "test.primary",
                "backend": "test",
                "secret_env_requirements": [{
                    "env": ["PRIMARY_TOKEN"],
                    "secret_env_sources": {
                        "PRIMARY_TOKEN": {"source": "env", "env_var": "PRIMARY_SOURCE"}
                    }
                }]
            }),
            serde_json::json!({
                "id": "test.plan-fallback",
                "backend": "test",
                "provider_defaults": {
                    "plan-account": {
                        "required_secret_env": ["PLAN_FALLBACK_TOKEN"],
                        "secret_env_sources": {
                            "PLAN_FALLBACK_TOKEN": {"source": "env", "env_var": "PLAN_FALLBACK_SOURCE"}
                        }
                    }
                }
            }),
            serde_json::json!({
                "id": "test.task-fallback",
                "backend": "test",
                "secret_env_requirements": [{
                    "env": ["TASK_FALLBACK_TOKEN"],
                    "when": {"path": "executor.config.provider", "equals": "task-account"},
                    "secret_env_sources": {
                        "TASK_FALLBACK_TOKEN": {"source": "env", "env_var": "TASK_FALLBACK_SOURCE"}
                    }
                }]
            }),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("provider"))
        .collect();

        let mut plan_task = request(Value::Null);
        plan_task.executor.selector = Some("test.primary".to_string());
        let mut task_local = request(Value::Null);
        task_local.task_id = "task-local".to_string();
        task_local.executor.selector = Some("test.primary".to_string());
        task_local.metadata = serde_json::json!({
            "provider_rotation": {
                "entries": [
                    {"selector": "test.primary"},
                    {
                        "selector": "test.task-fallback",
                        "provider_config": {"provider": "task-account"}
                    }
                ]
            }
        });
        let mut plan = AgentTaskPlan::new("secret-source-rotation", vec![plan_task, task_local]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    selector: Some("test.primary".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    selector: Some("test.plan-fallback".to_string()),
                    provider_config: serde_json::json!({"provider": "plan-account"}),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        let sources = provider_secret_sources_for_plan_with_providers(&plan, &providers);

        assert_eq!(sources.len(), 3);
        assert_eq!(
            sources["PRIMARY_TOKEN"].env_var.as_deref(),
            Some("PRIMARY_SOURCE")
        );
        assert_eq!(
            sources["PLAN_FALLBACK_TOKEN"].env_var.as_deref(),
            Some("PLAN_FALLBACK_SOURCE")
        );
        assert_eq!(
            sources["TASK_FALLBACK_TOKEN"].env_var.as_deref(),
            Some("TASK_FALLBACK_SOURCE")
        );
    }

    #[test]
    fn plan_secret_sources_dedupe_identical_mappings_and_omit_conflicts() {
        let providers: Vec<AgentTaskExecutorProvider> = [
            ("test.primary", "SHARED_SOURCE"),
            ("test.identical", "SHARED_SOURCE"),
            ("test.conflicting", "OTHER_SOURCE"),
        ]
        .into_iter()
        .map(|(id, env_var)| {
            serde_json::from_value(serde_json::json!({
                "id": id,
                "backend": "test",
                "secret_env_requirements": [{
                    "env": ["SHARED_TOKEN"],
                    "secret_env_sources": {
                        "SHARED_TOKEN": {"source": "env", "env_var": env_var}
                    }
                }]
            }))
            .expect("provider")
        })
        .collect();
        let mut task = request(Value::Null);
        task.executor.selector = Some("test.primary".to_string());
        let mut plan = AgentTaskPlan::new("secret-source-conflict", vec![task]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    selector: Some("test.primary".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    selector: Some("test.identical".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    selector: Some("test.conflicting".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        assert!(
            !provider_secret_sources_for_plan_with_providers(&plan, &providers)
                .contains_key("SHARED_TOKEN")
        );
    }
}
