//! `agent-task auth` handlers: provider secret configuration and mapping.

use std::io::Read;

use serde_json::Value;

use homeboy::core::agent_tasks::secrets as agent_task_secrets;

use super::super::CmdResult;
use super::args::{AgentTaskAuthArgs, AgentTaskAuthCommand};
use crate::commands::utils::tty::prompt_password;

pub(super) fn auth(args: AgentTaskAuthArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskAuthCommand::Status(status_args) => Ok((
            serde_json::json!({
                "schema": "homeboy/agent-task-auth-status/v1",
                "secret_env": agent_task_secrets::secret_env_status(&status_args.secret_env),
            }),
            0,
        )),
        AgentTaskAuthCommand::SetKeychain(set_args) => {
            let value = read_agent_task_secret_value(set_args.value, set_args.value_stdin)?;
            let status = agent_task_secrets::set_keychain_secret(
                &set_args.secret_env,
                &value,
                set_args.scope.as_deref(),
                set_args.keychain_name.as_deref(),
            )?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-configured/v1",
                    "secret_env": status,
                }),
                0,
            ))
        }
        AgentTaskAuthCommand::SetConfig(set_args) => {
            let value = read_agent_task_secret_value(set_args.value, set_args.value_stdin)?;
            let status = agent_task_secrets::set_config_secret(&set_args.secret_env, &value)?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-configured/v1",
                    "secret_env": status,
                }),
                0,
            ))
        }
        AgentTaskAuthCommand::SetKeychainBundle(set_args) => {
            let value = read_agent_task_secret_value(set_args.value, set_args.value_stdin)?;
            let keychain_name = agent_task_secrets::set_keychain_bundle(
                &set_args.bundle,
                &value,
                set_args.scope.as_deref(),
                set_args.keychain_name.as_deref(),
            )?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-bundle-configured/v1",
                    "bundle": set_args.bundle,
                    "source": "keychain-bundle",
                    "keychain_name": keychain_name,
                }),
                0,
            ))
        }
        AgentTaskAuthCommand::MapEnv(map_args) => {
            let status = agent_task_secrets::map_secret_to_env(
                &map_args.secret_env,
                map_args.source_env.as_deref(),
            )?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-configured/v1",
                    "secret_env": status,
                }),
                0,
            ))
        }
        AgentTaskAuthCommand::MapKeychainBundle(map_args) => {
            let status = agent_task_secrets::map_secret_to_keychain_bundle(
                &map_args.secret_env,
                &map_args.bundle,
                &map_args.field,
                map_args.scope.as_deref(),
                map_args.keychain_name.as_deref(),
            )?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-configured/v1",
                    "secret_env": status,
                }),
                0,
            ))
        }
        AgentTaskAuthCommand::Remove(remove_args) => {
            let status = agent_task_secrets::remove_secret_mapping(
                &remove_args.secret_env,
                remove_args.keychain,
            )?;
            Ok((
                serde_json::json!({
                    "schema": "homeboy/agent-task-auth-configured/v1",
                    "secret_env": status,
                }),
                0,
            ))
        }
    }
}

fn read_agent_task_secret_value(
    value: Option<String>,
    value_stdin: bool,
) -> homeboy::core::Result<String> {
    match (value, value_stdin) {
        (Some(_), true) => Err(homeboy::core::Error::validation_invalid_argument(
            "value-stdin",
            "cannot combine VALUE with --value-stdin",
            None,
            None,
        )),
        (Some(value), false) => Ok(value),
        (None, true) => {
            let mut raw = String::new();
            std::io::stdin().read_to_string(&mut raw).map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some("read agent-task secret value from stdin".to_string()),
                )
            })?;
            Ok(raw.trim_end_matches(['\r', '\n']).to_string())
        }
        (None, false) => prompt_password("Secret value: "),
    }
}
