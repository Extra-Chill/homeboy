use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::command_invocation::COMMAND_INVOCATION_SCHEMA;
use crate::extension_store::{load_all_extensions, load_extension};
use crate::{config, paths, Error, Result};
use homeboy_extension_contract::{ExtensionManifest, RequirementsConfig};

pub const AGENT_RUNTIME_MANIFEST_SCHEMA: &str = "homeboy/agent-runtime-manifest/v1";
pub const AGENT_RUNTIME_MATERIALIZATION_PLAN_SCHEMA: &str =
    "homeboy/agent-runtime-materialization-plan/v2";

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
    /// Agent-task executor providers declared by this runtime, carried opaquely
    /// as JSON so core does not depend on the agent-task provider types. The
    /// agent-task layer deserializes and validates them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_task_executors: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_path_fields: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "AgentRuntimeMaterializationContract::is_empty"
    )]
    pub materialization: AgentRuntimeMaterializationContract,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<RequirementsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeMaterializationContract {
    /// Runtime-local sources are copied into a job-scoped, content-addressed
    /// generation. `source_roots` remains the legacy runner-global contract.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_sources: Vec<AgentRuntimeMaterializationSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preparation: Vec<AgentRuntimePreparationAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_roots: Vec<homeboy_agents_contract::AgentTaskProviderRunnerSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<AgentRuntimeMaterializationDependency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executable_requirements: Vec<AgentRuntimeExecutableRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readiness_checks: Vec<Value>,
    #[serde(
        default,
        skip_serializing_if = "AgentRuntimeDiagnosticsContract::is_empty"
    )]
    pub diagnostics: AgentRuntimeDiagnosticsContract,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_passthrough: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Value>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl AgentRuntimeMaterializationContract {
    fn is_empty(&self) -> bool {
        self.source_roots.is_empty()
            && self.runtime_sources.is_empty()
            && self.preparation.is_empty()
            && self.dependencies.is_empty()
            && self.executable_requirements.is_empty()
            && self.readiness_checks.is_empty()
            && self.diagnostics.is_empty()
            && self.env_passthrough.is_empty()
            && self.workspace.is_none()
            && self.extra.is_empty()
    }
}

/// A reproducible input for a runtime generation. A source is either a
/// controller-local checkout (which Lab snapshots) or an immutable remote.
/// No credentials, environment values, or shell snippets belong here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeMaterializationSource {
    pub id: String,
    pub locator: AgentRuntimeSourceLocator,
    pub content_identity: String,
    pub destination_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentRuntimeSourceLocator {
    LocalPath {
        path: String,
    },
    Git {
        remote_url: String,
        revision: String,
    },
}

/// A bounded argv-only build step run inside the isolated generation root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimePreparationAction {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_outputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_identity: Option<String>,
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
    pub run_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload: Option<String>,
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
    /// The controller-selected runtime source. Lab consumers materialize this
    /// identity rather than repeating ambient runtime discovery.
    #[serde(default)]
    pub selected_identity: AgentRuntimeSelectedIdentity,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_selector: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(default)]
    pub freshness: AgentRuntimeFreshness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_sources: Vec<AgentRuntimeMaterializationSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preparation: Vec<AgentRuntimePreparationAction>,
    /// Stable identity for the requested source/build recipe. Lab uses this as
    /// the generation key, never a mutable checkout path.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub generation_identity: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_roots: Vec<homeboy_agents_contract::AgentTaskProviderRunnerSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<AgentRuntimeMaterializationDependency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executable_requirements: Vec<AgentRuntimeExecutableRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readiness_checks: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_passthrough: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentRuntimeSelectedIdentity {
    #[serde(default)]
    pub runtime_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// `current` means an immutable source revision was observed. Extracted
    /// installs without that proof are intentionally `unverifiable`.
    #[serde(default = "default_runtime_freshness")]
    pub freshness: String,
}

