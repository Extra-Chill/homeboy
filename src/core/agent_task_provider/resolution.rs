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
pub(super) fn discover_agent_task_executor_providers() -> Vec<AgentTaskExecutorProvider> {
    agent_runtime_manifest::discover_agent_task_executor_providers()
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
pub(crate) enum ProviderResolution<'a> {
    /// Exactly one provider matched the backend/selector.
    Resolved(&'a AgentTaskExecutorProvider),
    /// No provider matched the backend either exactly or via extension alias.
    NotFound,
    /// Multiple providers share `extension_id == backend` and no selector
    /// disambiguated them, so the alias is ambiguous. The candidate provider
    /// ids are surfaced so callers can tell the operator which `--selector`
    /// values would resolve it.
    AmbiguousExtensionAlias { candidate_ids: Vec<String> },
    /// One or more providers matched the backend/extension alias, but the
    /// supplied selector did not match any of them. The selectable provider
    /// ids are surfaced so the operator can correct the `--selector`.
    SelectorMismatch { available_ids: Vec<String> },
}

pub(crate) fn selector_runtime_provider_hint(
    backend: &str,
    selector: Option<&str>,
) -> Option<String> {
    let selector = selector?.trim();
    if !matches!(selector, "codex" | "opencode" | "claude-code") {
        return None;
    }

    Some(format!(
        "'{selector}' looks like a nested AI runtime provider, not a dispatch selector. --dispatch-selector selects the Homeboy executor provider id for backend '{backend}'; pass the AI provider in --dispatch-provider-config instead."
    ))
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
pub(crate) fn resolve_provider_for_backend<'a>(
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
            available_ids: exact_matches
                .iter()
                .map(|provider| provider.id.clone())
                .collect(),
        };
    }
    resolve_provider_by_extension_alias(providers, backend, selector)
}

pub(super) fn select_provider_by_backend<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> Option<&'a AgentTaskExecutorProvider> {
    resolve_provider_for_backend(providers, backend, selector).resolved()
}

fn resolve_provider_by_extension_alias<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> ProviderResolution<'a> {
    let alias_matches: Vec<&AgentTaskExecutorProvider> = providers
        .iter()
        .filter(|provider| provider.extension_id.as_deref() == Some(backend))
        .collect();

    if alias_matches.is_empty() {
        return ProviderResolution::NotFound;
    }

    match selector {
        None => {
            if alias_matches.len() == 1 {
                ProviderResolution::Resolved(alias_matches[0])
            } else {
                ProviderResolution::AmbiguousExtensionAlias {
                    candidate_ids: alias_matches
                        .iter()
                        .map(|provider| provider.id.clone())
                        .collect(),
                }
            }
        }
        Some(selector) => match alias_matches
            .iter()
            .find(|provider| provider.id == selector)
        {
            Some(provider) => ProviderResolution::Resolved(provider),
            None => ProviderResolution::SelectorMismatch {
                available_ids: alias_matches
                    .iter()
                    .map(|provider| provider.id.clone())
                    .collect(),
            },
        },
    }
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

pub(super) fn lab_runtime_component_ids_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> Vec<String> {
    let mut component_ids = BTreeSet::new();
    for request in &plan.tasks {
        if let Some(provider) = select_provider(providers, request) {
            for component_id in &provider.lab_runtime_components {
                let component_id = component_id.trim();
                if !component_id.is_empty() {
                    component_ids.insert(component_id.to_string());
                }
            }
        }
    }
    component_ids.into_iter().collect()
}
