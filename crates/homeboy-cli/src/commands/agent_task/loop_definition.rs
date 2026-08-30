use serde_json::Value;

use homeboy::agents::agent_task_controller_service::{
    self, AgentTaskRepoLoopSpec, ControllerPlanRequest,
};
use homeboy::agents::agent_tasks::loop_definition;
use homeboy::core::config;
use homeboy::core::Error;

use super::super::CmdResult;
use super::args::CompileLoopArgs;

pub(super) fn compile_loop(args: CompileLoopArgs) -> CmdResult<Value> {
    let raw = config::read_json_spec_to_string(&args.definition)?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task loop definition".to_string()),
            Some(raw.clone()),
        )
    })?;
    let plan = if value.get("tasks").is_some_and(|tasks| tasks.is_array())
        && value
            .get("tasks")
            .and_then(Value::as_array)
            .is_some_and(|tasks| tasks.iter().any(|task| task.get("request").is_some()))
    {
        let definition = serde_json::from_value(value).map_err(|error| {
            Error::validation_invalid_argument(
                "definition",
                error.to_string(),
                Some("agent-task loop definition".to_string()),
                None,
            )
        })?;
        serde_json::to_value(loop_definition::compile_loop_definition(definition)?)
            .unwrap_or(Value::Null)
    } else {
        let mut spec: AgentTaskRepoLoopSpec = serde_json::from_value(value).map_err(|error| {
            Error::validation_invalid_argument(
                "definition",
                error.to_string(),
                Some("repo loop spec".to_string()),
                None,
            )
        })?;
        agent_task_controller_service::apply_spec_dispatch_defaults(&mut spec, &args.definition);
        serde_json::to_value(
            agent_task_controller_service::compile_plan_from_spec(ControllerPlanRequest { spec })?
                .plan,
        )
        .unwrap_or(Value::Null)
    };
    Ok((plan, 0))
}
