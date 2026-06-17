use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::core::agent_task::{
    AgentTaskArtifactDeclaration, AgentTaskComponentContract, AgentTaskExecutor, AgentTaskLimits,
    AgentTaskPolicy, AgentTaskRequest, AgentTaskRuntimeSelection, AgentTaskSourceRef,
    AgentTaskWorkspace, AgentTaskWorkspaceMode,
};
use crate::core::agent_task_schedule::{AgentTaskPlan, AgentTaskScheduleOptions};

pub const WORDPRESS_RUNTIME_PLAN_REQUEST_SCHEMA: &str = "homeboy/wordpress-runtime-plan-request/v1";
pub const WORDPRESS_RUNTIME_TASK_SCHEMA: &str = "homeboy/wordpress-runtime-task/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WordPressRuntimePlanRequest {
    #[serde(default = "default_plan_request_schema")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    pub tasks: Vec<WordPressRuntimeTaskSpec>,
    #[serde(default)]
    pub options: AgentTaskScheduleOptions,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_contracts: Vec<AgentTaskComponentContract>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WordPressRuntimeTaskSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub runtime_task: Value,
    #[serde(default)]
    pub executor: WordPressRuntimeExecutorSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<AgentTaskSourceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_contracts: Vec<AgentTaskComponentContract>,
    #[serde(default)]
    pub policy: AgentTaskPolicy,
    #[serde(default)]
    pub limits: AgentTaskLimits,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_declarations: Vec<AgentTaskArtifactDeclaration>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WordPressRuntimeExecutorSpec {
    #[serde(default = "default_executor_backend")]
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub substrate_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env: Vec<String>,
}

impl Default for WordPressRuntimeExecutorSpec {
    fn default() -> Self {
        Self {
            backend: default_executor_backend(),
            selector: None,
            runtime_id: Some(default_runtime_id()),
            provider: None,
            model: None,
            substrate_ref: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
        }
    }
}

pub fn build_wordpress_runtime_plan(mut request: WordPressRuntimePlanRequest) -> AgentTaskPlan {
    let plan_id = request
        .plan_id
        .clone()
        .unwrap_or_else(|| format!("wordpress-runtime-{}", uuid::Uuid::new_v4()));
    let tasks = request
        .tasks
        .drain(..)
        .enumerate()
        .map(|(index, task)| task.into_agent_task_request(&plan_id, index))
        .collect();
    let mut plan = AgentTaskPlan::new(plan_id, tasks);
    plan.group_key = request.group_key;
    plan.options = request.options;
    plan.component_contracts = request.component_contracts;
    plan.metadata = metadata_with_schema(request.metadata, WORDPRESS_RUNTIME_PLAN_REQUEST_SCHEMA);
    plan.rebuild_homeboy_plan();
    plan
}

pub fn dla_extraction_task(url: impl Into<String>) -> WordPressRuntimeTaskSpec {
    let url = url.into();
    WordPressRuntimeTaskSpec {
        task_id: None,
        kind: Some("dla_extraction".to_string()),
        instructions: Some("Run the declared WordPress runtime task and return normalized artifacts, diagnostics, metrics, logs, and status evidence.".to_string()),
        runtime_task: json!({
            "schema": "datamachine/runtime-task-request/v1",
            "operation": "extract",
            "engine": {
                "id": "data-liberation-agent"
            },
            "source": {
                "url": url
            }
        }),
        executor: WordPressRuntimeExecutorSpec {
            required_capabilities: vec![
                "wordpress.runtime".to_string(),
                "wordpress.abilities".to_string(),
                "data-liberation-agent".to_string(),
            ],
            ..WordPressRuntimeExecutorSpec::default()
        },
        source_refs: vec![AgentTaskSourceRef {
            kind: "url".to_string(),
            uri: url,
            revision: None,
        }],
        component_contracts: Vec::new(),
        policy: AgentTaskPolicy::default(),
        limits: AgentTaskLimits::default(),
        expected_artifacts: vec!["runtime-task-result".to_string(), "artifact-bundle".to_string()],
        artifact_declarations: Vec::new(),
        metadata: Value::Null,
    }
}

impl WordPressRuntimeTaskSpec {
    fn into_agent_task_request(self, plan_id: &str, index: usize) -> AgentTaskRequest {
        let task_id = self
            .task_id
            .clone()
            .unwrap_or_else(|| format!("{plan_id}-task-{}", index + 1));
        let kind = self
            .kind
            .clone()
            .unwrap_or_else(|| "runtime_task".to_string());
        let runtime_task = metadata_with_schema(self.runtime_task, WORDPRESS_RUNTIME_TASK_SCHEMA);
        let instructions = self.instructions.unwrap_or_else(|| {
            "Run the declared WordPress runtime task and return normalized artifacts, diagnostics, metrics, logs, and status evidence.".to_string()
        });
        let executor = self.executor.into_agent_task_executor(&runtime_task);
        let mut request = AgentTaskRequest {
            schema: crate::core::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id,
            group_key: None,
            parent_plan_id: Some(plan_id.to_string()),
            executor,
            instructions,
            inputs: json!({
                "schema": WORDPRESS_RUNTIME_TASK_SCHEMA,
                "kind": kind,
                "runtime_task": runtime_task,
            }),
            source_refs: self.source_refs,
            workspace: AgentTaskWorkspace {
                kind: Some("wordpress-runtime".to_string()),
                mode: AgentTaskWorkspaceMode::Ephemeral,
                ..AgentTaskWorkspace::default()
            },
            component_contracts: self.component_contracts,
            policy: self.policy,
            limits: self.limits,
            expected_artifacts: self.expected_artifacts,
            artifact_declarations: self.artifact_declarations,
            metadata: metadata_with_schema(self.metadata, WORDPRESS_RUNTIME_TASK_SCHEMA),
        };
        request.normalize_artifact_declarations();
        request
    }
}

