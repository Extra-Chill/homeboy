//! Agent-task executor-provider discovery.
//!
//! Reads the (opaque) executor-provider declarations off core's agent-runtime
//! manifests, deserializes them into typed providers, validates + normalizes
//! them, and validates that an installed extension's declared providers are
//! discoverable. Moved out of core (which now carries the executor providers
//! opaquely) into the agents crate.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use homeboy_core::agent_runtime_manifest::{
    discover_agent_runtime_catalog, runtime_materialization_plan, AgentRuntimeDiscoveryDiagnostic,
    AgentRuntimeManifest, AgentRuntimeSelectedIdentity, AGENT_RUNTIME_REVISION_PROBE_TIMED_OUT,
    AGENT_RUNTIME_REVISION_PROBE_TIMEOUT,
};
use homeboy_core::command_invocation::COMMAND_INVOCATION_SCHEMA;
use homeboy_core::{Error, Result};
use homeboy_extension::{load_extension, ExtensionManifest};

use super::{
    AgentTaskExecutorProvider, AgentTaskProviderRunnerSource, AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
};

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

fn agent_task_executor_providers_from_runtime_manifests(
    runtime_manifests: Vec<AgentRuntimeManifest>,
    diagnostics: &mut Vec<AgentRuntimeDiscoveryDiagnostic>,
) -> Vec<AgentTaskExecutorProvider> {
    let mut providers = Vec::new();
    for runtime_manifest in runtime_manifests {
        // One runtime yields one probe outcome regardless of how many providers
        // it declares, so the partial is reported once per runtime.
        let mut reported_revision_probe_timeout = false;
        for provider_value in runtime_manifest.agent_task_executors.clone() {
            let Ok(mut provider) =
                serde_json::from_value::<AgentTaskExecutorProvider>(provider_value)
            else {
                continue;
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
    let extension = load_extension(extension_id)?;
    let expected = expected_agent_runtime_provider_refs(&extension)?;
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

fn expected_agent_runtime_provider_refs(
    extension: &ExtensionManifest,
) -> Result<Vec<ExpectedAgentRuntimeProviderRef>> {
    let mut expected = Vec::new();
    for runtime in &extension.agent_runtimes {
        for value in &runtime.agent_task_executors {
            let provider: AgentTaskExecutorProvider = serde_json::from_value(value.clone()).map_err(|err| {
                Error::validation_invalid_argument(
                    "agent_runtimes.agent_task_executors",
                    format!(
                        "Extension '{}' declares an agent runtime provider that cannot be parsed: {}",
                        extension.id, err
                    ),
                    Some(runtime.id.clone()),
                    None,
                )
            })?;
            expected.push(ExpectedAgentRuntimeProviderRef {
                runtime_id: runtime.id.clone(),
                provider_id: provider.id,
                backend: provider.backend,
            });
        }
    }
    Ok(expected)
}

struct ParsedAgentTaskExecutorProviderCatalog {
    providers: Vec<AgentTaskExecutorProvider>,
    diagnostics: Vec<AgentRuntimeDiscoveryDiagnostic>,
}

fn parse_agent_task_executor_provider_catalog(
    values: &[Value],
    runtime_id: &str,
    extension_id: Option<&str>,
    path: Option<&str>,
) -> ParsedAgentTaskExecutorProviderCatalog {
    let mut providers = Vec::new();
    let mut diagnostics = Vec::new();
    for value in values {
        match serde_json::from_value(value.clone()) {
            Ok(provider) => providers.push(provider),
            Err(error) => diagnostics.push(AgentRuntimeDiscoveryDiagnostic {
                class: "agent_task_executor_provider.parse_failed".to_string(),
                message: error.to_string(),
                runtime_id: Some(runtime_id.to_string()),
                extension_id: extension_id.map(str::to_string),
                path: path.map(str::to_string),
            }),
        }
    }
    ParsedAgentTaskExecutorProviderCatalog {
        providers,
        diagnostics,
    }
}

/// Agent-task implementation of core's extension provider-discovery validator.
struct ExtensionProviderDiscoveryValidatorImpl;

impl homeboy_core::extension_provider_discovery::ExtensionProviderDiscoveryValidator
    for ExtensionProviderDiscoveryValidatorImpl
{
    fn validate_installed_extension_provider_discovery(&self, extension_id: &str) -> Result<()> {
        validate_installed_extension_agent_runtime_provider_discovery(extension_id)
    }
}

/// Register the extension provider-discovery validator so core's extension
/// install/repair can verify declared agent-runtime providers are discoverable
/// without depending on the agent-task subsystem.
pub fn register() {
    homeboy_core::extension_provider_discovery::register_extension_provider_discovery_validator(
        Box::new(ExtensionProviderDiscoveryValidatorImpl),
    );
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
}
