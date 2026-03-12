use std::path::{Path, PathBuf};

use crate::component::{self, Component};
use crate::engine::shell;
use crate::error::{Error, Result};
use crate::ssh::{execute_local_command_passthrough, CommandOutput};
use crate::local_files;

/// Output from a extension runner script execution.
pub struct RunnerOutput {
    pub exit_code: i32,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

use super::{ExtensionCapability, ExtensionExecutionContext};

struct ResolvedRunnerContext {
    execution: ExtensionExecutionContext,
    settings_json: String,
}

/// Orchestrates extension script execution for test/lint runners.
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
    pub fn for_context(execution_context: ExtensionExecutionContext) -> Self {
        Self {
            execution_context,
            settings_overrides: Vec::new(),
            env_vars: Vec::new(),
            script_args: Vec::new(),
            path_override: None,
            pre_loaded_component: None,
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
    pub fn env_opt(mut self, key: &str, value: &Option<String>) -> Self {
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

    /// Execute the extension runner script.
    ///
    /// Performs the full orchestration:
    /// 1. Load component configuration
    /// 2. Determine extension from component config
    /// 3. Find extension path
    /// 4. Validate script exists
    /// 5. Load manifest
    /// 6. Merge settings (manifest defaults → component → overrides)
    /// 7. Prepare environment variables
    /// 8. Execute via shell
    pub fn run(&self) -> Result<RunnerOutput> {
        let resolved = self.resolve_context()?;
        let project_path = PathBuf::from(&resolved.execution.component.local_path);
        let env_vars = self.prepare_env_vars(
            &resolved.execution.extension_path,
            &project_path,
            &resolved.settings_json,
            &resolved.execution.extension_id,
        );

        let output = self.execute_script(&resolved.execution.extension_path, &env_vars)?;

        Ok(RunnerOutput {
            exit_code: output.exit_code,
            success: output.success,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn resolve_context(&self) -> Result<ResolvedRunnerContext> {
        let component = self.find_component()?;
        let execution = self.resolve_execution(component);

        self.validate_script_exists(&execution.extension_path, &execution.script_path)?;

        let manifest = self.load_extension_manifest(&execution.extension_path)?;
        let settings_json = self.merge_settings(&manifest, &execution.settings)?;

        Ok(ResolvedRunnerContext {
            execution,
            settings_json,
        })
    }

    fn resolve_execution(&self, component: Component) -> ExtensionExecutionContext {
        let mut execution = self.execution_context.clone();
        execution.component = component;
        if let Some(ref path) = self.path_override {
            execution.component.local_path = path.clone();
        }
        execution
    }

    fn find_component(&self) -> Result<Component> {
        let mut comp = if let Some(ref pre_loaded) = self.pre_loaded_component {
            pre_loaded.clone()
        } else {
            component::resolve_effective(
                Some(&self.execution_context.component.id),
                self.path_override.as_deref(),
                None,
            )?
        };
        if let Some(ref path) = self.path_override {
            comp.local_path = path.clone();
        }
        Ok(comp)
    }

    fn validate_script_exists(&self, extension_path: &Path, script_path: &str) -> Result<()> {
        let script_path = extension_path.join(script_path);
        if !script_path.exists() {
            return Err(Error::validation_invalid_argument(
                "extension",
                format!(
                    "Extension at {} does not have {} infrastructure (missing {})",
                    extension_path.display(),
                    self.script_description(),
                    script_path.display()
                ),
                None,
                None,
            ));
        }
        Ok(())
    }

    fn load_extension_manifest(&self, extension_path: &Path) -> Result<serde_json::Value> {
        let extension_name = extension_path
            .file_name()
            .ok_or_else(|| Error::internal_io("Extension path has no file name".to_string(), None))?
            .to_string_lossy();
        let manifest_path = extension_path.join(format!("{}.json", extension_name));

        if !manifest_path.exists() {
            return Err(Error::internal_io(
                format!("Extension manifest not found: {}", manifest_path.display()),
                None,
            ));
        }

        let content = local_files::read_file(&manifest_path, &format!("read {}", manifest_path.display()))?;

        serde_json::from_str(&content).map_err(|e| {
            Error::validation_invalid_json(e, Some("parse manifest".to_string()), None)
        })
    }

    fn merge_settings(
        &self,
        manifest: &serde_json::Value,
        extension_settings: &[(String, String)],
    ) -> Result<String> {
        let mut settings = serde_json::json!({});

        // Start with manifest defaults
        if let Some(manifest_settings) = manifest.get("settings") {
            if let Some(settings_array) = manifest_settings.as_array() {
                if let serde_json::Value::Object(ref mut obj) = settings {
                    for setting in settings_array {
                        if let (Some(id), Some(default)) = (
                            setting.get("id").and_then(|v| v.as_str()),
                            setting.get("default").and_then(|v| v.as_str()),
                        ) {
                            obj.insert(
                                id.to_string(),
                                serde_json::Value::String(default.to_string()),
                            );
                        }
                    }
                }
            }
        }

        if let serde_json::Value::Object(ref mut obj) = settings {
            // Add extension settings from component config
            for (key, value) in extension_settings {
                obj.insert(key.clone(), serde_json::Value::String(value.clone()));
            }

            // Add user overrides (these take precedence)
            for (key, value) in &self.settings_overrides {
                obj.insert(key.clone(), serde_json::Value::String(value.clone()));
            }
        }

        crate::config::to_json_string(&settings)
    }

    fn prepare_env_vars(
        &self,
        extension_path: &Path,
        project_path: &Path,
        settings_json: &str,
        extension_name: &str,
    ) -> Vec<(String, String)> {
        let component_path = project_path.to_string_lossy();
        let mut env = super::execution::build_exec_env(
            extension_name,
            None, // no project context in runner
            Some(&self.execution_context.component.id),
            settings_json,
            Some(&extension_path.to_string_lossy()),
            None,                  // no project base_path in runner
            None,                  // no individual settings
            Some(&component_path), // path_override (respects --path flag)
        );

        // Add command-specific environment variables (e.g. HOMEBOY_SKIP)
        env.extend(self.env_vars.iter().cloned());

        env
    }

    fn execute_script(
        &self,
        extension_path: &Path,
        env_vars: &[(String, String)],
    ) -> Result<CommandOutput> {
        let script_path = extension_path.join(&self.execution_context.script_path);
        let mut command = shell::quote_path(&script_path.to_string_lossy());

        // Append script arguments if any
        if !self.script_args.is_empty() {
            command.push(' ');
            command.push_str(&shell::quote_args(&self.script_args));
        }

        let env_refs: Vec<(&str, &str)> = env_vars
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        Ok(execute_local_command_passthrough(
            &command,
            None,
            Some(&env_refs),
        ))
    }

    fn script_description(&self) -> &str {
        match self.execution_context.capability {
            ExtensionCapability::Lint => "lint",
            ExtensionCapability::Test => "test",
            ExtensionCapability::Build => "build",
        }
    }
}
