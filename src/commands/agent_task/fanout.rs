//! Public provider-neutral fanout/reconcile command handlers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use homeboy::core::agent_task::{
    AgentTaskArtifactDeclaration, AgentTaskComponentContract, AgentTaskLimits,
};
use homeboy::core::agent_tasks::lifecycle;
use homeboy::core::agent_tasks::provider;
use homeboy::core::agent_tasks::scheduler::{AgentTaskPlan, AgentTaskScheduleOptions};
use homeboy::core::agent_tasks::{
    AgentTaskExecutor, AgentTaskFanoutPlan, AgentTaskFanoutPlane, AgentTaskPolicy,
    AgentTaskRequest, AgentTaskSourceRef, AgentTaskWorkspace, AGENT_TASK_FANOUT_PLAN_SCHEMA,
    AGENT_TASK_PLAN_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
};
use homeboy::core::{config, Error, Result};

use super::super::CmdResult;
use super::args::{
    AgentTaskFanoutArgs, AgentTaskFanoutCommand, AgentTaskFanoutInputArgs, AgentTaskFanoutPlaneArg,
    AgentTaskFanoutRunPlanArgs, AgentTaskFanoutSubmitArgs,
};
use super::command_json_value;
use super::run;

pub(super) fn fanout(args: AgentTaskFanoutArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskFanoutCommand::Plan(plan_args) => {
            let plan = load_fanout_agent_task_plan(&plan_args.input)?;
            Ok((command_json_value(plan)?, 0))
        }
        AgentTaskFanoutCommand::Submit(submit_args) => submit_fanout(submit_args),
        AgentTaskFanoutCommand::RunPlan(run_args) => run_fanout_plan(run_args),
    }
}

fn submit_fanout(args: AgentTaskFanoutSubmitArgs) -> CmdResult<Value> {
    let plan = load_fanout_agent_task_plan(&args.input)?;
    let record = lifecycle::submit_plan(&plan, args.run_id.as_deref())?;
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-fanout-submit-result/v1",
            "run": record,
            "commands": durable_commands(&record.run_id),
        }),
        0,
    ))
}

fn run_fanout_plan(args: AgentTaskFanoutRunPlanArgs) -> CmdResult<Value> {
    let plan = load_fanout_agent_task_plan(&args.input)?;
    run::run_loaded_plan(
        plan,
        args.record_run_id.as_deref(),
        provider::ExtensionProviderAgentTaskExecutor::discover(),
    )
}

fn load_fanout_agent_task_plan(args: &AgentTaskFanoutInputArgs) -> Result<AgentTaskPlan> {
    let raw = config::read_json_spec_to_string(&args.input)?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task fanout input".to_string()),
            Some(raw.clone()),
        )
    })?;
    fanout_value_to_plan(value, args)
}

fn fanout_value_to_plan(value: Value, args: &AgentTaskFanoutInputArgs) -> Result<AgentTaskPlan> {
    if value.get("schema").and_then(Value::as_str) == Some(AGENT_TASK_PLAN_SCHEMA) {
        let mut plan: AgentTaskPlan = serde_json::from_value(value).map_err(|error| {
            Error::validation_invalid_argument(
                "input",
                error.to_string(),
                None,
                Some(vec![
                    "Expected a valid homeboy/agent-task-plan/v1 payload.".to_string()
                ]),
            )
        })?;
        stamp_plan_fanout_metadata(&mut plan, args.fanout_id.clone());
        return Ok(plan);
    }

    if value.get("schema").and_then(Value::as_str) == Some(AGENT_TASK_FANOUT_PLAN_SCHEMA) {
        let fanout_plan: AgentTaskFanoutPlan = serde_json::from_value(value).map_err(|error| {
            Error::validation_invalid_argument(
                "input",
                error.to_string(),
                None,
                Some(vec![
                    "Expected a valid homeboy/agent-task-fanout-plan/v1 payload.".to_string(),
                ]),
            )
        })?;
        return Ok(fanout_plan.to_agent_task_plan());
    }

    let spec = FanoutPacketSpec::from_value(value)?;
    let fanout_id = spec
        .fanout_id
        .or_else(|| args.fanout_id.clone())
        .unwrap_or_else(|| format!("agent-task-fanout-{}", uuid::Uuid::new_v4()));
    let plane = spec.plane.unwrap_or_else(|| plane_from_arg(args.plane));
    let tasks = spec
        .packets
        .into_iter()
        .enumerate()
        .map(|(index, packet)| packet.into_request(index, args))
        .collect::<Result<Vec<_>>>()?;
    if tasks.is_empty() {
        return Err(Error::validation_invalid_argument(
            "input",
            "agent-task fanout requires at least one task or packet",
            None,
            None,
        ));
    }

    let mut fanout_plan = AgentTaskFanoutPlan::new(fanout_id, plane, tasks);
    fanout_plan.group_key = spec.group_key;
    fanout_plan.options = spec.options.unwrap_or_default();
    fanout_plan.metadata = spec.metadata.unwrap_or(Value::Null);
    Ok(fanout_plan.to_agent_task_plan())
}

