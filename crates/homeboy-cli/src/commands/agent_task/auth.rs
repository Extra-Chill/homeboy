//! `agent-task auth` handlers: provider secret configuration and mapping.

use std::io::Read;

use serde_json::Value;

use homeboy::agents::agent_tasks::provider as agent_task_provider;
use homeboy::agents::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::agents::agent_tasks::secrets as agent_task_secrets;

use super::super::CmdResult;
use super::args::{AgentTaskAuthArgs, AgentTaskAuthCommand, AgentTaskAuthStatusArgs};
use crate::commands::utils::tty::prompt_password;

pub(super) fn auth(args: AgentTaskAuthArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskAuthCommand::Status(status_args) => Ok((auth_status(status_args), 0)),
        AgentTaskAuthCommand::SetKeychain(set_args) => {
            let value = read_agent_task_secret_value(set_args.value, set_args.value_stdin)?;
            let legacy_file = agent_task_secrets::legacy_secrets_file();
            let status = agent_task_secrets::set_keychain_secret(
                &set_args.secret_env,
                &value,
                set_args.scope.as_deref(),
                set_args.keychain_name.as_deref(),
            )?;
            Ok((configured_output(status, legacy_file), 0))
        }
        AgentTaskAuthCommand::SetConfig(set_args) => {
            let value = read_agent_task_secret_value(set_args.value, set_args.value_stdin)?;
            let legacy_file = agent_task_secrets::legacy_secrets_file();
            let status = agent_task_secrets::set_config_secret(&set_args.secret_env, &value)?;
            Ok((configured_output(status, legacy_file), 0))
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
            let legacy_file = agent_task_secrets::legacy_secrets_file();
            let status = agent_task_secrets::map_secret_to_env(
                &map_args.secret_env,
                map_args.source_env.as_deref(),
            )?;
            Ok((configured_output(status, legacy_file), 0))
        }
        AgentTaskAuthCommand::MapKeychainBundle(map_args) => {
            let legacy_file = agent_task_secrets::legacy_secrets_file();
            let status = agent_task_secrets::map_secret_to_keychain_bundle(
                &map_args.secret_env,
                &map_args.bundle,
                &map_args.field,
                map_args.scope.as_deref(),
                map_args.keychain_name.as_deref(),
            )?;
            Ok((configured_output(status, legacy_file), 0))
        }
        AgentTaskAuthCommand::Remove(remove_args) => {
            let legacy_file = agent_task_secrets::legacy_secrets_file();
            let status = agent_task_secrets::remove_secret_mapping(
                &remove_args.secret_env,
                remove_args.keychain,
            )?;
            Ok((configured_output(status, legacy_file), 0))
        }
    }
}

/// Build the `agent-task-auth-configured` output.
///
/// Secret mappings live in the global config at `/agent_task/secrets`. When a
/// superseded standalone secrets file still existed at command start, the
/// mutation just migrated any remaining mappings into the global config and
/// removed that file — disclose both so exactly one storage location is
/// authoritative and the user is not left trusting a stale file.
fn configured_output(
    status: agent_task_secrets::AgentTaskSecretEnvStatus,
    legacy_file: Option<std::path::PathBuf>,
) -> Value {
    let mut output = serde_json::json!({
        "schema": "homeboy/agent-task-auth-configured/v1",
        "secret_env": status,
    });
    if let Some(legacy_file) = legacy_file {
        output["secrets_storage"] = serde_json::json!({
            "config_file": homeboy::core::defaults::config_path().ok(),
            "config_pointer": "/agent_task/secrets",
            "removed_legacy_file": legacy_file,
        });
    }
    output
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

    // Scope status to the selected backend (and optional selector). Falling
    // back to every discovered provider keeps status useful when no backend
    // can be resolved. Shared with `agent-task providers --secret-env` so
    // the two commands agree about the same secrets (#13629).
    let scoped_providers: Vec<_> = match selected_backend.as_deref() {
        Some(backend) => providers
            .iter()
            .filter(|provider| provider.backend == backend)
            .filter(|provider| {
                status_args
                    .selector
                    .as_deref()
                    .is_none_or(|selector| provider.id == selector)
            })
            .cloned()
            .collect(),
        None => providers.to_vec(),
    };
    let secret_env = agent_task_provider::secret_env_status_for_providers(
        &status_args.secret_env,
        &scoped_providers,
    );

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
