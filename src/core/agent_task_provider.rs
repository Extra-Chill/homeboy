use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFailureClassification,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::core::agent_task_scheduler::{
    AgentTaskExecutionContext, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use crate::core::agent_task_secrets::{
    resolve_secret_env_with_fallbacks, AgentTaskSecretResolutionError,
};
use crate::core::agent_task_timeout::timeout_with_grace;
use crate::core::{agent_runtime_manifest, component, defaults, extension, Error};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskExecutorProvider {
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub backend: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub default_backend: bool,
    pub command: String,
    pub request_schema: String,
    pub outcome_schema: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_requirements: Vec<AgentTaskProviderSecretRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env_requirements: Vec<AgentTaskProviderSecretEnvRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_materialization: Option<AgentTaskProviderWorkspaceMaterialization>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_defaults: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner_readiness: Vec<AgentTaskProviderRunnerReadiness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner_sources: Vec<AgentTaskProviderRunnerSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependency_failure_patterns: Vec<AgentTaskProviderDependencyFailurePattern>,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskProviderTimeoutArtifactDiscovery::is_empty"
    )]
    pub timeout_artifact_discovery: AgentTaskProviderTimeoutArtifactDiscovery,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskProviderRoleAliases::is_empty"
    )]
    pub role_aliases: AgentTaskProviderRoleAliases,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderSecretRequirement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderSecretEnvRequirement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(Value::String(item)) => vec![item],
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    })
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderRunnerReadiness {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_path: Option<AgentTaskProviderEnvPathReadiness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderEnvPathReadiness {
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<bool>,
    /// Optional extension-declared canonical root (e.g. a managed source clone
    /// kept under the runner's homeboy cache). When set, doctor WARNS if the
    /// env-resolved path does not live under this canonical root, catching
    /// stale / non-canonical checkout drift before it corrupts results.
    ///
    /// Core is runtime-agnostic: it does not know what the canonical path
    /// represents (a runtime checkout, a toolchain, etc.) — the value is supplied
    /// entirely by the declaring extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_path: Option<String>,
}

/// A named, extension-declared source checkout that homeboy keeps synced on the
/// runner. Core treats this generically: it materializes/refreshes a git
/// checkout to the intended ref/remote. It has no knowledge of what the source
/// is (a runtime checkout, a CLI, a toolchain) — extensions declare the path/remote/ref.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderRunnerSource {
    pub id: String,
    pub label: String,
    /// Absolute path (or `$HOME`/`~`-prefixed path) of the managed checkout on
    /// the runner, e.g. a path under the runner's homeboy cache directory.
    pub path: String,
    /// Optional canonical remote URL the checkout must track. When set, homeboy
    /// re-points `origin` if the checkout tracks a different remote (fixing the
    /// "tracks wrong remote" drift), then fetches and fast-forwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// Optional explicit ref (branch, tag, or sha) to check out and sync to.
    /// When omitted, homeboy fast-forwards the current branch to its upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderDependencyFailurePattern {
    pub id: String,
    pub label: String,
    pub path_contains: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_contains_any: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderTimeoutArtifactDiscovery {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata_path_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_path_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_patterns: Vec<AgentTaskProviderArtifactPattern>,
}

impl AgentTaskProviderTimeoutArtifactDiscovery {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
            && self.metadata_path_keys.is_empty()
            && self.config_path_keys.is_empty()
            && self.artifact_patterns.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderArtifactPattern {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filename_patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filename_contains: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(
        default = "default_metadata",
        skip_serializing_if = "is_empty_metadata"
    )]
    pub metadata: Value,
}

fn default_metadata() -> Value {
    Value::Object(Default::default())
}

fn is_empty_metadata(value: &Value) -> bool {
    value.as_object().is_none_or(|object| object.is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderRoleAliases {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifact_kinds: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifact_filenames: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Vec<String>>,
}

impl AgentTaskProviderRoleAliases {
    pub fn is_empty(&self) -> bool {
        self.artifact_kinds.is_empty()
            && self.artifact_filenames.is_empty()
            && self.outputs.is_empty()
            && self.metadata.is_empty()
    }

    pub fn artifact_kind_matches_role(&self, role: &str, kind: &str) -> bool {
        alias_matches(self.artifact_kinds.get(role), kind)
    }

    pub fn artifact_filename_matches_role(&self, role: &str, filename: &str) -> bool {
        alias_matches(self.artifact_filenames.get(role), filename)
    }

    pub fn role_for_artifact_kind(&self, kind: &str) -> Option<&str> {
        self.artifact_kinds
            .iter()
            .find_map(|(role, aliases)| alias_matches(Some(aliases), kind).then_some(role.as_str()))
    }

    pub fn output_aliases_for_role(&self, role: &str) -> Vec<&str> {
        aliases_for_role(&self.outputs, role)
    }

    pub fn metadata_aliases_for_role(&self, role: &str) -> Vec<&str> {
        aliases_for_role(&self.metadata, role)
    }
}

fn aliases_for_role<'a>(map: &'a BTreeMap<String, Vec<String>>, role: &str) -> Vec<&'a str> {
    map.get(role)
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect()
}

fn alias_matches(aliases: Option<&Vec<String>>, value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    aliases.into_iter().flatten().any(|alias| {
        let alias = alias.to_ascii_lowercase();
        alias == value || wildcard_match(&alias, &value)
    })
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == value;
    }

    let mut remainder = value;
    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let parts: Vec<&str> = pattern.split('*').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return true;
    }
    if anchored_start && !value.starts_with(parts[0]) {
        return false;
    }
    for part in &parts {
        let Some(index) = remainder.find(part) else {
            return false;
        };
        remainder = &remainder[index + part.len()..];
    }
    !anchored_end || value.ends_with(parts[parts.len() - 1])
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderWorkspaceMaterialization {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_git: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_paths: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ExtensionProviderAgentTaskExecutor {
    providers: Vec<AgentTaskExecutorProvider>,
}

impl ExtensionProviderAgentTaskExecutor {
    pub fn discover() -> Self {
        Self {
            providers: discover_agent_task_executor_providers(),
        }
    }

    #[cfg(test)]
    fn with_providers(providers: Vec<AgentTaskExecutorProvider>) -> Self {
        Self { providers }
    }

    pub fn providers(&self) -> &[AgentTaskExecutorProvider] {
        &self.providers
    }

    pub fn default_backend(&self) -> crate::core::Result<Option<String>> {
        default_backend_from_policy(None)
    }

    pub fn required_extension_ids_for_plan(&self, plan: &AgentTaskPlan) -> Vec<String> {
        required_extension_ids_for_plan_with_providers(plan, &self.providers)
    }
}

pub fn default_backend() -> crate::core::Result<Option<String>> {
    default_backend_from_policy(None)
}

pub fn default_backend_for_component(
    component_id: Option<&str>,
) -> crate::core::Result<Option<String>> {
    default_backend_from_policy(component_id)
}

pub fn provider_runner_readiness_contracts() -> Vec<AgentTaskProviderRunnerReadiness> {
    discover_agent_task_executor_providers()
        .into_iter()
        .flat_map(|provider| provider.runner_readiness)
        .collect()
}

pub fn provider_runner_source_contracts() -> Vec<AgentTaskProviderRunnerSource> {
    discover_agent_task_executor_providers()
        .into_iter()
        .flat_map(|provider| provider.runner_sources)
        .collect()
}

pub fn dependency_failure_patterns() -> Vec<AgentTaskProviderDependencyFailurePattern> {
    discover_agent_task_executor_providers()
        .into_iter()
        .flat_map(|provider| provider.dependency_failure_patterns)
        .collect()
}

pub fn required_extension_ids_for_plan(plan: &AgentTaskPlan) -> Vec<String> {
    ExtensionProviderAgentTaskExecutor::discover().required_extension_ids_for_plan(plan)
}

pub fn provider_requires_cwd_git_checkout(backend: &str, selector: Option<&str>) -> bool {
    let providers = discover_agent_task_executor_providers();
    provider_requires_cwd_git_checkout_with_providers(&providers, backend, selector)
}

pub fn apply_provider_runner_secret_env_contracts(plan: &mut AgentTaskPlan) {
    let providers = discover_agent_task_executor_providers();
    apply_provider_runner_secret_env_contracts_with_providers(plan, &providers);
}

pub fn provider_runner_secret_env_for_plan(plan: &AgentTaskPlan) -> Vec<String> {
    let providers = discover_agent_task_executor_providers();
    provider_runner_secret_env_for_plan_with_providers(plan, &providers)
}

pub fn provider_secret_sources_for_plan(
    plan: &AgentTaskPlan,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let providers = discover_agent_task_executor_providers();
    provider_secret_sources_for_plan_with_providers(plan, &providers)
}

pub fn provider_secret_sources_for_discovered_providers(
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let providers = discover_agent_task_executor_providers();
    provider_secret_sources_for_providers(&providers)
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

pub(crate) fn role_aliases_for_executor(
    backend: &str,
    selector: Option<&str>,
) -> AgentTaskProviderRoleAliases {
    let providers = discover_agent_task_executor_providers();
    select_provider_by_backend(&providers, backend, selector)
        .map(|provider| provider.role_aliases.clone())
        .unwrap_or_default()
}

pub(crate) fn timeout_artifact_discovery_for_executor(
    backend: &str,
    selector: Option<&str>,
) -> AgentTaskProviderTimeoutArtifactDiscovery {
    let providers = discover_agent_task_executor_providers();
    select_provider_by_backend(&providers, backend, selector)
        .map(|provider| provider.timeout_artifact_discovery.clone())
        .unwrap_or_default()
}

pub(crate) fn role_aliases_for_provider(
    provider_id_or_backend: &str,
) -> AgentTaskProviderRoleAliases {
    let providers = discover_agent_task_executor_providers();
    providers
        .iter()
        .find(|provider| {
            provider.id == provider_id_or_backend || provider.backend == provider_id_or_backend
        })
        .map(|provider| provider.role_aliases.clone())
        .unwrap_or_default()
}

fn provider_requires_cwd_git_checkout_with_providers(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> bool {
    select_provider_by_backend(providers, backend, selector)
        .and_then(|provider| provider.workspace_materialization.as_ref())
        .map(|materialization| {
            materialization.requires_git == Some(true)
                || materialization.cwd.as_deref() == Some("git_checkout")
        })
        .unwrap_or(false)
}

fn apply_provider_runner_secret_env_contracts_with_providers(
    plan: &mut AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) {
    for request in &mut plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        for name in provider_secret_env(provider, Some(request)) {
            if !request.executor.secret_env.contains(&name) {
                request.executor.secret_env.push(name);
            }
        }
    }
}

fn provider_runner_secret_env_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> Vec<String> {
    let mut names = Vec::new();
    for request in &plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        names.extend(provider_secret_env(provider, Some(request)));
    }
    names.sort();
    names.dedup();
    names
}

fn provider_secret_sources_for_plan_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = HashMap::new();
    for request in &plan.tasks {
        let Some(provider) = select_provider(providers, request) else {
            continue;
        };
        sources.extend(provider_secret_sources(provider, Some(request)));
    }
    sources
}

