use serde_json::Value;

use homeboy::core::agent_tasks::agent_task_core_contract;

use super::super::CmdResult;
use super::{command_json_value, ContractArgs, ContractFormat};

pub(crate) fn contract(args: ContractArgs) -> CmdResult<Value> {
    match args.format {
        ContractFormat::Json => Ok((command_json_value(agent_task_core_contract())?, 0)),
    }
}
