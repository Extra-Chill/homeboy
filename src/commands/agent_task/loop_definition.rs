use serde_json::Value;

use homeboy::core::config;
use homeboy::core::{agent_tasks::loop_definition, Error};

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
    let plan = loop_definition::compile_loop_spec_value(value)?;
    Ok((serde_json::to_value(plan).unwrap_or(Value::Null), 0))
}
