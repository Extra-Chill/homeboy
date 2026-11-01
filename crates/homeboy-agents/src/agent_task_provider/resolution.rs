use super::*;

pub(super) fn provider_requires_cwd_git_checkout_with_providers(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> bool {
    select_provider_by_backend(providers, backend, selector)
        .map(|provider| {
            provider
                .workspace_materialization
                .as_ref()
                .is_some_and(AgentTaskProviderWorkspaceMaterialization::requires_cwd_git_checkout)
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
            selector_hint: None,
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
