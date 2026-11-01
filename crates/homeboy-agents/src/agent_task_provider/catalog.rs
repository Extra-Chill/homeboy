use super::resolution::{
    provider_requires_cwd_git_checkout_with_providers,
    required_extension_ids_for_plan_with_providers,
};
use super::*;
use homeboy_engine_primitives::content_hash;
use std::sync::OnceLock;

static PROVIDER_EVIDENCE: OnceLock<Arc<Mutex<ProviderEvidenceStore>>> = OnceLock::new();

#[derive(Debug, Default)]
pub(super) struct ProviderEvidenceStore {
    pub(super) readiness: ProviderRuntimeReadinessCache,
    pub(super) usage_caps: ProviderUsageCapRegistry,
    pub(super) account_blocks: BTreeMap<String, AccountBlockEvidence>,
    pub(super) launch_credentials: HashMap<String, VecDeque<BoundProviderCredentials>>,
}

#[derive(Debug)]
pub(super) struct BoundProviderCredentials {
    pub(super) env: Vec<(String, String)>,
}

#[derive(Debug)]
pub(super) struct AccountBlockEvidence {
    pub(super) expires_at: chrono::DateTime<chrono::Utc>,
}

fn shared_provider_evidence() -> Arc<Mutex<ProviderEvidenceStore>> {
    Arc::clone(PROVIDER_EVIDENCE.get_or_init(|| Arc::new(Mutex::new(Default::default()))))
}

#[derive(Debug, Clone, Default)]
pub struct ExtensionProviderAgentTaskExecutor {
    providers: Vec<AgentTaskExecutorProvider>,
    diagnostics: Vec<AgentRuntimeDiscoveryDiagnostic>,
    pub(super) evidence: Arc<Mutex<ProviderEvidenceStore>>,
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
        discover_provider_catalog()
    }

    pub fn refresh() -> Self {
        discover_provider_catalog()
    }

    pub fn providers(&self) -> &[AgentTaskExecutorProvider] {
        &self.providers
    }

    pub fn diagnostics(&self) -> &[AgentRuntimeDiscoveryDiagnostic] {
        &self.diagnostics
    }

    /// The distinct backends this catalog declares, sorted and deduped.
    ///
    /// This is the answer to "what are the valid `--backend` values here?".
    /// Without it that answer is only reachable by running `homeboy agent-task
    /// providers` and parsing its JSON, which is what made the missing-default
    /// backend error a papercut (#11478).
    ///
    /// Presence here means a provider *declares* the backend, not that the
    /// backend is usable right now — live readiness is a separate probe.
    /// Callers that enumerate these must not imply readiness.
    pub fn backends(&self) -> Vec<String> {
        let mut backends = self
            .providers
            .iter()
            .map(|provider| provider.backend.trim().to_string())
            .filter(|backend| !backend.is_empty())
            .collect::<Vec<_>>();
        backends.sort();
        backends.dedup();
        backends
    }

    pub fn provider_requires_cwd_git_checkout(
        &self,
        backend: &str,
        selector: Option<&str>,
    ) -> bool {
        provider_requires_cwd_git_checkout_with_providers(&self.providers, backend, selector)
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
    format!("resolved:{}", content_hash::sha256_hex(&content))
}

impl ExtensionProviderAgentTaskExecutor {
    pub fn discover() -> Self {
        Self::from_catalog(AgentTaskProviderCatalog::discover())
    }

    pub fn from_catalog(catalog: AgentTaskProviderCatalog) -> Self {
        Self {
            providers: catalog.providers,
            diagnostics: catalog.diagnostics,
            evidence: shared_provider_evidence(),
        }
    }

    /// Test executors own their evidence. The process-wide store would let
    /// one test's readiness, capacity, and account evidence leak into
    /// another, and a single panicking holder would poison it for every test
    /// in the binary (#14832). Tests that exercise cross-executor sharing
    /// opt in with [`Self::sharing_evidence_with`].
    #[cfg(test)]
    pub(super) fn with_providers(providers: Vec<AgentTaskExecutorProvider>) -> Self {
        Self {
            providers,
            diagnostics: Vec::new(),
            evidence: Arc::new(Mutex::new(ProviderEvidenceStore {
                readiness: ProviderRuntimeReadinessCache::isolated(),
                ..Default::default()
            })),
        }
    }

