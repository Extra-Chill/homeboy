use super::*;

pub(crate) fn role_aliases_for_executor(
    backend: &str,
    selector: Option<&str>,
) -> AgentTaskProviderRoleAliases {
    let catalog = AgentTaskProviderCatalog::discover();
    select_provider_by_backend(catalog.providers(), backend, selector)
        .map(|provider| provider.role_aliases.clone())
        .unwrap_or_default()
}

pub(crate) fn timeout_artifact_discovery_for_executor(
    backend: &str,
    selector: Option<&str>,
) -> AgentTaskProviderTimeoutArtifactDiscovery {
    let catalog = AgentTaskProviderCatalog::discover();
    select_provider_by_backend(catalog.providers(), backend, selector)
        .map(|provider| provider.timeout_artifact_discovery.clone())
        .unwrap_or_default()
}

pub(crate) fn role_aliases_for_provider(
    provider_id_or_backend: &str,
) -> AgentTaskProviderRoleAliases {
    let catalog = AgentTaskProviderCatalog::discover();
    catalog
        .providers()
        .iter()
        .find(|provider| {
            provider.id == provider_id_or_backend || provider.backend == provider_id_or_backend
        })
        .map(|provider| provider.role_aliases.clone())
        .unwrap_or_default()
}

pub(super) fn provider_requires_cwd_git_checkout_with_providers(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> bool {
    select_provider_by_backend(providers, backend, selector)
        .map(|provider| {
            provider.runtime_contract.apply_back.requires_git_checkout == Some(true)
                || provider.workspace_materialization.as_ref().is_some_and(
                    AgentTaskProviderWorkspaceMaterialization::requires_cwd_git_checkout,
                )
        })
        .unwrap_or(false)
}
pub(super) fn select_provider<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    request: &AgentTaskRequest,
) -> Option<&'a AgentTaskExecutorProvider> {
    select_provider_by_backend(
        providers,
        &request.executor.backend,
        request.executor.selector.as_deref(),
    )
}

/// Structured outcome of resolving a `--backend`/`--selector` request against a
/// concrete provider list. This is the single source of truth shared by every
/// caller that asks "can this backend/selector run here?" — execution-time
/// selection, the local availability check, and the Lab runner preflight. By
/// returning a typed reason (rather than a bare `Option`/`bool`) the preflight
/// can explain *why* a provider that `agent-task providers` lists is still not
/// selectable, instead of emitting a misleading "availability is false".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderResolution<'a> {
    /// Exactly one provider matched the backend/selector.
    Resolved(&'a AgentTaskExecutorProvider),
    NotFound,
    /// The selector did not match a provider for the backend.
    SelectorMismatch {
        available_ids: Vec<String>,
        selector_hint: Option<String>,
    },
}

pub(crate) fn selector_runtime_provider_hint(
    backend: &str,
    selector: Option<&str>,
) -> Option<String> {
    let selector = selector?.trim();
    let catalog = AgentTaskProviderCatalog::discover();
    let reserved = catalog
        .providers()
        .iter()
        .flat_map(|provider| provider.cli.reserved_selector_hints.iter())
        .any(|hint| hint == selector);
    if !reserved {
        return None;
    }

    Some(format!(
        "'{selector}' is declared by an executor provider as runtime-specific provider configuration, not a dispatch selector. --selector selects the Homeboy executor provider id for backend '{backend}'; pass runtime/provider configuration through --provider-config instead."
    ))
}

fn selector_runtime_provider_hint_from_providers(
    providers: &[&AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> Option<String> {
    let selector = selector?.trim();
    let reserved = providers
        .iter()
        .flat_map(|provider| provider.cli.reserved_selector_hints.iter())
        .any(|hint| hint == selector);
    reserved.then(|| {
        format!(
            "'{selector}' is declared by an executor provider as runtime-specific provider configuration, not a dispatch selector. --selector selects the Homeboy executor provider id for backend '{backend}'; pass runtime/provider configuration through --provider-config instead."
        )
    })
}

impl<'a> ProviderResolution<'a> {
    pub(crate) fn resolved(self) -> Option<&'a AgentTaskExecutorProvider> {
        match self {
            ProviderResolution::Resolved(provider) => Some(provider),
            _ => None,
        }
    }
}

/// Resolve a backend/selector request against a provider list, returning a
/// structured outcome. This is the shared resolution contract; execution-time
/// `select_provider`, the local availability check, and the Lab preflight all
/// funnel through here so they can never disagree about the same provider list.
pub fn resolve_provider_for_backend<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> ProviderResolution<'a> {
    let exact_matches: Vec<&AgentTaskExecutorProvider> = providers
        .iter()
        .filter(|provider| provider.backend == backend)
        .collect();

    if !exact_matches.is_empty() {
        if let Some(provider) = exact_matches
            .iter()
            .find(|provider| selector.is_none_or(|selector| provider.id == selector))
        {
            return ProviderResolution::Resolved(provider);
        }
        return ProviderResolution::SelectorMismatch {
            selector_hint: selector_runtime_provider_hint_from_providers(
                &exact_matches,
                backend,
                selector,
            ),
            available_ids: exact_matches
                .iter()
                .map(|provider| provider.id.clone())
                .collect(),
        };
    }
    ProviderResolution::NotFound
}

pub(super) fn select_provider_by_backend<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> Option<&'a AgentTaskExecutorProvider> {
    resolve_provider_for_backend(providers, backend, selector).resolved()
}

pub(super) fn required_extension_ids_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> Vec<String> {
    let mut extension_ids = BTreeSet::new();
    for request in &plan.tasks {
        if let Some(extension_id) = select_provider(providers, request)
            .and_then(|provider| provider.extension_id.as_ref())
            .filter(|extension_id| !extension_id.trim().is_empty())
        {
            extension_ids.insert(extension_id.clone());
        }
    }
    extension_ids.into_iter().collect()
}