fn default_runtime_freshness() -> String {
    "unverifiable".to_string()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeFreshness {
    Pinned,
    Unverifiable,
}

impl Default for AgentRuntimeFreshness {
    fn default() -> Self {
        Self::Unverifiable
    }
}

pub fn runtime_materialization_plan(
    manifest: &AgentRuntimeManifest,
    provider_id: impl Into<String>,
) -> AgentRuntimeMaterializationPlan {
    let mut env_passthrough = manifest.materialization.env_passthrough.clone();
    env_passthrough.sort();
    env_passthrough.dedup();

    let revision = manifest
        .runtime_path
        .as_deref()
        .and_then(|path| runtime_source_revision(Path::new(path)));
    let freshness = if revision.as_deref().is_some_and(is_immutable_revision) {
        "current"
    } else {
        "unverifiable"
    };
    let source_roots = manifest
        .materialization
        .source_roots
        .iter()
        .cloned()
        .map(|mut source| {
            if source.git_ref.is_some() && freshness == "current" {
                source.git_ref = revision.clone();
            }
            source
        })
        .collect();

    let mut runtime_sources = manifest.materialization.runtime_sources.clone();
    if runtime_sources.is_empty() {
        if let (Some(path), Some(revision)) = (manifest.runtime_path.as_ref(), revision.as_ref()) {
            runtime_sources.push(AgentRuntimeMaterializationSource {
                id: "controller-runtime".to_string(),
                locator: AgentRuntimeSourceLocator::LocalPath { path: path.clone() },
                content_identity: revision.clone(),
                destination_path: "runtime".to_string(),
            });
        }
    }
    let generation_identity = runtime_generation_identity(
        &manifest.id,
        &runtime_sources,
        &manifest.materialization.preparation,
    );

    AgentRuntimeMaterializationPlan {
        schema: AGENT_RUNTIME_MATERIALIZATION_PLAN_SCHEMA.to_string(),
        runtime_id: manifest.id.clone(),
        selected_identity: AgentRuntimeSelectedIdentity {
            runtime_id: manifest.id.clone(),
            extension_id: manifest.extension_id.clone(),
            source_path: manifest.runtime_path.clone(),
            revision,
            freshness: freshness.to_string(),
        },
        provider_id: provider_id.into(),
        source_selector: runtime_source_description(manifest),
        source_revision: manifest.source_revision.clone(),
        // A controller-selected revision is not freshness evidence. Only a
        // materializer that verifies the selected object may report it current.
        freshness: AgentRuntimeFreshness::Unverifiable,
        runtime_path: manifest.runtime_path.clone(),
        runtime_sources,
        preparation: manifest.materialization.preparation.clone(),
        generation_identity,
        source_roots,
        dependencies: manifest.materialization.dependencies.clone(),
        executable_requirements: manifest.materialization.executable_requirements.clone(),
        readiness_checks: manifest.materialization.readiness_checks.clone(),
        env_passthrough,
        workspace: manifest.materialization.workspace.clone(),
    }
}

/// Validate declarations before a controller hands them to Lab. This is kept in
/// core so every producer and consumer agrees on the safe, portable shape.
pub fn validate_runtime_materialization_plan(plan: &AgentRuntimeMaterializationPlan) -> Result<()> {
    let mut source_ids = std::collections::BTreeSet::new();
    let mut destinations = std::collections::BTreeSet::new();
    for source in &plan.runtime_sources {
        if source.id.trim().is_empty() || source.content_identity.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "runtime_sources",
                "runtime source requires id and content_identity",
                None,
                None,
            ));
        }
        if !source_ids.insert(&source.id) {
            return Err(Error::validation_invalid_argument(
                "runtime_sources.id",
                "runtime source IDs must be unique",
                Some(source.id.clone()),
                None,
            ));
        }
        if !is_immutable_revision(&source.content_identity) {
            return Err(Error::validation_invalid_argument(
                "runtime_sources.content_identity",
                "runtime source content identity must be an immutable revision",
                Some(source.content_identity.clone()),
                None,
            ));
        }
        validate_runtime_relative_path(
            "runtime_sources.destination_path",
            &source.destination_path,
        )?;
        if !destinations.insert(&source.destination_path) {
            return Err(Error::validation_invalid_argument(
                "runtime_sources.destination_path",
                "runtime source destinations must be unique",
                Some(source.destination_path.clone()),
                None,
            ));
        }
        match &source.locator {
            AgentRuntimeSourceLocator::LocalPath { path } if Path::new(path).is_absolute() => {}
            AgentRuntimeSourceLocator::LocalPath { .. } => {
                return Err(Error::validation_invalid_argument(
                    "runtime_sources.locator.path",
                    "controller-local runtime source path must be absolute",
                    None,
                    None,
                ))
            }
            AgentRuntimeSourceLocator::Git {
                remote_url,
                revision,
            } => {
                if remote_url.trim().is_empty()
                    || remote_url.contains('@')
                    || !is_immutable_revision(revision)
                    || revision != &source.content_identity
                {
                    return Err(Error::validation_invalid_argument(
                        "runtime_sources.locator",
                        "runtime git source requires a sanitized remote and a revision matching its immutable content identity",
                        None,
                        None,
                    ));
                }
            }
        }
    }
    for action in &plan.preparation {
        if action.argv.is_empty()
            || action
                .argv
                .iter()
                .any(|arg| arg.is_empty() || arg.contains('\0'))
        {
            return Err(Error::validation_invalid_argument(
                "preparation.argv",
                "runtime preparation requires a non-empty argv without NUL bytes",
                None,
                None,
            ));
        }
        validate_runtime_relative_path("preparation.cwd", &action.cwd)?;
        for output in &action.expected_outputs {
            validate_runtime_relative_path("preparation.expected_outputs", output)?;
        }
    }
    Ok(())
}