impl WordPressRuntimeExecutorSpec {
    fn into_agent_task_executor(self, runtime_task: &Value) -> AgentTaskExecutor {
        let mut required_capabilities = vec!["wordpress.runtime".to_string()];
        if runtime_task.get("ability").is_some() || runtime_task.get("operation").is_some() {
            push_unique(&mut required_capabilities, "wordpress.abilities");
        }
        if runtime_task
            .get("engine")
            .and_then(|engine| engine.get("id"))
            .and_then(Value::as_str)
            == Some("data-liberation-agent")
        {
            push_unique(&mut required_capabilities, "data-liberation-agent");
        }
        for capability in self.required_capabilities {
            push_unique(&mut required_capabilities, &capability);
        }

        let mut config = Map::new();
        if let Some(provider) = &self.provider {
            config.insert("provider".to_string(), Value::String(provider.clone()));
        }

        AgentTaskExecutor {
            backend: self.backend.clone(),
            selector: self.selector.clone(),
            runtime_selection: Some(AgentTaskRuntimeSelection {
                runtime_id: self.runtime_id,
                executor_backend: Some(self.backend),
                executor_provider_id: self.selector,
                provider: self.provider.clone(),
                model: self.model.clone(),
                substrate_ref: self.substrate_ref,
            }),
            required_capabilities,
            secret_env: self.secret_env,
            model: self.model,
            config: Value::Object(config),
        }
    }
}

fn metadata_with_schema(value: Value, schema: &str) -> Value {
    let mut object = match value {
        Value::Object(object) => object,
        Value::Null => Map::new(),
        other => Map::from_iter([("payload".to_string(), other)]),
    };
    object
        .entry("schema".to_string())
        .or_insert_with(|| Value::String(schema.to_string()));
    Value::Object(object)
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn default_plan_request_schema() -> String {
    WORDPRESS_RUNTIME_PLAN_REQUEST_SCHEMA.to_string()
}

fn default_executor_backend() -> String {
    "codebox".to_string()
}

fn default_runtime_id() -> String {
    "wp-codebox".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_codebox_runtime_task_plan() {
        let plan = build_wordpress_runtime_plan(WordPressRuntimePlanRequest {
            schema: WORDPRESS_RUNTIME_PLAN_REQUEST_SCHEMA.to_string(),
            plan_id: Some("wp-runtime-plan".to_string()),
            group_key: Some("imports".to_string()),
            tasks: vec![WordPressRuntimeTaskSpec {
                task_id: Some("extract".to_string()),
                kind: Some("runtime_task".to_string()),
                instructions: None,
                runtime_task: json!({
                    "ability": "datamachine/run-runtime-task",
                    "input": { "operation": "extract" }
                }),
                executor: WordPressRuntimeExecutorSpec::default(),
                source_refs: Vec::new(),
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits {
                    timeout_ms: Some(30_000),
                    ..AgentTaskLimits::default()
                },
                expected_artifacts: vec!["runtime-task-result".to_string()],
                artifact_declarations: Vec::new(),
                metadata: Value::Null,
            }],
            options: AgentTaskScheduleOptions {
                max_concurrency: 2,
                ..AgentTaskScheduleOptions::default()
            },
            component_contracts: Vec::new(),
            metadata: Value::Null,
        });

        assert_eq!(plan.plan_id, "wp-runtime-plan");
        assert_eq!(plan.group_key.as_deref(), Some("imports"));
        assert_eq!(plan.options.max_concurrency, 2);
        assert_eq!(plan.tasks[0].executor.backend, "codebox");
        assert_eq!(plan.tasks[0].executor.runtime_id(), Some("wp-codebox"));
        assert!(plan.tasks[0]
            .executor
            .required_capabilities
            .contains(&"wordpress.runtime".to_string()));
        assert_eq!(
            plan.tasks[0].inputs["runtime_task"]["ability"],
            "datamachine/run-runtime-task"
        );
        assert_eq!(plan.tasks[0].limits.timeout_ms, Some(30_000));
    }

    #[test]
    fn dla_extraction_shorthand_records_source_and_capability() {
        let plan = build_wordpress_runtime_plan(WordPressRuntimePlanRequest {
            schema: WORDPRESS_RUNTIME_PLAN_REQUEST_SCHEMA.to_string(),
            plan_id: Some("dla-plan".to_string()),
            group_key: None,
            tasks: vec![dla_extraction_task("https://example.com")],
            options: AgentTaskScheduleOptions::default(),
            component_contracts: Vec::new(),
            metadata: Value::Null,
        });

        let task = &plan.tasks[0];
        assert_eq!(task.inputs["kind"], "dla_extraction");
        assert_eq!(
            task.inputs["runtime_task"]["engine"]["id"],
            "data-liberation-agent"
        );
        assert_eq!(task.source_refs[0].uri, "https://example.com");
        assert!(task
            .executor
            .required_capabilities
            .contains(&"data-liberation-agent".to_string()));
        assert!(task
            .artifact_declarations
            .iter()
            .any(|artifact| artifact.name == "artifact-bundle"));
    }
}
