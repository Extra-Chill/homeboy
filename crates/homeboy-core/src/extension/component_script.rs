use std::path::{Path, PathBuf};
use std::time::Duration;

use super::runner::ExtensionRunner;
use crate::component::Component;
pub use crate::component_script_provider::ComponentScriptOutput;
use crate::engine::invocation::{InvocationGuard, InvocationRequirements};
use crate::engine::resource::ExtensionChildResourceSummary;
use crate::engine::run_dir::RunDir;
use crate::error::{Error, Result};
use crate::extension::{
    env_provider, exec_context, load_extension, ExtensionCapability, ExtensionPhaseTiming,
    RunnerOutput,
};

impl From<RunnerOutput> for ComponentScriptOutput {
    fn from(output: RunnerOutput) -> Self {
        Self {
            exit_code: output.exit_code,
            success: output.success,
            stdout: output.stdout,
            stderr: output.stderr,
            timed_out: output.timed_out,
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
            timed_out: output.timed_out,
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
    run_component_scripts_with_env_and_timeout(
        component,
        capability,
        source_path,
        passthrough,
        &[],
        &[],
        None,
    )
}

fn run_component_scripts_with_env_and_timeout(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
    passthrough: bool,
    extra_env: &[(String, String)],
    script_args: &[String],
    timeout: Option<Duration>,
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
    let mut timed_out = false;
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
                timeout,
            },
        )?;

        stdout.push_str(&output.stdout);
        stderr.push_str(&output.stderr);
        timed_out |= output.timed_out;
        child_resource = output.child_resource.clone();

        if !output.success {
            return Ok(ComponentScriptOutput {
                exit_code: output.exit_code,
                success: false,
                stdout,
                stderr,
                timed_out,
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
        timed_out,
        child_resource,
        extension_phase_timings: Vec::new(),
    })
}

pub fn run_component_scripts_with_env(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
    passthrough: bool,
    extra_env: &[(String, String)],
    script_args: &[String],
) -> Result<ComponentScriptOutput> {
    run_component_scripts_with_env_and_timeout(
        component,
        capability,
        source_path,
        passthrough,
        extra_env,
        script_args,
        None,
    )
}

pub fn run_component_scripts_with_run_dir(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
    run_dir: &RunDir,
    passthrough: bool,
    extra_env: &[(String, String)],
    script_args: &[String],
) -> Result<ComponentScriptOutput> {
    run_component_scripts_with_run_dir_and_timeout(
        component,
        capability,
        source_path,
        run_dir,
        passthrough,
        extra_env,
        script_args,
        None,
    )
}

pub(crate) fn run_component_scripts_with_run_dir_and_timeout(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
    run_dir: &RunDir,
    passthrough: bool,
    extra_env: &[(String, String)],
    script_args: &[String],
    timeout: Option<Duration>,
) -> Result<ComponentScriptOutput> {
    let mut env = run_dir.legacy_env_vars();
    let invocation = InvocationGuard::acquire(run_dir, &InvocationRequirements::default())?;
    env.extend(invocation.env_vars());
    env.extend(extra_env.iter().cloned());
    let mut output = run_component_scripts_with_env_and_timeout(
        component,
        capability,
        source_path,
        passthrough,
        &env,
        script_args,
        timeout,
    )?;
    output.extension_phase_timings = super::runner::read_extension_phase_timings(run_dir.path())?;
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
    let component_env = component_env_vars(component);
    if let Some(extensions) = &component.extensions {
        let mut extension_ids = extensions.keys().collect::<Vec<_>>();
        extension_ids.sort();
        for extension_id in extension_ids {
            let extension = load_extension(extension_id)?;
            let mut provider_env = env.clone();
            provider_env.extend(component_env.iter().cloned());
            provider_env.extend(extra_env.iter().cloned());
            env.extend(env_provider::env_vars(
                &extension,
                source_path,
                &provider_env,
            )?);
        }
    }
    env.extend(component_env);
    env.extend(extra_env.iter().cloned());
    Ok(env)
}

pub(crate) fn component_env_vars(component: &Component) -> Vec<(String, String)> {
    component
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn command_with_args(command: &str, script_args: &[String]) -> String {
    if script_args.is_empty() {
        return command.to_string();
    }

    format!(
        "{} {}",
        command,
        homeboy_engine_primitives::shell::quote_args(script_args)
    )
}

pub fn source_path(component: &Component, path_override: Option<&str>) -> PathBuf {
    path_override
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&component.local_path))
}

// NOTE: the component_script tests exercise the `test` CLI command (they run
// component scripts end-to-end through commands::test), so they live in the
// homeboy-cli crate's test scope rather than here — core cannot depend on the CLI
// command layer. See crates/homeboy-cli tests for component-script coverage.

/// Provider that wires the extension component-script runner into core's
/// `component_script_provider` hook.
pub struct ExtensionComponentScriptRunner;

impl crate::component_script_provider::ComponentScriptRunner for ExtensionComponentScriptRunner {
    fn run_component_scripts_with_env(
        &self,
        component: &crate::component::Component,
        capability: homeboy_extension_contract::ExtensionCapability,
        source_path: &std::path::Path,
        passthrough: bool,
        extra_env: &[(String, String)],
        script_args: &[String],
    ) -> crate::Result<ComponentScriptOutput> {
        run_component_scripts_with_env(
            component,
            capability,
            source_path,
            passthrough,
            extra_env,
            script_args,
        )
    }

    fn run_with_context(
        &self,
        context: &crate::extension_execution::ExtensionExecutionContext,
        component: &crate::component::Component,
        path_override: Option<String>,
        script_args: &[String],
    ) -> crate::Result<ComponentScriptOutput> {
        let mut runner = ExtensionRunner::for_context(context.clone())
            .component(component.clone())
            .passthrough(false)
            .script_args(script_args);
        if let Some(path) = path_override {
            runner = runner.path_override(Some(path.clone())).working_dir(&path);
        }
        Ok(runner.run()?.into())
    }
}

/// Register the extension component-script runner with core. Call once at
/// startup.
pub fn register_component_script_runner() {
    crate::component_script_provider::register_component_script_runner(Box::new(
        ExtensionComponentScriptRunner,
    ));
}