fn validate_runtime_relative_path(field: &str, value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(Error::validation_invalid_argument(
            field,
            "runtime materialization paths must be non-empty relative paths without traversal",
            Some(value.to_string()),
            None,
        ));
    }
    Ok(())
}

fn runtime_generation_identity(
    runtime_id: &str,
    sources: &[AgentRuntimeMaterializationSource],
    preparation: &[AgentRuntimePreparationAction],
) -> String {
    use sha2::{Digest, Sha256};
    let encoded = serde_json::to_vec(&(runtime_id, sources, preparation)).unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(encoded))
}

fn runtime_source_revision(path: &Path) -> Option<String> {
    crate::git::head_sha(path).or_else(|| {
        std::fs::read_to_string(path.join(".source-revision"))
            .ok()
            .map(|revision| revision.trim().to_string())
            .filter(|revision| !revision.is_empty())
    })
}

pub fn discover_agent_runtime_catalog() -> AgentRuntimeDiscoveryCatalog {
    let standalone = discover_standalone_agent_runtime_catalog();
    let extensions = load_all_extensions().unwrap_or_default();
    let extension = discover_agent_runtime_catalog_from_extensions(&extensions);
    let mut diagnostics = standalone.diagnostics;
    diagnostics.extend(extension.diagnostics);
    let resolved = resolve_agent_runtime_manifests(standalone.manifests, extension.manifests);
    diagnostics.extend(resolved.diagnostics);
    AgentRuntimeDiscoveryCatalog {
        manifests: resolved.manifests,
        diagnostics,
    }
}

pub fn discover_agent_runtime_tool_diagnostic_manifests() -> Vec<AgentRuntimeManifest> {
    discover_agent_runtime_catalog()
        .manifests
        .into_iter()
        .filter(|manifest| !manifest.materialization.diagnostics.tools.is_empty())
        .collect()
}

struct ResolvedAgentRuntimeCatalog {
    manifests: Vec<AgentRuntimeManifest>,
    diagnostics: Vec<AgentRuntimeDiscoveryDiagnostic>,
}