fn provider_secret_env(
    provider: &AgentTaskExecutorProvider,
    request: Option<&AgentTaskRequest>,
) -> Vec<String> {
    let mut names = Vec::new();
    for readiness in &provider.runner_readiness {
        names.extend(readiness.secret_env.iter().cloned());
    }
    for requirement in &provider.secret_requirements {
        if requirement.required == Some(false) {
            continue;
        }
        if let Some(name) = &requirement.name {
            names.push(name.clone());
        }
        names.extend(requirement.env.iter().cloned());
    }
    for requirement in &provider.secret_env_requirements {
        if requirement_matches_request(requirement.when.as_ref(), request) {
            names.extend(requirement.env.iter().cloned());
        }
    }
    if let Some(request) = request {
        if let Some(provider_name) = request
            .executor
            .config
            .get("provider")
            .and_then(Value::as_str)
        {
            if let Some(defaults) = provider.provider_defaults.get(provider_name) {
                names.extend(provider_config_secret_env(defaults));
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

fn provider_secret_sources(
    provider: &AgentTaskExecutorProvider,
    request: Option<&AgentTaskRequest>,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let mut sources = HashMap::new();
    for requirement in &provider.secret_env_requirements {
        if requirement_matches_request(requirement.when.as_ref(), request) {
            sources.extend(secret_source_map_from_extra(&requirement.extra));
        }
    }
    if let Some(request) = request {
        if let Some(provider_name) = request
            .executor
            .config
            .get("provider")
            .and_then(Value::as_str)
        {
            if let Some(defaults) = provider.provider_defaults.get(provider_name) {
                sources.extend(provider_config_secret_sources(defaults));
            }
        }
    }
    sources
}

fn secret_source_map_from_extra(
    extra: &BTreeMap<String, Value>,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    for key in [
        "secret_env_sources",
        "secretEnvSources",
        "credential_sources",
        "credentialSources",
    ] {
        if let Some(value) = extra.get(key) {
            return secret_source_map(value);
        }
    }
    HashMap::new()
}

fn provider_config_secret_sources(
    config: &Value,
) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let Some(config) = config.as_object() else {
        return HashMap::new();
    };
    for key in [
        "secret_env_sources",
        "secretEnvSources",
        "credential_sources",
        "credentialSources",
    ] {
        if let Some(value) = config.get(key) {
            return secret_source_map(value);
        }
    }
    HashMap::new()
}

fn secret_source_map(value: &Value) -> HashMap<String, defaults::AgentTaskSecretSource> {
    let Some(entries) = value.as_object() else {
        return HashMap::new();
    };
    entries
        .iter()
        .filter_map(|(name, source)| {
            serde_json::from_value::<defaults::AgentTaskSecretSource>(source.clone())
                .ok()
                .map(|source| (name.clone(), source))
        })
        .collect()
}

fn provider_config_secret_env(config: &Value) -> Vec<String> {
    let Some(config) = config.as_object() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for key in ["secret_env", "secretEnv"] {
        match config.get(key) {
            Some(Value::String(name)) => names.push(name.clone()),
            Some(Value::Array(items)) => names.extend(
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string)),
            ),
            _ => {}
        }
    }
    names
}

fn requirement_matches_request(when: Option<&Value>, request: Option<&AgentTaskRequest>) -> bool {
    let Some(when) = when else {
        return true;
    };
    let Some(request) = request else {
        return false;
    };
    let Ok(request_value) = serde_json::to_value(request) else {
        return false;
    };
    condition_matches(when, &request_value)
}

fn condition_matches(condition: &Value, request: &Value) -> bool {
    if let Some(any) = condition.get("any").and_then(Value::as_array) {
        return any.iter().any(|item| condition_matches(item, request));
    }
    if let Some(all) = condition.get("all").and_then(Value::as_array) {
        return all.iter().all(|item| condition_matches(item, request));
    }
    let Some(path) = condition.get("path").and_then(Value::as_str) else {
        return false;
    };
    let actual = value_at_contract_path(request, path);
    match condition.get("equals") {
        Some(expected) => actual == Some(expected),
        None => actual.is_some(),
    }
}

fn value_at_contract_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path == "provider" {
        return value_at_contract_path(value, "executor.config.provider");
    }
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn default_backend_from_policy(component_id: Option<&str>) -> crate::core::Result<Option<String>> {
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

fn component_default_backend(component: &component::Component) -> Option<String> {
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

impl AgentTaskExecutorAdapter for ExtensionProviderAgentTaskExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        if request.executor.backend == "fixture" {
            return run_fixture_provider(&request);
        }

        let Some(provider) = select_provider(&self.providers, &request) else {
            return failure_outcome(
                &request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::CapabilityMissing,
                "agent_task.provider_missing",
                format!(
                    "no extension agent-task provider found for backend '{}'",
                    request.executor.backend
                ),
                json!({ "backend": request.executor.backend }),
            );
        };

        let missing_capabilities: Vec<String> = request
            .executor
            .required_capabilities
            .iter()
            .filter(|capability| !provider.capabilities.contains(capability))
            .cloned()
            .collect();
        if !missing_capabilities.is_empty() {
            return failure_outcome(
                &request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::CapabilityMissing,
                "agent_task.capability_missing",
                format!(
                    "provider '{}' is missing required capabilities: {}",
                    provider.id,
                    missing_capabilities.join(", ")
                ),
                json!({ "provider": provider.id, "missing_capabilities": missing_capabilities }),
            );
        }

        run_provider_command(&request, provider)
    }
}

fn run_fixture_provider(request: &AgentTaskRequest) -> AgentTaskOutcome {
    let mode = request
        .executor
        .config
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("success");
    let artifact_root = fixture_artifact_root(request);
    if let Err(error) = std::fs::create_dir_all(&artifact_root) {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.fixture_artifact_root_failed",
            error.to_string(),
            json!({ "artifact_root": artifact_root.display().to_string() }),
        );
    }

    match mode {
        "empty_patch" => fixture_empty_patch_outcome(request, &artifact_root),
        "empty_runtime_bundle" => fixture_empty_runtime_bundle_outcome(request, &artifact_root),
        _ => fixture_success_outcome(request, &artifact_root),
    }
}

