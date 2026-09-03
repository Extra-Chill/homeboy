//! Agent-task executor-provider discovery.
//!
//! Reads the (opaque) executor-provider declarations off core's agent-runtime
//! manifests, deserializes them into typed providers, validates + normalizes
//! them, and validates that an installed extension's declared providers are
//! discoverable. Moved out of core (which now carries the executor providers
//! opaquely) into the agents crate.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use homeboy_core::agent_runtime_manifest::{
    discover_agent_runtime_catalog, runtime_materialization_plan, AgentRuntimeDiscoveryDiagnostic,
    AgentRuntimeManifest, AgentRuntimeSelectedIdentity, AGENT_RUNTIME_REVISION_PROBE_TIMED_OUT,
    AGENT_RUNTIME_REVISION_PROBE_TIMEOUT,
};
use homeboy_core::command_invocation::COMMAND_INVOCATION_SCHEMA;
use homeboy_core::{Error, Result};
use homeboy_extension_contract::api::v1::{
    ExtensionApiAgentTaskExecutorInventoryRequest,
    EXTENSION_API_AGENT_TASK_EXECUTOR_INVENTORY_REQUEST_SCHEMA, EXTENSION_API_V1,
};

use super::AgentTaskExecutorProvider;

pub(crate) fn discover_agent_task_executor_providers() -> Vec<AgentTaskExecutorProvider> {
    discover_agent_task_executor_provider_catalog().providers
}