fn resolve_agent_runtime_manifests(
    standalone_manifests: Vec<AgentRuntimeManifest>,
    extension_manifests: Vec<AgentRuntimeManifest>,
) -> ResolvedAgentRuntimeCatalog {
    let mut candidates = standalone_manifests;
    candidates.extend(extension_manifests);
    candidates.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.extension_id.cmp(&right.extension_id))
            .then_with(|| left.runtime_path.cmp(&right.runtime_path))
    });

    let mut manifests = Vec::new();
    let mut diagnostics = Vec::new();
    let mut candidates = candidates.into_iter().peekable();
    while let Some(manifest) = candidates.next() {
        let id = manifest.id.clone();
        let mut collisions = vec![manifest];
        while candidates
            .peek()
            .is_some_and(|candidate| candidate.id == id)
        {
            collisions.push(candidates.next().expect("peeked runtime candidate"));
        }
        if collisions.len() == 1 {
            manifests.push(collisions.pop().expect("single runtime candidate"));
            continue;
        }
        let sources = collisions
            .iter()
            .map(runtime_source_description)
            .collect::<Vec<_>>();
        diagnostics.push(AgentRuntimeDiscoveryDiagnostic {
            class: "agent_runtime_catalog.conflict".to_string(),
            message: format!(
                "Runtime id '{id}' is declared by multiple sources: {}. Select one source explicitly before dispatching.",
                sources.join(", ")
            ),
            runtime_id: Some(id),
            extension_id: None,
            path: None,
        });
    }
    ResolvedAgentRuntimeCatalog {
        manifests,
        diagnostics,
    }
}

fn runtime_source_description(manifest: &AgentRuntimeManifest) -> String {
    match (&manifest.extension_id, &manifest.runtime_path) {
        (Some(extension_id), Some(path)) => format!("extension:{extension_id} ({path})"),
        (Some(extension_id), None) => format!("extension:{extension_id}"),
        (None, Some(path)) => format!("standalone ({path})"),
        (None, None) => "standalone".to_string(),
    }
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
    manifest_path_for: fn(&str) -> crate::Result<PathBuf>,
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
    manifest_path_for: fn(&str) -> crate::Result<PathBuf>,
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
            let class = match error.validation_json_category() {
                Some("data") => "agent_runtime_manifest.schema_mismatch",
                _ => "agent_runtime_manifest.parse_failed",
            };
            return StandaloneAgentRuntimeManifestLoad::Invalid(AgentRuntimeDiscoveryDiagnostic {
                class: class.to_string(),
                message: error
                    .validation_json_error()
                    .unwrap_or_else(|| error.message.as_str())
                    .to_string(),
                runtime_id: Some(id),
                extension_id: None,
                path: Some(manifest_path.display().to_string()),
            });
        }
    };
    manifest.id = id;
    manifest.extension_id = None;
    manifest.extension_path = None;
    manifest.runtime_path = Some(path.to_string_lossy().to_string());
    manifest.source_revision = crate::git::head_sha(path);
    if let Some(diagnostic) = mutable_runtime_source_diagnostic(
        &manifest.id,
        None,
        manifest.runtime_path.as_deref(),
        &manifest.materialization,
    ) {
        return StandaloneAgentRuntimeManifestLoad::Invalid(diagnostic);
    }
    if let Some(diagnostic) = agent_runtime_core_incompatible_diagnostic(
        &manifest.id,
        None,
        manifest.runtime_path.as_deref(),
        manifest
            .requires
            .as_ref()
            .and_then(|requires| requires.homeboy.as_deref()),
        None,
    ) {
        return StandaloneAgentRuntimeManifestLoad::Invalid(diagnostic);
    }
    StandaloneAgentRuntimeManifestLoad::Loaded(manifest)
}