fn fixture_artifact_root(request: &AgentTaskRequest) -> PathBuf {
    request
        .executor
        .config
        .get("artifact_root")
        .and_then(Value::as_str)
        .map(|path| {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(path)
            }
        })
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("homeboy-agent-task-fixture-{}", request.task_id))
        })
}

fn fixture_success_outcome(
    request: &AgentTaskRequest,
    artifact_root: &PathBuf,
) -> AgentTaskOutcome {
    let changed_file = request
        .executor
        .config
        .get("changed_file")
        .and_then(Value::as_str)
        .unwrap_or("docs/agent-task-smoke.md");
    let patch_path = artifact_root.join("changes.patch");
    let transcript_path = artifact_root.join("transcript.log");
    let result_path = artifact_root.join("agent-result.json");
    let patch = format!(
        "diff --git a/{changed_file} b/{changed_file}\n--- a/{changed_file}\n+++ b/{changed_file}\n@@ -1 +1 @@\n-before\n+after\n"
    );
    let _ = std::fs::write(&patch_path, patch);
    let _ = std::fs::write(
        &transcript_path,
        "fixture provider completed successfully\n",
    );

    let metadata = request
        .executor
        .config
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({ "fixture_mode": "success" }));
    let mut outcome = AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: request.task_id.clone(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: Some("fixture provider wrote deterministic smoke artifacts".to_string()),
        failure_classification: None,
        artifacts: vec![
            fixture_artifact("patch", "patch", &patch_path, Some("text/x-patch")),
            fixture_artifact(
                "agent-result",
                "agent_result",
                &result_path,
                Some("application/json"),
            ),
        ],
        typed_artifacts: Vec::new(),
        evidence_refs: vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: transcript_path.display().to_string(),
            label: Some("fixture transcript".to_string()),
        }],
        diagnostics: vec![AgentTaskDiagnostic {
            class: "agent_task.fixture_success".to_string(),
            message: "fixture provider generated patch, transcript, and result artifacts"
                .to_string(),
            data: json!({ "artifact_root": artifact_root.display().to_string() }),
        }],
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata,
    };
    let _ = std::fs::write(
        &result_path,
        serde_json::to_string_pretty(&outcome).unwrap_or_else(|_| "{}".to_string()),
    );
    outcome.artifacts[1] = fixture_artifact(
        "agent-result",
        "agent_result",
        &result_path,
        Some("application/json"),
    );
    outcome
}

fn fixture_empty_patch_outcome(
    request: &AgentTaskRequest,
    artifact_root: &PathBuf,
) -> AgentTaskOutcome {
    let patch_path = artifact_root.join("empty.patch");
    let _ = std::fs::write(&patch_path, "");
    failure_outcome(
        request,
        AgentTaskOutcomeStatus::NoOp,
        AgentTaskFailureClassification::InvalidInput,
        "agent_task.fixture_empty_patch",
        "fixture produced an empty patch; promotion should reject it".to_string(),
        json!({ "patch_path": patch_path.display().to_string() }),
    )
}

fn fixture_empty_runtime_bundle_outcome(
    request: &AgentTaskRequest,
    artifact_root: &PathBuf,
) -> AgentTaskOutcome {
    let bundle_path = artifact_root.join("runtime-bundle");
    let _ = std::fs::create_dir_all(&bundle_path);
    let mut outcome = failure_outcome(
        request,
        AgentTaskOutcomeStatus::ProviderError,
        AgentTaskFailureClassification::Provider,
        "agent_task.fixture_empty_runtime_bundle",
        "fixture produced an empty runtime bundle".to_string(),
        json!({ "bundle_path": bundle_path.display().to_string() }),
    );
    outcome.artifacts.push(fixture_artifact(
        "runtime-bundle",
        "runtime_bundle",
        &bundle_path,
        None,
    ));
    outcome
}

fn fixture_artifact(id: &str, kind: &str, path: &PathBuf, mime: Option<&str>) -> AgentTaskArtifact {
    AgentTaskArtifact {
        schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: id.to_string(),
        kind: kind.to_string(),
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().to_string()),
        path: Some(path.display().to_string()),
        url: None,
        mime: mime.map(str::to_string),
        size_bytes: std::fs::metadata(path).ok().map(|metadata| metadata.len()),
        sha256: None,
        metadata: json!({ "fixture": true }),
    }
}

fn discover_agent_task_executor_providers() -> Vec<AgentTaskExecutorProvider> {
    agent_runtime_manifest::discover_agent_task_executor_providers()
}

fn select_provider<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    request: &AgentTaskRequest,
) -> Option<&'a AgentTaskExecutorProvider> {
    select_provider_by_backend(
        providers,
        &request.executor.backend,
        request.executor.selector.as_deref(),
    )
}

pub(crate) fn provider_available_for_backend(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> bool {
    select_provider_by_backend(providers, backend, selector).is_some()
}

fn select_provider_by_backend<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> Option<&'a AgentTaskExecutorProvider> {
    providers
        .iter()
        .find(|provider| provider_matches_exact_backend(provider, backend, selector))
        .or_else(|| select_provider_by_extension_alias(providers, backend, selector))
}

fn provider_matches_exact_backend(
    provider: &AgentTaskExecutorProvider,
    backend: &str,
    selector: Option<&str>,
) -> bool {
    provider.backend == backend && selector.is_none_or(|selector| provider.id == selector)
}

fn select_provider_by_extension_alias<'a>(
    providers: &'a [AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> Option<&'a AgentTaskExecutorProvider> {
    let mut matches = providers
        .iter()
        .filter(|provider| provider.extension_id.as_deref() == Some(backend));
    let provider = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    if selector.is_none_or(|selector| provider.id == selector) {
        Some(provider)
    } else {
        None
    }
}

