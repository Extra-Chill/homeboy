use super::resolution::select_provider;
use super::*;
use homeboy_core::secret_env_plan::{
    SecretEnvCredentialSource, SecretEnvProviderCredentialMapping,
};

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
    for request in &plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        sources.extend(provider_secret_sources(provider, Some(request)));
    }
    sources
}

/// Declared credential sources across every route the plan may dispatch,
/// rotation fallbacks included, for best-effort Lab delivery only (#15783).
///
/// Admission and readiness keep [`provider_secret_sources_for_plan_with_providers`],
/// which describes exactly the admitted route (#14194). This wider map only
/// lets a runner that evaluates rotation itself find a fallback's credentials
/// already delivered. The selected route comes first and wins a shared name.
pub fn provider_route_secret_sources_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = HashMap::new();
    for request in plan_provider_route_requests(plan) {
        let Some(provider) = select_provider(providers, &request) else {
            continue;
        };
        for (name, source) in provider_secret_sources(provider, Some(&request)) {
            sources.entry(name).or_insert(source);
        }
    }
    sources
}

/// Every route the scheduler may dispatch for the plan: each task's own route
/// followed by its rotation fallbacks.
fn plan_provider_route_requests(plan: &AgentTaskPlan) -> Vec<AgentTaskRequest> {
    plan.tasks
        .iter()
        .flat_map(|task| {
            crate::agent_task_scheduler::AgentTaskScheduleSupport::provider_route_candidates(
                task,
                plan.options.rotation.as_ref(),
            )
        })
        .map(|(request, _)| request)
        .collect()
}

/// Provider secret env names across every route the plan may dispatch,
/// rotation fallbacks included. Used for best-effort Lab delivery of fallback
/// credentials; admission keeps using the selected routes' names only.
pub fn provider_route_secret_env_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> Vec<String> {
    let mut names = Vec::new();
    for request in plan_provider_route_requests(plan) {
        let Some(provider) = select_provider(providers, &request) else {
            continue;
        };
        names.extend(provider_secret_env_plan(provider, &request).secret_env_names());
    }
    names.sort();
    names.dedup();
    names
}

/// A controller-side command that makes a route's declared credentials current
/// before they are delivered to a runner (#15783).
///
/// A `provider_defaults` account may declare `controller_refresh` as a command
/// invocation. The controller is then the sole owner of that account's OAuth
/// refresh chain: it refreshes before every delivery, and the runner receives
/// only what the account declares, never an undeclared refresh token.
#[derive(Debug, Clone)]
pub struct ProviderControllerRefresh {
    pub provider: AgentTaskExecutorProvider,
    pub invocation: CommandInvocation,
    /// Secret env names of the account this refresh keeps current.
    pub secret_env: Vec<String>,
}

pub fn provider_controller_refreshes_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> Vec<ProviderControllerRefresh> {
    let mut refreshes: Vec<ProviderControllerRefresh> = Vec::new();
    for request in plan_provider_route_requests(plan) {
        let Some(provider) = select_provider(providers, &request) else {
            continue;
        };
        let Some(config) = effective_provider_default(provider, &request) else {
            continue;
        };
        let Some(invocation) = config
            .get("controller_refresh")
            .or_else(|| config.get("controllerRefresh"))
            .and_then(|value| serde_json::from_value::<CommandInvocation>(value.clone()).ok())
            .filter(|invocation| !invocation.argv.is_empty())
        else {
            continue;
        };
        let secret_env = provider_config_secret_env(config);
        if refreshes.iter().any(|existing| {
            existing.provider.id == provider.id && existing.invocation.argv == invocation.argv
        }) {
            continue;
        }
        refreshes.push(ProviderControllerRefresh {
            provider: provider.clone(),
            invocation,
            secret_env,
        });
    }
    refreshes
}