#[derive(Debug, Deserialize)]
struct FanoutPacketSpec {
    #[serde(default)]
    fanout_id: Option<String>,
    #[serde(default)]
    plane: Option<AgentTaskFanoutPlane>,
    #[serde(default)]
    group_key: Option<String>,
    #[serde(default)]
    options: Option<AgentTaskScheduleOptions>,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    packets: Vec<FanoutTaskPacket>,
}

impl FanoutPacketSpec {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Array(packets) => Ok(Self {
                packets: parse_packet_array(packets)?,
                fanout_id: None,
                plane: None,
                group_key: None,
                options: None,
                metadata: None,
            }),
            Value::Object(mut object) => {
                if !object.contains_key("packets") {
                    if let Some(tasks) = object.remove("tasks") {
                        object.insert("packets".to_string(), tasks);
                    } else if object.contains_key("task_id")
                        || object.contains_key("key")
                        || object.contains_key("instructions")
                        || object.contains_key("prompt")
                        || object.contains_key("title")
                    {
                        return Ok(Self {
                            packets: vec![serde_json::from_value(Value::Object(object)).map_err(
                                packet_parse_error,
                            )?],
                            fanout_id: None,
                            plane: None,
                            group_key: None,
                            options: None,
                            metadata: None,
                        });
                    }
                }
                serde_json::from_value(Value::Object(object)).map_err(|error| {
                    Error::validation_invalid_argument("input", error.to_string(), None, None)
                })
            }
            _ => Err(Error::validation_invalid_argument(
                "input",
                "agent-task fanout input must be an AgentTaskPlan, AgentTaskFanoutPlan, packet object, or packet array",
                None,
                None,
            )),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct FanoutTaskPacket {
    #[serde(default = "request_schema")]
    schema: String,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    group_key: Option<String>,
    #[serde(default)]
    parent_plan_id: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    executor: Option<AgentTaskExecutor>,
    #[serde(default)]
    inputs: Value,
    #[serde(default)]
    source_refs: Vec<AgentTaskSourceRef>,
    #[serde(default)]
    workspace: AgentTaskWorkspace,
    #[serde(default)]
    component_contracts: Vec<AgentTaskComponentContract>,
    #[serde(default)]
    policy: AgentTaskPolicy,
    #[serde(default)]
    limits: AgentTaskLimits,
    #[serde(default)]
    expected_artifacts: Vec<String>,
    #[serde(default, alias = "artifactDeclarations")]
    artifact_declarations: Vec<AgentTaskArtifactDeclaration>,
    #[serde(default)]
    metadata: Value,
}

impl FanoutTaskPacket {
    fn into_request(
        self,
        index: usize,
        args: &AgentTaskFanoutInputArgs,
    ) -> Result<AgentTaskRequest> {
        if let Ok(request) = serde_json::from_value::<AgentTaskRequest>(
            serde_json::to_value(&self).unwrap_or(Value::Null),
        ) {
            return Ok(request);
        }

        let task_id = self
            .task_id
            .or(self.key)
            .unwrap_or_else(|| format!("task-{}", index + 1));
        let instructions = self
            .instructions
            .or(self.prompt)
            .or(self.title)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "instructions",
                    format!(
                        "fanout packet '{}' is missing instructions/prompt/title",
                        task_id
                    ),
                    Some(task_id.clone()),
                    None,
                )
            })?;
        let executor = match self.executor {
            Some(executor) => executor,
            None => default_executor(args)?,
        };

        Ok(AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id,
            group_key: self.group_key,
            parent_plan_id: self.parent_plan_id,
            executor,
            instructions,
            inputs: self.inputs,
            source_refs: self.source_refs,
            workspace: self.workspace,
            component_contracts: self.component_contracts,
            policy: self.policy,
            limits: self.limits,
            expected_artifacts: self.expected_artifacts,
            artifact_declarations: self.artifact_declarations,
            metadata: self.metadata,
        })
    }
}

fn request_schema() -> String {
    AGENT_TASK_REQUEST_SCHEMA.to_string()
}