pub(crate) fn discover_agent_runtime_catalog_from_extensions(
    extensions: &[ExtensionManifest],
) -> AgentRuntimeDiscoveryCatalog {
    let mut runtime_manifests = Vec::new();
    let mut diagnostics = Vec::new();
    for extension in extensions {
        for runtime in &extension.agent_runtimes {
            if let Some(diagnostic) = agent_runtime_core_incompatible_diagnostic(
                &runtime.id,
                Some(&extension.id),
                extension.extension_path.as_deref(),
                runtime_core_constraint(runtime, extension),
                crate::extension_update_check::read_source_revision(&extension.id),
            ) {
                diagnostics.push(diagnostic);
                continue;
            }
            let materialization: AgentRuntimeMaterializationContract = serde_json::from_value(
                runtime
                    .extra
                    .get("materialization")
                    .cloned()
                    .unwrap_or(Value::Null),
            )
            .unwrap_or_default();
            if let Some(diagnostic) = mutable_runtime_source_diagnostic(
                &runtime.id,
                Some(&extension.id),
                extension.extension_path.as_deref(),
                &materialization,
            ) {
                diagnostics.push(diagnostic);
                continue;
            }
            // The executor providers are carried opaquely as JSON; the
            // agent-task layer validates them at discovery time. Core just
            // passes them through into the runtime manifest.
            let providers = runtime.agent_task_executors.clone();
            if providers.is_empty() {
                continue;
            }
            runtime_manifests.push(AgentRuntimeManifest {
                schema: AGENT_RUNTIME_MANIFEST_SCHEMA.to_string(),
                id: runtime.id.clone(),
                label: runtime.label.clone(),
                agent_task_executors: providers,
                config_path_fields: runtime.config_path_fields.clone(),
                materialization,
                extension_id: Some(extension.id.clone()),
                requires: runtime_requires(runtime, extension),
                extension_path: extension.extension_path.clone(),
                runtime_path: extension.extension_path.clone(),
                source_revision: crate::extension_update_check::read_source_revision(&extension.id)
                    .filter(|revision| is_immutable_revision(revision)),
                extra: runtime
                    .extra
                    .clone()
                    .into_iter()
                    .filter(|(key, _)| key != "materialization" && key != "requires")
                    .collect(),
            });
        }
    }
    AgentRuntimeDiscoveryCatalog {
        manifests: runtime_manifests,
        diagnostics,
    }
}

fn mutable_runtime_source_diagnostic(
    runtime_id: &str,
    extension_id: Option<&str>,
    path: Option<&str>,
    materialization: &AgentRuntimeMaterializationContract,
) -> Option<AgentRuntimeDiscoveryDiagnostic> {
    let source = materialization.source_roots.iter().find(|source| {
        source
            .git_ref
            .as_deref()
            .is_some_and(|git_ref| !is_immutable_revision(git_ref))
    })?;
    Some(AgentRuntimeDiscoveryDiagnostic {
        class: "agent_runtime_manifest.mutable_ref".to_string(),
        message: format!(
            "Agent runtime '{}' source '{}' declares mutable git_ref '{}'. Materialization requires an immutable commit revision.",
            runtime_id,
            source.id,
            source.git_ref.as_deref().unwrap_or_default(),
        ),
        runtime_id: Some(runtime_id.to_string()),
        extension_id: extension_id.map(str::to_string),
        path: path.map(str::to_string),
    })
}

pub fn is_immutable_revision(value: &str) -> bool {
    let value = value.trim();
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn runtime_requires(
    runtime: &homeboy_extension_contract::AgentRuntimeManifestConfig,
    extension: &ExtensionManifest,
) -> Option<RequirementsConfig> {
    runtime
        .extra
        .get("requires")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .or_else(|| extension.requires.clone())
}

fn runtime_core_constraint<'a>(
    runtime: &'a homeboy_extension_contract::AgentRuntimeManifestConfig,
    extension: &'a ExtensionManifest,
) -> Option<&'a str> {
    runtime
        .extra
        .get("requires")
        .and_then(|value| value.get("homeboy"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            extension
                .requires
                .as_ref()
                .and_then(|requires| requires.homeboy.as_deref())
        })
}

fn agent_runtime_core_incompatible_diagnostic(
    runtime_id: &str,
    extension_id: Option<&str>,
    path: Option<&str>,
    requires_homeboy: Option<&str>,
    source_revision: Option<String>,
) -> Option<AgentRuntimeDiscoveryDiagnostic> {
    let report =
        homeboy_extension_contract::evaluate_core_compatibility(requires_homeboy, source_revision)
            .ok()?;
    (report.status == "incompatible").then(|| AgentRuntimeDiscoveryDiagnostic {
        class: "agent_runtime_manifest.core_incompatible".to_string(),
        message: format!(
            "Agent runtime manifest '{}' requires homeboy {}, but installed homeboy is {} (source_revision: {}). Run `{}` and retry.",
            runtime_id,
            report.requires_homeboy.as_deref().unwrap_or("<undeclared>"),
            report.installed_homeboy,
            report.source_revision.as_deref().unwrap_or("<missing>"),
            report
                .remediation_command
                .as_deref()
                .unwrap_or(homeboy_extension_contract::core_compat::CORE_COMPAT_REMEDIATION_COMMAND)
        ),
        runtime_id: Some(runtime_id.to_string()),
        extension_id: extension_id.map(str::to_string),
        path: path.map(str::to_string),
    })
}