pub fn provider_secret_credential_mappings_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> homeboy_core::Result<BTreeMap<String, SecretEnvProviderCredentialMapping>> {
    let mut mappings: BTreeMap<String, SecretEnvProviderCredentialMapping> = BTreeMap::new();
    for request in &plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        let secret_env = provider_secret_env_plan(provider, request).secret_env_names();
        let sources = provider_secret_sources(provider, Some(request))
            .into_iter()
            .map(|(name, source)| {
                (
                    name,
                    SecretEnvCredentialSource {
                        source: source.source,
                        env_var: source.env_var,
                        path: source.path,
                        scope: source.scope,
                        name: source.name,
                        field: source.field,
                        fallback_fields: source.fallback_fields,
                        fallback_value: source.value.and_then(|value| value.parse::<bool>().ok()),
                    },
                )
            })
            .collect();
        let mapping = SecretEnvProviderCredentialMapping {
            secret_env,
            sources,
        };
        if let Some(existing) = mappings.get_mut(&provider.id) {
            for (name, source) in &mapping.sources {
                if existing
                    .sources
                    .get(name)
                    .is_some_and(|current| current != source)
                {
                    return Err(homeboy_core::Error::validation_invalid_argument(
                        "provider_credentials",
                        format!(
                            "provider {} has conflicting credential source mappings for {}",
                            provider.id, name
                        ),
                        None,
                        None,
                    ));
                }
            }
            existing.secret_env.extend(mapping.secret_env);
            existing.secret_env.sort();
            existing.secret_env.dedup();
            existing.sources.extend(mapping.sources);
        } else {
            mappings.insert(provider.id.clone(), mapping);
        }
    }
    Ok(mappings)
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
    if let Some(defaults) =
        request.and_then(|request| effective_provider_default(provider, request))
    {
        names.extend(provider_config_secret_env(defaults));
    }
    names.sort();
    names.dedup();
    names
}

