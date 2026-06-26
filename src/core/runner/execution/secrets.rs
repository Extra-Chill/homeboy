use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::core::agent_tasks::{
    provider_secret_sources_for_discovered_providers, secrets as agent_task_secrets,
};
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::secret_env_plan::SecretEnvPlan;
use crate::core::server::{self, SshClient};

use super::super::resolve_runner_secret_env;
use super::super::{Runner, RunnerCapabilityPreflight};

#[allow(unused_imports)]
use super::*;

pub(super) fn resolve_runner_secret_env_for_plan(
    secret_env: &HashMap<String, server::RunnerSecretEnvRef>,
    plan: &SecretEnvPlan,
    env: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    resolve_runner_secret_env_for_command_with_fallbacks(
        secret_env,
        &plan.secret_env_names(),
        env,
        &provider_secret_sources_for_discovered_providers(),
    )
}

pub(super) fn resolve_runner_secret_env_for_command_with_fallbacks(
    secret_env: &HashMap<String, server::RunnerSecretEnvRef>,
    required_names: &[String],
    env: &HashMap<String, String>,
    fallback_sources: &HashMap<String, crate::core::defaults::AgentTaskSecretSource>,
) -> Result<HashMap<String, String>> {
    if required_names.is_empty() {
        return Ok(HashMap::new());
    }

    let mut refs = HashMap::new();
    let mut resolved = HashMap::new();
    for name in required_names {
        if env.contains_key(name.as_str()) {
            continue;
        }
        if let Some(source) = secret_env.get(name.as_str()) {
            refs.insert(name.clone(), source.clone());
            continue;
        }
        if fallback_sources.contains_key(name) {
            if let Ok(values) = agent_task_secrets::resolve_secret_env_with_fallbacks(
                std::slice::from_ref(name),
                fallback_sources,
            ) {
                for (name, value) in values {
                    resolved.insert(name, value);
                }
                continue;
            }
        }
        return Err(Error::validation_invalid_argument(
            "secret_env",
            format!("missing runner secret env ref for {name}"),
            Some(name.clone()),
            Some(vec![
                "Configure the selected runner secret_env reference, declare provider secret_env_sources that resolve on the runner, or pass the secret in the exec request environment.".to_string(),
            ]),
        ));
    }

    resolved.extend(resolve_runner_secret_env(&refs)?);
    Ok(resolved)
}

