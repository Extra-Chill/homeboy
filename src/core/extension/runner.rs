use std::path::{Path, PathBuf};

use crate::component::Component;
use crate::error::Result;
use crate::server::CommandOutput;

/// Output from a extension runner script execution.
pub struct RunnerOutput {
    pub exit_code: i32,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

use super::ExtensionExecutionContext;

/// Orchestrates extension script execution for lint/test/build runners.
///
/// Encapsulates the shared logic for finding components, resolving extensions,
/// loading manifests, merging settings, and executing runner scripts.
pub struct ExtensionRunner {
    execution_context: ExtensionExecutionContext,
    settings_overrides: Vec<(String, String)>,
    env_vars: Vec<(String, String)>,
    script_args: Vec<String>,
    path_override: Option<String>,
    pre_loaded_component: Option<Component>,
    /// Override the working directory for script execution.
    /// When set, the script runs in this directory instead of deriving it from the extension path.
    /// Used by Build to run in the component's `local_path`.
    working_dir: Option<String>,
    /// Override the command string instead of constructing from extension_path + script_path.
    /// Used by Build when `command_template` produces a pre-resolved command.
    command_override: Option<String>,
}

impl ExtensionRunner {
    /// Use a pre-loaded component instead of loading by ID.
    ///
    /// This avoids re-loading from config when the caller already has a
    /// resolved component (e.g., from portable config discovery in CI).
    pub fn component(mut self, comp: Component) -> Self {
        self.pre_loaded_component = Some(comp);
        self
    }

    /// Create a runner from a pre-resolved execution context.
    pub(crate) fn for_context(execution_context: ExtensionExecutionContext) -> Self {
        Self {
            execution_context,
            settings_overrides: Vec::new(),
            env_vars: Vec::new(),
            script_args: Vec::new(),
            path_override: None,
            pre_loaded_component: None,
            working_dir: None,
            command_override: None,
        }
    }

    /// Override the component's `local_path` for this execution.
    ///
    /// Use this when running against a workspace clone or temporary checkout
    /// instead of the configured component path.
    pub fn path_override(mut self, path: Option<String>) -> Self {
        self.path_override = path;
        self
    }

    /// Add settings overrides from key=value pairs.
    pub fn settings(mut self, overrides: &[(String, String)]) -> Self {
        self.settings_overrides.extend(overrides.iter().cloned());
        self
    }

    /// Add an environment variable.
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env_vars.push((key.to_string(), value.to_string()));
        self
    }

    /// Add an environment variable if condition is true.
    pub fn env_if(mut self, condition: bool, key: &str, value: &str) -> Self {
        if condition {
            self.env_vars.push((key.to_string(), value.to_string()));
        }
        self
    }

    /// Add an environment variable if the Option is Some.
    pub(crate) fn env_opt(mut self, key: &str, value: &Option<String>) -> Self {
        if let Some(v) = value {
            self.env_vars.push((key.to_string(), v.clone()));
        }
        self
    }

    /// Add arguments to pass to the script.
    pub fn script_args(mut self, args: &[String]) -> Self {
        self.script_args.extend(args.iter().cloned());
        self
    }

    /// Set the working directory for script execution.
    ///
    /// By default, scripts run relative to the extension path. Use this to
    /// run in a different directory (e.g., the component's `local_path` for builds).
    pub(crate) fn working_dir(mut self, dir: &str) -> Self {
        self.working_dir = Some(dir.to_string());
        self
    }

    /// Override the command string instead of constructing from extension_path + script_path.
    ///
    /// Use this when the command is pre-resolved (e.g., Build's `command_template`
    /// has already been interpolated with the script path).
    pub(crate) fn command_override(mut self, command: String) -> Self {
        self.command_override = Some(command);
        self
    }

    /// Execute the extension runner script.
    ///
    /// Performs the full orchestration:
    /// 1. Load component configuration
    /// 2. Determine extension from component config
    /// 3. Find extension path
    /// 4. Validate script exists (unless command_override is set)
    /// 5. Load manifest
    /// 6. Merge settings (manifest defaults → component → overrides)
    /// 7. Prepare environment variables
    /// 8. Execute via shell
    pub fn run(&self) -> Result<RunnerOutput> {
        let prepared = super::execution::prepare_capability_run(
            &self.execution_context,
            self.pre_loaded_component.as_ref(),
            self.path_override.as_deref(),
            &self.settings_overrides,
            self.command_override.is_some(),
        )?;

        let project_path = PathBuf::from(&prepared.execution.component.local_path);
        let env_vars = self.prepare_env_vars(
            &prepared.execution.extension_path,
            &project_path,
            &prepared.settings_json,
            &prepared.execution.extension_id,
        );

        let output = self.execute_script(&prepared.execution.extension_path, &env_vars)?;

        Ok(RunnerOutput {
            exit_code: output.exit_code,
            success: output.success,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn prepare_env_vars(
        &self,
        extension_path: &Path,
        project_path: &Path,
        settings_json: &str,
        extension_name: &str,
    ) -> Vec<(String, String)> {
        super::execution::build_capability_env(
            extension_name,
            &self.execution_context.component.id,
            extension_path,
            project_path,
            settings_json,
            &self.env_vars,
        )
    }

    fn execute_script(
        &self,
        extension_path: &Path,
        env_vars: &[(String, String)],
    ) -> Result<CommandOutput> {
        super::execution::execute_capability_script(
            extension_path,
            &self.execution_context.script_path,
            &self.script_args,
            env_vars,
            self.working_dir.as_deref(),
            self.command_override.as_deref(),
        )
    }
}