    /// A separately constructed executor that shares `self`'s evidence, as
    /// every production executor in one process does.
    #[cfg(test)]
    pub(super) fn sharing_evidence_with(&self, providers: Vec<AgentTaskExecutorProvider>) -> Self {
        Self {
            providers,
            diagnostics: Vec::new(),
            evidence: Arc::clone(&self.evidence),
        }
    }

    pub fn providers(&self) -> &[AgentTaskExecutorProvider] {
        &self.providers
    }

    pub fn diagnostics(&self) -> &[AgentRuntimeDiscoveryDiagnostic] {
        &self.diagnostics
    }

    pub fn default_backend(&self) -> homeboy_core::Result<Option<String>> {
        default_backend_for_component(None)
    }

    #[cfg(test)]
    pub(super) fn default_backend_with_policy(
        &self,
        extensions: Vec<homeboy_extension_contract::ExtensionManifest>,
        config: &defaults::HomeboyConfig,
    ) -> homeboy_core::Result<Option<String>> {
        default_backend_from_policy_sources(None, extensions, || {
            config.agent_task.default_backend.clone()
        })
    }

    pub fn required_extension_ids_for_plan(&self, plan: &AgentTaskPlan) -> Vec<String> {
        required_extension_ids_for_plan_with_providers(plan, &self.providers)
    }
}

pub fn default_backend() -> homeboy_core::Result<Option<String>> {
    default_backend_for_component(None)
}

pub fn default_backend_for_component(
    component: Option<&component::Component>,
) -> homeboy_core::Result<Option<String>> {
    default_backend_from_policy_sources(
        component,
        homeboy_core::extension::catalog::load_all_extensions().unwrap_or_default(),
        || defaults::load_config().agent_task.default_backend,
    )
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
    let mut base = format!(
        "no extension agent-task provider found for backend '{backend}' (no installed provider can serve this backend)"
    );
    if backend.trim() != crate::agent_task_builtin_backend::BUILTIN_DEFAULT_BACKEND {
        base.push_str("; ");
        base.push_str(&crate::agent_task_builtin_backend::builtin_backend_remediation(backend));
    }
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

pub fn provider_requires_cwd_git_checkout(backend: &str, selector: Option<&str>) -> bool {
    AgentTaskProviderCatalog::discover().provider_requires_cwd_git_checkout(backend, selector)
}

pub(super) fn default_backend_from_policy_sources<F>(
    component: Option<&component::Component>,
    extensions: Vec<homeboy_extension_contract::ExtensionManifest>,
    config_default: F,
) -> homeboy_core::Result<Option<String>>
where
    F: FnOnce() -> Option<String>,
{
    if let Some(component) = component {
        if let Some(default_backend) = component_default_backend(component) {
            return Ok(Some(default_backend));
        }
    }

    let extension_defaults: Vec<String> = extensions
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

    Ok(config_default().filter(|backend| !backend.trim().is_empty()))
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

    fn provider_with_backend(id: &str, backend: &str) -> AgentTaskExecutorProvider {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "backend": backend,
        }))
        .expect("valid provider fixture")
    }

    #[test]
    fn backends_are_sorted_deduped_and_skip_blanks() {
        let catalog = AgentTaskProviderCatalog {
            providers: vec![
                provider_with_backend("b.codex", "codex"),
                provider_with_backend("a.pi", "pi"),
                // Two providers for one backend must not double-list it.
                provider_with_backend("c.codex", "codex"),
                provider_with_backend("d.claude", "claude-code"),
                provider_with_backend("e.blank", "   "),
            ],
            ..Default::default()
        };

        assert_eq!(
            catalog.backends(),
            vec![
                "claude-code".to_string(),
                "codex".to_string(),
                "pi".to_string()
            ]
        );
    }

    #[test]
    fn backends_of_an_empty_catalog_is_empty() {
        assert!(AgentTaskProviderCatalog::default().backends().is_empty());
    }
}
