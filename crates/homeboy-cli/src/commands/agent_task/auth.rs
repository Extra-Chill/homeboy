//! `agent-task auth` handlers: provider secret configuration and mapping.

use std::io::Read;

use serde_json::Value;

use homeboy::core::agent_tasks::provider as agent_task_provider;
use homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_tasks::secrets as agent_task_secrets;

use super::super::CmdResult;
use super::args::{AgentTaskAuthArgs, AgentTaskAuthCommand, AgentTaskAuthStatusArgs};
use crate::commands::utils::tty::prompt_password;

pub(super) fn auth(args: AgentTaskAuthArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskAuthCommand::Status(status_args) => Ok((auth_status(status_args), 0)),
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

/// Report redacted secret-env readiness for the selected/default backend.
///
/// Resolves the same backend cook/dispatch would use (explicit `--backend`,
/// else the extension/policy default), scopes the provider secret sources to
/// that backend, and reports readiness for its required secrets. When the
/// operator passes explicit `--secret-env` names those are used verbatim.
/// Raw secret values are never emitted — only configured/source/value-present
/// states. This replaces the previous behavior that returned an empty list
/// whenever no `--secret-env` was passed, which made configured auth look
/// absent.
fn auth_status(status_args: AgentTaskAuthStatusArgs) -> Value {
    let executor = ExtensionProviderAgentTaskExecutor::discover();
    let providers = executor.providers();

    // Backend the cook would use: explicit flag, else the policy/default backend.
    let default_backend = agent_task_provider::default_backend().ok().flatten();
    let selected_backend = status_args
        .backend
        .clone()
        .or_else(|| default_backend.clone());

    // Scope provider secret sources to the selected backend (and optional
    // selector). Falling back to all-provider sources keeps status useful when
    // no backend can be resolved.
    let fallback_sources = match selected_backend.as_deref() {
        Some(backend) => agent_task_provider::provider_secret_sources_for_backend(
            providers,
            backend,
            status_args.selector.as_deref(),
        ),
        None => agent_task_provider::provider_secret_sources_for_providers(providers),
    };

    // Required secret env names: explicit operator-supplied names win; otherwise
    // report every secret the selected backend declares.
    let names: Vec<String> = if status_args.secret_env.is_empty() {
        let mut names: Vec<String> = fallback_sources.keys().cloned().collect();
        names.sort();
        names
    } else {
        status_args.secret_env.clone()
    };

    let secret_env =
        agent_task_secrets::secret_env_status_with_fallbacks(&names, &fallback_sources);

    serde_json::json!({
        "schema": "homeboy/agent-task-auth-status/v1",
        "selected_backend": selected_backend,
        "default_backend": default_backend,
        "selector": status_args.selector,
        "secret_env": secret_env,
    })
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
