use super::resolution::{
    lab_runtime_component_ids_for_plan_with_providers,
    provider_requires_cwd_git_checkout_with_providers,
    required_extension_ids_for_plan_with_providers, select_provider,
};
use super::runner_readiness::provider_executable_env;
use super::secrets::{
    apply_provider_runner_secret_env_contracts_with_providers, provider_config_secret_sources,
    provider_runner_secret_env_for_plan_with_providers, provider_secret_sources,
    provider_secret_sources_for_plan_with_providers,
};
use super::*;
use sha2::{Digest, Sha256};

#[cfg(not(test))]
static PROVIDER_CATALOG: OnceLock<RwLock<AgentTaskProviderCatalog>> = OnceLock::new();

#[derive(Debug, Clone, Default)]
pub struct ExtensionProviderAgentTaskExecutor {
    providers: Vec<AgentTaskExecutorProvider>,
    diagnostics: Vec<AgentRuntimeDiscoveryDiagnostic>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderCatalog {
    pub providers: Vec<AgentTaskExecutorProvider>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentRuntimeDiscoveryDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl AgentTaskProviderCatalog {
    pub fn discover() -> Self {
        #[cfg(not(test))]
        {
            let catalog = PROVIDER_CATALOG.get_or_init(|| RwLock::new(discover_provider_catalog()));
            return catalog.read().expect("provider catalog lock").clone();
        }
        #[cfg(test)]
        {
            discover_provider_catalog()
        }
    }

    pub fn refresh() -> Self {
        #[cfg(not(test))]
        {
            let refreshed = discover_provider_catalog();
            let catalog = PROVIDER_CATALOG.get_or_init(|| RwLock::new(refreshed.clone()));
            *catalog.write().expect("provider catalog lock") = refreshed.clone();
            refreshed
        }
        #[cfg(test)]
        {
            discover_provider_catalog()
        }
    }

    pub fn providers(&self) -> &[AgentTaskExecutorProvider] {
        &self.providers
    }

    pub fn diagnostics(&self) -> &[AgentRuntimeDiscoveryDiagnostic] {
        &self.diagnostics
    }

    pub fn provider_requires_cwd_git_checkout(
        &self,
        backend: &str,
        selector: Option<&str>,
    ) -> bool {
        provider_requires_cwd_git_checkout_with_providers(&self.providers, backend, selector)
    }

    pub fn apply_provider_runner_secret_env_contracts(&self, plan: &mut AgentTaskPlan) {
        apply_provider_runner_secret_env_contracts_with_providers(plan, &self.providers);
    }

    /// Run declared runtime preflight checks for each task's resolved provider.
    pub fn enforce_runtime_preflight_checks_for_plan(
        &self,
        plan: &AgentTaskPlan,
    ) -> homeboy_core::Result<()> {
        enforce_runtime_preflight_checks_for_plan_with_providers(plan, &self.providers)
    }

    pub fn provider_secret_sources_for_providers(
        &self,
    ) -> HashMap<String, defaults::AgentTaskSecretSource> {
        provider_secret_sources_for_providers(&self.providers)
    }
}

fn discover_provider_catalog() -> AgentTaskProviderCatalog {
    let catalog = super::discovery::discover_agent_task_executor_provider_catalog();
    let version = provider_catalog_version(&catalog.providers, &catalog.diagnostics);
    AgentTaskProviderCatalog {
        providers: catalog.providers,
        diagnostics: catalog.diagnostics,
        version: Some(version),
    }
}

fn provider_catalog_version(
    providers: &[AgentTaskExecutorProvider],
    diagnostics: &[AgentRuntimeDiscoveryDiagnostic],
) -> String {
    // The handoff version represents the selected catalog content, not the
    // instant discovery happened. That makes stale controller/Lab catalogs
    // detectable without treating an unchanged rediscovery as drift.
    let content = serde_json::to_vec(&(providers, diagnostics))
        .expect("agent-task provider catalog must serialize");
    format!("resolved:{:x}", Sha256::digest(content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_version_is_deterministic_and_detects_stale_content() {
        let empty = provider_catalog_version(&[], &[]);
        assert_eq!(empty, provider_catalog_version(&[], &[]));

        let changed = provider_catalog_version(
            &[],
            &[AgentRuntimeDiscoveryDiagnostic {
                class: "agent_runtime_catalog.conflict".to_string(),
                message: "runtime collision".to_string(),
                runtime_id: Some("example".to_string()),
                extension_id: None,
                path: None,
            }],
        );

        assert_ne!(empty, changed);
        assert!(empty.starts_with("resolved:"));
    }
}

impl ExtensionProviderAgentTaskExecutor {
    pub fn discover() -> Self {
        Self::from_catalog(AgentTaskProviderCatalog::discover())
    }

    pub fn from_catalog(catalog: AgentTaskProviderCatalog) -> Self {
        Self {
            providers: catalog.providers,
            diagnostics: catalog.diagnostics,
        }
    }

    #[cfg(test)]
    pub(super) fn with_providers(providers: Vec<AgentTaskExecutorProvider>) -> Self {
        Self {
            providers,
            diagnostics: Vec::new(),
        }
    }

    pub fn providers(&self) -> &[AgentTaskExecutorProvider] {
        &self.providers
    }

    pub fn diagnostics(&self) -> &[AgentRuntimeDiscoveryDiagnostic] {
        &self.diagnostics
    }

    pub fn default_backend(&self) -> homeboy_core::Result<Option<String>> {
        default_backend_from_policy(None)
    }

    pub fn required_extension_ids_for_plan(&self, plan: &AgentTaskPlan) -> Vec<String> {
        required_extension_ids_for_plan_with_providers(plan, &self.providers)
    }

    pub fn lab_runtime_component_ids_for_plan(&self, plan: &AgentTaskPlan) -> Vec<String> {
        lab_runtime_component_ids_for_plan_with_providers(plan, &self.providers)
    }
}

pub fn default_backend() -> homeboy_core::Result<Option<String>> {
    default_backend_from_policy(None)
}

pub fn default_backend_for_component(
    component_id: Option<&str>,
) -> homeboy_core::Result<Option<String>> {
    default_backend_from_policy(component_id)
}

pub fn provider_runner_readiness_contracts() -> Vec<AgentTaskProviderRunnerReadiness> {
    AgentTaskProviderCatalog::discover()
        .providers
        .into_iter()
        .flat_map(|provider| provider.runner_readiness)
        .collect()
}

pub fn provider_runner_source_contracts() -> Vec<AgentTaskProviderRunnerSource> {
    AgentTaskProviderCatalog::discover()
        .providers
        .into_iter()
        .flat_map(|provider| provider.runner_sources)
        .collect()
}

pub fn dependency_failure_patterns() -> Vec<AgentTaskProviderDependencyFailurePattern> {
    AgentTaskProviderCatalog::discover()
        .providers
        .into_iter()
        .flat_map(|provider| provider.dependency_failure_patterns)
        .collect()
}

pub fn validate_provider_runner_readiness_for_backend(
    backend: &str,
    selector: Option<&str>,
) -> homeboy_core::Result<()> {
    let catalog = AgentTaskProviderCatalog::discover();
    validate_provider_runner_readiness_for_backend_with_catalog(&catalog, backend, selector)
}

fn validate_provider_runner_readiness_for_backend_with_catalog(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
) -> homeboy_core::Result<()> {
    validate_provider_runner_readiness_for_backend_with_diagnostics(
        catalog.providers(),
        catalog.diagnostics(),
        backend,
        selector,
    )
}

#[cfg(test)]
pub(super) fn validate_provider_runner_readiness_for_backend_with_providers(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> homeboy_core::Result<()> {
    validate_provider_runner_readiness_for_backend_with_diagnostics(
        providers,
        &[],
        backend,
        selector,
    )
}

fn validate_provider_runner_readiness_for_backend_with_diagnostics(
    providers: &[AgentTaskExecutorProvider],
    diagnostics: &[AgentRuntimeDiscoveryDiagnostic],
    backend: &str,
    selector: Option<&str>,
) -> homeboy_core::Result<()> {
    let provider = match resolve_provider_for_backend(providers, backend, selector) {
        ProviderResolution::Resolved(provider) => provider,
        ProviderResolution::NotFound => {
            let matching_diagnostics =
                runtime_discovery_diagnostics_for_backend(diagnostics, backend);
            let mut suggestions = vec![
                "Run `homeboy agent-task providers` on the same machine/runner to inspect registered providers.".to_string(),
                "Install or sync the extension/runtime that declares the requested backend, or pass --backend with a registered backend.".to_string(),
            ];
            suggestions.extend(
                matching_diagnostics
                    .iter()
                    .map(format_runtime_discovery_diagnostic),
            );
            return Err(Error::validation_invalid_argument(
                "backend",
                provider_not_found_message(backend, &matching_diagnostics),
                Some(backend.to_string()),
                Some(suggestions),
            ));
        }
        ProviderResolution::AmbiguousExtensionAlias { candidate_ids } => {
            return Err(Error::validation_invalid_argument(
                "backend",
                format!(
                    "backend '{backend}' matches multiple extension agent-task providers; pass --selector with one provider id"
                ),
                Some(backend.to_string()),
                Some(vec![format!(
                    "Available provider ids for selector: {}.",
                    candidate_ids.join(", ")
                )]),
            ));
        }
        ProviderResolution::SelectorMismatch {
            available_ids,
            selector_hint,
        } => {
            let mut suggestions = vec![format!(
                "Available provider ids for backend '{backend}': {}.",
                available_ids.join(", ")
            )];
            if let Some(hint) = selector_hint {
                suggestions.push(hint);
            }
            return Err(Error::validation_invalid_argument(
                "selector",
                format!(
                    "no extension agent-task provider for backend '{backend}' matched selector '{}'",
                    selector.unwrap_or("")
                ),
                selector.map(str::to_string),
                Some(suggestions),
            ));
        }
    };

    provider_executable_env(provider).map_err(|error| {
        Error::validation_invalid_argument(
            "backend",
            format!(
                "agent-task backend '{backend}' is registered but runner readiness failed for provider '{}': {}",
                provider.id,
                error.message()
            ),
            Some(backend.to_string()),
            Some(vec![
                format!(
                    "Selected provider: {} (backend '{}', selector '{}').",
                    provider.id,
                    provider.backend,
                    selector.unwrap_or("<default>")
                ),
                "Fix the executable/env on this machine or runner before dispatching the task wave.".to_string(),
            ]),
        )
    })?;

    Ok(())
}

pub(super) fn runtime_discovery_diagnostics_for_backend(
    diagnostics: &[AgentRuntimeDiscoveryDiagnostic],
    backend: &str,
) -> Vec<AgentRuntimeDiscoveryDiagnostic> {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.runtime_id.as_deref() == Some(backend))
        .cloned()
        .collect()
}

pub(super) fn provider_not_found_message(
    backend: &str,
    diagnostics: &[AgentRuntimeDiscoveryDiagnostic],
) -> String {
    let base = format!("no extension agent-task provider found for backend '{backend}'");
    if diagnostics.is_empty() {
        return base;
    }
    let details = diagnostics
        .iter()
        .map(format_runtime_discovery_diagnostic)
        .collect::<Vec<_>>()
        .join("; ");
    format!("{base}; runtime discovery diagnostics: {details}")
}

pub(super) fn format_runtime_discovery_diagnostic(
    diagnostic: &AgentRuntimeDiscoveryDiagnostic,
) -> String {
    match diagnostic.path.as_deref() {
        Some(path) => format!("{} at {}: {}", diagnostic.class, path, diagnostic.message),
        None => format!("{}: {}", diagnostic.class, diagnostic.message),
    }
}

pub fn required_extension_ids_for_plan(plan: &AgentTaskPlan) -> Vec<String> {
    ExtensionProviderAgentTaskExecutor::discover().required_extension_ids_for_plan(plan)
}

pub fn lab_runtime_component_ids_for_plan(plan: &AgentTaskPlan) -> Vec<String> {
    ExtensionProviderAgentTaskExecutor::discover().lab_runtime_component_ids_for_plan(plan)
}

pub fn provider_requires_cwd_git_checkout(backend: &str, selector: Option<&str>) -> bool {
    AgentTaskProviderCatalog::discover().provider_requires_cwd_git_checkout(backend, selector)
}

pub fn apply_provider_runner_secret_env_contracts(plan: &mut AgentTaskPlan) {
    AgentTaskProviderCatalog::discover().apply_provider_runner_secret_env_contracts(plan);
}

pub fn enforce_runtime_preflight_checks_for_plan(plan: &AgentTaskPlan) -> homeboy_core::Result<()> {
    AgentTaskProviderCatalog::discover().enforce_runtime_preflight_checks_for_plan(plan)
}

pub(super) fn enforce_runtime_preflight_checks_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> homeboy_core::Result<()> {
    for request in &plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        let checks = &provider.runtime_contract.preflight_checks;
        if checks.is_empty() {
            continue;
        }
        ensure_runtime_preflight_checks(checks, &request.component_contracts)?;
    }
    Ok(())
}

pub fn provider_runner_secret_env_for_plan(plan: &AgentTaskPlan) -> Vec<String> {
    let catalog = AgentTaskProviderCatalog::discover();
    provider_runner_secret_env_for_plan_with_providers(plan, catalog.providers())
}

pub fn provider_secret_sources_for_plan(
    plan: &AgentTaskPlan,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let catalog = AgentTaskProviderCatalog::discover();
    provider_secret_sources_for_plan_with_providers(plan, catalog.providers())
}

pub fn provider_secret_sources_for_discovered_providers(
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    AgentTaskProviderCatalog::discover().provider_secret_sources_for_providers()
}

pub fn provider_secret_sources_for_providers(
    providers: &[AgentTaskExecutorProvider],
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = HashMap::new();
    for provider in providers {
        sources.extend(provider_secret_sources(provider, None));
        for defaults in provider.provider_defaults.values() {
            sources.extend(provider_config_secret_sources(defaults));
        }
    }
    sources
}

/// Secret sources scoped to a single backend (and optional provider selector).
///
/// Mirrors the backend/selector resolution `agent-task doctor` uses so auth
/// status reports readiness for the exact backend cook/dispatch would target.
/// When `selector` is `None`, all providers for `backend` are included.
pub fn provider_secret_sources_for_backend(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let scoped: Vec<&AgentTaskExecutorProvider> = providers
        .iter()
        .filter(|provider| provider.backend == backend)
        .filter(|provider| selector.is_none_or(|selector| provider.id == selector))
        .collect();
    let mut sources = HashMap::new();
    for provider in scoped {
        sources.extend(provider_secret_sources(provider, None));
        for defaults in provider.provider_defaults.values() {
            sources.extend(provider_config_secret_sources(defaults));
        }
    }
    sources
}

fn default_backend_from_policy(component_id: Option<&str>) -> homeboy_core::Result<Option<String>> {
    if let Some(component_id) = component_id {
        if let Ok(component) = component::load(component_id) {
            if let Some(default_backend) = component_default_backend(&component) {
                return Ok(Some(default_backend));
            }
        }
    }

    let extension_defaults: Vec<String> = extension::load_all_extensions()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|manifest| {
            manifest
                .agent_task
                .and_then(|agent_task| agent_task.default_backend)
        })
        .filter(|backend| !backend.trim().is_empty())
        .collect();

    if extension_defaults.len() > 1 {
        return Err(Error::validation_invalid_argument(
            "backend",
            "agent-task default backend is ambiguous because multiple extension policies declare agent_task.default_backend",
            None,
            Some(vec![
                "Set /agent_task/default_backend in Homeboy config or pass --backend explicitly.".to_string(),
            ]),
        ));
    }
    if let Some(default_backend) = extension_defaults.into_iter().next() {
        return Ok(Some(default_backend));
    }

    Ok(defaults::load_config()
        .agent_task
        .default_backend
        .filter(|backend| !backend.trim().is_empty()))
}

pub(super) fn component_default_backend(component: &component::Component) -> Option<String> {
    component
        .extensions
        .as_ref()?
        .values()
        .find_map(|extension| {
            extension
                .settings
                .get("agent_task")
                .and_then(|value| value.get("default_backend"))
                .and_then(Value::as_str)
                .or_else(|| {
                    extension
                        .settings
                        .get("agent_task_default_backend")
                        .and_then(Value::as_str)
                })
                .filter(|backend| !backend.trim().is_empty())
                .map(String::from)
        })
}
