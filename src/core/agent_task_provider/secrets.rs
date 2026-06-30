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

pub(crate) fn provider_runner_secret_env_for_plan_with_providers(
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

pub(crate) fn provider_secret_sources_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = HashMap::new();
    for request in &plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        sources.extend(provider_secret_sources(provider, Some(request)));
    }
    sources
}

fn provider_secret_env(
    provider: &AgentTaskExecutorProvider,
    request: Option<&AgentTaskRequest>,
) -> Vec<String> {
    let mut names = Vec::new();
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
    if let Some(request) = request {
        if let Some(provider_name) = request
            .executor
            .config
            .get("provider")
            .and_then(Value::as_str)
        {
            if let Some(defaults) = provider.provider_defaults.get(provider_name) {
                names.extend(provider_config_secret_env(defaults));
            }
        }
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
    if let Some(request) = request {
        if let Some(provider_name) = request
            .executor
            .config
            .get("provider")
            .and_then(Value::as_str)
        {
            if let Some(defaults) = provider.provider_defaults.get(provider_name) {
                sources.extend(provider_config_secret_sources(defaults));
            }
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
    for key in ["secret_env", "secretEnv"] {
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
