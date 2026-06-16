use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent_task_provider::AgentTaskExecutorProvider;
use crate::core::extension::{load_all_extensions, ExtensionManifest};

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
}

pub fn discover_agent_runtime_manifests() -> Vec<AgentRuntimeManifest> {
    discover_agent_runtime_manifests_from_extensions(&load_all_extensions().unwrap_or_default())
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
            });
        }

        let Some(legacy_providers) = extension.extra.get("agent_task_executors") else {
            continue;
        };
        let Ok(providers) =
            serde_json::from_value::<Vec<AgentTaskExecutorProvider>>(legacy_providers.clone())
        else {
            continue;
        };
        if providers.is_empty() {
            continue;
        }
        runtime_manifests.push(AgentRuntimeManifest {
            schema: AGENT_RUNTIME_MANIFEST_SCHEMA.to_string(),
            id: format!("{}.legacy-agent-task-executors", extension.id),
            label: Some("Legacy agent-task executors".to_string()),
            agent_task_executors: providers,
            extension_id: Some(extension.id.clone()),
            extension_path: extension.extension_path.clone(),
        });
    }
    runtime_manifests
}

pub(crate) fn discover_agent_task_executor_providers() -> Vec<AgentTaskExecutorProvider> {
    let mut providers = Vec::new();
    for runtime_manifest in discover_agent_runtime_manifests() {
        for mut provider in runtime_manifest.agent_task_executors {
            provider.extension_id = runtime_manifest.extension_id.clone();
            provider.extension_path = runtime_manifest.extension_path.clone();
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
    }

    #[test]
    fn keeps_legacy_agent_task_executor_manifest_discovery() {
        let mut extension = extension("legacy-extension");
        extension.extra.insert(
            "agent_task_executors".to_string(),
            json!([provider_json("legacy.default", "legacy")]),
        );

        let manifests = discover_agent_runtime_manifests_from_extensions(&[extension]);

        assert_eq!(manifests.len(), 1);
        assert_eq!(
            manifests[0].id,
            "legacy-extension.legacy-agent-task-executors"
        );
        assert_eq!(manifests[0].agent_task_executors[0].backend, "legacy");
    }
}
