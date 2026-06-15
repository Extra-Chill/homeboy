use std::path::{Path, PathBuf};

use crate::core::component::Component;
use crate::core::engine::invocation::{InvocationGuard, InvocationRequirements};
use crate::core::engine::resource::ExtensionChildResourceSummary;
use crate::core::engine::run_dir::RunDir;
use crate::core::error::{Error, Result};
use crate::core::extension::{
    env_provider, exec_context, load_extension, ExtensionCapability, ExtensionPhaseTiming,
    RunnerOutput,
};

#[derive(Debug, Clone)]
pub struct ComponentScriptOutput {
    pub exit_code: i32,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub child_resource: Option<ExtensionChildResourceSummary>,
    pub extension_phase_timings: Vec<ExtensionPhaseTiming>,
}

impl From<RunnerOutput> for ComponentScriptOutput {
    fn from(output: RunnerOutput) -> Self {
        Self {
            exit_code: output.exit_code,
            success: output.success,
            stdout: output.stdout,
            stderr: output.stderr,
            child_resource: output.child_resource,
            extension_phase_timings: output.extension_phase_timings,
        }
    }
}

impl From<ComponentScriptOutput> for RunnerOutput {
    fn from(output: ComponentScriptOutput) -> Self {
        Self {
            exit_code: output.exit_code,
            success: output.success,
            stdout: output.stdout,
            stderr: output.stderr,
            child_resource: output.child_resource,
            extension_phase_timings: output.extension_phase_timings,
        }
    }
}

pub fn run_component_scripts(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
    passthrough: bool,
) -> Result<ComponentScriptOutput> {
    run_component_scripts_with_env(component, capability, source_path, passthrough, &[], &[])
}

pub(crate) fn run_component_scripts_with_env(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
    passthrough: bool,
    extra_env: &[(String, String)],
    script_args: &[String],
) -> Result<ComponentScriptOutput> {
    let commands = component.script_commands(capability);
    if commands.is_empty() {
        return Err(Error::validation_invalid_argument(
            "scripts",
            format!(
                "Component '{}' has no scripts.{} commands configured",
                component.id,
                capability.label()
            ),
            None,
            None,
        ));
    }

    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut child_resource = None;
    let env = component_script_env(component, source_path, extra_env)?;

    for command in commands {
        if passthrough {
            crate::log_status!(
                "component-script",
                "running {} script for {}: {}",
                capability.label(),
                component.id,
                command
            );
        }

        let command = command_with_args(command, script_args);
        let output = super::execution::execute_capability_script(
            source_path,
            "",
            &[],
            &env,
            Some(&source_path.to_string_lossy()),
            Some(&command),
            super::execution::CapabilityScriptOptions {
                passthrough,
                stderr_passthrough: false,
            },
        )?;

        stdout.push_str(&output.stdout);
        stderr.push_str(&output.stderr);
        child_resource = output.child_resource.clone();

        if !output.success {
            return Ok(ComponentScriptOutput {
                exit_code: output.exit_code,
                success: false,
                stdout,
                stderr,
                child_resource,
                extension_phase_timings: Vec::new(),
            });
        }
    }

    Ok(ComponentScriptOutput {
        exit_code: 0,
        success: true,
        stdout,
        stderr,
        child_resource,
        extension_phase_timings: Vec::new(),
    })
}

pub(crate) fn run_component_scripts_with_run_dir(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
    run_dir: &RunDir,
    passthrough: bool,
    extra_env: &[(String, String)],
    script_args: &[String],
) -> Result<ComponentScriptOutput> {
    let mut env = run_dir.legacy_env_vars();
    let invocation = InvocationGuard::acquire(run_dir, &InvocationRequirements::default())?;
    env.extend(invocation.env_vars());
    env.extend(extra_env.iter().cloned());
    let mut output = run_component_scripts_with_env(
        component,
        capability,
        source_path,
        passthrough,
        &env,
        script_args,
    )?;
    output.extension_phase_timings =
        super::runner::read_extension_phase_timings(&run_dir.path().to_path_buf())?;
    Ok(output)
}

fn component_script_env(
    component: &Component,
    source_path: &Path,
    extra_env: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    let source_path_value = source_path.to_string_lossy().to_string();
    let mut env = vec![
        (
            exec_context::VERSION.to_string(),
            exec_context::CURRENT_VERSION.to_string(),
        ),
        (
            exec_context::EXTENSION_ID.to_string(),
            "component-script".to_string(),
        ),
        (
            exec_context::EXTENSION_PATH.to_string(),
            source_path_value.clone(),
        ),
        (exec_context::COMPONENT_ID.to_string(), component.id.clone()),
        (exec_context::COMPONENT_PATH.to_string(), source_path_value),
        (exec_context::SETTINGS_JSON.to_string(), "{}".to_string()),
    ];
    if let Some(extensions) = &component.extensions {
        let mut extension_ids = extensions.keys().collect::<Vec<_>>();
        extension_ids.sort();
        for extension_id in extension_ids {
            let extension = load_extension(extension_id)?;
            let mut provider_env = env.clone();
            provider_env.extend(extra_env.iter().cloned());
            env.extend(env_provider::env_vars(
                &extension,
                source_path,
                &provider_env,
            )?);
        }
    }
    env.extend(extra_env.iter().cloned());
    Ok(env)
}

fn command_with_args(command: &str, script_args: &[String]) -> String {
    if script_args.is_empty() {
        return command.to_string();
    }

    format!(
        "{} {}",
        command,
        crate::core::engine::shell::quote_args(script_args)
    )
}

pub(crate) fn source_path(component: &Component, path_override: Option<&str>) -> PathBuf {
    path_override
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&component.local_path))
}

#[cfg(test)]
#[path = "../../../tests/core/extension/component_script_test.rs"]
mod component_script_test;
