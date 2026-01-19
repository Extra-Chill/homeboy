use std::path::PathBuf;

use crate::component::{self, Component, ScopedModuleConfig};
use crate::error::{Error, Result};
use crate::ssh::{execute_local_command_in_dir, CommandOutput};

use super::exec_context;

/// Output from a module runner script execution.
pub struct RunnerOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub success: bool,
}

/// Orchestrates module script execution for test/lint runners.
///
/// Encapsulates the shared logic for finding components, resolving modules,
/// loading manifests, merging settings, and executing runner scripts.
pub struct ModuleRunner {
    component_id: String,
    script_name: String,
    settings_overrides: Vec<(String, String)>,
    env_vars: Vec<(String, String)>,
}

impl ModuleRunner {
    /// Create a new ModuleRunner for a component and script.
    ///
    /// - `component_id`: The component to run the script for
    /// - `script_name`: The script to execute (e.g., "test-runner.sh", "lint-runner.sh")
    pub fn new(component_id: &str, script_name: &str) -> Self {
        Self {
            component_id: component_id.to_string(),
            script_name: script_name.to_string(),
            settings_overrides: Vec::new(),
            env_vars: Vec::new(),
        }
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

    /// Execute the module runner script.
    ///
    /// Performs the full orchestration:
    /// 1. Load component configuration
    /// 2. Determine module from component config
    /// 3. Find module path
    /// 4. Validate script exists
    /// 5. Load manifest
    /// 6. Merge settings (manifest defaults → component → overrides)
    /// 7. Prepare environment variables
    /// 8. Execute via shell
    pub fn run(&self) -> Result<RunnerOutput> {
        let component = self.find_component()?;
        let (module_name, module_settings) = self.determine_module(&component)?;
        let module_path = self.find_module_path(&module_name)?;

        self.validate_script_exists(&module_path)?;

        let manifest = self.load_module_manifest(&module_path)?;
        let settings_json = self.merge_settings(&manifest, &module_settings)?;
        let project_path = PathBuf::from(&component.local_path);
        let env_vars = self.prepare_env_vars(&module_path, &project_path, &settings_json);

        let output = self.execute_script(&module_path, &env_vars)?;

        Ok(RunnerOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
            success: output.success,
        })
    }

    fn find_component(&self) -> Result<Component> {
        component::load(&self.component_id)
    }

    fn determine_module(&self, component: &Component) -> Result<(String, Vec<(String, String)>)> {
        let modules = component.modules.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "component",
                format!(
                    "Component '{}' has no modules configured for {}",
                    component.id,
                    self.script_description()
                ),
                None,
                None,
            )
        })?;

        // Prefer wordpress module if available
        if modules.contains_key("wordpress") {
            let settings = extract_module_settings(
                modules.get("wordpress").expect("wordpress module checked above"),
            );
            return Ok(("wordpress".to_string(), settings));
        }

        // Otherwise use the first module with settings
        for (module_name, module_config) in modules {
            let settings = extract_module_settings(module_config);
            if !settings.is_empty() {
                return Ok((module_name.clone(), settings));
            }
        }

        Err(Error::validation_invalid_argument(
            "component",
            format!(
                "Component '{}' has no modules configured for {}",
                component.id,
                self.script_description()
            ),
            None,
            None,
        ))
    }

    fn find_module_path(&self, module_name: &str) -> Result<PathBuf> {
        let module_path = super::module_path(module_name);

        if module_path.exists() {
            Ok(module_path)
        } else {
            Err(Error::validation_invalid_argument(
                "module",
                format!(
                    "Module '{}' not found in ~/.config/homeboy/modules/",
                    module_name
                ),
                None,
                None,
            ))
        }
    }

    fn validate_script_exists(&self, module_path: &PathBuf) -> Result<()> {
        let script_path = module_path.join("scripts").join(&self.script_name);
        if !script_path.exists() {
            return Err(Error::validation_invalid_argument(
                "module",
                format!(
                    "Module at {} does not have {} infrastructure (missing scripts/{})",
                    module_path.display(),
                    self.script_description(),
                    self.script_name
                ),
                None,
                None,
            ));
        }
        Ok(())
    }

    fn load_module_manifest(&self, module_path: &PathBuf) -> Result<serde_json::Value> {
        let module_name = module_path.file_name().unwrap().to_string_lossy();
        let manifest_path = module_path.join(format!("{}.json", module_name));

        if !manifest_path.exists() {
            return Err(Error::internal_io(
                format!("Module manifest not found: {}", manifest_path.display()),
                None,
            ));
        }

        let content = std::fs::read_to_string(&manifest_path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read {}", manifest_path.display())),
            )
        })?;

        serde_json::from_str(&content)
            .map_err(|e| Error::validation_invalid_json(e, Some("parse manifest".to_string()), None))
    }

    fn merge_settings(
        &self,
        manifest: &serde_json::Value,
        module_settings: &[(String, String)],
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
                            obj.insert(id.to_string(), serde_json::Value::String(default.to_string()));
                        }
                    }
                }
            }
        }

        if let serde_json::Value::Object(ref mut obj) = settings {
            // Add module settings from component config
            for (key, value) in module_settings {
                obj.insert(key.clone(), serde_json::Value::String(value.clone()));
            }

            // Add user overrides (these take precedence)
            for (key, value) in &self.settings_overrides {
                obj.insert(key.clone(), serde_json::Value::String(value.clone()));
            }
        }

        serde_json::to_string(&settings).map_err(|e| {
            Error::internal_io(format!("Failed to serialize settings JSON: {}", e), None)
        })
    }

    fn prepare_env_vars(
        &self,
        module_path: &PathBuf,
        project_path: &PathBuf,
        settings_json: &str,
    ) -> Vec<(String, String)> {
        let module_name = module_path.file_name().unwrap().to_string_lossy();

        let mut env = vec![
            (exec_context::VERSION.to_string(), exec_context::CURRENT_VERSION.to_string()),
            (exec_context::MODULE_ID.to_string(), module_name.to_string()),
            (exec_context::MODULE_PATH.to_string(), module_path.to_string_lossy().to_string()),
            (exec_context::PROJECT_PATH.to_string(), project_path.to_string_lossy().to_string()),
            (exec_context::COMPONENT_ID.to_string(), self.component_id.clone()),
            ("HOMEBOY_COMPONENT_PATH".to_string(), project_path.to_string_lossy().to_string()),
            (exec_context::SETTINGS_JSON.to_string(), settings_json.to_string()),
        ];

        // Add command-specific environment variables
        env.extend(self.env_vars.iter().cloned());

        env
    }

    fn execute_script(
        &self,
        module_path: &PathBuf,
        env_vars: &[(String, String)],
    ) -> Result<CommandOutput> {
        let script_path = module_path.join("scripts").join(&self.script_name);
        let command = script_path.to_string_lossy().to_string();

        let env_refs: Vec<(&str, &str)> = env_vars
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        Ok(execute_local_command_in_dir(&command, None, Some(&env_refs)))
    }

    fn script_description(&self) -> &str {
        if self.script_name.contains("test") {
            "test"
        } else if self.script_name.contains("lint") {
            "lint"
        } else {
            "script"
        }
    }
}

fn extract_module_settings(module_config: &ScopedModuleConfig) -> Vec<(String, String)> {
    let mut settings = Vec::new();
    for (key, value) in &module_config.settings {
        if let Some(str_val) = value.as_str() {
            settings.push((key.clone(), str_val.to_string()));
        }
    }
    settings
}
