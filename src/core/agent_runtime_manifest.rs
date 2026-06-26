use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::core::agent_task_provider::{
    AgentTaskExecutorProvider, AgentTaskProviderRunnerReadiness, AgentTaskProviderRunnerSource,
    AgentTaskProviderWorkspaceMaterialization,
};
use crate::core::extension::{load_all_extensions, load_extension, ExtensionManifest};
use crate::core::{config, paths, Error, Result};

pub const AGENT_RUNTIME_MANIFEST_SCHEMA: &str = "homeboy/agent-runtime-manifest/v1";
pub const AGENT_RUNTIME_MATERIALIZATION_PLAN_SCHEMA: &str =
    "homeboy/agent-runtime-materialization-plan/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeDiscoveryDiagnostic {
    pub class: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeDiscoveryCatalog {
    pub manifests: Vec<AgentRuntimeManifest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentRuntimeDiscoveryDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeManifest {
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_task_executors: Vec<AgentTaskExecutorProvider>,
    #[serde(
        default,
        skip_serializing_if = "AgentRuntimeMaterializationContract::is_empty"
    )]
    pub materialization: AgentRuntimeMaterializationContract,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeMaterializationContract {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_roots: Vec<AgentTaskProviderRunnerSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<AgentRuntimeMaterializationDependency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executable_requirements: Vec<AgentRuntimeExecutableRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readiness_checks: Vec<AgentTaskProviderRunnerReadiness>,
    #[serde(
        default,
        skip_serializing_if = "AgentRuntimeDiagnosticsContract::is_empty"
    )]
    pub diagnostics: AgentRuntimeDiagnosticsContract,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_passthrough: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<AgentTaskProviderWorkspaceMaterialization>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl AgentRuntimeMaterializationContract {
    fn is_empty(&self) -> bool {
        self.source_roots.is_empty()
            && self.dependencies.is_empty()
            && self.executable_requirements.is_empty()
            && self.readiness_checks.is_empty()
            && self.diagnostics.is_empty()
            && self.env_passthrough.is_empty()
            && self.workspace.is_none()
            && self.extra.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeDiagnosticsContract {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<AgentRuntimeToolDiagnosticDeclaration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtimes: Vec<AgentRuntimeRuntimeDiagnosticDeclaration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub followups: Vec<AgentRuntimeDiagnosticFollowup>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl AgentRuntimeDiagnosticsContract {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
            && self.runtimes.is_empty()
            && self.followups.is_empty()
            && self.extra.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeToolDiagnosticDeclaration {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_output: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configured_binary_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_dir_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_install_dir: Option<String>,
    pub managed_cache_source: String,
    pub managed_cache_binary: String,
    pub effective_binary_rule: String,
    pub diagnostic_script: String,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeRuntimeDiagnosticDeclaration {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_output: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configured_binary_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_dir_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_install_dir: Option<String>,
    pub managed_cache_source: String,
    pub managed_cache_binary: String,
    pub effective_binary_rule: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<AgentRuntimePackageDiagnosticDeclaration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<AgentRuntimeProbeDiagnosticDeclaration>,
    pub runtime_probe_script: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_consistency: Vec<AgentRuntimeSourceConsistencyDiagnostic>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimePackageDiagnosticDeclaration {
    pub field: String,
    pub package: String,
    pub expected_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeProbeDiagnosticDeclaration {
    pub field: String,
    #[serde(default = "default_runtime_probe_source")]
    pub source: String,
}

fn default_runtime_probe_source() -> String {
    "runtime_probe_command".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeSourceConsistencyDiagnostic {
    pub id: String,
    pub severity: String,
    pub path: String,
    pub root: String,
    pub message: String,
    pub remediation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeDiagnosticFollowup {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_output: Option<String>,
    pub command_script: String,
    pub purpose: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeMaterializationDependency {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeExecutableRequirement {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeMaterializationPlan {
    pub schema: String,
    pub runtime_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_roots: Vec<AgentTaskProviderRunnerSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<AgentRuntimeMaterializationDependency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executable_requirements: Vec<AgentRuntimeExecutableRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readiness_checks: Vec<AgentTaskProviderRunnerReadiness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_passthrough: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<AgentTaskProviderWorkspaceMaterialization>,
}

pub(crate) fn runtime_materialization_plan(
    manifest: &AgentRuntimeManifest,
) -> AgentRuntimeMaterializationPlan {
    let mut env_passthrough = manifest.materialization.env_passthrough.clone();
    env_passthrough.sort();
    env_passthrough.dedup();

    AgentRuntimeMaterializationPlan {
        schema: AGENT_RUNTIME_MATERIALIZATION_PLAN_SCHEMA.to_string(),
        runtime_id: manifest.id.clone(),
        runtime_path: manifest.runtime_path.clone(),
        source_roots: manifest.materialization.source_roots.clone(),
        dependencies: manifest.materialization.dependencies.clone(),
        executable_requirements: manifest.materialization.executable_requirements.clone(),
        readiness_checks: manifest.materialization.readiness_checks.clone(),
        env_passthrough,
        workspace: manifest.materialization.workspace.clone(),
    }
}

pub(crate) fn discover_agent_runtime_catalog() -> AgentRuntimeDiscoveryCatalog {
    let standalone = discover_standalone_agent_runtime_catalog();
    let extensions = load_all_extensions().unwrap_or_default();
    let extension = discover_agent_runtime_catalog_from_extensions(&extensions);
    let mut diagnostics = standalone.diagnostics;
    diagnostics.extend(extension.diagnostics);
    AgentRuntimeDiscoveryCatalog {
        manifests: merge_agent_runtime_manifests(standalone.manifests, extension.manifests),
        diagnostics,
    }
}

fn merge_agent_runtime_manifests(
    standalone_manifests: Vec<AgentRuntimeManifest>,
    extension_manifests: Vec<AgentRuntimeManifest>,
) -> Vec<AgentRuntimeManifest> {
    let extension_runtime_ids: BTreeSet<String> = extension_manifests
        .iter()
        .map(|manifest| manifest.id.clone())
        .collect();
    let mut manifests = extension_manifests;
    manifests.extend(
        standalone_manifests
            .into_iter()
            .filter(|manifest| !extension_runtime_ids.contains(manifest.id.as_str())),
    );
    manifests
}

fn discover_standalone_agent_runtime_catalog() -> AgentRuntimeDiscoveryCatalog {
    let mut manifests = Vec::new();
    let mut diagnostics = Vec::new();
    if let Ok(runtime_dir) = paths::agent_runtimes() {
        let catalog = discover_standalone_agent_runtime_catalog_in(
            runtime_dir,
            paths::agent_runtime_manifest,
        );
        manifests.extend(catalog.manifests);
        diagnostics.extend(catalog.diagnostics);
    }
    manifests.sort_by(|a, b| a.id.cmp(&b.id));
    AgentRuntimeDiscoveryCatalog {
        manifests,
        diagnostics,
    }
}

fn discover_standalone_agent_runtime_catalog_in(
    runtime_dir: PathBuf,
    manifest_path_for: fn(&str) -> crate::core::Result<PathBuf>,
) -> AgentRuntimeDiscoveryCatalog {
    let Ok(entries) = std::fs::read_dir(runtime_dir) else {
        return AgentRuntimeDiscoveryCatalog::default();
    };

    let mut catalog = AgentRuntimeDiscoveryCatalog::default();
    for entry in entries.flatten() {
        match load_standalone_agent_runtime_manifest(&entry.path(), manifest_path_for) {
            StandaloneAgentRuntimeManifestLoad::Loaded(manifest) => {
                catalog.manifests.push(manifest)
            }
            StandaloneAgentRuntimeManifestLoad::Skipped => {}
            StandaloneAgentRuntimeManifestLoad::Invalid(diagnostic) => {
                catalog.diagnostics.push(diagnostic)
            }
        }
    }
    catalog
}

enum StandaloneAgentRuntimeManifestLoad {
    Loaded(AgentRuntimeManifest),
    Skipped,
    Invalid(AgentRuntimeDiscoveryDiagnostic),
}

fn load_standalone_agent_runtime_manifest(
    path: &Path,
    manifest_path_for: fn(&str) -> crate::core::Result<PathBuf>,
) -> StandaloneAgentRuntimeManifestLoad {
    if !path.is_dir() {
        return StandaloneAgentRuntimeManifestLoad::Skipped;
    }
    let Some(file_name) = path.file_name() else {
        return StandaloneAgentRuntimeManifestLoad::Skipped;
    };
    let id = file_name.to_string_lossy().to_string();
    let Ok(manifest_path) = manifest_path_for(&id) else {
        return StandaloneAgentRuntimeManifestLoad::Skipped;
    };
    if !manifest_path.exists() {
        return StandaloneAgentRuntimeManifestLoad::Skipped;
    }
    let content = match std::fs::read_to_string(&manifest_path) {
        Ok(content) => content,
        Err(error) => {
            return StandaloneAgentRuntimeManifestLoad::Invalid(AgentRuntimeDiscoveryDiagnostic {
                class: "agent_runtime_manifest.read_failed".to_string(),
                message: error.to_string(),
                runtime_id: Some(id),
                extension_id: None,
                path: Some(manifest_path.display().to_string()),
            })
        }
    };
    let mut manifest: AgentRuntimeManifest = match config::from_str(&content) {
        Ok(manifest) => manifest,
        Err(error) => {
            return StandaloneAgentRuntimeManifestLoad::Invalid(AgentRuntimeDiscoveryDiagnostic {
                class: "agent_runtime_manifest.parse_failed".to_string(),
                message: error.to_string(),
                runtime_id: Some(id),
                extension_id: None,
                path: Some(manifest_path.display().to_string()),
            })
        }
    };
    manifest.id = id;
    manifest.extension_id = None;
    manifest.extension_path = None;
    manifest.runtime_path = Some(path.to_string_lossy().to_string());
    StandaloneAgentRuntimeManifestLoad::Loaded(manifest)
}

pub(crate) fn discover_agent_runtime_catalog_from_extensions(
    extensions: &[ExtensionManifest],
) -> AgentRuntimeDiscoveryCatalog {
    let mut runtime_manifests = Vec::new();
    let mut diagnostics = Vec::new();
    for extension in extensions {
        for runtime in &extension.agent_runtimes {
            let provider_catalog = parse_agent_task_executor_provider_catalog(
                &runtime.agent_task_executors,
                &runtime.id,
                Some(&extension.id),
                extension.extension_path.as_deref(),
            );
            diagnostics.extend(provider_catalog.diagnostics);
            let providers = provider_catalog.providers;
            if providers.is_empty() {
                continue;
            }
            runtime_manifests.push(AgentRuntimeManifest {
                schema: AGENT_RUNTIME_MANIFEST_SCHEMA.to_string(),
                id: runtime.id.clone(),
                label: runtime.label.clone(),
                agent_task_executors: providers,
                materialization: serde_json::from_value(
                    runtime
                        .extra
                        .get("materialization")
                        .cloned()
                        .unwrap_or(Value::Null),
                )
                .unwrap_or_default(),
                extension_id: Some(extension.id.clone()),
                extension_path: extension.extension_path.clone(),
                runtime_path: extension.extension_path.clone(),
                extra: runtime
                    .extra
                    .clone()
                    .into_iter()
                    .filter(|(key, _)| key != "materialization")
                    .collect(),
            });
        }
    }
    AgentRuntimeDiscoveryCatalog {
        manifests: runtime_manifests,
        diagnostics,
    }
}

pub(crate) fn discover_agent_task_executor_providers() -> Vec<AgentTaskExecutorProvider> {
    discover_agent_task_executor_provider_catalog().providers
}

pub(crate) fn discover_agent_task_executor_provider_catalog(
) -> AgentTaskExecutorProviderDiscoveryCatalog {
    let catalog = discover_agent_runtime_catalog();
    AgentTaskExecutorProviderDiscoveryCatalog {
        providers: agent_task_executor_providers_from_runtime_manifests(catalog.manifests),
        diagnostics: catalog.diagnostics,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskExecutorProviderDiscoveryCatalog {
    pub providers: Vec<AgentTaskExecutorProvider>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentRuntimeDiscoveryDiagnostic>,
}

fn agent_task_executor_providers_from_runtime_manifests(
    runtime_manifests: Vec<AgentRuntimeManifest>,
) -> Vec<AgentTaskExecutorProvider> {
    let mut providers = Vec::new();
    for runtime_manifest in runtime_manifests {
        let materialization_plan = runtime_materialization_plan(&runtime_manifest);
        for mut provider in runtime_manifest.agent_task_executors {
            provider.extension_id = runtime_manifest.extension_id.clone();
            provider.extension_path = runtime_manifest.extension_path.clone();
            provider.runtime_id = Some(runtime_manifest.id.clone());
            provider.runtime_path = runtime_manifest.runtime_path.clone();
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::core::agent_task::{AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA};
    use crate::core::agent_task_provider::AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA;

    fn extension(id: &str) -> ExtensionManifest {
        ExtensionManifest {
            id: id.to_string(),
            name: id.to_string(),
            version: "1.0.0".to_string(),
            provides: None,
            scripts: None,
            icon: None,
            description: None,
            author: None,
            homepage: None,
            source_url: None,
            deploy: None,
            audit: None,
            executable: None,
            platform: None,
            component_env: None,
            env_provider: None,
            ci: None,
            runtime: None,
            cli: None,
            build: None,
            deps: None,
            lint: None,
            test: None,
            bench: None,
            fuzz: None,
            trace: None,
            autofix_verify: None,
            structured_sidecars: Default::default(),
            release_preflights: Vec::new(),
            agent_runtimes: Vec::new(),
            agent_task: None,
            actions: Vec::new(),
            hooks: Default::default(),
            settings: Vec::new(),
            requires: None,
            extra: HashMap::new(),
            extension_path: Some(format!("/extensions/{id}")),
        }
    }

    fn provider_json(id: &str, backend: &str) -> Value {
        json!({
            "schema": AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
            "id": id,
            "backend": backend,
            "command": "agent-task-provider",
            "request_schema": AGENT_TASK_REQUEST_SCHEMA,
            "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA
        })
    }

    #[test]
    fn discovers_first_class_agent_runtime_manifests_from_extensions() {
        let mut extension = extension("runtime-extension");
        extension.agent_runtimes.push(
            serde_json::from_value(json!({
                "id": "example-runtime",
                "label": "Example Runtime",
                "materialization": {
                    "source_roots": [{
                        "id": "example-runtime-source",
                        "label": "Example Runtime Source",
                        "path": "~/.cache/homeboy/example-runtime",
                        "remote_url": "https://example.com/runtime.git",
                        "git_ref": "main"
                    }],
                    "dependencies": [{
                        "id": "example-runtime-package",
                        "source_root": "example-runtime-source",
                        "requirement": "runtime package checkout"
                    }],
                    "executable_requirements": [{
                        "id": "example-runtime-cli",
                        "env": ["EXAMPLE_RUNTIME_BIN"],
                        "candidates": ["example-runtime"],
                        "version_command": ["--version"]
                    }],
                    "readiness_checks": [{
                        "id": "example-runtime.ready",
                        "label": "Example Runtime Ready",
                        "secret_env": ["EXAMPLE_RUNTIME_TOKEN"]
                    }],
                    "diagnostics": {
                        "tools": [{
                            "tool": "example-runtime",
                            "legacy_output": "example_runtime",
                            "configured_binary_env": ["EXAMPLE_RUNTIME_BIN"],
                            "install_dir_env": "EXAMPLE_RUNTIME_INSTALL_DIR",
                            "default_install_dir": "${HOME}/.cache/homeboy/example-runtime",
                            "managed_cache_source": "${install_dir}/source",
                            "managed_cache_binary": "${managed_cache_source}/bin/example-runtime",
                            "effective_binary_rule": "managed cache binary wins",
                            "diagnostic_script": "printf example-runtime"
                        }],
                        "followups": [{
                            "label": "example_runtime_binary",
                            "legacy_output": "managed_followups",
                            "command_script": "printf example-runtime",
                            "purpose": "Inspect the declared runtime binary."
                        }]
                    },
                    "env_passthrough": ["EXAMPLE_RUNTIME_BIN", "EXAMPLE_RUNTIME_TOKEN", "EXAMPLE_RUNTIME_BIN"],
                    "workspace": {
                        "cwd": "git_checkout",
                        "requires_git": true
                    }
                },
                "runtime_metadata": { "owner": "extension" },
                "agent_task_executors": [provider_json("example.default", "example")]
            }))
            .expect("runtime manifest"),
        );

        let manifests = discover_agent_runtime_catalog_from_extensions(&[extension]).manifests;

        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].schema, AGENT_RUNTIME_MANIFEST_SCHEMA);
        assert_eq!(manifests[0].id, "example-runtime");
        assert_eq!(
            manifests[0].extension_id.as_deref(),
            Some("runtime-extension")
        );
        assert_eq!(manifests[0].agent_task_executors[0].backend, "example");
        assert!(!manifests[0].extra.contains_key("materialization"));
        let materialization_plan = runtime_materialization_plan(&manifests[0]);
        assert_eq!(
            materialization_plan.schema,
            AGENT_RUNTIME_MATERIALIZATION_PLAN_SCHEMA
        );
        assert_eq!(materialization_plan.runtime_id, "example-runtime");
        assert_eq!(
            materialization_plan.source_roots[0].remote_url.as_deref(),
            Some("https://example.com/runtime.git")
        );
        assert_eq!(
            materialization_plan.executable_requirements[0].candidates,
            vec!["example-runtime".to_string()]
        );
        assert_eq!(
            materialization_plan.env_passthrough,
            vec![
                "EXAMPLE_RUNTIME_BIN".to_string(),
                "EXAMPLE_RUNTIME_TOKEN".to_string()
            ]
        );
        assert_eq!(
            manifests[0].materialization.diagnostics.tools[0].tool,
            "example-runtime"
        );
        assert_eq!(
            manifests[0].materialization.diagnostics.followups[0].label,
            "example_runtime_binary"
        );
        assert_eq!(
            serde_json::to_value(&manifests[0]).expect("runtime export")["materialization"]
                ["diagnostics"]["tools"][0]["managed_cache_binary"],
            "${managed_cache_source}/bin/example-runtime"
        );
        assert_eq!(
            materialization_plan
                .workspace
                .as_ref()
                .expect("workspace")
                .requires_git,
            Some(true)
        );
        assert_eq!(
            manifests[0].runtime_path.as_deref(),
            Some("/extensions/runtime-extension")
        );
        assert_eq!(manifests[0].extra["runtime_metadata"]["owner"], "extension");
        assert_eq!(
            serde_json::to_value(&manifests[0]).expect("runtime export")["runtime_metadata"]
                ["owner"],
            "extension"
        );
    }

    #[test]
    fn discovers_standalone_agent_runtime_manifests() {
        crate::test_support::with_isolated_home(|home| {
            let runtime_dir = home
                .path()
                .join(".config/homeboy/agent-runtimes")
                .join("standalone-example");
            std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
            std::fs::write(
                runtime_dir.join("standalone-example.json"),
                json!({
                    "schema": AGENT_RUNTIME_MANIFEST_SCHEMA,
                    "id": "ignored-on-disk",
                    "name": "Standalone Example Package",
                    "version": "1.0.0",
                    "description": "Standalone runtime package fixture.",
                    "runtime_metadata": { "owner": "standalone" },
                    "label": "Standalone Example",
                    "materialization": {
                        "env_passthrough": ["STANDALONE_RUNTIME_HOME"]
                    },
                    "agent_task_executors": [{
                        "schema": AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
                        "id": "standalone-example.default",
                        "backend": "example",
                        "command": "agent-task-provider",
                        "request_schema": AGENT_TASK_REQUEST_SCHEMA,
                        "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA,
                        "workspace_materialization": {
                            "cwd": "git_checkout",
                            "requires_git": true,
                            "write_scope": "artifacts",
                            "artifact_paths": [".homeboy/example"]
                        },
                        "secret_requirements": [{
                            "name": "EXAMPLE_RUNTIME_TOKEN",
                            "required": true,
                            "purpose": "fixture"
                        }],
                        "secret_env_requirements": [{
                            "schema": "homeboy/secret-env-requirement/v1",
                            "env": ["EXAMPLE_RUNTIME_REFRESH_TOKEN"],
                            "when": { "path": "executor.config.provider", "equals": "example-provider" }
                        }],
                        "provider_defaults": {
                            "example-provider": { "secret_env": ["EXAMPLE_RUNTIME_REFRESH_TOKEN"] }
                        }
                    }]
                })
                .to_string(),
            )
            .expect("runtime manifest");

            let manifests = discover_standalone_agent_runtime_catalog().manifests;

            assert_eq!(manifests.len(), 1);
            assert_eq!(manifests[0].id, "standalone-example");
            assert_eq!(manifests[0].extension_id, None);
            assert_eq!(manifests[0].extension_path, None);
            assert_eq!(
                manifests[0].runtime_path.as_deref(),
                Some(runtime_dir.to_str().expect("runtime dir utf-8"))
            );
            assert_eq!(manifests[0].extra["name"], "Standalone Example Package");
            assert_eq!(manifests[0].extra["version"], "1.0.0");
            assert_eq!(
                manifests[0].extra["runtime_metadata"]["owner"],
                "standalone"
            );
            let exported = serde_json::to_value(&manifests[0]).expect("runtime export");
            assert_eq!(exported["name"], "Standalone Example Package");
            assert_eq!(exported["version"], "1.0.0");
            assert_eq!(exported["runtime_metadata"]["owner"], "standalone");
            assert_eq!(manifests[0].agent_task_executors[0].backend, "example");
            let provider = &manifests[0].agent_task_executors[0];
            let materialization = provider
                .workspace_materialization
                .as_ref()
                .expect("workspace materialization");
            assert_eq!(materialization.requires_git, Some(true));
            assert_eq!(materialization.write_scope.as_deref(), Some("artifacts"));
            assert_eq!(materialization.artifact_paths, vec![".homeboy/example"]);
            assert_eq!(
                provider.secret_requirements[0].name.as_deref(),
                Some("EXAMPLE_RUNTIME_TOKEN")
            );
            assert_eq!(
                provider.secret_env_requirements[0].env,
                vec!["EXAMPLE_RUNTIME_REFRESH_TOKEN"]
            );
            assert_eq!(
                provider.provider_defaults["example-provider"]["secret_env"][0],
                "EXAMPLE_RUNTIME_REFRESH_TOKEN"
            );
            let materialization_plan = runtime_materialization_plan(&manifests[0]);
            assert_eq!(
                materialization_plan.env_passthrough,
                vec!["STANDALONE_RUNTIME_HOME".to_string()]
            );
        });
    }

    #[test]
    fn standalone_runtime_catalog_reports_invalid_manifests() {
        crate::test_support::with_isolated_home(|home| {
            let runtime_dir = home
                .path()
                .join(".config/homeboy/agent-runtimes")
                .join("broken-runtime");
            std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
            std::fs::write(runtime_dir.join("broken-runtime.json"), "{not json")
                .expect("runtime manifest");

            let catalog = discover_standalone_agent_runtime_catalog();

            assert!(catalog.manifests.is_empty());
            assert_eq!(catalog.diagnostics.len(), 1);
            assert_eq!(
                catalog.diagnostics[0].class,
                "agent_runtime_manifest.parse_failed"
            );
            assert_eq!(
                catalog.diagnostics[0].runtime_id.as_deref(),
                Some("broken-runtime")
            );
        });
    }

    #[test]
    fn extension_runtime_catalog_reports_invalid_provider_entries() {
        let mut extension = extension("runtime-extension");
        extension.agent_runtimes.push(
            serde_json::from_value(json!({
                "id": "example-runtime",
                "agent_task_executors": [{
                    "schema": AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
                    "id": "missing-backend"
                }]
            }))
            .expect("runtime manifest"),
        );

        let catalog = discover_agent_runtime_catalog_from_extensions(&[extension]);

        assert!(catalog.manifests.is_empty());
        assert_eq!(catalog.diagnostics.len(), 1);
        assert_eq!(
            catalog.diagnostics[0].class,
            "agent_task_executor_provider.parse_failed"
        );
        assert_eq!(
            catalog.diagnostics[0].runtime_id.as_deref(),
            Some("example-runtime")
        );
        assert_eq!(
            catalog.diagnostics[0].extension_id.as_deref(),
            Some("runtime-extension")
        );
    }

    #[test]
    fn extension_agent_runtime_wins_over_same_id_standalone_cache() {
        crate::test_support::with_isolated_home(|home| {
            let runtime_dir = home
                .path()
                .join(".config/homeboy/agent-runtimes/sample-runtime");
            std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
            std::fs::write(
                runtime_dir.join("sample-runtime.json"),
                json!({
                    "schema": AGENT_RUNTIME_MANIFEST_SCHEMA,
                    "id": "ignored-on-disk",
                    "label": "Stale cached sample runtime",
                    "agent_task_executors": [provider_json("sample.default", "sample")],
                    "runtime_metadata": { "origin": "stale-cache" }
                })
                .to_string(),
            )
            .expect("runtime manifest");

            let standalone = discover_standalone_agent_runtime_catalog().manifests;
            let mut extension = extension("sample-runtime-extension");
            extension.agent_runtimes.push(
                serde_json::from_value(json!({
                    "id": "sample-runtime",
                    "label": "Extension sample runtime",
                    "agent_task_executors": [{
                        "schema": AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
                        "id": "sample.default",
                        "backend": "sample",
                        "command": "fresh-extension-executor",
                        "request_schema": AGENT_TASK_REQUEST_SCHEMA,
                        "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA
                    }],
                    "runtime_metadata": { "origin": "extension" }
                }))
                .expect("extension runtime"),
            );
            let extension_manifests =
                discover_agent_runtime_catalog_from_extensions(&[extension]).manifests;

            let merged = merge_agent_runtime_manifests(standalone, extension_manifests);
            assert_eq!(merged.len(), 1);
            assert_eq!(merged[0].id, "sample-runtime");
            assert_eq!(
                merged[0].extension_id.as_deref(),
                Some("sample-runtime-extension")
            );
            assert_eq!(merged[0].extra["runtime_metadata"]["origin"], "extension");

            let providers = agent_task_executor_providers_from_runtime_manifests(merged);
            assert_eq!(providers.len(), 1);
            assert_eq!(providers[0].id, "sample.default");
            assert_eq!(providers[0].runtime_id.as_deref(), Some("sample-runtime"));
            assert_eq!(
                providers[0].extension_id.as_deref(),
                Some("sample-runtime-extension")
            );
            assert_eq!(providers[0].command, "fresh-extension-executor");
            assert_eq!(
                providers[0].runtime_path.as_deref(),
                Some("/extensions/sample-runtime-extension")
            );
        });
    }

    #[test]
    fn sample_runtime_materialization_fixture_is_plain_data() {
        let manifest: AgentRuntimeManifest = serde_json::from_str(include_str!(
            "../../tests/fixtures/sample_runtime_materialization_manifest.json"
        ))
        .expect("sample runtime fixture parses");

        let materialization_plan = runtime_materialization_plan(&manifest);

        assert_eq!(manifest.id, "sample-runtime");
        assert_eq!(materialization_plan.runtime_id, "sample-runtime");
        assert_eq!(materialization_plan.source_roots[0].id, "sample-runtime");
        assert_eq!(
            materialization_plan.source_roots[0].remote_url.as_deref(),
            Some("https://github.com/example-org/sample-runtime.git")
        );
        assert_eq!(
            materialization_plan.executable_requirements[0].env,
            vec!["SAMPLE_RUNTIME_BIN".to_string()]
        );
        assert_eq!(
            materialization_plan.readiness_checks[0].label,
            "Sample Runtime available"
        );
        assert_eq!(
            materialization_plan.env_passthrough,
            vec![
                "EXAMPLE_PROVIDER_API_KEY".to_string(),
                "SAMPLE_RUNTIME_BIN".to_string(),
                "SAMPLE_RUNTIME_HOME".to_string()
            ]
        );
    }
}