pub(crate) fn provider_secret_env_plan(
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

/// Secret env names this provider declares whose value is present only in the
/// current process environment.
///
/// A process that does not inherit this environment (a daemon `step`) still
/// resolves configured and provider-default sources itself, so only these
/// ambient-only names would silently disappear there.
pub(crate) fn ambient_only_provider_secret_env(
    provider: &AgentTaskExecutorProvider,
) -> Vec<String> {
    let Ok(request) = serde_json::from_value::<AgentTaskRequest>(serde_json::json!({
        "task_id": "step-environment-admission",
        "executor": { "backend": provider.backend },
        "instructions": "",
    })) else {
        return Vec::new();
    };
    let names = provider_secret_env_plan(provider, &request).secret_env_names();
    secret_env_status_with_fallbacks(&names, &provider_secret_sources(provider, Some(&request)))
        .into_iter()
        .filter(|status| {
            status.source == "env"
                && !status
                    .alternatives
                    .iter()
                    .any(|alternative| alternative.configured)
        })
        .map(|status| status.name)
        .collect()
}

pub(super) fn provider_secret_sources(
    provider: &AgentTaskExecutorProvider,
    request: Option<&AgentTaskRequest>,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = HashMap::new();
    if let Some(defaults) =
        request.and_then(|request| effective_provider_default(provider, request))
    {
        sources.extend(provider_config_secret_sources(defaults));
    }
    sources
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
        .runtime_selection
        .as_ref()
        .and_then(|selection| selection.ai_provider_id.as_deref())
    {
        return provider.provider_defaults.get(name);
    }
    if let Some(name) = request
        .executor
        .config
        .get("provider")
        .and_then(Value::as_str)
    {
        return provider.provider_defaults.get(name);
    }
    // Provider defaults are route-scoped. Only a request that names the default
    // via executor.config.provider or a provider/model route may inherit that
    // default's credentials. An unused alternative must not become mandatory
    // merely because the executor declares exactly one account.
    let requested = request
        .executor
        .model
        .as_deref()
        .and_then(|model| model.split_once('/').map(|(provider, _)| provider))
        .filter(|provider| !provider.trim().is_empty())
        .and_then(|model_provider| provider.provider_defaults.get(model_provider));

    // A model prefix may use an alternate default only when that default
    // explicitly declares compatibility with the requested model provider.
    if requested.is_some_and(provider_default_credentials_available) {
        return requested;
    }
    provider
        .provider_defaults
        .values()
        .filter(|config| Some(*config) != requested)
        .find(|config| {
            provider_default_supports_model_provider(config, request)
                && provider_default_credentials_available(config)
        })
        .or(requested)
}

fn provider_default_supports_model_provider(config: &Value, request: &AgentTaskRequest) -> bool {
    let Some(model_provider) = request
        .executor
        .model
        .as_deref()
        .and_then(|model| model.split_once('/').map(|(provider, _)| provider))
    else {
        return false;
    };
    config
        .get("model_providers")
        .or_else(|| config.get("modelProviders"))
        .and_then(Value::as_array)
        .is_some_and(|providers| {
            providers
                .iter()
                .any(|item| item.as_str() == Some(model_provider))
        })
}

fn provider_default_credentials_available(config: &Value) -> bool {
    let names = provider_config_secret_env(config);
    !names.is_empty()
        && secret_env_status_with_fallbacks(&names, &provider_config_secret_sources(config))
            .iter()
            .all(|status| status.configured)
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
    fn unselected_sole_default_is_not_required() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "only-account": { "required_secret_env": ["ONLY_ACCOUNT_TOKEN"] }
            }
        }))
        .expect("provider");

        assert!(provider_secret_env(&provider, Some(&request(Value::Null))).is_empty());
        assert!(provider_secret_sources(&provider, Some(&request(Value::Null))).is_empty());
    }

    #[test]
    fn model_default_uses_alias_only_when_compatible_and_available() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "openai": {
                    "secret_env": ["MISSING_API_KEY"],
                    "secret_env_sources": {
                        "MISSING_API_KEY": {"source": "env", "env": "HOMEBOY_TEST_MISSING_API_KEY"}
                    }
                },
                "openai-oauth": {
                    "model_providers": ["openai"],
                    "secret_env": ["AVAILABLE_OAUTH_TOKEN"],
                    "secret_env_sources": {
                        "AVAILABLE_OAUTH_TOKEN": {"source": "env", "env_var": "PATH"}
                    }
                }
            }
        }))
        .expect("provider");
        let mut task = request(Value::Null);
        task.executor.model = Some("openai/gpt-5".to_string());

        assert_eq!(
            provider_secret_env(&provider, Some(&task)),
            vec!["AVAILABLE_OAUTH_TOKEN"]
        );
        assert_eq!(
            provider_secret_sources(&provider, Some(&task))["AVAILABLE_OAUTH_TOKEN"]
                .env_var
                .as_deref(),
            Some("PATH")
        );
    }

    #[test]
    fn model_default_does_not_fall_back_to_unrelated_provider() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "openai": { "required_secret_env": ["MISSING_API_KEY"] },
                "xai": {
                    "model_providers": ["xai"],
                    "secret_env": ["AVAILABLE_XAI_TOKEN"],
                    "secret_env_sources": {
                        "AVAILABLE_XAI_TOKEN": {"source": "env", "env_var": "PATH"}
                    }
                }
            }
        }))
        .expect("provider");
        let mut task = request(Value::Null);
        task.executor.model = Some("openai/gpt-5".to_string());

        assert_eq!(
            provider_secret_env(&provider, Some(&task)),
            vec!["MISSING_API_KEY"]
        );
    }

    #[test]
    fn unavailable_oauth_compatible_default_does_not_fall_back() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "openai": { "required_secret_env": ["MISSING_API_KEY"] },
                "openai-oauth": {
                    "model_providers": ["openai"],
                    "required_secret_env": ["MISSING_OAUTH_TOKEN"]
                }
            }
        }))
        .expect("provider");
        let mut task = request(Value::Null);
        task.executor.model = Some("openai/gpt-5".to_string());

        assert_eq!(
            provider_secret_env(&provider, Some(&task)),
            vec!["MISSING_API_KEY"]
        );
    }

    #[test]
    fn explicit_runtime_provider_selection_does_not_fall_back() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "openai": { "required_secret_env": ["MISSING_API_KEY"] },
                "openai-oauth": {
                    "model_providers": ["openai"],
                    "secret_env": ["AVAILABLE_OAUTH_TOKEN"],
                    "secret_env_sources": {
                        "AVAILABLE_OAUTH_TOKEN": {"source": "env", "env_var": "PATH"}
                    }
                }
            }
        }))
        .expect("provider");
        let mut task = request(Value::Null);
        task.executor.model = Some("openai/gpt-5".to_string());
        task.executor.runtime_selection = Some(crate::agent_task::AgentTaskRuntimeSelection {
            ai_provider_id: Some("openai".to_string()),
            ..Default::default()
        });

        assert_eq!(
            provider_secret_env(&provider, Some(&task)),
            vec!["MISSING_API_KEY"]
        );
    }

    #[test]
    fn dispatch_secret_plan_omits_unrelated_sole_default_credentials() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "capabilities": ["provider_owned_auth"],
            "provider_defaults": {
                "unused-account": { "required_secret_env": ["UNUSED_ACCOUNT_TOKEN"] }
            }
        }))
        .expect("provider");
        let mut task = request(Value::Null);
        task.executor.selector = Some("test.provider".to_string());
        let plan = AgentTaskPlan::new("secret-selection", vec![task]);

        assert!(
            provider_runner_secret_env_for_plan_with_providers(&plan, &[provider.clone()])
                .is_empty()
        );
        assert!(provider_secret_env_plan(&provider, &plan.tasks[0])
            .secret_env_names()
            .is_empty());
    }

    #[test]
    fn dispatch_secret_plan_enforces_selected_declared_credentials() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "used-account": { "required_secret_env": ["USED_ACCOUNT_TOKEN"] },
                "unused-account": { "required_secret_env": ["UNUSED_ACCOUNT_TOKEN"] }
            }
        }))
        .expect("provider");
        let mut task = request(serde_json::json!({"provider": "used-account"}));
        task.executor.selector = Some("test.provider".to_string());
        let plan = AgentTaskPlan::new("secret-selection-selected", vec![task]);

        assert_eq!(
            provider_runner_secret_env_for_plan_with_providers(&plan, &[provider.clone()]),
            vec!["USED_ACCOUNT_TOKEN".to_string()]
        );
        assert_eq!(
            provider_secret_env_plan(&provider, &plan.tasks[0]).secret_env_names(),
            vec!["USED_ACCOUNT_TOKEN".to_string()]
        );
    }

    #[test]
    fn selected_model_route_does_not_inherit_an_unrelated_sole_default() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "codex": { "required_secret_env": ["CODEX_TOKEN"] }
            }
        }))
        .expect("provider");
        let mut request = request(Value::Null);
        request.executor.model = Some("xai/grok-4.6".to_string());

        assert!(provider_secret_env(&provider, Some(&request)).is_empty());
        assert!(provider_secret_sources(&provider, Some(&request)).is_empty());
    }

    #[test]
    fn selected_model_route_uses_its_matching_provider_default() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "xai": { "required_secret_env": ["XAI_TOKEN"] },
                "codex": { "required_secret_env": ["CODEX_TOKEN"] }
            }
        }))
        .expect("provider");
        let mut request = request(Value::Null);
        request.executor.model = Some("xai/grok-4.6".to_string());

        assert_eq!(
            provider_secret_env(&provider, Some(&request)),
            vec!["XAI_TOKEN".to_string()]
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

    fn controller_refresh_rotation_fixture() -> (Vec<AgentTaskExecutorProvider>, AgentTaskPlan) {
        let providers = [
            serde_json::json!({"id":"test.primary", "backend":"test"}),
            serde_json::json!({
                "id":"test.fallback", "backend":"test",
                "provider_defaults": {"refreshed-account": {
                    "secret_env": ["REFRESHED_ACCESS", "REFRESHED_EXPIRES"],
                    "secret_env_sources": {
                        "REFRESHED_ACCESS": {"source":"json-file", "path":"~/.auth.json", "field":"acct.access"},
                        "REFRESHED_EXPIRES": {"source":"json-file", "path":"~/.auth.json", "field":"acct.expires"}
                    },
                    "controller_refresh": {"argv":["node", "{{runtime_path}}/refresh.cjs"]}
                }}
            })
        ].into_iter().map(|value| serde_json::from_value(value).unwrap()).collect();
        let mut task = request(Value::Null);
        task.executor.selector = Some("test.primary".to_string());
        let mut plan = AgentTaskPlan::new("controller-refresh-rotation", vec![task]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    selector: Some("test.primary".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    selector: Some("test.fallback".to_string()),
                    provider_config: serde_json::json!({"provider":"refreshed-account"}),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        (providers, plan)
    }

    #[test]
    fn route_secret_sources_cover_rotation_fallbacks_for_lab_delivery() {
        // #15783: the admitted-route map above stays narrow (#14194); the
        // route map used only for best-effort Lab delivery includes fallbacks.
        let (providers, plan) = controller_refresh_rotation_fixture();

        let admitted = provider_secret_sources_for_plan_with_providers(&plan, &providers);
        let routes = provider_route_secret_sources_for_plan_with_providers(&plan, &providers);
        let route_names = provider_route_secret_env_for_plan_with_providers(&plan, &providers);

        assert!(!admitted.contains_key("REFRESHED_ACCESS"));
        assert_eq!(
            routes["REFRESHED_ACCESS"].field.as_deref(),
            Some("acct.access")
        );
        assert!(route_names.contains(&"REFRESHED_ACCESS".to_string()));
    }

    #[test]
    fn controller_refreshes_include_a_rotation_fallback_account() {
        let (providers, plan) = controller_refresh_rotation_fixture();

        let refreshes = provider_controller_refreshes_for_plan_with_providers(&plan, &providers);

        assert_eq!(refreshes.len(), 1);
        assert_eq!(refreshes[0].provider.id, "test.fallback");
        assert_eq!(
            refreshes[0].invocation.argv,
            vec![
                "node".to_string(),
                "{{runtime_path}}/refresh.cjs".to_string()
            ]
        );
        assert_eq!(
            refreshes[0].secret_env,
            vec![
                "REFRESHED_ACCESS".to_string(),
                "REFRESHED_EXPIRES".to_string()
            ]
        );
    }
}
