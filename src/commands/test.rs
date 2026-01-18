use clap::Args;
use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;
use homeboy::component::ScopedModuleConfig;
use homeboy::module;

use super::CmdResult;

#[derive(Args)]
pub struct TestArgs {
    /// Component name to test
    component: String,

    /// Override settings as key=value pairs
    #[arg(long, value_parser = parse_key_val)]
    setting: Vec<(String, String)>,
}

#[derive(Serialize)]
pub struct TestOutput {
    status: String,
    component: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    exit_code: i32,
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

pub fn run_json(args: TestArgs) -> CmdResult<TestOutput> {
    // Find component configuration
    let component = find_component(&args.component)?;

    // Determine which module to use for testing
    let (module_name, module_settings) = determine_test_module(&component)?;

    // Find module directory
    let module_path = find_module_path(&module_name)?;

    // Validate module has test infrastructure
    validate_testable_module(&module_path)?;

    // Load module manifest
    let manifest = load_module_manifest(&module_path)?;

    // Merge settings: component module settings + user overrides
    let settings_json = merge_settings(&manifest, &module_settings, &args.setting)?;

    // Get component's local path as project path
    let project_path = PathBuf::from(&component.local_path);

    // Set environment variables
    let env_vars = prepare_env_vars(&module_path, &project_path, &settings_json, &args.component)?;

    // Execute test runner script
    let output = execute_test_runner(&module_path, &env_vars)?;

    let status = if output.status.success() { "passed" } else { "failed" };
    let exit_code = output.status.code().unwrap_or(-1);

    Ok((
        TestOutput {
            status: status.to_string(),
            component: args.component,
            output: Some(String::from_utf8_lossy(&output.stdout).to_string()),
            exit_code,
        },
        exit_code,
    ))
}

fn find_component(component_id: &str) -> homeboy::Result<homeboy::component::Component> {
    homeboy::component::load(component_id)
}

fn determine_test_module(component: &homeboy::component::Component) -> homeboy::Result<(String, Vec<(String, String)>)> {
    // Check if component has modules
    if let Some(modules) = &component.modules {
        // For now, prefer wordpress module if available, otherwise use first available
        if modules.contains_key("wordpress") {
            let settings = extract_module_settings(modules.get("wordpress").unwrap());
            return Ok(("wordpress".to_string(), settings));
        }

        // If no wordpress, use the first module that has settings
        for (module_name, module_config) in modules {
            let settings = extract_module_settings(module_config);
            if !settings.is_empty() {
                return Ok((module_name.clone(), settings));
            }
        }
    }

    // No testable modules found
    Err(homeboy::Error::validation_invalid_argument(
        "component",
        format!("Component '{}' has no testable modules configured", component.id),
        None,
        None,
    ))
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

fn find_module_path(module_name: &str) -> homeboy::Result<PathBuf> {
    // Use the public module API to get the module path
    let module_path = module::module_path(module_name);

    if module_path.exists() {
        Ok(module_path)
    } else {
        Err(homeboy::Error::validation_invalid_argument(
            "module",
            format!("Module '{}' not found in ~/.config/homeboy/modules/", module_name),
            None,
            None,
        ))
    }
}

fn validate_testable_module(module_path: &PathBuf) -> homeboy::Result<()> {
    let test_runner = module_path.join("scripts/test-runner.sh");
    if !test_runner.exists() {
        return Err(homeboy::Error::validation_invalid_argument(
            "module",
            format!("Module at {} does not have test infrastructure (missing scripts/test-runner.sh)", module_path.display()),
            None,
            None,
        ));
    }
    Ok(())
}

fn load_module_manifest(module_path: &PathBuf) -> homeboy::Result<serde_json::Value> {
    let manifest_path = module_path.join(format!("{}.json", module_path.file_name().unwrap().to_string_lossy()));
    if !manifest_path.exists() {
        return Err(homeboy::Error::internal_io(
            format!("Module manifest not found: {}", manifest_path.display()),
            None,
        ));
    }

    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| homeboy::Error::internal_io(e.to_string(), Some(format!("read {}", manifest_path.display()))))?;
    let manifest: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| homeboy::Error::validation_invalid_json(e, Some("parse manifest".to_string()), None))?;
    Ok(manifest)
}

fn merge_settings(manifest: &serde_json::Value, module_settings: &[(String, String)], user_overrides: &[(String, String)]) -> homeboy::Result<String> {
    let mut settings = serde_json::json!({});

    // Start with manifest defaults
    if let Some(manifest_settings) = manifest.get("settings") {
        if let Some(settings_array) = manifest_settings.as_array() {
            if let serde_json::Value::Object(ref mut obj) = settings {
                for setting in settings_array {
                    if let (Some(id), Some(default)) = (setting.get("id").and_then(|v| v.as_str()), setting.get("default").and_then(|v| v.as_str())) {
                        obj.insert(id.to_string(), serde_json::Value::String(default.to_string()));
                    }
                }
            }
        }
    }

    if let serde_json::Value::Object(ref mut obj) = settings {
        // Add module settings (from component config)
        for (key, value) in module_settings {
            obj.insert(key.clone(), serde_json::Value::String(value.clone()));
        }

        // Add user overrides (these take precedence)
        for (key, value) in user_overrides {
            obj.insert(key.clone(), serde_json::Value::String(value.clone()));
        }
    }

    serde_json::to_string(&settings).map_err(|e| homeboy::Error::internal_io(
        format!("Failed to serialize settings JSON: {}", e),
        None,
    ))
}

fn prepare_env_vars(module_path: &PathBuf, project_path: &PathBuf, settings_json: &str, component_id: &str) -> homeboy::Result<Vec<(String, String)>> {
    let module_name = module_path.file_name().unwrap().to_string_lossy();

    Ok(vec![
        ("HOMEBOY_EXEC_CONTEXT_VERSION".to_string(), "1".to_string()),
        ("HOMEBOY_MODULE_ID".to_string(), module_name.to_string()),
        ("HOMEBOY_MODULE_PATH".to_string(), module_path.to_string_lossy().to_string()),
        ("HOMEBOY_PROJECT_PATH".to_string(), project_path.to_string_lossy().to_string()),
        ("HOMEBOY_COMPONENT_ID".to_string(), component_id.to_string()),
        ("HOMEBOY_SETTINGS_JSON".to_string(), settings_json.to_string()),
    ])
}

fn execute_test_runner(module_path: &PathBuf, env_vars: &[(String, String)]) -> homeboy::Result<std::process::Output> {
    let test_runner_path = module_path.join("scripts/test-runner.sh");

    let output = Command::new(&test_runner_path)
        .envs(env_vars.iter().cloned())
        .output()
        .map_err(|e| homeboy::Error::internal_io(
            format!("Failed to execute test runner: {}", e),
            Some(format!("Command: {}", test_runner_path.display())),
        ))?;

    Ok(output)
}