pub(super) fn provision_provider_file_secret_sources_for_runner(
    runner: &Runner,
    command: &[String],
    required_names: &[String],
    request_env: &HashMap<String, String>,
) -> Result<()> {
    if !is_agent_task_run_plan_command(command) || required_names.is_empty() {
        return Ok(());
    }
    let fallback_sources = provider_secret_sources_for_discovered_providers();
    let provisions = provider_file_secret_source_provisions(required_names, &fallback_sources);
    if provisions.is_empty() {
        return Ok(());
    }

    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner",
            "SSH runner requires server_id before provider secret source provisioning",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let server = server::load(server_id)?;
    let client = SshClient::from_server(&server, server_id)?;
    for provision in provisions {
        if provision
            .env_names
            .iter()
            .all(|name| request_env.contains_key(name.as_str()))
        {
            continue;
        }
        agent_task_secrets::resolve_secret_env_with_fallbacks(
            &provision.env_names,
            &fallback_sources,
        )
        .map_err(|err| {
            provider_file_secret_source_error(
                &runner.id,
                &provision,
                format!(
                    "controller credential source does not satisfy provider env names: {}",
                    err.message
                ),
            )
        })?;
        provision_provider_file_secret_source(&client, &runner.id, &provision)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProviderFileSecretSourceProvision {
    pub(super) path: String,
    pub(super) env_names: Vec<String>,
}

pub(super) fn provider_file_secret_source_provisions(
    required_names: &[String],
    fallback_sources: &HashMap<String, crate::core::defaults::AgentTaskSecretSource>,
) -> Vec<ProviderFileSecretSourceProvision> {
    let mut by_path: HashMap<String, Vec<String>> = HashMap::new();
    for name in required_names {
        let Some(source) = fallback_sources.get(name) else {
            continue;
        };
        if source.source != "json-file" && source.source != "json-file-jwt-expiration" {
            continue;
        }
        let Some(path) = source
            .path
            .as_deref()
            .filter(|path| !path.trim().is_empty())
        else {
            continue;
        };
        by_path
            .entry(path.to_string())
            .or_default()
            .push(name.clone());
    }

    let mut provisions = by_path
        .into_iter()
        .map(|(path, mut env_names)| {
            env_names.sort();
            env_names.dedup();
            ProviderFileSecretSourceProvision { path, env_names }
        })
        .collect::<Vec<_>>();
    provisions.sort_by(|left, right| left.path.cmp(&right.path));
    provisions
}

pub(super) fn provision_provider_file_secret_source(
    client: &SshClient,
    runner_id: &str,
    provision: &ProviderFileSecretSourceProvision,
) -> Result<()> {
    let local_path = expanded_home_path(&provision.path);
    let local_raw = std::fs::read_to_string(&local_path).map_err(|err| {
        provider_file_secret_source_error(
            runner_id,
            provision,
            format!("controller credential source is not readable: {err}"),
        )
    })?;
    let remote_path = remote_secret_source_path(client, &provision.path)?;
    let Some(parent) = Path::new(&remote_path).parent().and_then(Path::to_str) else {
        return Err(provider_file_secret_source_error(
            runner_id,
            provision,
            "runner credential source path has no parent directory".to_string(),
        ));
    };

    let prepare = client.execute(&format!(
        "mkdir -p {} && chmod 700 {}",
        shell::quote_arg(parent),
        shell::quote_arg(parent)
    ));
    if !prepare.success {
        return Err(provider_file_secret_source_error(
            runner_id,
            provision,
            "failed to prepare runner credential directory".to_string(),
        ));
    }

    let temp = tempfile::NamedTempFile::new().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("create provider credential temp file".to_string()),
        )
    })?;
    std::fs::write(temp.path(), local_raw).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("write provider credential temp file".to_string()),
        )
    })?;
    let upload = client.upload_file(&temp.path().to_string_lossy(), &remote_path);
    if !upload.success {
        return Err(provider_file_secret_source_error(
            runner_id,
            provision,
            "failed to upload credential source to runner".to_string(),
        ));
    }
    let chmod = client.execute(&format!("chmod 600 {}", shell::quote_arg(&remote_path)));
    if !chmod.success {
        return Err(provider_file_secret_source_error(
            runner_id,
            provision,
            "failed to lock down runner credential source permissions".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn provider_file_secret_source_error(
    runner_id: &str,
    provision: &ProviderFileSecretSourceProvision,
    reason: String,
) -> Error {
    Error::validation_invalid_argument(
        "secret_env",
        format!(
            "provider runner credential source for {} cannot be provisioned on runner `{}`: {}",
            provision.env_names.join(", "),
            runner_id,
            reason
        ),
        Some(runner_id.to_string()),
        Some(vec![
            "Refresh the provider credentials on the controller, then rerun the Lab offload so Homeboy can provision the runner-side credential source before dispatch.".to_string(),
            "Credential values are not printed; inspect provider auth with the provider's own auth status command if refresh continues to fail.".to_string(),
        ]),
    )
}

pub(super) fn remote_secret_source_path(client: &SshClient, path: &str) -> Result<String> {
    if path == "~" || path.starts_with("~/") {
        let home = client.execute("printf %s \"$HOME\"");
        if !home.success || home.stdout.trim().is_empty() {
            return Err(Error::internal_unexpected(
                "failed to resolve runner home directory for provider credential source",
            ));
        }
        let suffix = path.strip_prefix('~').unwrap_or_default();
        return Ok(format!("{}{}", home.stdout.trim_end_matches('/'), suffix));
    }
    Ok(path.to_string())
}

pub(super) fn expanded_home_path(path: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(path).to_string())
}

pub(super) fn is_agent_task_run_plan_command(command: &[String]) -> bool {
    command
        .windows(2)
        .any(|items| items[0] == "agent-task" && items[1] == "run-plan")
}

pub(super) fn resolve_controller_secret_env_for_command(
    secret_env: &HashMap<String, server::RunnerSecretEnvRef>,
    required_names: &[String],
    env: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut resolved = HashMap::new();
    let fallback_sources = provider_secret_sources_for_discovered_providers();
    for name in required_names {
        if env.contains_key(name.as_str()) {
            continue;
        }
        let Some(source) = secret_env.get(name.as_str()) else {
            continue;
        };
        if source
            .secret
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            resolved.extend(resolve_runner_secret_env(&HashMap::from([(
                name.clone(),
                source.clone(),
            )]))?);
            continue;
        }

        if fallback_sources.contains_key(name) {
            if let Ok(values) = agent_task_secrets::resolve_secret_env_with_fallbacks(
                std::slice::from_ref(name),
                &fallback_sources,
            ) {
                resolved.extend(values);
            }
        }
    }
    Ok(resolved)
}

pub(crate) fn runner_exec_secret_env_names(
    command: &[String],
    preflight: Option<&RunnerCapabilityPreflight>,
    explicit_names: &[String],
    env: &HashMap<String, String>,
) -> Vec<String> {
    runner_exec_secret_env_plan(command, preflight, explicit_names, env).secret_env_names()
}

pub(crate) fn runner_exec_secret_env_plan(
    command: &[String],
    preflight: Option<&RunnerCapabilityPreflight>,
    explicit_names: &[String],
    env: &HashMap<String, String>,
) -> SecretEnvPlan {
    let mut names = Vec::new();
    names.extend(explicit_names.iter().cloned());
    if let Some(preflight) = preflight {
        names.extend(preflight.required_env.iter().cloned());
    }
    names.extend(super::super::lab::secrets::declared_agent_task_secret_env(
        command,
    ));
    names.extend(super::super::lab::secrets::declared_trace_secret_env(
        command,
    ));
    names.extend(super::super::lab::secrets::declared_tunnel_secret_env(
        command,
    ));
    names.extend(declared_runtime_provider_secret_env(env));
    SecretEnvPlan::from_secret_env_names(names)
}

fn declared_runtime_provider_secret_env(env: &HashMap<String, String>) -> Vec<String> {
    let explicit = env
        .get("HOMEBOY_AGENT_RUNTIME_SECRET_ENV")
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !explicit.is_empty() {
        return explicit;
    }

    match env
        .get("HOMEBOY_AGENT_RUNTIME_PROVIDER")
        .map(String::as_str)
        .map(str::trim)
    {
        Some("codex") => [
            "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN",
            "AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN",
            "AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT",
            "AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID",
            "AI_PROVIDER_OPENAI_CODEX_FEDRAMP",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        Some("openai") => vec!["OPENAI_API_KEY".to_string()],
        Some("claude-code") => [
            "AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN",
            "AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN",
            "AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        _ => Vec::new(),
    }
}