fn required_extension_ids_for_plan_with_providers(
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

fn run_provider_command(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> AgentTaskOutcome {
    let command = render_provider_command(provider);
    let Some((program, args)) = provider_command_parts(&command) else {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_command_empty",
            format!("provider '{}' has an empty command", provider.id),
            json!({ "provider": provider.id }),
        );
    };
    let timeout = request
        .limits
        .timeout_ms
        .or(request.limits.max_runtime_ms)
        .map(|timeout_ms| (timeout_ms, timeout_with_grace(timeout_ms)));
    let mut provider_request = request.clone();
    provider_request.normalize_artifact_declarations();
    let input = match serde_json::to_vec(&provider_request) {
        Ok(input) => input,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.request_encode_failed",
                error.to_string(),
                json!({ "provider": provider.id }),
            )
        }
    };
    let env = match provider_command_env(request, provider) {
        Ok(env) => env,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.secret_env_missing",
                error.message,
                json!({ "provider": provider.id, "missing_secret_env": error.missing_secret_env }),
            )
        }
    };

    let mut child = match Command::new(&program)
        .args(&args)
        .envs(
            env.iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return failure_outcome(
                request,
                AgentTaskOutcomeStatus::ProviderError,
                AgentTaskFailureClassification::Provider,
                "agent_task.provider_spawn_failed",
                error.to_string(),
                json!({ "provider": provider.id, "command": command }),
            )
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = std::io::Write::write_all(&mut stdin, &input);
    }

    let output = if let Some((requested_timeout_ms, process_timeout)) = timeout {
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break child.wait_with_output(),
                Ok(None) if started.elapsed() >= process_timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return failure_outcome(
                        request,
                        AgentTaskOutcomeStatus::Timeout,
                        AgentTaskFailureClassification::Timeout,
                        "agent_task.provider_timeout",
                        format!(
                            "provider '{}' exceeded timeout_ms={}",
                            provider.id, requested_timeout_ms
                        ),
                        json!({ "provider": provider.id, "command": command, "timeout_ms": requested_timeout_ms, "process_timeout_ms": process_timeout.as_millis() }),
                    );
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => break Err(error),
            }
        }
    } else {
        child.wait_with_output()
    };

    let Ok(output) = output else {
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_io_failed",
            "provider command failed while collecting output".to_string(),
            json!({ "provider": provider.id, "command": command }),
        );
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stdout.is_empty() {
        if let Some(mut outcome) = parse_provider_outcome_from_mixed_output(&stderr) {
            normalize_provider_outcome_roles(&mut outcome, provider);
            return outcome;
        }
        return failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_empty_stdout",
            format!("provider '{}' produced no JSON outcome", provider.id),
            json!({ "provider": provider.id, "command": command, "exit_code": output.status.code(), "stderr": stderr }),
        );
    }

    let parsed: Result<AgentTaskOutcome, _> = serde_json::from_str(&stdout);
    match parsed {
        Ok(mut outcome) => {
            if outcome.schema != AGENT_TASK_OUTCOME_SCHEMA {
                outcome.schema = AGENT_TASK_OUTCOME_SCHEMA.to_string();
            }
            normalize_provider_outcome_roles(&mut outcome, provider);
            outcome
        }
        Err(error) => failure_outcome(
            request,
            AgentTaskOutcomeStatus::ProviderError,
            AgentTaskFailureClassification::Provider,
            "agent_task.provider_malformed_json",
            format!(
                "provider '{}' returned malformed JSON: {error}",
                provider.id
            ),
            json!({ "provider": provider.id, "command": command, "exit_code": output.status.code(), "stderr": stderr, "stdout": stdout }),
        ),
    }
}

fn parse_provider_outcome_from_mixed_output(output: &str) -> Option<AgentTaskOutcome> {
    if output.trim().is_empty() {
        return None;
    }
    if let Ok(outcome) = serde_json::from_str::<AgentTaskOutcome>(output) {
        return Some(outcome);
    }

    for (index, _) in output.match_indices('{') {
        let mut stream =
            serde_json::Deserializer::from_str(&output[index..]).into_iter::<AgentTaskOutcome>();
        if let Some(Ok(outcome)) = stream.next() {
            return Some(outcome);
        }
    }
    None
}

fn normalize_provider_outcome_roles(
    outcome: &mut AgentTaskOutcome,
    provider: &AgentTaskExecutorProvider,
) {
    normalize_provider_artifact_roles(&mut outcome.artifacts, &provider.role_aliases);
    normalize_provider_run_result_output(outcome, &provider.role_aliases);
}

fn normalize_provider_artifact_roles(
    artifacts: &mut [AgentTaskArtifact],
    role_aliases: &AgentTaskProviderRoleAliases,
) {
    for artifact in artifacts {
        let Some(role) = role_aliases.role_for_artifact_kind(&artifact.kind) else {
            continue;
        };
        let original_kind = artifact.kind.clone();
        artifact.kind = role.to_string();
        if !artifact.metadata.is_object() {
            artifact.metadata = json!({});
        }
        if let Some(metadata) = artifact.metadata.as_object_mut() {
            metadata.entry("role".to_string()).or_insert(json!(role));
            metadata
                .entry("provider_kind".to_string())
                .or_insert(json!(original_kind));
        }
    }
}

fn normalize_provider_run_result_output(
    outcome: &mut AgentTaskOutcome,
    role_aliases: &AgentTaskProviderRoleAliases,
) {
    if output_value(&outcome.outputs, "provider_run_result").is_some() {
        return;
    }

    let value = role_aliases
        .output_aliases_for_role("provider_run_result")
        .into_iter()
        .find_map(|alias| output_value(&outcome.outputs, alias))
        .or_else(|| {
            role_aliases
                .metadata_aliases_for_role("provider_run_result")
                .into_iter()
                .find_map(|alias| output_value(&outcome.metadata, alias))
        });

    if let Some(value) = value.cloned() {
        let mut outputs = outcome.outputs.as_object().cloned().unwrap_or_default();
        outputs.insert("provider_run_result".to_string(), value);
        outcome.outputs = Value::Object(outputs);
    }
}

fn output_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|value| !value.is_null())
}

fn render_provider_command(provider: &AgentTaskExecutorProvider) -> String {
    let extension_path = provider.extension_path.as_deref().unwrap_or_default();
    let runtime_path = provider.runtime_path.as_deref().unwrap_or(extension_path);
    provider
        .command
        .replace("{{extension_path}}", extension_path)
        .replace("{{runtime_path}}", runtime_path)
}

fn provider_command_parts(command: &str) -> Option<(String, Vec<String>)> {
    let mut parts = command.split_whitespace().map(str::to_string);
    let program = parts.next()?;
    Some((program, parts.collect()))
}

