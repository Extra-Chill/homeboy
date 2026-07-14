use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskExecutor {
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, alias = "runtime", skip_serializing_if = "Option::is_none")]
    pub runtime_selection: Option<AgentTaskRuntimeSelection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub config: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, alias = "backend", skip_serializing_if = "Option::is_none")]
    pub executor_backend: Option<String>,
    #[serde(default, alias = "selector", skip_serializing_if = "Option::is_none")]
    pub executor_provider_id: Option<String>,
    #[serde(default, alias = "provider", skip_serializing_if = "Option::is_none")]
    pub ai_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub substrate_ref: Option<String>,
}

impl AgentTaskExecutor {
    /// Remap the explicitly supported provider workspace configuration fields.
    ///
    /// Providers receive the task workspace as the authoritative root. Providers
    /// that retain a workspace root in their config must use one of these
    /// declared shapes rather than relying on ambient config key matching.
    pub fn remap_workspace_root(&mut self, root: &str) {
        let Some(config) = self.config.as_object_mut() else {
            return;
        };

        if let Some(workspace) = config.get_mut("workspace").and_then(Value::as_object_mut) {
            if workspace.contains_key("root") {
                workspace.insert("root".to_string(), Value::String(root.to_string()));
            }
        }
        if config.contains_key("workspace_root") {
            config.insert(
                "workspace_root".to_string(),
                Value::String(root.to_string()),
            );
        }
    }

    pub fn runtime_selection(&self) -> AgentTaskRuntimeSelection {
        let explicit = self.runtime_selection.clone().unwrap_or_default();
        AgentTaskRuntimeSelection {
            runtime_id: explicit.runtime_id,
            executor_backend: explicit
                .executor_backend
                .or_else(|| Some(self.backend.clone())),
            executor_provider_id: explicit
                .executor_provider_id
                .or_else(|| self.selector.clone()),
            ai_provider_id: explicit
                .ai_provider_id
                .or_else(|| config_string(&self.config, "provider")),
            model: explicit.model.or_else(|| self.model.clone()),
            substrate_ref: explicit.substrate_ref,
        }
    }

    pub fn runtime_id(&self) -> Option<&str> {
        self.runtime_selection
            .as_ref()
            .and_then(|selection| selection.runtime_id.as_deref())
    }

    pub fn executor_backend(&self) -> &str {
        self.runtime_selection
            .as_ref()
            .and_then(|selection| selection.executor_backend.as_deref())
            .unwrap_or(&self.backend)
    }

    pub fn provider(&self) -> Option<&str> {
        self.runtime_selection
            .as_ref()
            .and_then(|selection| selection.ai_provider_id.as_deref())
            .or_else(|| config_str(&self.config, "provider"))
    }

    pub fn model(&self) -> Option<&str> {
        self.runtime_selection
            .as_ref()
            .and_then(|selection| selection.model.as_deref())
            .or(self.model.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn remaps_only_declared_provider_workspace_fields() {
        let mut executor = AgentTaskExecutor {
            backend: "test".to_string(),
            selector: None,
            runtime_selection: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            model: None,
            config: json!({
                "workspace": { "root": "/original", "label": "keep" },
                "workspace_root": "/original",
                "unrelated_root": "/original"
            }),
        };

        executor.remap_workspace_root("/candidate");

        assert_eq!(executor.config["workspace"]["root"], "/candidate");
        assert_eq!(executor.config["workspace_root"], "/candidate");
        assert_eq!(executor.config["unrelated_root"], "/original");
    }
}

fn config_str<'a>(config: &'a Value, key: &str) -> Option<&'a str> {
    config.get(key).and_then(Value::as_str)
}

fn config_string(config: &Value, key: &str) -> Option<String> {
    config_str(config, key).map(str::to_string)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskSourceRef {
    pub kind: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskWorkspace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub mode: AgentTaskWorkspaceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub materialization: Value,
}

impl Default for AgentTaskWorkspace {
    fn default() -> Self {
        Self {
            kind: None,
            mode: AgentTaskWorkspaceMode::Ephemeral,
            root: None,
            slug: None,
            component_id: None,
            branch: None,
            base_ref: None,
            task_url: None,
            cleanup: None,
            materialization: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AgentTaskWorkspaceMode {
    #[default]
    Ephemeral,
    Existing,
    Materialized,
}
