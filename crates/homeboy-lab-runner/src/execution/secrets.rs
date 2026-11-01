use std::collections::HashMap;

use homeboy_core::error::{Error, Result};
use homeboy_core::secret_env_plan::SecretEnvPlan;
use homeboy_core::server;

use super::super::resolve_runner_secret_env;
use super::super::RunnerCapabilityPreflight;

pub(super) const RUNTIME_SECRET_ENV_ALLOWLIST_ENV: &str = "HOMEBOY_AGENT_RUNTIME_SECRET_ENV";

pub(super) fn resolve_runner_secret_env_for_plan(
    secret_env: &HashMap<String, server::RunnerSecretEnvRef>,
    plan: &SecretEnvPlan,
    env: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut refs = HashMap::new();
    for name in plan.secret_env_names() {
        if env.contains_key(&name) {
            continue;
        }
        let source = secret_env.get(&name).ok_or_else(|| {
            Error::validation_invalid_argument(
                "secret_env",
                format!("missing runner secret env ref for {name}"),
                Some(name.clone()),
                Some(vec!["Configure the selected runner secret_env reference or pass the secret in the exec request environment.".to_string()]),
            )
        })?;
        refs.insert(name, source.clone());
    }
    resolve_runner_secret_env(&refs)
}

pub(super) fn resolve_controller_secret_env_for_command(
    secret_env: &HashMap<String, server::RunnerSecretEnvRef>,
    plan: &SecretEnvPlan,
    env: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut controller_secret_env = HashMap::new();
    let mut controller_required_names = Vec::new();
    for name in plan.secret_env_names() {
        if env.contains_key(&name) {
            continue;
        }
        if plan
            .env_materialization
            .as_ref()
            .is_some_and(|materialization| {
                materialization.secret_refs.iter().any(|reference| {
                    reference.name == name && reference.owner.as_deref() == Some("runner")
                })
            })
        {
            continue;
        }
        let Some(source) = secret_env.get(&name) else {
            controller_required_names.push(name);
            continue;
        };
        if is_runner_deferred_secret_env_ref(source) {
            continue;
        }
        controller_required_names.push(name.clone());
        controller_secret_env.insert(name, source.clone());
    }
    resolve_runner_secret_env_for_plan(
        &controller_secret_env,
        &SecretEnvPlan::from_secret_env_names(controller_required_names),
        env,
    )
}

fn is_runner_deferred_secret_env_ref(source: &server::RunnerSecretEnvRef) -> bool {
    let has_env = source
        .env
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty());
    let has_file = source
        .file
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty());
    let has_secret = source
        .secret
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty());
    (has_env || has_file) && !has_secret && has_env != has_file
}

#[cfg(test)]
pub(crate) fn runner_exec_secret_env_names(
    command: &[String],
    preflight: Option<&RunnerCapabilityPreflight>,
    explicit_names: &[String],
    env: &HashMap<String, String>,
) -> Vec<String> {
    runner_exec_secret_env_plan(command, preflight, explicit_names, env, None).secret_env_names()
}

pub(crate) fn runner_exec_secret_env_plan(
    command: &[String],
    preflight: Option<&RunnerCapabilityPreflight>,
    explicit_names: &[String],
    env: &HashMap<String, String>,
    base_plan: Option<SecretEnvPlan>,
) -> SecretEnvPlan {
    let has_base_plan = base_plan.is_some();
    let mut names = Vec::new();
    names.extend(explicit_names.iter().cloned());
    if let Some(preflight) = preflight {
        names.extend(preflight.required_env.iter().cloned());
    }
    if !has_base_plan {
        names.extend(super::super::lab::secrets::declared_agent_task_secret_env(
            command,
        ));
        names.extend(super::super::lab::secrets::declared_trace_secret_env(
            command,
        ));
        names.extend(super::super::lab::secrets::declared_tunnel_secret_env(
            command,
        ));
    }
    names.extend(declared_runtime_provider_secret_env(env));
    let mut plan = SecretEnvPlan::from_secret_env_names(names);
    plan.allow_inherited_env_names([RUNTIME_SECRET_ENV_ALLOWLIST_ENV.to_string()]);
    if let Some(base_plan) = base_plan {
        plan.merge_from(base_plan);
    }
    plan
}

/// Runtime/extension-declared secret env names for a hosted agent run.
fn declared_runtime_provider_secret_env(env: &HashMap<String, String>) -> Vec<String> {
    env.get(RUNTIME_SECRET_ENV_ALLOWLIST_ENV)
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}
