//! execute — extracted from execution.rs.

use crate::engine::shell;
use crate::engine::{template, validation};
use crate::error::{Error, Result};
use crate::project::{self, Project};
use crate::server::{
    execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, CommandOutput,
};
use std::path::Path;
use super::super::load_extension;
use crate::component::{self, Component};
use crate::engine::command::CapturedOutput;
use crate::server::http::ApiClient;
use serde::Serialize;
use std::collections::HashMap;
use super::super::manifest::{ActionConfig, ActionType, ExtensionManifest, HttpMethod, RuntimeConfig};
use super::super::runner_contract::RunnerStepFilter;
use super::super::scope::ExtensionScope;
use super::serialize_settings;
use super::resolve_extension_context;
use super::ExtensionExecutionOutcome;
use super::ExtensionExecutionMode;
use super::extension_runtime;
use super::ExtensionStepFilter;


pub fn execute_capability_script(
    extension_path: &Path,
    script_path: &str,
    script_args: &[String],
    env_vars: &[(String, String)],
    working_dir: Option<&str>,
    command_override: Option<&str>,
) -> Result<CommandOutput> {
    let command = if let Some(cmd) = command_override {
        cmd.to_string()
    } else {
        let resolved = extension_path.join(script_path);
        let mut cmd = shell::quote_path(&resolved.to_string_lossy());
        if !script_args.is_empty() {
            cmd.push(' ');
            cmd.push_str(&shell::quote_args(script_args));
        }
        cmd
    };

    let env_refs: Vec<(&str, &str)> = env_vars
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let env_opt = if env_refs.is_empty() {
        None
    } else {
        Some(env_refs.as_slice())
    };

    if let Some(dir) = working_dir {
        Ok(execute_local_command_in_dir(&command, Some(dir), env_opt))
    } else {
        Ok(execute_local_command_passthrough(&command, None, env_opt))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_extension_runtime(
    extension_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
    payload: Option<&serde_json::Value>,
    working_dir: Option<&str>,
    mode: ExtensionExecutionMode,
    filter: &ExtensionStepFilter,
) -> Result<ExtensionExecutionOutcome> {
    // Shell execution is required for extension runtime commands by design:
    // - Runtime commands execute bash scripts (set -euo pipefail, arrays, jq)
    // - Scripts use bash features (arrays, variable expansion, subshells)
    // - Commands like "{{extensionPath}}/scripts/publish-github.sh" need shell
    // - Environment variable passing requires shell environment
    // - Direct execution cannot handle bash scripts or shell features
    // See executor.rs for detailed execution strategy decision tree
    let extension = load_extension(extension_id)?;
    let runtime = extension_runtime(&extension)?;
    let run_command = runtime.run_command.as_ref().ok_or_else(|| {
        Error::config(format!(
            "Extension '{}' does not have a runCommand defined",
            extension_id
        ))
    })?;

    let extension_path = validation::require(
        extension.extension_path.as_ref(),
        "extension",
        "extension_path not set",
    )?;

    let args_str = build_args_string(&extension, inputs, args);
    let context = resolve_extension_context(
        &extension,
        extension_id,
        project_id,
        component_id,
        run_command,
    )?;

    let settings_json = if let Some(payload) = payload {
        payload.to_string()
    } else {
        serialize_settings(&context.settings)?
    };

    let vars = build_template_vars(
        extension_path,
        &args_str,
        runtime,
        context.project.as_ref(),
        &context.project_id,
    );
    let mut env_pairs = build_runtime_env(runtime, &context, &vars, &settings_json, extension_path);

    env_pairs.extend(filter.to_env_pairs());

    let execution = execute_extension_command(
        run_command,
        &vars,
        working_dir.or(Some(extension_path.as_str())),
        &env_pairs,
        mode,
    )?;

    Ok(ExtensionExecutionOutcome {
        project_id: context.project_id,
        result: execution,
    })
}