pub(crate) fn discover_agent_task_executor_provider_catalog(
) -> AgentTaskExecutorProviderDiscoveryCatalog {
    let catalog = discover_agent_runtime_catalog();
    let mut diagnostics = catalog.diagnostics;
    let discovered =
        agent_task_executor_providers_from_runtime_manifests(catalog.manifests, &mut diagnostics);
    let providers = reject_duplicate_provider_ids(discovered, &mut diagnostics);
    AgentTaskExecutorProviderDiscoveryCatalog {
        providers,
        diagnostics,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskExecutorProviderDiscoveryCatalog {
    pub providers: Vec<AgentTaskExecutorProvider>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentRuntimeDiscoveryDiagnostic>,
}

/// Diagnostic class raised when a runtime's bounded revision probe ran out of
/// budget. The provider stays in the catalog; the diagnostic is what makes the
/// result an explicitly labelled partial rather than a silent unknown (#9763).
pub const AGENT_RUNTIME_REVISION_PROBE_TIMEOUT_DIAGNOSTIC: &str =
    "agent_runtime_manifest.revision_probe_timeout";

/// Turn a timed-out runtime revision probe into a scoped discovery diagnostic.
///
/// The provider stays in the catalog: losing a revision must not lose the
/// provider. What the diagnostic adds is the distinction between "this runtime
/// has no revision" and "we could not find out inside the budget", so callers
/// read a labelled partial rather than a confident wrong answer (#9763).
fn revision_probe_timeout_diagnostic(
    runtime_manifest: &AgentRuntimeManifest,
    selected_identity: &AgentRuntimeSelectedIdentity,
) -> Option<AgentRuntimeDiscoveryDiagnostic> {
    if selected_identity.revision_probe.as_deref() != Some(AGENT_RUNTIME_REVISION_PROBE_TIMED_OUT) {
        return None;
    }
    Some(AgentRuntimeDiscoveryDiagnostic {
        class: AGENT_RUNTIME_REVISION_PROBE_TIMEOUT_DIAGNOSTIC.to_string(),
        message: format!(
            "runtime revision probe exceeded its {}s budget; this catalog is a partial and the runtime revision is unknown, not absent",
            AGENT_RUNTIME_REVISION_PROBE_TIMEOUT.as_secs()
        ),
        runtime_id: Some(runtime_manifest.id.clone()),
        extension_id: runtime_manifest.extension_id.clone(),
        path: runtime_manifest.runtime_path.clone(),
    })
}

pub(super) fn agent_task_executor_providers_from_runtime_manifests(
    runtime_manifests: Vec<AgentRuntimeManifest>,
    diagnostics: &mut Vec<AgentRuntimeDiscoveryDiagnostic>,
) -> Vec<AgentTaskExecutorProvider> {
    let mut providers = Vec::new();
    for runtime_manifest in runtime_manifests {
        // One runtime yields one probe outcome regardless of how many providers
        // it declares, so the partial is reported once per runtime.
        let mut reported_revision_probe_timeout = false;
        for provider_value in runtime_manifest.agent_task_executors.clone() {
            let mut provider =
                match serde_json::from_value::<AgentTaskExecutorProvider>(provider_value) {
                    Ok(provider) => provider,
                    Err(error) => {
                        diagnostics.push(AgentRuntimeDiscoveryDiagnostic {
                            class: "agent_task_executor_provider.invalid_declaration".to_string(),
                            message: format!(
                            "agent-task provider declaration is invalid and was not loaded: {error}"
                        ),
                            runtime_id: Some(runtime_manifest.id.clone()),
                            extension_id: runtime_manifest.extension_id.clone(),
                            path: runtime_manifest.runtime_path.clone(),
                        });
                        continue;
                    }
                };
            normalize_agent_task_executor_provider_invocation(&mut provider);
            provider.extension_id = runtime_manifest.extension_id.clone();
            provider.extension_path = runtime_manifest.extension_path.clone();
            if provider.runtime_package_source.is_none() {
                provider.runtime_package_source = runtime_manifest.extension_id.clone();
            }
            provider.runtime_id = Some(runtime_manifest.id.clone());
            provider.runtime_path = runtime_manifest.runtime_path.clone();
            let materialization_plan =
                runtime_materialization_plan(&runtime_manifest, &provider.id);
            if !reported_revision_probe_timeout {
                if let Some(diagnostic) = revision_probe_timeout_diagnostic(
                    &runtime_manifest,
                    &materialization_plan.selected_identity,
                ) {
                    diagnostics.push(diagnostic);
                    reported_revision_probe_timeout = true;
                }
            }
            if let Ok(value) = serde_json::to_value(&materialization_plan) {
                provider
                    .extra
                    .insert("runtime_materialization_plan".to_string(), value);
            }
            providers.push(provider);
        }
    }
    providers
}

fn reject_duplicate_provider_ids(
    providers: Vec<AgentTaskExecutorProvider>,
    diagnostics: &mut Vec<AgentRuntimeDiscoveryDiagnostic>,
) -> Vec<AgentTaskExecutorProvider> {
    let mut by_id = BTreeMap::<String, Vec<AgentTaskExecutorProvider>>::new();
    for provider in providers {
        by_id.entry(provider.id.clone()).or_default().push(provider);
    }

    by_id
        .into_iter()
        .filter_map(|(id, providers)| {
            if providers.len() == 1 {
                return providers.into_iter().next();
            }
            let sources = providers
                .iter()
                .map(|provider| {
                    format!(
                        "runtime:{} source:{}",
                        provider.runtime_id.as_deref().unwrap_or("<unknown>"),
                        provider
                            .extension_id
                            .as_deref()
                            .unwrap_or("standalone")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            diagnostics.push(AgentRuntimeDiscoveryDiagnostic {
                class: "agent_task_executor_provider.id_conflict".to_string(),
                message: format!(
                    "Agent-task provider id '{}' is declared by multiple sources: {}. Select one source explicitly before dispatching this provider.",
                    id, sources
                ),
                runtime_id: None,
                extension_id: None,
                path: None,
            });
            None
        })
        .collect()
}

fn normalize_agent_task_executor_provider_invocation(provider: &mut AgentTaskExecutorProvider) {
    if !provider.invocation.argv.is_empty()
        || !provider.command_argv.is_empty()
        || provider.command.trim().is_empty()
    {
        return;
    }

    provider.invocation.schema = Some(COMMAND_INVOCATION_SCHEMA.to_string());
    provider.invocation.argv = provider
        .command
        .split_whitespace()
        .map(str::to_string)
        .collect();
}

pub(crate) fn validate_installed_extension_agent_runtime_provider_discovery(
    extension_id: &str,
) -> Result<()> {
    let expected = expected_agent_runtime_provider_refs(extension_id)?;
    if expected.is_empty() {
        return Ok(());
    }

    let discovered = discover_agent_task_executor_providers();
    let missing: Vec<_> = expected
        .iter()
        .filter(|expected| {
            !discovered.iter().any(|provider| {
                provider.extension_id.as_deref() == Some(extension_id)
                    && provider.runtime_id.as_deref() == Some(expected.runtime_id.as_str())
                    && provider.id == expected.provider_id
                    && provider.backend == expected.backend
            })
        })
        .cloned()
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "source",
        format!(
            "Extension '{}' declares agent runtime providers that were not discoverable after install/setup",
            extension_id
        ),
        Some(extension_id.to_string()),
        None,
    )
    .with_hint(format!(
        "Missing provider discovery: {}",
        missing
            .iter()
            .map(|entry| format!(
                "runtime={} provider={} backend={}",
                entry.runtime_id, entry.provider_id, entry.backend
            ))
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpectedAgentRuntimeProviderRef {
    runtime_id: String,
    provider_id: String,
    backend: String,
}

/// Registration identities the extension advertises, read from the typed
/// Extension API inventory.
///
/// The inventory is the single place declarations are turned into identities, so
/// the install-time gate and ordinary discovery cannot disagree about what an
/// extension registers. A declaration that cannot be parsed is surfaced by the
/// inventory as an unusable entry, and is rejected here rather than silently
/// installed (#12206).
fn expected_agent_runtime_provider_refs(
    extension_id: &str,
) -> Result<Vec<ExpectedAgentRuntimeProviderRef>> {
    let inventory =
        homeboy_core::extension::agent_task_executor_api::AgentTaskExecutorApi::discover(
            &ExtensionApiAgentTaskExecutorInventoryRequest {
                schema: EXTENSION_API_AGENT_TASK_EXECUTOR_INVENTORY_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
            },
        );
    let mut expected = Vec::new();
    for executor in inventory.registered_by(extension_id) {
        if let Some(diagnostic) = executor.diagnostic.as_ref() {
            return Err(Error::validation_invalid_argument(
                "agent_runtimes.agent_task_executors",
                format!(
                    "Extension '{}' declares an agent runtime provider that cannot be registered: {}",
                    extension_id, diagnostic.message
                ),
                Some(executor.runtime_id.clone()),
                None,
            ));
        }
        expected.push(ExpectedAgentRuntimeProviderRef {
            runtime_id: executor.runtime_id.clone(),
            provider_id: executor.id.clone(),
            backend: executor.backend.clone(),
        });
    }
    Ok(expected)
}

/// Resolves an extension's registered executors against this host's agent-task
/// providers, so extension install, replace, and relink can reject a declaration
/// that registers cleanly but cannot actually be dispatched.
///
/// Pass this into a lifecycle mutation from a caller that has the agent-task
/// subsystem. A caller without it uses
/// `ExtensionLifecycleValidation::declaration_only`, which still enforces
/// registrability.
pub struct AgentTaskExecutorDiscovery;

impl homeboy_core::extension::registry::ExtensionExecutorDiscovery for AgentTaskExecutorDiscovery {
    fn validate_registered_executors(&self, extension_id: &str) -> Result<()> {
        validate_installed_extension_agent_runtime_provider_discovery(extension_id)
    }
}

#[cfg(test)]
mod revision_probe_tests {
    use super::*;

    fn manifest() -> AgentRuntimeManifest {
        serde_json::from_value(serde_json::json!({
            "schema": homeboy_core::agent_runtime_manifest::AGENT_RUNTIME_MANIFEST_SCHEMA,
            "id": "opencode",
            "extension_id": "homeboy-opencode",
            "runtime_path": "/runtimes/opencode",
        }))
        .expect("valid runtime manifest fixture")
    }

    #[test]
    fn a_timed_out_revision_probe_becomes_a_labelled_partial() {
        // The bounded probe must degrade, not hang and not lie: the provider
        // stays discoverable and the catalog says the revision is unknown
        // rather than absent (#9763).
        let identity = AgentRuntimeSelectedIdentity {
            revision_probe: Some(AGENT_RUNTIME_REVISION_PROBE_TIMED_OUT.to_string()),
            ..Default::default()
        };

        let diagnostic = revision_probe_timeout_diagnostic(&manifest(), &identity)
            .expect("a timed-out probe must be reported");

        assert_eq!(
            diagnostic.class,
            AGENT_RUNTIME_REVISION_PROBE_TIMEOUT_DIAGNOSTIC
        );
        assert_eq!(diagnostic.runtime_id.as_deref(), Some("opencode"));
        assert_eq!(diagnostic.extension_id.as_deref(), Some("homeboy-opencode"));
        assert_eq!(diagnostic.path.as_deref(), Some("/runtimes/opencode"));
        assert!(diagnostic.message.contains("unknown, not absent"));
    }

    #[test]
    fn an_in_budget_probe_adds_no_diagnostic() {
        let identity = AgentRuntimeSelectedIdentity::default();

        assert!(revision_probe_timeout_diagnostic(&manifest(), &identity).is_none());
    }

    #[test]
    fn invalid_readiness_timeout_is_a_scoped_discovery_diagnostic() {
        let mut runtime = manifest();
        runtime.agent_task_executors = vec![serde_json::json!({
            "id": "opencode.agent-task-executor",
            "backend": "opencode",
            "readiness_invocation": { "argv": ["opencode-provider"], "timeout_ms": 120001 }
        })];
        let mut diagnostics = Vec::new();

        let providers =
            agent_task_executor_providers_from_runtime_manifests(vec![runtime], &mut diagnostics);

        assert!(providers.is_empty());
        assert_eq!(
            diagnostics[0].class,
            "agent_task_executor_provider.invalid_declaration"
        );
        assert!(diagnostics[0]
            .message
            .contains("readiness_invocation.timeout_ms"));
        assert_eq!(diagnostics[0].runtime_id.as_deref(), Some("opencode"));
    }
}
