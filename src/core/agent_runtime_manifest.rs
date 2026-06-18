use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::core::agent_task_provider::{
    AgentTaskExecutorProvider, AgentTaskProviderRunnerReadiness, AgentTaskProviderRunnerSource,
    AgentTaskProviderWorkspaceMaterialization,
};
use crate::core::extension::{load_all_extensions, ExtensionManifest};
use crate::core::{config, paths};

pub const AGENT_RUNTIME_MANIFEST_SCHEMA: &str = "homeboy/agent-runtime-manifest/v1";
pub const AGENT_RUNTIME_MATERIALIZATION_PLAN_SCHEMA: &str =
    "homeboy/agent-runtime-materialization-plan/v1";

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
            && self.env_passthrough.is_empty()
            && self.workspace.is_none()
            && self.extra.is_empty()
    }
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

pub fn runtime_materialization_plan(
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

pub fn discover_agent_runtime_manifests() -> Vec<AgentRuntimeManifest> {
    let mut manifests = discover_standalone_agent_runtime_manifests();
    manifests.extend(discover_agent_runtime_manifests_from_extensions(
        &load_all_extensions().unwrap_or_default(),
    ));
    manifests
}

fn discover_standalone_agent_runtime_manifests() -> Vec<AgentRuntimeManifest> {
    let mut manifests = Vec::new();
    if let Ok(runtime_dir) = paths::agent_runtimes() {
        manifests.extend(discover_standalone_agent_runtime_manifests_in(
            runtime_dir,
            paths::agent_runtime_manifest,
        ));
    }
    manifests.sort_by(|a, b| a.id.cmp(&b.id));
    manifests
}

fn discover_standalone_agent_runtime_manifests_in(
    runtime_dir: PathBuf,
    manifest_path_for: fn(&str) -> crate::core::Result<PathBuf>,
) -> Vec<AgentRuntimeManifest> {
    let Ok(entries) = std::fs::read_dir(runtime_dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| {
            load_standalone_agent_runtime_manifest(&entry.path(), manifest_path_for)
        })
        .collect()
}

fn load_standalone_agent_runtime_manifest(
    path: &Path,
    manifest_path_for: fn(&str) -> crate::core::Result<PathBuf>,
) -> Option<AgentRuntimeManifest> {
    if !path.is_dir() {
        return None;
    }
    let id = path.file_name()?.to_string_lossy().to_string();
    let manifest_path = manifest_path_for(&id).ok()?;
    if !manifest_path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(manifest_path).ok()?;
    let mut manifest: AgentRuntimeManifest = config::from_str(&content).ok()?;
    manifest.id = id;
    manifest.extension_id = None;
    manifest.extension_path = None;
    manifest.runtime_path = Some(path.to_string_lossy().to_string());
    Some(manifest)
}

pub(crate) fn discover_agent_runtime_manifests_from_extensions(
    extensions: &[ExtensionManifest],
) -> Vec<AgentRuntimeManifest> {
    let mut runtime_manifests = Vec::new();
    for extension in extensions {
        for runtime in &extension.agent_runtimes {
            let providers = parse_agent_task_executor_providers(&runtime.agent_task_executors);
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
    runtime_manifests
}

pub(crate) fn discover_agent_task_executor_providers() -> Vec<AgentTaskExecutorProvider> {
    let mut providers = Vec::new();
    for runtime_manifest in discover_agent_runtime_manifests() {
        for mut provider in runtime_manifest.agent_task_executors {
            provider.extension_id = runtime_manifest.extension_id.clone();
            provider.extension_path = runtime_manifest.extension_path.clone();
            provider.runtime_id = Some(runtime_manifest.id.clone());
            provider.runtime_path = runtime_manifest.runtime_path.clone();
            providers.push(provider);
        }
    }
    providers
}

fn parse_agent_task_executor_providers(values: &[Value]) -> Vec<AgentTaskExecutorProvider> {
    values
        .iter()
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect()
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

        let manifests = discover_agent_runtime_manifests_from_extensions(&[extension]);

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

            let manifests = discover_standalone_agent_runtime_manifests();

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
    fn wp_codebox_runtime_materialization_fixture_is_plain_data() {
        let manifest: AgentRuntimeManifest = serde_json::from_str(include_str!(
            "../../tests/fixtures/wp_codebox_runtime_materialization_manifest.json"
        ))
        .expect("wp-codebox fixture parses");

        let materialization_plan = runtime_materialization_plan(&manifest);

        assert_eq!(manifest.id, "wp-codebox");
        assert_eq!(materialization_plan.runtime_id, "wp-codebox");
        assert_eq!(materialization_plan.source_roots[0].id, "wp-codebox");
        assert_eq!(
            materialization_plan.source_roots[0].remote_url.as_deref(),
            Some("https://github.com/Automattic/wp-codebox.git")
        );
        assert_eq!(
            materialization_plan.executable_requirements[0].env,
            vec!["WP_CODEBOX_BIN".to_string()]
        );
        assert_eq!(
            materialization_plan.readiness_checks[0].label,
            "WP Codebox runtime available"
        );
        assert_eq!(
            materialization_plan.env_passthrough,
            vec![
                "AI_PROVIDER_OPENAI_CODEX_API_KEY".to_string(),
                "WP_CODEBOX_BIN".to_string(),
                "WP_CODEBOX_HOME".to_string()
            ]
        );
    }
}
