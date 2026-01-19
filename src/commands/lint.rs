use clap::Args;
use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;

use homeboy::component::ScopedModuleConfig;
use homeboy::module;

use super::CmdResult;

#[derive(Args)]
pub struct LintArgs {
    /// Component name to lint
    component: String,

    /// Auto-fix formatting issues before validating
    #[arg(long)]
    fix: bool,

    /// Override settings as key=value pairs
    #[arg(long, value_parser = parse_key_val)]
    setting: Vec<(String, String)>,
}

#[derive(Serialize)]
pub struct LintOutput {
    status: String,
    component: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    hints: Option<Vec<String>>,
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

pub fn run_json(args: LintArgs) -> CmdResult<LintOutput> {
    let component = find_component(&args.component)?;
    let (module_name, module_settings) = determine_lint_module(&component)?;
    let module_path = find_module_path(&module_name)?;

    validate_lintable_module(&module_path)?;

    let manifest = load_module_manifest(&module_path)?;
    let settings_json = merge_settings(&manifest, &module_settings, &args.setting)?;
    let project_path = PathBuf::from(&component.local_path);
    let env_vars = prepare_env_vars(&module_path, &project_path, &settings_json, &args.component, args.fix)?;

    let output = execute_lint_runner(&module_path, &env_vars)?;

    let status = if output.status.success() { "passed" } else { "failed" };
    let exit_code = output.status.code().unwrap_or(-1);

    let hints = if !output.status.success() && !args.fix {
        Some(vec![
            format!("Run 'homeboy lint {} --fix' to auto-fix formatting issues", args.component),
            "Some issues may require manual fixes".to_string(),
        ])
    } else {
        None
    };

    Ok((
        LintOutput {
            status: status.to_string(),
            component: args.component,
            output: Some(String::from_utf8_lossy(&output.stdout).to_string()),
            exit_code,
            hints,
        },
        exit_code,
    ))
}

fn find_component(component_id: &str) -> homeboy::Result<homeboy::component::Component> {
    homeboy::component::load(component_id)
}

fn determine_lint_module(
    component: &homeboy::component::Component,
) -> homeboy::Result<(String, Vec<(String, String)>)> {
    if let Some(modules) = &component.modules {
        if modules.contains_key("wordpress") {
            let settings = extract_module_settings(
                modules.get("wordpress").expect("wordpress module checked above"),
            );
            return Ok(("wordpress".to_string(), settings));
        }

        for (module_name, module_config) in modules {
            let settings = extract_module_settings(module_config);
            if !settings.is_empty() {
                return Ok((module_name.clone(), settings));
            }
        }
    }

    Err(homeboy::Error::validation_invalid_argument(
        "component",
        format!(
            "Component '{}' has no lintable modules configured",
            component.id
        ),
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
    let module_path = module::module_path(module_name);

    if module_path.exists() {
        Ok(module_path)
    } else {
        Err(homeboy::Error::validation_invalid_argument(
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

fn validate_lintable_module(module_path: &PathBuf) -> homeboy::Result<()> {
    let lint_runner = module_path.join("scripts/lint-runner.sh");
    if !lint_runner.exists() {
        return Err(homeboy::Error::validation_invalid_argument(
            "module",
            format!(
                "Module at {} does not have lint infrastructure (missing scripts/lint-runner.sh)",
                module_path.display()
            ),
            None,
            None,
        ));
    }
    Ok(())
}

fn load_module_manifest(module_path: &PathBuf) -> homeboy::Result<serde_json::Value> {
    let manifest_path = module_path.join(format!(
        "{}.json",
        module_path.file_name().unwrap().to_string_lossy()
    ));
    if !manifest_path.exists() {
        return Err(homeboy::Error::internal_io(
            format!("Module manifest not found: {}", manifest_path.display()),
            None,
        ));
    }

    let content = std::fs::read_to_string(&manifest_path).map_err(|e| {
        homeboy::Error::internal_io(
            e.to_string(),
            Some(format!("read {}", manifest_path.display())),
        )
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| homeboy::Error::validation_invalid_json(e, Some("parse manifest".to_string()), None))?;
    Ok(manifest)
}

fn merge_settings(
    manifest: &serde_json::Value,
    module_settings: &[(String, String)],
    user_overrides: &[(String, String)],
) -> homeboy::Result<String> {
    let mut settings = serde_json::json!({});

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
        for (key, value) in module_settings {
            obj.insert(key.clone(), serde_json::Value::String(value.clone()));
        }

        for (key, value) in user_overrides {
            obj.insert(key.clone(), serde_json::Value::String(value.clone()));
        }
    }

    serde_json::to_string(&settings).map_err(|e| {
        homeboy::Error::internal_io(format!("Failed to serialize settings JSON: {}", e), None)
    })
}

fn prepare_env_vars(
    module_path: &PathBuf,
    project_path: &PathBuf,
    settings_json: &str,
    component_id: &str,
    auto_fix: bool,
) -> homeboy::Result<Vec<(String, String)>> {
    let module_name = module_path.file_name().unwrap().to_string_lossy();

    let mut env_vars = vec![
        ("HOMEBOY_EXEC_CONTEXT_VERSION".to_string(), "1".to_string()),
        ("HOMEBOY_MODULE_ID".to_string(), module_name.to_string()),
        (
            "HOMEBOY_MODULE_PATH".to_string(),
            module_path.to_string_lossy().to_string(),
        ),
        (
            "HOMEBOY_PROJECT_PATH".to_string(),
            project_path.to_string_lossy().to_string(),
        ),
        ("HOMEBOY_COMPONENT_ID".to_string(), component_id.to_string()),
        (
            "HOMEBOY_COMPONENT_PATH".to_string(),
            project_path.to_string_lossy().to_string(),
        ),
        ("HOMEBOY_SETTINGS_JSON".to_string(), settings_json.to_string()),
    ];

    if auto_fix {
        env_vars.push(("HOMEBOY_AUTO_FIX".to_string(), "1".to_string()));
    }

    Ok(env_vars)
}

fn execute_lint_runner(
    module_path: &PathBuf,
    env_vars: &[(String, String)],
) -> homeboy::Result<std::process::Output> {
    let lint_runner_path = module_path.join("scripts/lint-runner.sh");

    let output = Command::new(&lint_runner_path)
        .envs(env_vars.iter().cloned())
        .output()
        .map_err(|e| {
            homeboy::Error::internal_io(
                format!("Failed to execute lint runner: {}", e),
                Some(format!("Command: {}", lint_runner_path.display())),
            )
        })?;

    Ok(output)
}
