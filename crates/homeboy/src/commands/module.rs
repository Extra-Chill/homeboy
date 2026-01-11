use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

use homeboy_core::config::{AppPaths, ConfigManager, ProjectConfiguration};
use homeboy_core::module::{load_all_modules, load_module, ModuleManifest, RuntimeType};
use homeboy_core::template;

use crate::commands::CmdResult;

const SYSTEM_PYTHON_PATH: &str = "/opt/homebrew/bin/python3";

#[derive(Args)]
pub struct ModuleArgs {
    #[command(subcommand)]
    command: ModuleCommand,
}

#[derive(Subcommand)]
enum ModuleCommand {
    /// Show available modules with compatibility status
    List {
        /// Project ID to filter compatible modules
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Execute a module (Python, Shell, or CLI)
    Run {
        /// Module ID
        module_id: String,
        /// Project ID (defaults to active project)
        #[arg(short, long)]
        project: Option<String>,
        /// Input values as key=value pairs
        #[arg(short, long, value_parser = parse_key_val)]
        input: Vec<(String, String)>,
        /// Arguments to pass to the module (for CLI modules)
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Setup a Python module (create venv and install dependencies)
    Setup {
        /// Module ID
        module_id: String,
    },
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

pub fn run(args: ModuleArgs) -> CmdResult<ModuleOutput> {
    match args.command {
        ModuleCommand::List { project } => list(project),
        ModuleCommand::Run {
            module_id,
            project,
            input,
            args,
        } => run_module(&module_id, project, input, args),
        ModuleCommand::Setup { module_id } => setup_module(&module_id),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleOutput {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modules: Option<Vec<ModuleEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_type: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub runtime: String,
    pub compatible: bool,
    pub ready: bool,
}

fn list(project: Option<String>) -> CmdResult<ModuleOutput> {
    let modules = load_all_modules();

    let project_config: Option<ProjectConfiguration> = project
        .as_ref()
        .and_then(|id| ConfigManager::load_project(id).ok());

    let entries: Vec<ModuleEntry> = modules
        .iter()
        .map(|module| {
            let ready = is_module_ready(module);
            ModuleEntry {
                id: module.id.clone(),
                name: module.name.clone(),
                version: module.version.clone(),
                description: module.description.lines().next().unwrap_or("").to_string(),
                runtime: format!("{:?}", module.runtime.runtime_type).to_lowercase(),
                compatible: is_module_compatible(module, project_config.as_ref()),
                ready,
            }
        })
        .collect();

    Ok((
        ModuleOutput {
            command: "module.list".to_string(),
            project_id: project,
            module_id: None,
            modules: Some(entries),
            runtime_type: None,
        },
        0,
    ))
}

fn run_module(
    module_id: &str,
    project: Option<String>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
) -> CmdResult<ModuleOutput> {
    let module = load_module(module_id)
        .ok_or_else(|| homeboy_core::Error::Other(format!("Module '{}' not found", module_id)))?;

    let input_values: HashMap<String, String> = inputs.into_iter().collect();

    let (runtime_type, code) = match module.runtime.runtime_type {
        RuntimeType::Python => ("python", run_python_module(&module, input_values)?),
        RuntimeType::Shell => ("shell", run_shell_module(&module, input_values)?),
        RuntimeType::Cli => ("cli", run_cli_module(&module, project, input_values, args)?),
    };

    Ok((
        ModuleOutput {
            command: "module.run".to_string(),
            project_id: None,
            module_id: Some(module_id.to_string()),
            modules: None,
            runtime_type: Some(runtime_type.to_string()),
        },
        code,
    ))
}

fn run_python_module(
    module: &ModuleManifest,
    input_values: HashMap<String, String>,
) -> homeboy_core::Result<i32> {
    let module_path = module
        .module_path
        .as_ref()
        .ok_or_else(|| homeboy_core::Error::Other("module_path not set".to_string()))?;
    let venv_path = format!("{}/venv", module_path);
    let venv_python = format!("{}/bin/python3", venv_path);

    // Determine Python executable
    let python_path = if Path::new(&venv_python).exists() {
        &venv_python
    } else if Path::new(SYSTEM_PYTHON_PATH).exists() {
        SYSTEM_PYTHON_PATH
    } else {
        return Err(homeboy_core::Error::Other(
            "Python not found. Install Python or run module setup.".to_string(),
        ));
    };

    // Build entrypoint path
    let entrypoint = match &module.runtime.entrypoint {
        Some(e) => format!("{}/{}", module_path, e),
        None => {
            return Err(homeboy_core::Error::Other(
                "Module has no entrypoint defined".to_string(),
            ));
        }
    };

    // Build arguments from module inputs
    let mut arguments = vec![entrypoint];
    for input in &module.inputs {
        if let Some(value) = input_values.get(&input.id) {
            if !value.is_empty() {
                arguments.push(input.arg.clone());
                arguments.push(value.clone());
            }
        }
    }

    // Set environment for Playwright
    let playwright_path = AppPaths::playwright_browsers()
        .to_string_lossy()
        .to_string();

    let status = Command::new(python_path)
        .args(&arguments)
        .env("PLAYWRIGHT_BROWSERS_PATH", &playwright_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let status = status.map_err(|e| homeboy_core::Error::Other(e.to_string()))?;
    Ok(status.code().unwrap_or(1))
}

fn run_shell_module(
    module: &ModuleManifest,
    input_values: HashMap<String, String>,
) -> homeboy_core::Result<i32> {
    let module_path = module
        .module_path
        .as_ref()
        .ok_or_else(|| homeboy_core::Error::Other("module_path not set".to_string()))?;

    // Build entrypoint path
    let entrypoint = match &module.runtime.entrypoint {
        Some(e) => format!("{}/{}", module_path, e),
        None => {
            return Err(homeboy_core::Error::Other(
                "Module has no entrypoint defined".to_string(),
            ));
        }
    };

    // Build arguments from module inputs
    let mut arguments = vec![entrypoint];
    for input in &module.inputs {
        if let Some(value) = input_values.get(&input.id) {
            if !value.is_empty() {
                arguments.push(input.arg.clone());
                arguments.push(value.clone());
            }
        }
    }

    let status = Command::new("/bin/bash")
        .args(&arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let status = status.map_err(|e| homeboy_core::Error::Other(e.to_string()))?;
    Ok(status.code().unwrap_or(1))
}

fn run_cli_module(
    module: &ModuleManifest,
    project: Option<String>,
    input_values: HashMap<String, String>,
    extra_args: Vec<String>,
) -> homeboy_core::Result<i32> {
    let command_template = match &module.runtime.args {
        Some(args) if !args.trim().is_empty() => args.as_str(),
        _ => {
            return Err(homeboy_core::Error::Other(
                "CLI module has no runtime.args command template".to_string(),
            ));
        }
    };

    let requires_project = module.requires.is_some()
        || template::is_present(command_template, "projectId")
        || template::is_present(command_template, "sitePath")
        || template::is_present(command_template, "cliPath")
        || template::is_present(command_template, "domain");

    let project_config = if requires_project {
        let project_id = project
            .or_else(|| {
                ConfigManager::load_app_config()
                    .ok()
                    .and_then(|c| c.active_project_id)
            })
            .ok_or_else(|| {
                homeboy_core::Error::Other(
                    "This module requires a project; pass --project <id>".to_string(),
                )
            })?;

        let project_config = ConfigManager::load_project(&project_id)?;

        if !project_config.local_environment.is_configured() {
            return Err(homeboy_core::Error::Other(format!(
                "Local environment not configured for project '{}'. Configure 'Local Site Path' in Homeboy.app Settings.",
                project_id
            )));
        }

        Some(project_config)
    } else {
        None
    };

    let mut argv = Vec::new();

    for input in &module.inputs {
        if let Some(value) = input_values.get(&input.id) {
            if !value.is_empty() {
                argv.push(input.arg.clone());
                argv.push(value.clone());
            }
        }
    }

    argv.extend(extra_args);
    let args_str = argv.join(" ");

    let local_domain: String;
    let cli_path: String;

    let vars = if let Some(ref project_config) = project_config {
        local_domain = if project_config.local_environment.domain.is_empty() {
            "localhost".to_string()
        } else {
            project_config.local_environment.domain.clone()
        };

        cli_path = project_config
            .local_environment
            .cli_path
            .clone()
            .unwrap_or("wp".to_string());

        vec![
            ("projectId", project_config.id.as_str()),
            ("domain", local_domain.as_str()),
            (
                "sitePath",
                project_config.local_environment.site_path.as_str(),
            ),
            ("cliPath", cli_path.as_str()),
            ("args", args_str.as_str()),
        ]
    } else {
        vec![("args", args_str.as_str())]
    };

    let command = template::render(command_template, &vars);

    let status = Command::new("sh")
        .args(["-c", &command])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let status = status.map_err(|e| homeboy_core::Error::Other(e.to_string()))?;
    Ok(status.code().unwrap_or(1))
}

fn setup_module(module_id: &str) -> CmdResult<ModuleOutput> {
    let module = load_module(module_id)
        .ok_or_else(|| homeboy_core::Error::Other(format!("Module '{}' not found", module_id)))?;

    if module.runtime.runtime_type != RuntimeType::Python {
        return Ok((
            ModuleOutput {
                command: "module.setup".to_string(),
                project_id: None,
                module_id: Some(module_id.to_string()),
                modules: None,
                runtime_type: Some(format!("{:?}", module.runtime.runtime_type).to_lowercase()),
            },
            0,
        ));
    }

    let module_path = module
        .module_path
        .as_ref()
        .ok_or_else(|| homeboy_core::Error::Other("module_path not set".to_string()))?;

    let venv_path = format!("{}/venv", module_path);

    let venv_status = Command::new(SYSTEM_PYTHON_PATH)
        .args(["-m", "venv", "--copies", &venv_path])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| homeboy_core::Error::Other(e.to_string()))?;

    if !venv_status.success() {
        return Err(homeboy_core::Error::Other(
            "Failed to create virtual environment".to_string(),
        ));
    }

    if let Some(deps) = module.runtime.dependencies.as_ref() {
        if !deps.is_empty() {
            let pip_path = format!("{}/bin/pip", venv_path);
            let mut pip_args = vec!["install".to_string()];
            pip_args.extend(deps.clone());

            let pip_status = Command::new(&pip_path)
                .args(&pip_args)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .map_err(|e| homeboy_core::Error::Other(e.to_string()))?;

            if !pip_status.success() {
                return Err(homeboy_core::Error::Other(
                    "Failed to install dependencies".to_string(),
                ));
            }
        }
    }

    if let Some(browsers) = module.runtime.playwright_browsers.as_ref() {
        if !browsers.is_empty() {
            let venv_python = format!("{}/bin/python3", venv_path);
            let playwright_path = AppPaths::playwright_browsers()
                .to_string_lossy()
                .to_string();

            let mut pw_args = vec![
                "-m".to_string(),
                "playwright".to_string(),
                "install".to_string(),
            ];
            pw_args.extend(browsers.clone());

            let pw_status = Command::new(&venv_python)
                .args(&pw_args)
                .env("PLAYWRIGHT_BROWSERS_PATH", &playwright_path)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .map_err(|e| homeboy_core::Error::Other(e.to_string()))?;

            if !pw_status.success() {
                return Err(homeboy_core::Error::Other(
                    "Failed to install Playwright browsers".to_string(),
                ));
            }
        }
    }

    Ok((
        ModuleOutput {
            command: "module.setup".to_string(),
            project_id: None,
            module_id: Some(module_id.to_string()),
            modules: None,
            runtime_type: Some("python".to_string()),
        },
        0,
    ))
}

fn is_module_ready(module: &ModuleManifest) -> bool {
    match module.runtime.runtime_type {
        RuntimeType::Python => {
            // Python modules need venv to be ready
            if let Some(ref path) = module.module_path {
                let venv_python = format!("{}/venv/bin/python3", path);
                Path::new(&venv_python).exists()
            } else {
                false
            }
        }
        RuntimeType::Shell | RuntimeType::Cli => true,
    }
}

fn is_module_compatible(module: &ModuleManifest, project: Option<&ProjectConfiguration>) -> bool {
    let Some(project) = project else {
        return true;
    };

    let Some(ref requires) = module.requires else {
        return true;
    };

    // Check project type
    if let Some(ref required_type) = requires.project_type {
        if *required_type != project.project_type {
            return false;
        }
    }

    // Check components
    if let Some(ref required_components) = requires.components {
        for component in required_components {
            if !project.component_ids.contains(component) {
                return false;
            }
        }
    }

    // For CLI modules, check local environment is configured
    if module.runtime.runtime_type == RuntimeType::Cli && !project.local_environment.is_configured()
    {
        return false;
    }

    true
}