fn provider_command_env(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> Result<Vec<(String, String)>, AgentTaskSecretResolutionError> {
    let mut env = vec![
        (
            "HOMEBOY_AGENT_TASK_PROVIDER_ID".to_string(),
            provider.id.clone(),
        ),
        (
            "HOMEBOY_AGENT_TASK_EXECUTOR_CONFIG_JSON".to_string(),
            serde_json::to_string(&request.executor.config).unwrap_or_else(|_| "null".to_string()),
        ),
        (
            "HOMEBOY_EXTENSION_ID".to_string(),
            provider.extension_id.clone().unwrap_or_default(),
        ),
        (
            "HOMEBOY_EXTENSION_PATH".to_string(),
            provider.extension_path.clone().unwrap_or_default(),
        ),
        (
            "HOMEBOY_RUNTIME_PATH".to_string(),
            provider
                .runtime_path
                .clone()
                .or_else(|| provider.extension_path.clone())
                .unwrap_or_default(),
        ),
        (
            "HOMEBOY_AI_RUNTIME_ID".to_string(),
            provider.runtime_id.clone().unwrap_or_default(),
        ),
        (
            "HOMEBOY_AI_RUNTIME_PATH".to_string(),
            provider
                .runtime_path
                .clone()
                .or_else(|| provider.extension_path.clone())
                .unwrap_or_default(),
        ),
    ];
    env.extend(resolve_secret_env_with_fallbacks(
        &request.executor.secret_env,
        &provider_secret_sources(provider, Some(request)),
    )?);
    Ok(env)
}

fn failure_outcome(
    request: &AgentTaskRequest,
    status: AgentTaskOutcomeStatus,
    classification: AgentTaskFailureClassification,
    diagnostic_class: &str,
    message: String,
    data: Value,
) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: request.task_id.clone(),
        status,
        summary: Some(message.clone()),
        failure_classification: Some(classification),
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: vec![AgentTaskEvidenceRef {
            kind: "agent-task-provider".to_string(),
            uri: format!("homeboy://agent-task/{}", diagnostic_class),
            label: Some("agent task provider dispatch".to_string()),
        }],
        diagnostics: vec![AgentTaskDiagnostic {
            class: diagnostic_class.to_string(),
            message,
            data,
        }],
        outputs: Value::Null,
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::agent_task_scheduler::{AgentTaskPlan, AgentTaskScheduler};
    use std::fs;

    fn script(body: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "homeboy-agent-task-provider-{}-{}.js",
            std::process::id(),
            body.len()
        ));
        fs::write(&path, body).expect("script written");
        path.to_string_lossy().to_string()
    }

    fn request(task_id: &str, command: String) -> (AgentTaskRequest, AgentTaskExecutorProvider) {
        let provider = AgentTaskExecutorProvider {
            schema: "homeboy/agent-task-executor-provider/v1".to_string(),
            id: "test.provider".to_string(),
            label: None,
            backend: "test".to_string(),
            default_backend: false,
            command,
            request_schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            outcome_schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            capabilities: vec!["structured_outcome".to_string()],
            secret_requirements: Vec::new(),
            secret_env_requirements: Vec::new(),
            workspace_materialization: None,
            provider_defaults: BTreeMap::new(),
            runner_readiness: Vec::new(),
            runner_sources: Vec::new(),
            dependency_failure_patterns: Vec::new(),
            timeout_artifact_discovery: AgentTaskProviderTimeoutArtifactDiscovery::default(),
            role_aliases: AgentTaskProviderRoleAliases::default(),
            extension_id: None,
            extension_path: None,
            runtime_id: None,
            runtime_path: None,
        };
        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: task_id.to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "run".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        };
        (request, provider)
    }

    #[test]
    fn required_extension_ids_follow_selected_agent_task_providers() {
        let (request_a, mut provider_a) = request("task-a", "node provider-a.js".to_string());
        provider_a.id = "provider-a".to_string();
        provider_a.extension_id = Some("extension-a".to_string());
        let (mut request_b, mut provider_b) = request("task-b", "node provider-b.js".to_string());
        request_b.executor.selector = Some("provider-b".to_string());
        provider_b.id = "provider-b".to_string();
        provider_b.extension_id = Some("extension-b".to_string());
        let executor =
            ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

        let extension_ids = executor.required_extension_ids_for_plan(&AgentTaskPlan::new(
            "plan-a",
            vec![request_a, request_b],
        ));

        assert_eq!(extension_ids, vec!["extension-a", "extension-b"]);
    }

    #[test]
    fn provider_selection_matches_exact_backend_first() {
        let (_, mut exact_provider) = request("task-a", "node exact-provider.js".to_string());
        exact_provider.id = "exact-provider".to_string();
        exact_provider.backend = "requested-backend".to_string();
        exact_provider.extension_id = Some("other-extension".to_string());
        let (_, mut extension_provider) =
            request("task-b", "node extension-provider.js".to_string());
        extension_provider.id = "extension-provider".to_string();
        extension_provider.backend = "renamed-backend".to_string();
        extension_provider.extension_id = Some("requested-backend".to_string());

        let providers = [extension_provider, exact_provider];
        let selected = select_provider_by_backend(&providers, "requested-backend", None)
            .expect("provider selected");

        assert_eq!(selected.id, "exact-provider");
    }

    #[test]
    fn provider_selection_matches_unique_extension_alias() {
        let (_, mut provider) = request("task-a", "node provider.js".to_string());
        provider.id = "extension-a.provider".to_string();
        provider.backend = "renamed-backend".to_string();
        provider.extension_id = Some("extension-a".to_string());

        let providers = [provider];
        let selected =
            select_provider_by_backend(&providers, "extension-a", None).expect("provider selected");

        assert_eq!(selected.backend, "renamed-backend");
    }

    #[test]
    fn provider_selection_rejects_ambiguous_extension_alias() {
        let (_, mut provider_a) = request("task-a", "node provider-a.js".to_string());
        provider_a.id = "provider-a".to_string();
        provider_a.backend = "renamed-backend".to_string();
        provider_a.extension_id = Some("extension-a".to_string());
        let (_, mut provider_b) = request("task-b", "node provider-b.js".to_string());
        provider_b.id = "provider-b".to_string();
        provider_b.backend = "fixture".to_string();
        provider_b.extension_id = Some("extension-a".to_string());

        assert!(
            select_provider_by_backend(&[provider_a, provider_b], "extension-a", None).is_none()
        );
    }

    #[test]
    fn provider_selection_applies_selector_to_unique_extension_alias() {
        let (_, mut provider) = request("task-a", "node provider.js".to_string());
        provider.id = "selected-provider".to_string();
        provider.backend = "renamed-backend".to_string();
        provider.extension_id = Some("extension-a".to_string());

        assert!(
            select_provider_by_backend(&[provider.clone()], "extension-a", Some("missing"))
                .is_none()
        );
        assert_eq!(
            select_provider_by_backend(&[provider], "extension-a", Some("selected-provider"))
                .expect("provider selected")
                .id,
            "selected-provider"
        );
    }

    #[test]
    fn provider_manifest_parses_role_aliases() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-executor-provider/v1",
            "id": "custom.provider",
            "backend": "custom",
            "default_backend": true,
            "command": "custom-agent-task",
            "request_schema": AGENT_TASK_REQUEST_SCHEMA,
            "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA,
            "role_aliases": {
                "artifact_kinds": {
                    "patch": ["custom-patch"]
                },
                "artifact_filenames": {
                    "preflight_evidence": ["*-preflight.json"]
                },
                "outputs": {
                    "provider_run_result": ["custom_run_result"]
                },
                "metadata": {
                    "provider_run_result": ["customRunResult"]
                }
            }
        }))
        .expect("provider manifest");

        assert!(provider.default_backend);
        assert!(provider
            .role_aliases
            .artifact_kind_matches_role("patch", "custom-patch"));
        assert!(provider
            .role_aliases
            .artifact_filename_matches_role("preflight_evidence", "runner-preflight.json"));
        assert_eq!(
            provider
                .role_aliases
                .output_aliases_for_role("provider_run_result"),
            vec!["custom_run_result"]
        );
    }

    #[test]
    fn provider_command_receives_canonical_artifact_declarations() {
        let command = format!(
            "node {}",
            script(
                r#"
const fs = require('fs');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
process.stdout.write(JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: input.task_id,
  status: 'succeeded',
  artifacts: [],
  typed_artifacts: [],
  evidence_refs: [],
  diagnostics: [],
  outputs: { artifact_declarations: input.artifact_declarations },
  metadata: null
}));
"#
            )
        );
        let (mut request, provider) = request("task-artifact-normalization", command);
        request.expected_artifacts = vec!["patch".to_string()];

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        assert_eq!(outcome.outputs["artifact_declarations"][0]["name"], "patch");
        assert_eq!(
            outcome.outputs["artifact_declarations"][0]["required"],
            true
        );
    }

    #[test]
    fn provider_manifest_parses_runner_and_dependency_contracts() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-executor-provider/v1",
            "id": "custom.provider",
            "backend": "custom",
            "command": "custom-agent-task",
            "request_schema": AGENT_TASK_REQUEST_SCHEMA,
            "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA,
            "runner_readiness": [{
                "id": "custom.runtime_cache",
                "label": "Custom runtime cache",
                "secret_env": ["CUSTOM_RUNTIME_TOKEN"],
                "env_path": { "env": ["CUSTOM_RUNTIME_BIN"], "revision": true },
                "remediation": "Refresh the custom runtime cache."
            }],
            "dependency_failure_patterns": [{
                "id": "custom.prepared_dependency",
                "label": "Custom prepared dependency",
                "path_contains": "prepared-dependencies/",
                "error_contains_any": ["enoent", "no such file or directory"],
                "remediation": "Refresh prepared dependencies."
            }]
        }))
        .expect("provider manifest");

        assert_eq!(provider.runner_readiness[0].id, "custom.runtime_cache");
        assert_eq!(
            provider.runner_readiness[0].secret_env,
            vec!["CUSTOM_RUNTIME_TOKEN"]
        );
        assert_eq!(
            provider.runner_readiness[0].env_path.as_ref().unwrap().env,
            vec!["CUSTOM_RUNTIME_BIN"]
        );
        assert_eq!(
            provider.dependency_failure_patterns[0].path_contains,
            "prepared-dependencies/"
        );
    }

    #[test]
    fn default_backend_ignores_provider_declaration() {
        let (_request, mut provider_a) = request("task-a", "node provider-a.js".to_string());
        provider_a.backend = "first".to_string();
        let (_request, mut provider_b) = request("task-b", "node provider-b.js".to_string());
        provider_b.backend = "preferred".to_string();
        provider_b.default_backend = true;

        let executor =
            ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

        crate::test_support::with_isolated_home(|_| {
            assert_eq!(executor.default_backend().unwrap(), None);
        });
    }

    #[test]
    fn default_backend_uses_global_config_policy() {
        crate::test_support::with_isolated_home(|_| {
            defaults::save_config(&defaults::HomeboyConfig {
                agent_task: defaults::AgentTaskConfig {
                    default_backend: Some("configured".to_string()),
                    ..defaults::AgentTaskConfig::default()
                },
                ..defaults::HomeboyConfig::default()
            })
            .expect("config saved");

            assert_eq!(default_backend().unwrap().as_deref(), Some("configured"));
        });
    }

    #[test]
    fn default_backend_uses_extension_policy() {
        crate::test_support::with_isolated_home(|home| {
            defaults::save_config(&defaults::HomeboyConfig {
                agent_task: defaults::AgentTaskConfig {
                    default_backend: Some("global-policy".to_string()),
                    ..defaults::AgentTaskConfig::default()
                },
                ..defaults::HomeboyConfig::default()
            })
            .expect("config saved");
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/runtime-extension");
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            std::fs::write(
                extension_dir.join("runtime-extension.json"),
                json!({
                    "name": "Runtime Extension",
                    "version": "1.0.0",
                    "agent_task": { "default_backend": "extension-policy" }
                })
                .to_string(),
            )
            .expect("extension manifest");

            assert_eq!(
                default_backend().unwrap().as_deref(),
                Some("extension-policy")
            );
        });
    }

    #[test]
    fn default_backend_rejects_ambiguous_extension_policy() {
        crate::test_support::with_isolated_home(|home| {
            for (id, backend) in [("runtime-a", "backend-a"), ("runtime-b", "backend-b")] {
                let extension_dir = home.path().join(format!(".config/homeboy/extensions/{id}"));
                std::fs::create_dir_all(&extension_dir).expect("extension dir");
                std::fs::write(
                    extension_dir.join(format!("{id}.json")),
                    json!({
                        "name": id,
                        "version": "1.0.0",
                        "agent_task": { "default_backend": backend }
                    })
                    .to_string(),
                )
                .expect("extension manifest");
            }

            let error = default_backend().expect_err("ambiguous policy should fail");
            assert!(error.message.contains("ambiguous"));
        });
    }

    #[test]
    fn default_backend_reads_component_scoped_extension_policy() {
        let mut component = component::Component::new(
            "fixture".to_string(),
            "/tmp/fixture".to_string(),
            String::new(),
            None,
        );
        component.extensions = Some(std::collections::HashMap::from([(
            "runtime-extension".to_string(),
            component::ScopedExtensionConfig {
                settings: std::collections::HashMap::from([(
                    "agent_task".to_string(),
                    json!({ "default_backend": "component-policy" }),
                )]),
                ..component::ScopedExtensionConfig::default()
            },
        )]));

        assert_eq!(
            component_default_backend(&component).as_deref(),
            Some("component-policy")
        );
    }

    #[test]
    fn default_backend_ignores_provider_manifest_default_backend() {
        crate::test_support::with_isolated_home(|home| {
            let runtime_dir = home
                .path()
                .join(".config/homeboy/agent-runtimes/standalone-runtime");
            std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
            std::fs::write(
                runtime_dir.join("standalone-runtime.json"),
                json!({
                    "schema": agent_runtime_manifest::AGENT_RUNTIME_MANIFEST_SCHEMA,
                    "id": "standalone-runtime",
                    "agent_task_executors": [{
                        "schema": "homeboy/agent-task-executor-provider/v1",
                        "id": "runtime.provider",
                        "backend": "runtime-default",
                        "default_backend": true,
                        "command": "runtime-provider",
                        "request_schema": AGENT_TASK_REQUEST_SCHEMA,
                        "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA
                    }]
                })
                .to_string(),
            )
            .expect("runtime manifest");

            assert_eq!(default_backend().unwrap(), None);
        });
    }

    #[test]
    fn default_backend_is_absent_without_provider_declaration() {
        let (_request, mut provider_a) = request("task-a", "node provider-a.js".to_string());
        provider_a.backend = "first".to_string();
        let (_request, mut provider_b) = request("task-b", "node provider-b.js".to_string());
        provider_b.backend = "second".to_string();

        let executor =
            ExtensionProviderAgentTaskExecutor::with_providers(vec![provider_a, provider_b]);

        assert_eq!(executor.default_backend().unwrap(), None);
    }

    #[test]
    fn provider_command_interpolates_runtime_path_separately_from_extension_path() {
        let (_, mut provider) = request(
            "task-a",
            "{{runtime_path}}/bin/provider --extension {{extension_path}}".to_string(),
        );
        provider.extension_path = Some("/extensions/project-type".to_string());
        provider.runtime_path = Some("/agent-runtimes/example".to_string());

        assert_eq!(
            render_provider_command(&provider),
            "/agent-runtimes/example/bin/provider --extension /extensions/project-type"
        );
    }

    #[test]
    fn provider_runner_secret_env_contracts_are_applied_to_selected_plan_tasks() {
        let (mut request_a, mut provider_a) = request("task-a", "node provider-a.js".to_string());
        request_a.executor.backend = "provider-a".to_string();
        provider_a.backend = "provider-a".to_string();
        provider_a.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
            id: "provider-a.auth".to_string(),
            label: "Provider A auth".to_string(),
            secret_env: vec!["PROVIDER_A_TOKEN".to_string()],
            env_path: None,
            remediation: Some("Configure provider A auth.".to_string()),
        }];
        let (mut request_b, mut provider_b) = request("task-b", "node provider-b.js".to_string());
        request_b.executor.backend = "provider-b".to_string();
        request_b.executor.secret_env = vec!["EXPLICIT_SECRET".to_string()];
        provider_b.backend = "provider-b".to_string();
        provider_b.runner_readiness = vec![AgentTaskProviderRunnerReadiness {
            id: "provider-b.auth".to_string(),
            label: "Provider B auth".to_string(),
            secret_env: vec![
                "PROVIDER_B_TOKEN".to_string(),
                "EXPLICIT_SECRET".to_string(),
            ],
            env_path: None,
            remediation: None,
        }];
        let mut plan = AgentTaskPlan::new("plan-a", vec![request_a, request_b]);

        apply_provider_runner_secret_env_contracts_with_providers(
            &mut plan,
            &[provider_a, provider_b],
        );

        assert_eq!(
            plan.tasks[0].executor.secret_env,
            vec!["PROVIDER_A_TOKEN".to_string()]
        );
        assert_eq!(
            plan.tasks[1].executor.secret_env,
            vec![
                "EXPLICIT_SECRET".to_string(),
                "PROVIDER_B_TOKEN".to_string()
            ]
        );
    }

    #[test]
    fn discovers_agent_task_providers_from_agent_runtime_manifests() {
        crate::test_support::with_isolated_home(|home| {
            let runtime_dir = home
                .path()
                .join(".config/homeboy/agent-runtimes/custom-runtime");
            fs::create_dir_all(&runtime_dir).expect("runtime dir");
            fs::write(
                runtime_dir.join("custom-runtime.json"),
                serde_json::to_string(&json!({
                    "schema": agent_runtime_manifest::AGENT_RUNTIME_MANIFEST_SCHEMA,
                    "id": "custom-runtime",
                    "name": "Custom Runtime",
                    "version": "1.0.0",
                    "agent_task_executors": [{
                        "schema": "homeboy/agent-task-executor-provider/v1",
                        "id": "custom.runtime.executor",
                        "backend": "custom",
                        "command": "node {{runtime_path}}/runner.cjs",
                        "request_schema": AGENT_TASK_REQUEST_SCHEMA,
                        "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA
                    }]
                }))
                .unwrap(),
            )
            .expect("runtime manifest");

            let providers = discover_agent_task_executor_providers();

            assert_eq!(providers.len(), 1);
            assert_eq!(providers[0].id, "custom.runtime.executor");
            assert_eq!(providers[0].runtime_id.as_deref(), Some("custom-runtime"));
            assert_eq!(
                providers[0].runtime_path.as_deref(),
                Some(runtime_dir.to_string_lossy().as_ref())
            );
            assert_eq!(
                render_provider_command(&providers[0]),
                format!("node {}/runner.cjs", runtime_dir.display())
            );
        });
    }

    #[test]
    fn provider_command_env_includes_runtime_identity() {
        let (request, mut provider) =
            request("task-a", "node {{runtime_path}}/provider.js".to_string());
        provider.runtime_id = Some("custom-runtime".to_string());
        provider.runtime_path = Some("/tmp/custom-runtime".to_string());

        let env = provider_command_env(&request, &provider).expect("provider env");

        assert!(env.contains(&(
            "HOMEBOY_AI_RUNTIME_ID".to_string(),
            "custom-runtime".to_string()
        )));
        assert!(env.contains(&(
            "HOMEBOY_AI_RUNTIME_PATH".to_string(),
            "/tmp/custom-runtime".to_string()
        )));
        assert_eq!(
            render_provider_command(&provider),
            "node /tmp/custom-runtime/provider.js"
        );
    }

    #[test]
    fn provider_outcome_roles_normalize_from_declared_aliases() {
        let (_, mut provider) = request("task-a", "node provider.js".to_string());
        provider.role_aliases = serde_json::from_value(json!({
            "artifact_kinds": {
                "patch": ["custom-patch"]
            },
            "outputs": {
                "provider_run_result": ["custom_run_result"]
            }
        }))
        .expect("role aliases");
        let patch_path = std::env::temp_dir().join("custom.patch");
        fs::write(&patch_path, "diff --git a/a b/a\n").expect("patch");
        let mut outcome = AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: None,
            failure_classification: None,
            artifacts: vec![fixture_artifact(
                "patch",
                "custom-patch",
                &patch_path,
                Some("text/x-patch"),
            )],
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: json!({
                "custom_run_result": {
                    "run_id": "custom-run-1"
                }
            }),
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        };

        normalize_provider_outcome_roles(&mut outcome, &provider);

        assert_eq!(outcome.artifacts[0].kind, "patch");
        assert_eq!(
            outcome.artifacts[0].metadata["provider_kind"],
            "custom-patch"
        );
        assert_eq!(
            outcome.outputs["provider_run_result"]["run_id"],
            "custom-run-1"
        );
        assert_eq!(
            outcome.outputs["custom_run_result"]["run_id"],
            "custom-run-1"
        );
    }

    #[test]
    fn provider_workspace_materialization_declares_cwd_git_checkout_requirement() {
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
            cwd: Some("git_checkout".to_string()),
            requires_git: None,
            write_scope: None,
            artifact_paths: Vec::new(),
        });

        assert!(provider_requires_cwd_git_checkout_with_providers(
            &[provider],
            "test",
            None
        ));
    }

    #[test]
    fn provider_default_secret_sources_resolve_required_env_without_duplicate_mapping() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let auth_path = temp.path().join("codex-auth.json");
            fs::write(
                &auth_path,
                json!({
                    "tokens": {
                        "access_token": "provider-owned-access-token",
                        "refresh_token": "provider-owned-refresh-token"
                    }
                })
                .to_string(),
            )
            .expect("write auth");
            let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
            request.executor.config = json!({ "provider": "codex" });
            request.executor.secret_env = vec![
                "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
                "AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN".to_string(),
            ];
            provider.provider_defaults.insert(
                "codex".to_string(),
                json!({
                    "secret_env": request.executor.secret_env,
                    "secret_env_sources": {
                        "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN": {
                            "source": "json-file",
                            "path": auth_path,
                            "field": "tokens.access_token"
                        },
                        "AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN": {
                            "source": "json-file",
                            "path": auth_path,
                            "field": "tokens.refresh_token"
                        }
                    }
                }),
            );

            let env = provider_command_env(&request, &provider).expect("provider env resolves");

            assert!(env.contains(&(
                "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
                "provider-owned-access-token".to_string()
            )));
            let rendered =
                serde_json::to_string(&provider_secret_sources(&provider, Some(&request)))
                    .expect("sources json");
            assert!(!rendered.contains("provider-owned-access-token"));
        });
    }

    #[test]
    fn provider_secret_sources_for_providers_include_default_json_sources() {
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.provider_defaults.insert(
            "codex".to_string(),
            json!({
                "secret_env": ["AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN"],
                "secret_env_sources": {
                    "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN": {
                        "source": "json-file",
                        "path": "~/.codex/auth.json",
                        "field": "tokens.access_token"
                    }
                }
            }),
        );

        let sources = provider_secret_sources_for_providers(&[provider]);

        let source = sources
            .get("AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN")
            .expect("provider default source discovered");
        assert_eq!(source.source, "json-file");
        assert_eq!(source.path.as_deref(), Some("~/.codex/auth.json"));
        assert_eq!(source.field.as_deref(), Some("tokens.access_token"));
    }

    #[test]
    fn provider_default_secret_sources_accept_nested_json_sources() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let auth_path = temp.path().join("provider-auth.json");
            fs::write(
                &auth_path,
                json!({
                    "provider": {
                        "access": "provider-access-token",
                        "refresh": "provider-refresh-token",
                        "expires": 12345
                    }
                })
                .to_string(),
            )
            .expect("write auth");
            let auth_path = auth_path.to_string_lossy().to_string();
            let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
            request.executor.config = json!({ "provider": "example-oauth" });
            request.executor.secret_env = vec![
                "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
                "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
                "EXAMPLE_PROVIDER_EXPIRES_AT".to_string(),
            ];
            provider.provider_defaults.insert(
                "example-oauth".to_string(),
                json!({
                    "secret_env": [
                        "EXAMPLE_PROVIDER_ACCESS_TOKEN",
                        "EXAMPLE_PROVIDER_REFRESH_TOKEN",
                        "EXAMPLE_PROVIDER_EXPIRES_AT"
                    ],
                    "secret_env_sources": {
                        "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                            "source": "json-file",
                            "path": auth_path.clone(),
                            "field": "provider.access"
                        },
                        "EXAMPLE_PROVIDER_REFRESH_TOKEN": {
                            "source": "json-file",
                            "path": auth_path.clone(),
                            "field": "provider.refresh"
                        },
                        "EXAMPLE_PROVIDER_EXPIRES_AT": {
                            "source": "json-file",
                            "path": auth_path.clone(),
                            "field": "provider.expires"
                        }
                    }
                }),
            );

            let env = provider_command_env(&request, &provider).expect("provider env resolves");

            assert!(env.contains(&(
                "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
                "provider-refresh-token".to_string()
            )));
            assert!(env.contains(&(
                "EXAMPLE_PROVIDER_EXPIRES_AT".to_string(),
                "12345".to_string()
            )));
        });
    }

    #[test]
    fn provider_default_secret_sources_feed_secret_readiness_status() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let auth_path = temp.path().join("codex-auth.json");
            fs::write(
                &auth_path,
                json!({
                    "tokens": {
                        "access_token": "provider-owned-access-token"
                    }
                })
                .to_string(),
            )
            .expect("write auth");
            let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
            provider.provider_defaults.insert(
                "codex".to_string(),
                json!({
                    "secret_env_sources": {
                        "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN": {
                            "source": "json-file",
                            "path": auth_path,
                            "field": "tokens.access_token"
                        }
                    }
                }),
            );
            let fallback_sources = provider_secret_sources_for_providers(&[provider]);

            let status = crate::core::agent_task_secrets::secret_env_status_with_fallbacks(
                &["AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string()],
                &fallback_sources,
            );

            assert_eq!(status.len(), 1);
            assert!(status[0].configured);
            assert_eq!(status[0].source, "json-file");
        });
    }

    #[test]
    fn provider_workspace_materialization_declares_requires_git_requirement() {
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
            cwd: None,
            requires_git: Some(true),
            write_scope: Some("artifacts".to_string()),
            artifact_paths: vec![".homeboy/provider".to_string()],
        });

        assert!(provider_requires_cwd_git_checkout_with_providers(
            &[provider],
            "test",
            None
        ));
    }

    #[test]
    fn provider_workspace_materialization_ignores_unselected_provider() {
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
            cwd: Some("git_checkout".to_string()),
            requires_git: None,
            write_scope: None,
            artifact_paths: Vec::new(),
        });

        assert!(!provider_requires_cwd_git_checkout_with_providers(
            &[provider],
            "other",
            None
        ));
    }

    #[test]
    fn provider_secret_contracts_are_applied_generically() {
        let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
        request.executor.config = json!({ "provider": "example-provider" });
        provider.secret_requirements = vec![
            AgentTaskProviderSecretRequirement {
                name: Some("REQUIRED_TOKEN".to_string()),
                required: Some(true),
                ..AgentTaskProviderSecretRequirement::default()
            },
            AgentTaskProviderSecretRequirement {
                name: Some("OPTIONAL_TOKEN".to_string()),
                required: Some(false),
                ..AgentTaskProviderSecretRequirement::default()
            },
        ];
        provider.secret_env_requirements = vec![AgentTaskProviderSecretEnvRequirement {
            env: vec!["EXAMPLE_PROVIDER_TOKEN".to_string()],
            when: Some(json!({
                "any": [
                    { "path": "executor.config.provider", "equals": "example-provider" },
                    { "path": "provider", "equals": "example-provider" }
                ]
            })),
            ..AgentTaskProviderSecretEnvRequirement::default()
        }];
        provider.provider_defaults.insert(
            "example-provider".to_string(),
            json!({ "secret_env": ["EXAMPLE_PROVIDER_REFRESH_TOKEN"] }),
        );
        let mut plan = AgentTaskPlan::new("plan-a", vec![request]);

        apply_provider_runner_secret_env_contracts_with_providers(&mut plan, &[provider]);

        assert_eq!(
            plan.tasks[0].executor.secret_env,
            vec![
                "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
                "EXAMPLE_PROVIDER_TOKEN".to_string(),
                "REQUIRED_TOKEN".to_string(),
            ]
        );
    }

    #[test]
    fn scheduler_dispatches_extension_provider_command() {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'succeeded',summary:'ok',outputs:{issue_number:3447}}));")
        );
        let (request, provider) = request("task-a", command);
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-a", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::Succeeded
        );
        assert_eq!(aggregate.outcomes[0].outputs["issue_number"], json!(3447));
    }

    #[test]
    fn scheduler_reports_missing_extension_provider() {
        let (request, _provider) = request("task-missing-provider", "unused".to_string());
        let scheduler = AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::default());

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-missing-provider", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::CapabilityMissing)
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.provider_missing"
        );
    }

    #[test]
    fn scheduler_reports_missing_provider_capability() {
        let (mut request, provider) = request("task-missing-capability", "unused".to_string());
        request.executor.required_capabilities = vec!["workspace_write".to_string()];
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-missing-capability", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::CapabilityMissing)
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.capability_missing"
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].data["missing_capabilities"],
            json!(["workspace_write"])
        );
    }

    #[test]
    fn scheduler_normalizes_malformed_provider_output() {
        let command = format!("node {}", script("process.stdout.write('{not json');"));
        let (request, provider) = request("task-malformed-provider", command);
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-malformed-provider", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Provider)
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.provider_malformed_json"
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].data["stdout"],
            "{not json"
        );
    }

    #[test]
    fn provider_preserves_structured_outcome_from_stderr_when_stdout_empty() {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stderr.write('diagnostic prefix\\n' + JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'failed',summary:'captured provider evidence',failure_classification:'provider',diagnostics:[{class:'codebox.empty_data_packet_returned',message:'empty data packet returned',data:{typed_artifacts:{}}}]}));")
        );
        let (request, provider) = request("task-stderr-outcome", command);

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Failed);
        assert_eq!(
            outcome.summary.as_deref(),
            Some("captured provider evidence")
        );
        assert_eq!(
            outcome.diagnostics[0].class,
            "codebox.empty_data_packet_returned"
        );
        assert_eq!(outcome.diagnostics[0].data["typed_artifacts"], json!({}));
    }

    #[test]
    fn provider_timeout_returns_structured_outcome() {
        let command = format!("node {}", script("setInterval(() => {}, 1000);"));
        let (mut request, provider) = request("task-timeout", command);
        request.limits.timeout_ms = Some(50);
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-timeout", vec![request]));

        assert_eq!(aggregate.totals.timed_out, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Timeout)
        );
    }

    #[test]
    fn provider_can_return_timeout_payload_during_wrapper_grace() {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); setTimeout(()=>process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'timeout',summary:'provider serialized timeout',failure_classification:'timeout',artifacts:[{schema:'homeboy/agent-task-artifact/v1',id:'timeout-evidence',kind:'provider-task-runner-preflight',path:'/tmp/timeout-evidence.json'}]})), 3050);")
        );
        let (mut request, provider) = request("task-timeout-payload", command);
        request.limits.timeout_ms = Some(3000);

        let outcome = run_provider_command(&request, &provider);

        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Timeout);
        assert_eq!(
            outcome.summary.as_deref(),
            Some("provider serialized timeout")
        );
        assert_eq!(outcome.artifacts.len(), 1);
        assert_eq!(outcome.artifacts[0].id, "timeout-evidence");
    }

    #[test]
    fn provider_command_receives_executor_config_env() {
        let command = format!(
            "node {}",
            script("let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); let config=JSON.parse(process.env.HOMEBOY_AGENT_TASK_EXECUTOR_CONFIG_JSON); process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:config.marker==='configured'?'succeeded':'failed',summary:process.env.HOMEBOY_AGENT_TASK_PROVIDER_ID}));")
        );
        let (mut request, mut provider) = request("task-config", command);
        request.executor.config = json!({ "marker": "configured" });
        provider.extension_id = Some("wordpress".to_string());
        provider.extension_path = Some("/tmp/homeboy-extension".to_string());
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-config", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(
            aggregate.outcomes[0].summary.as_deref(),
            Some("test.provider")
        );
    }

    #[test]
    fn provider_command_receives_declared_secret_env() {
        let secret_name = format!("HOMEBOY_TEST_AGENT_TASK_SECRET_{}", std::process::id());
        std::env::set_var(&secret_name, "hydrated-secret");
        let command = format!(
            "node {}",
            script(&format!(
                "let fs=require('fs'); let req=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:process.env.{secret_name}==='hydrated-secret'?'succeeded':'failed',summary:'checked'}}));"
            ))
        );
        let (mut request, provider) = request("task-secret-env", command);
        request.executor.secret_env = vec![secret_name.clone()];
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-secret-env", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        std::env::remove_var(secret_name);
    }

    #[test]
    fn missing_declared_secret_env_fails_before_provider_spawn() {
        let secret_name = format!(
            "HOMEBOY_TEST_MISSING_AGENT_TASK_SECRET_{}",
            std::process::id()
        );
        std::env::remove_var(&secret_name);
        let command = format!(
            "node {}",
            script("throw new Error('provider should not run');")
        );
        let (mut request, provider) = request("task-missing-secret-env", command);
        request.executor.secret_env = vec![secret_name.clone()];
        let scheduler =
            AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::with_providers(vec![
                provider,
            ]));

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-missing-secret-env", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::InvalidInput)
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.secret_env_missing"
        );
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].data["missing_secret_env"],
            json!([secret_name])
        );
    }

    #[test]
    fn fixture_backend_produces_deterministic_smoke_artifacts() {
        let artifact_root = tempfile::tempdir().expect("artifact root");
        let (mut request, _provider) = request("task-fixture", "unused".to_string());
        request.executor.backend = "fixture".to_string();
        request.executor.config = json!({
            "artifact_root": artifact_root.path().display().to_string(),
            "changed_file": "docs/smoke.md"
        });
        let scheduler = AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::default());

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-fixture", vec![request]));

        assert_eq!(aggregate.totals.succeeded, 1);
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
        assert!(outcome.artifacts.iter().any(
            |artifact| artifact.kind == "patch" && artifact.size_bytes.unwrap_or_default() > 0
        ));
        assert!(outcome
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "agent_result"));
        assert!(outcome
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "transcript"));
    }

    #[test]
    fn fixture_backend_classifies_empty_runtime_bundle() {
        let artifact_root = tempfile::tempdir().expect("artifact root");
        let (mut request, _provider) = request("task-empty-runtime", "unused".to_string());
        request.executor.backend = "fixture".to_string();
        request.executor.config = json!({
            "artifact_root": artifact_root.path().display().to_string(),
            "mode": "empty_runtime_bundle"
        });
        let scheduler = AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::default());

        let aggregate = scheduler.run(AgentTaskPlan::new("plan-empty-runtime", vec![request]));

        assert_eq!(aggregate.totals.failed, 1);
        assert_eq!(
            aggregate.outcomes[0].diagnostics[0].class,
            "agent_task.fixture_empty_runtime_bundle"
        );
        assert!(aggregate.outcomes[0]
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "runtime_bundle"));
    }
}