fn parse_packet_array(packets: Vec<Value>) -> Result<Vec<FanoutTaskPacket>> {
    packets
        .into_iter()
        .map(|packet| serde_json::from_value(packet).map_err(packet_parse_error))
        .collect()
}

fn packet_parse_error(error: serde_json::Error) -> Error {
    Error::validation_invalid_argument("packet", error.to_string(), None, None)
}

fn default_executor(args: &AgentTaskFanoutInputArgs) -> Result<AgentTaskExecutor> {
    let backend = match &args.backend {
        Some(backend) => Some(backend.clone()),
        None => provider::default_backend()?,
    }
    .ok_or_else(|| {
        Error::validation_invalid_argument(
            "backend",
            "fanout packets without executor objects require --backend or a configured default agent-task backend",
            None,
            None,
        )
    })?;
    Ok(AgentTaskExecutor {
        backend,
        selector: args.selector.clone(),
        runtime_selection: None,
        required_capabilities: Vec::new(),
        secret_env: Vec::new(),
        model: args.model.clone(),
        config: Value::Null,
    })
}

fn plane_from_arg(plane: AgentTaskFanoutPlaneArg) -> AgentTaskFanoutPlane {
    match plane {
        AgentTaskFanoutPlaneArg::IsolatedTasks => AgentTaskFanoutPlane::IsolatedTasks,
        AgentTaskFanoutPlaneArg::Workflow => AgentTaskFanoutPlane::Workflow,
    }
}

fn stamp_plan_fanout_metadata(plan: &mut AgentTaskPlan, requested_fanout_id: Option<String>) {
    let fanout_id = requested_fanout_id.unwrap_or_else(|| plan.plan_id.clone());
    let mut metadata = plan.metadata.take();
    if !metadata.is_object() {
        metadata = serde_json::json!({ "base": metadata });
    }
    if let Some(object) = metadata.as_object_mut() {
        object.entry("fanout".to_string()).or_insert_with(|| {
            serde_json::json!({
                "id": fanout_id,
                "plane": "existing_plan",
                "lineage": "compiled_from_agent_task_plan"
            })
        });
    }
    plan.metadata = metadata;
}

fn durable_commands(run_id: &str) -> Value {
    serde_json::json!({
        "status": format!("homeboy agent-task status {run_id}"),
        "logs": format!("homeboy agent-task logs {run_id}"),
        "artifacts": format!("homeboy agent-task artifacts {run_id}"),
        "review": format!("homeboy agent-task review {run_id}"),
        "run": format!("homeboy agent-task run {run_id}"),
        "retry": format!("homeboy agent-task retry {run_id} --run")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn packet_array_compiles_to_public_agent_task_plan_with_lineage() {
        let args = AgentTaskFanoutInputArgs {
            input: "inline".to_string(),
            fanout_id: Some("fanout/audit".to_string()),
            plane: AgentTaskFanoutPlaneArg::IsolatedTasks,
            backend: Some("test".to_string()),
            selector: None,
            model: None,
        };

        let plan = fanout_value_to_plan(
            json!([
                { "task_id": "finding-a", "instructions": "inspect a" },
                { "task_id": "finding-b", "prompt": "inspect b" }
            ]),
            &args,
        )
        .expect("fanout plan");

        assert_eq!(plan.plan_id, "fanout/audit");
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(
            plan.tasks[0].parent_plan_id.as_deref(),
            Some("fanout/audit")
        );
        assert_eq!(plan.tasks[0].executor.backend, "test");
        assert_eq!(plan.tasks[1].instructions, "inspect b");
        assert_eq!(plan.metadata["fanout"]["id"], json!("fanout/audit"));
    }

    #[test]
    fn existing_agent_task_plan_is_accepted_and_stamped() {
        let args = AgentTaskFanoutInputArgs {
            input: "inline".to_string(),
            fanout_id: None,
            plane: AgentTaskFanoutPlaneArg::Workflow,
            backend: None,
            selector: None,
            model: None,
        };
        let value = json!({
            "schema": AGENT_TASK_PLAN_SCHEMA,
            "plan_id": "existing-plan",
            "tasks": [],
            "options": {},
            "metadata": {}
        });

        let plan = fanout_value_to_plan(value, &args).expect("agent task plan");

        assert_eq!(plan.plan_id, "existing-plan");
        assert_eq!(plan.metadata["fanout"]["id"], json!("existing-plan"));
        assert_eq!(plan.metadata["fanout"]["plane"], json!("existing_plan"));
    }
}