#[cfg(test)]
mod materialization_tests {
    use super::*;

    fn plan() -> AgentRuntimeMaterializationPlan {
        AgentRuntimeMaterializationPlan {
            schema: AGENT_RUNTIME_MATERIALIZATION_PLAN_SCHEMA.to_string(),
            runtime_id: "runtime".to_string(),
            selected_identity: Default::default(),
            provider_id: "provider".to_string(),
            source_selector: "test".to_string(),
            source_revision: None,
            freshness: Default::default(),
            runtime_path: None,
            runtime_sources: vec![AgentRuntimeMaterializationSource {
                id: "source".to_string(),
                locator: AgentRuntimeSourceLocator::LocalPath {
                    path: "/tmp/runtime-source".to_string(),
                },
                content_identity: "0123456789abcdef0123456789abcdef01234567".to_string(),
                destination_path: "runtime".to_string(),
            }],
            preparation: vec![AgentRuntimePreparationAction {
                argv: vec!["build".to_string()],
                cwd: "runtime".to_string(),
                expected_outputs: vec!["runtime/bin/tool".to_string()],
                runtime_identity: Some("tool@1".to_string()),
            }],
            generation_identity: "sha256:test".to_string(),
            source_roots: Vec::new(),
            dependencies: Vec::new(),
            executable_requirements: Vec::new(),
            readiness_checks: Vec::new(),
            env_passthrough: Vec::new(),
            workspace: None,
        }
    }

    #[test]
    fn runtime_materialization_contract_accepts_bounded_noop() {
        assert!(validate_runtime_materialization_plan(&plan()).is_ok());
    }

    #[test]
    fn runtime_materialization_contract_rejects_path_traversal_and_unsafe_argv() {
        let mut invalid = plan();
        invalid.runtime_sources[0].destination_path = "../shared".to_string();
        assert!(validate_runtime_materialization_plan(&invalid).is_err());

        let mut invalid = plan();
        invalid.preparation[0].cwd = "/shared".to_string();
        assert!(validate_runtime_materialization_plan(&invalid).is_err());

        let mut invalid = plan();
        invalid.preparation[0].argv = vec!["run\0bad".to_string()];
        assert!(validate_runtime_materialization_plan(&invalid).is_err());
    }

    #[test]
    fn runtime_materialization_contract_rejects_ambiguous_source_identity() {
        let mut invalid = plan();
        invalid
            .runtime_sources
            .push(invalid.runtime_sources[0].clone());
        assert!(validate_runtime_materialization_plan(&invalid).is_err());

        let mut invalid = plan();
        invalid.runtime_sources[0].content_identity = "mutable".to_string();
        assert!(validate_runtime_materialization_plan(&invalid).is_err());

        let mut invalid = plan();
        invalid.runtime_sources[0].locator = AgentRuntimeSourceLocator::Git {
            remote_url: "https://example.test/runtime.git".to_string(),
            revision: "ffffffffffffffffffffffffffffffffffffffff".to_string(),
        };
        assert!(validate_runtime_materialization_plan(&invalid).is_err());
    }

    #[test]
    fn runtime_generation_identity_changes_with_source_revision() {
        let first = plan();
        let mut second = first.clone();
        second.runtime_sources[0].content_identity =
            "ffffffffffffffffffffffffffffffffffffffff".to_string();
        assert_ne!(
            runtime_generation_identity(
                &first.runtime_id,
                &first.runtime_sources,
                &first.preparation
            ),
            runtime_generation_identity(
                &second.runtime_id,
                &second.runtime_sources,
                &second.preparation
            )
        );
    }
}
