use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::core::agent_task_provider::AgentTaskExecutorProvider;
use crate::core::extension::{load_all_extensions, ExtensionManifest};
use crate::core::{config, paths};

pub const AGENT_RUNTIME_MANIFEST_SCHEMA: &str = "homeboy/agent-runtime-manifest/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeManifest {
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_task_executors: Vec<AgentTaskExecutorProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<String>,
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
                extension_id: Some(extension.id.clone()),
                extension_path: extension.extension_path.clone(),
                runtime_path: extension.extension_path.clone(),
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
            "schema": "homeboy/agent-task-executor-provider/v1",
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
                "id": "codex-runtime",
                "label": "Codex Runtime",
                "agent_task_executors": [provider_json("codex.default", "codex")]
            }))
            .expect("runtime manifest"),
        );

        let manifests = discover_agent_runtime_manifests_from_extensions(&[extension]);

        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].schema, AGENT_RUNTIME_MANIFEST_SCHEMA);
        assert_eq!(manifests[0].id, "codex-runtime");
        assert_eq!(
            manifests[0].extension_id.as_deref(),
            Some("runtime-extension")
        );
        assert_eq!(manifests[0].agent_task_executors[0].backend, "codex");
        assert_eq!(
            manifests[0].runtime_path.as_deref(),
            Some("/extensions/runtime-extension")
        );
    }

    #[test]
    fn discovers_standalone_agent_runtime_manifests() {
        crate::test_support::with_isolated_home(|home| {
            let runtime_dir = home
                .path()
                .join(".config/homeboy/agent-runtimes")
                .join("standalone-codex");
            std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
            std::fs::write(
                runtime_dir.join("standalone-codex.json"),
                json!({
                    "schema": AGENT_RUNTIME_MANIFEST_SCHEMA,
                    "id": "ignored-on-disk",
                    "name": "Standalone Codex Package",
                    "version": "1.0.0",
                    "description": "Standalone runtime package fixture.",
                    "label": "Standalone Codex",
                    "agent_task_executors": [{
                        "schema": "homeboy/agent-task-executor-provider/v1",
                        "id": "standalone-codex.default",
                        "backend": "codex",
                        "command": "agent-task-provider",
                        "request_schema": AGENT_TASK_REQUEST_SCHEMA,
                        "outcome_schema": AGENT_TASK_OUTCOME_SCHEMA,
                        "workspace_materialization": {
                            "cwd": "git_checkout",
                            "requires_git": true,
                            "write_scope": "artifacts",
                            "artifact_paths": [".homeboy/codex"]
                        },
                        "secret_requirements": [{
                            "name": "CODEX_TOKEN",
                            "required": true,
                            "purpose": "fixture"
                        }],
                        "secret_env_requirements": [{
                            "schema": "homeboy/secret-env-requirement/v1",
                            "env": ["CODEX_REFRESH_TOKEN"],
                            "when": { "path": "executor.config.provider", "equals": "codex" }
                        }],
                        "provider_defaults": {
                            "codex": { "secret_env": ["CODEX_REFRESH_TOKEN"] }
                        }
                    }]
                })
                .to_string(),
            )
            .expect("runtime manifest");

            let manifests = discover_standalone_agent_runtime_manifests();

            assert_eq!(manifests.len(), 1);
            assert_eq!(manifests[0].id, "standalone-codex");
            assert_eq!(manifests[0].extension_id, None);
            assert_eq!(manifests[0].extension_path, None);
            assert_eq!(
                manifests[0].runtime_path.as_deref(),
                Some(runtime_dir.to_str().expect("runtime dir utf-8"))
            );
            assert_eq!(manifests[0].agent_task_executors[0].backend, "codex");
            let provider = &manifests[0].agent_task_executors[0];
            let materialization = provider
                .workspace_materialization
                .as_ref()
                .expect("workspace materialization");
            assert_eq!(materialization.requires_git, Some(true));
            assert_eq!(materialization.write_scope.as_deref(), Some("artifacts"));
            assert_eq!(materialization.artifact_paths, vec![".homeboy/codex"]);
            assert_eq!(
                provider.secret_requirements[0].name.as_deref(),
                Some("CODEX_TOKEN")
            );
            assert_eq!(
                provider.secret_env_requirements[0].env,
                vec!["CODEX_REFRESH_TOKEN"]
            );
            assert_eq!(
                provider.provider_defaults["codex"]["secret_env"][0],
                "CODEX_REFRESH_TOKEN"
            );
        });
    }
}
