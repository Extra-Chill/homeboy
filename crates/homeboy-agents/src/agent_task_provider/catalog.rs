use super::resolution::provider_requires_cwd_git_checkout_with_providers;
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
    pub(super) evidence: Arc<Mutex<ProviderEvidenceStore>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderCatalog {
    pub providers: Vec<AgentTaskExecutorProvider>,
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
    let providers = super::discovery::discover_agent_task_executor_providers();
    let version = provider_catalog_version(&providers);
    AgentTaskProviderCatalog {
        providers,
        version: Some(version),
    }
}

fn provider_catalog_version(providers: &[AgentTaskExecutorProvider]) -> String {
    // The handoff version represents the selected catalog content, not the
    // instant discovery happened. That makes stale controller/Lab catalogs
    // detectable without treating an unchanged rediscovery as drift.
    let content =
        serde_json::to_vec(providers).expect("agent-task provider catalog must serialize");
    format!("resolved:{}", content_hash::sha256_hex(&content))
}

impl ExtensionProviderAgentTaskExecutor {
    pub fn discover() -> Self {
        Self::from_catalog(AgentTaskProviderCatalog::discover())
    }

    pub fn from_catalog(catalog: AgentTaskProviderCatalog) -> Self {
        Self {
            providers: catalog.providers,
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
            evidence: Arc::clone(&self.evidence),
        }
    }

    pub fn providers(&self) -> &[AgentTaskExecutorProvider] {
        &self.providers
    }
}

pub fn default_backend() -> Option<String> {
    defaults::load_config()
        .agent_task
        .default_backend
        .filter(|backend| !backend.trim().is_empty())
}

pub(super) fn provider_not_found_message(backend: &str) -> String {
    let mut base = format!(
        "no extension agent-task provider found for backend '{backend}' (no installed provider can serve this backend)"
    );
    if backend.trim() != crate::agent_task_builtin_backend::BUILTIN_DEFAULT_BACKEND {
        base.push_str("; ");
        base.push_str(&crate::agent_task_builtin_backend::builtin_backend_remediation(backend));
    }
    base
}

pub fn provider_requires_cwd_git_checkout(backend: &str, selector: Option<&str>) -> bool {
    AgentTaskProviderCatalog::discover().provider_requires_cwd_git_checkout(backend, selector)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_version_is_deterministic_and_detects_stale_content() {
        let empty = provider_catalog_version(&[]);
        assert_eq!(empty, provider_catalog_version(&[]));

        let changed = provider_catalog_version(&[provider_with_backend("pi", "pi")]);

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
