use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use homeboy_core::config::ModuleScope;
use homeboy_core::config::{AppPaths, ConfigManager, InstalledModuleConfig, ProjectConfiguration};
use homeboy_core::module::{load_all_modules, load_module, ModuleManifest, RuntimeType};
use homeboy_core::ssh::execute_local_command_interactive;
use homeboy_core::template;

use crate::commands::CmdResult;

struct ModuleExecContext {
    module_id: String,
    project_id: Option<String>,
    component_id: Option<String>,
    settings_json: String,
}

fn module_exec_context_env(context: &ModuleExecContext) -> Vec<(&'static str, String)> {
    let mut env: Vec<(&'static str, String)> = vec![
        ("HOMEBOY_EXEC_CONTEXT_VERSION", "1".to_string()),
        ("HOMEBOY_MODULE_ID", context.module_id.clone()),
        ("HOMEBOY_SETTINGS_JSON", context.settings_json.clone()),
    ];

    if let Some(ref project_id) = context.project_id {
        env.push(("HOMEBOY_PROJECT_ID", project_id.clone()));
    }

    if let Some(ref component_id) = context.component_id {
        env.push(("HOMEBOY_COMPONENT_ID", component_id.clone()));
    }

    env
}

/// Find system Python by checking PATH first, then common locations (cross-platform)
fn find_system_python() -> Option<String> {
    // Platform-specific lookup command and Python names
    #[cfg(windows)]
    let (lookup_cmd, python_names) = ("where", &["python", "python3"]);

    #[cfg(not(windows))]
    let (lookup_cmd, python_names) = ("which", &["python3", "python"]);

    // Try PATH lookup first (most portable)
    for name in python_names {
        if let Ok(output) = Command::new(lookup_cmd).arg(name).output() {
            if output.status.success() {
                // Take first line (Windows `where` may return multiple paths)
                let path = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !path.is_empty() && Path::new(&path).exists() {
                    return Some(path);
                }
            }
        }
    }

    // Platform-specific fallback paths
    #[cfg(windows)]
    let common_paths: &[&str] = &[]; // Windows relies on PATH; no standard locations

    #[cfg(target_os = "macos")]
    let common_paths: &[&str] = &[
        "/opt/homebrew/bin/python3", // M1/M2 Mac (Homebrew)
        "/usr/local/bin/python3",    // Intel Mac (Homebrew)
        "/usr/bin/python3",          // System Python
    ];

    #[cfg(all(not(windows), not(target_os = "macos")))]
    let common_paths: &[&str] = &[
        "/usr/bin/python3",       // System Python
        "/usr/local/bin/python3", // Local install
    ];

    for path in common_paths {
        if Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    None
}

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
        /// Component ID (required when ambiguous)
        #[arg(short, long)]
        component: Option<String>,
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
    /// Install a module from a git repository URL
    Install {
        /// Git repository URL
        url: String,
        /// Override module id (directory name)
        #[arg(long)]
        id: Option<String>,
    },
    /// Update an installed module (git pull)
    Update {
        /// Module ID
        module_id: String,
        /// Force update even if module has local changes
        #[arg(long)]
        force: bool,
    },
    /// Uninstall a module (remove its directory)
    Uninstall {
        /// Module ID
        module_id: String,
        /// Delete without confirmation
        #[arg(long)]
        force: bool,
    },
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

pub fn run(args: ModuleArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ModuleOutput> {
    match args.command {
        ModuleCommand::List { project } => list(project),
        ModuleCommand::Run {
            module_id,
            project,
            component,
            input,
            args,
        } => run_module(&module_id, project, component, input, args),
        ModuleCommand::Setup { module_id } => setup_module(&module_id),
        ModuleCommand::Install { url, id } => install_module(&url, id),
        ModuleCommand::Update { module_id, force } => update_module(&module_id, force),
        ModuleCommand::Uninstall { module_id, force } => uninstall_module(&module_id, force),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed: Option<ModuleInstallOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<ModuleUpdateOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uninstalled: Option<ModuleUninstallOutput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleInstallOutput {
    pub url: String,
    pub path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleUpdateOutput {
    pub url: String,
    pub path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleUninstallOutput {
    pub path: String,
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
    pub configured: bool,
}

fn list(project: Option<String>) -> CmdResult<ModuleOutput> {
    let modules = load_all_modules();

    let app_config = ConfigManager::load_app_config().ok();
    let project_config: Option<ProjectConfiguration> = project
        .as_ref()
        .and_then(|id| ConfigManager::load_project(id).ok());

    let entries: Vec<ModuleEntry> = modules
        .iter()
        .map(|module| {
            let ready = is_module_ready(module);
            let compatible = is_module_compatible(module, project_config.as_ref());

            let configured = app_config
                .as_ref()
                .and_then(|app| app.installed_modules.as_ref())
                .is_some_and(|installed| installed.contains_key(&module.id));

            ModuleEntry {
                id: module.id.clone(),
                name: module.name.clone(),
                version: module.version.clone(),
                description: module
                    .description
                    .as_ref()
                    .and_then(|d| d.lines().next())
                    .unwrap_or("")
                    .to_string(),
                runtime: module
                    .runtime
                    .as_ref()
                    .map(|r| format!("{:?}", r.runtime_type).to_lowercase())
                    .unwrap_or_else(|| "none".to_string()),
                compatible,
                ready,
                configured,
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
            installed: None,
            updated: None,
            uninstalled: None,
        },
        0,
    ))
}

fn run_module(
    module_id: &str,
    project: Option<String>,
    component: Option<String>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
) -> CmdResult<ModuleOutput> {
    let module = load_module(module_id)
        .ok_or_else(|| homeboy_core::Error::other(format!("Module '{}' not found", module_id)))?;

    let runtime = module.runtime.as_ref().ok_or_else(|| {
        homeboy_core::Error::other(format!(
            "Module '{}' does not have a runtime configuration and cannot be executed",
            module_id
        ))
    })?;

    let app_config = ConfigManager::load_app_config()?;
    let installed_module = app_config
        .installed_modules
        .as_ref()
        .and_then(|m| m.get(module_id));

    if installed_module.is_none() {
        return Err(homeboy_core::Error::config(format!(
            "Module '{}' is not configured. Install it with `homeboy module install <git-url>`.",
            module_id
        )));
    }

    let input_values: HashMap<String, String> = inputs.into_iter().collect();

    let mut resolved_project_id: Option<String> = None;
    let mut resolved_component_id: Option<String> = None;

    let (runtime_type, code) = match runtime.runtime_type {
        RuntimeType::Python => {
            let effective_settings =
                ModuleScope::effective_settings(module_id, installed_module, None, None);
            let settings_json = serde_json::to_string(&effective_settings)
                .map_err(|e| homeboy_core::Error::other(e.to_string()))?;

            let exec_context = ModuleExecContext {
                module_id: module_id.to_string(),
                project_id: None,
                component_id: None,
                settings_json,
            };

            (
                "python",
                run_python_module(&module, input_values, &exec_context)?,
            )
        }
        RuntimeType::Shell => {
            let effective_settings =
                ModuleScope::effective_settings(module_id, installed_module, None, None);
            let settings_json = serde_json::to_string(&effective_settings)
                .map_err(|e| homeboy_core::Error::other(e.to_string()))?;

            let exec_context = ModuleExecContext {
                module_id: module_id.to_string(),
                project_id: None,
                component_id: None,
                settings_json,
            };

            (
                "shell",
                run_shell_module(&module, input_values, &exec_context)?,
            )
        }
        RuntimeType::Cli => {
            let requires_project = module.requires.is_some();

            let (project_config, project_id) = if requires_project {
                let project_id = project.ok_or_else(|| {
                    homeboy_core::Error::other(
                        "This module requires a project; pass --project <id>".to_string(),
                    )
                })?;

                let project_config = ConfigManager::load_project(&project_id)?;
                ModuleScope::validate_project_compatibility(&module, &project_config)?;

                (Some(project_config), Some(project_id))
            } else {
                (None, None)
            };

            resolved_project_id = project_id.clone();

            if let Some(project_config) = project_config.as_ref() {
                resolved_component_id = ModuleScope::resolve_component_scope(
                    &module,
                    project_config,
                    component.as_deref(),
                )?;
            }

            let component_config = if let Some(ref component_id) = resolved_component_id {
                Some(ConfigManager::load_component(component_id).map_err(|_| {
                    homeboy_core::Error::config(format!(
                        "Component '{}' required by module '{}' is not configured",
                        component_id, module.id
                    ))
                })?)
            } else {
                None
            };

            let effective_settings = ModuleScope::effective_settings(
                module_id,
                installed_module,
                project_config.as_ref(),
                component_config.as_ref(),
            );

            let settings_json = serde_json::to_string(&effective_settings)
                .map_err(|e| homeboy_core::Error::other(e.to_string()))?;

            let exec_context = ModuleExecContext {
                module_id: module_id.to_string(),
                project_id: project_id.clone(),
                component_id: resolved_component_id.clone(),
                settings_json,
            };

            (
                "cli",
                run_cli_module(
                    &module,
                    project_config.as_ref(),
                    project_id.as_deref(),
                    input_values,
                    args,
                    &exec_context,
                )?,
            )
        }
    };

    Ok((
        ModuleOutput {
            command: "module.run".to_string(),
            project_id: resolved_project_id,
            module_id: Some(module_id.to_string()),
            modules: None,
            runtime_type: Some(runtime_type.to_string()),
            installed: None,
            updated: None,
            uninstalled: None,
        },
        code,
    ))
}

fn run_python_module(
    module: &ModuleManifest,
    input_values: HashMap<String, String>,
    exec_context: &ModuleExecContext,
) -> homeboy_core::Result<i32> {
    let module_path = module
        .module_path
        .as_ref()
        .ok_or_else(|| homeboy_core::Error::other("module_path not set".to_string()))?;
    #[cfg(windows)]
    let venv_path = format!("{}\\venv", module_path);
    #[cfg(not(windows))]
    let venv_path = format!("{}/venv", module_path);

    #[cfg(windows)]
    let venv_python = format!("{}\\Scripts\\python.exe", venv_path);
    #[cfg(not(windows))]
    let venv_python = format!("{}/bin/python3", venv_path);

    // Determine Python executable
    let python_path = if Path::new(&venv_python).exists() {
        venv_python
    } else if let Some(system_python) = find_system_python() {
        system_python
    } else {
        return Err(homeboy_core::Error::other(
            "Python3 not found. Install Python3 and ensure it's in your PATH.".to_string(),
        ));
    };

    let runtime = module.runtime.as_ref().ok_or_else(|| {
        homeboy_core::Error::other("Module has no runtime configuration".to_string())
    })?;

    // Build entrypoint path
    let entrypoint = match &runtime.entrypoint {
        Some(e) => format!("{}/{}", module_path, e),
        None => {
            return Err(homeboy_core::Error::other(
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
    let playwright_path = AppPaths::playwright_browsers()?
        .to_string_lossy()
        .to_string();

    let mut command = Command::new(python_path);
    command
        .args(&arguments)
        .env("PLAYWRIGHT_BROWSERS_PATH", &playwright_path)
        .envs(module_exec_context_env(exec_context).into_iter())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = command.status();

    let status = status.map_err(|e| homeboy_core::Error::other(e.to_string()))?;
    Ok(status.code().unwrap_or(1))
}

fn run_shell_module(
    module: &ModuleManifest,
    input_values: HashMap<String, String>,
    exec_context: &ModuleExecContext,
) -> homeboy_core::Result<i32> {
    let module_path = module
        .module_path
        .as_ref()
        .ok_or_else(|| homeboy_core::Error::other("module_path not set".to_string()))?;

    let runtime = module.runtime.as_ref().ok_or_else(|| {
        homeboy_core::Error::other("Module has no runtime configuration".to_string())
    })?;

    // Build entrypoint path
    let entrypoint = match &runtime.entrypoint {
        Some(e) => format!("{}/{}", module_path, e),
        None => {
            return Err(homeboy_core::Error::other(
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

    let command = arguments.join(" ");

    let env = module_exec_context_env(exec_context);
    let env_pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();

    let exit_code =
        execute_local_command_interactive(&command, Some(module_path), Some(&env_pairs));
    Ok(exit_code)
}

fn run_cli_module(
    module: &ModuleManifest,
    project_config: Option<&ProjectConfiguration>,
    project_id: Option<&str>,
    input_values: HashMap<String, String>,
    extra_args: Vec<String>,
    exec_context: &ModuleExecContext,
) -> homeboy_core::Result<i32> {
    let runtime = module.runtime.as_ref().ok_or_else(|| {
        homeboy_core::Error::other("Module has no runtime configuration".to_string())
    })?;

    let command_template = match &runtime.args {
        Some(args) if !args.trim().is_empty() => args.as_str(),
        _ => {
            return Err(homeboy_core::Error::other(
                "CLI module has no runtime.args command template".to_string(),
            ));
        }
    };

    let requires_project = module.requires.is_some()
        || template::is_present(command_template, "projectId")
        || template::is_present(command_template, "sitePath")
        || template::is_present(command_template, "cliPath")
        || template::is_present(command_template, "domain");

    if requires_project {
        let project_config = project_config.ok_or_else(|| {
            homeboy_core::Error::other(
                "This module requires a project; pass --project <id>".to_string(),
            )
        })?;

        if project_id.is_none() {
            return Err(homeboy_core::Error::other(
                "This module requires a project; pass --project <id>".to_string(),
            ));
        }

        if !project_config.local_environment.is_configured() {
            return Err(homeboy_core::Error::other(
                "Local environment not configured for project. Configure 'Local Site Path' in Homeboy.app Settings."
                    .to_string(),
            ));
        }
    }

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

    let vars = if requires_project {
        let Some(project_config) = project_config else {
            return Err(homeboy_core::Error::other(
                "This module requires a project; pass --project <id>".to_string(),
            ));
        };

        local_domain = if project_config.local_environment.domain.is_empty() {
            "localhost".to_string()
        } else {
            project_config.local_environment.domain.clone()
        };

        cli_path = project_config
            .local_environment
            .cli_path
            .clone()
            .unwrap_or_default();

        vec![
            ("projectId", project_id.unwrap_or("")),
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

    let env = module_exec_context_env(exec_context);
    let env_pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();

    let exit_code = execute_local_command_interactive(&command, None, Some(&env_pairs));
    Ok(exit_code)
}

fn slugify_module_id(value: &str) -> homeboy_core::Result<String> {
    let mut output = String::new();
    let mut last_was_dash = false;

    for ch in value.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            output.push(lower);
            last_was_dash = false;
            continue;
        }

        if !last_was_dash && !output.is_empty() {
            output.push('-');
            last_was_dash = true;
        }
    }

    while output.ends_with('-') {
        output.pop();
    }

    if output.is_empty() {
        return Err(homeboy_core::Error::other(
            "Unable to derive module id".to_string(),
        ));
    }

    Ok(output)
}

#[derive(Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModuleInstallMetadata {
    source_url: String,
}

fn install_metadata_path(module_id: &str) -> homeboy_core::Result<std::path::PathBuf> {
    Ok(AppPaths::module(module_id)?.join(".install.json"))
}

fn write_install_metadata(module_id: &str, url: &str) -> homeboy_core::Result<()> {
    let path = install_metadata_path(module_id)?;
    let content = serde_json::to_string_pretty(&ModuleInstallMetadata {
        source_url: url.to_string(),
    })
    .map_err(|err| {
        homeboy_core::Error::internal_json(
            err.to_string(),
            Some("serialize module install metadata".to_string()),
        )
    })?;

    fs::write(path, content).map_err(|err| {
        homeboy_core::Error::internal_io(
            err.to_string(),
            Some("write module install metadata".to_string()),
        )
    })?;
    Ok(())
}

fn read_install_metadata(module_id: &str) -> homeboy_core::Result<ModuleInstallMetadata> {
    let path = install_metadata_path(module_id)?;
    if !path.exists() {
        return Err(homeboy_core::Error::other(format!(
            "No .install.json found for module '{module_id}'. Reinstall it with `homeboy module install`.",
        )));
    }

    let content = fs::read_to_string(path).map_err(|err| {
        homeboy_core::Error::internal_io(
            err.to_string(),
            Some("read module install metadata".to_string()),
        )
    })?;

    serde_json::from_str(&content).map_err(|err| {
        homeboy_core::Error::internal_json(
            err.to_string(),
            Some("parse module install metadata".to_string()),
        )
    })
}

fn derive_module_id_from_url(url: &str) -> homeboy_core::Result<String> {
    let trimmed = url.trim_end_matches('/');
    let segment = trimmed
        .split('/')
        .next_back()
        .unwrap_or(trimmed)
        .trim_end_matches(".git");

    slugify_module_id(segment)
}

fn confirm_dangerous_action(force: bool, message: &str) -> homeboy_core::Result<()> {
    if force {
        return Ok(());
    }

    Err(homeboy_core::Error::other(format!(
        "{message} Re-run with --force to confirm.",
    )))
}

fn is_git_workdir_clean(path: &Path) -> bool {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output();

    match output {
        Ok(output) => output.status.success() && output.stdout.is_empty(),
        Err(_) => false,
    }
}

fn install_module(url: &str, id: Option<String>) -> CmdResult<ModuleOutput> {
    let module_id = match id {
        Some(id) => slugify_module_id(&id)?,
        None => derive_module_id_from_url(url)?,
    };

    let module_dir = AppPaths::module(&module_id)?;
    if module_dir.exists() {
        return Err(homeboy_core::Error::other(format!(
            "Module '{module_id}' already exists",
        )));
    }

    AppPaths::ensure_directories()?;

    let status = Command::new("git")
        .args(["clone", url, module_dir.to_string_lossy().as_ref()])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| homeboy_core::Error::other(e.to_string()))?;

    if !status.success() {
        return Err(homeboy_core::Error::other("git clone failed".to_string()));
    }

    write_install_metadata(&module_id, url)?;

    let mut app_config = ConfigManager::load_app_config()?;
    let installed_modules = app_config
        .installed_modules
        .get_or_insert_with(Default::default);
    installed_modules
        .entry(module_id.clone())
        .and_modify(|existing| {
            if existing.source_url.is_none() {
                existing.source_url = Some(url.to_string());
            }
        })
        .or_insert_with(|| InstalledModuleConfig {
            settings: Default::default(),
            source_url: Some(url.to_string()),
        });
    ConfigManager::save_app_config(&app_config)?;

    if let Some(module) = load_module(&module_id) {
        if module
            .runtime
            .as_ref()
            .is_some_and(|r| r.runtime_type == RuntimeType::Python)
        {
            let _ = setup_module(&module_id)?;
        }
    }

    Ok((
        ModuleOutput {
            command: "module.install".to_string(),
            project_id: None,
            module_id: Some(module_id.clone()),
            modules: None,
            runtime_type: None,
            installed: Some(ModuleInstallOutput {
                url: url.to_string(),
                path: module_dir.to_string_lossy().to_string(),
            }),
            updated: None,
            uninstalled: None,
        },
        0,
    ))
}

fn update_module(module_id: &str, force: bool) -> CmdResult<ModuleOutput> {
    let module_dir = AppPaths::module(module_id)?;
    if !module_dir.exists() {
        return Err(homeboy_core::Error::other(format!(
            "Module '{module_id}' not found",
        )));
    }

    if !is_git_workdir_clean(&module_dir) {
        confirm_dangerous_action(
            force,
            "Module has uncommitted changes; update may overwrite them.",
        )?;
    }

    let metadata = read_install_metadata(module_id)?;

    let status = Command::new("git")
        .args(["pull", "--ff-only"])
        .current_dir(&module_dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| homeboy_core::Error::other(e.to_string()))?;

    if !status.success() {
        return Err(homeboy_core::Error::other("git pull failed".to_string()));
    }

    if let Some(module) = load_module(module_id) {
        if module
            .runtime
            .as_ref()
            .is_some_and(|r| r.runtime_type == RuntimeType::Python)
        {
            let _ = setup_module(module_id)?;
        }
    }

    Ok((
        ModuleOutput {
            command: "module.update".to_string(),
            project_id: None,
            module_id: Some(module_id.to_string()),
            modules: None,
            runtime_type: None,
            installed: None,
            updated: Some(ModuleUpdateOutput {
                url: metadata.source_url,
                path: module_dir.to_string_lossy().to_string(),
            }),
            uninstalled: None,
        },
        0,
    ))
}

fn uninstall_module(module_id: &str, force: bool) -> CmdResult<ModuleOutput> {
    let module_dir = AppPaths::module(module_id)?;
    if !module_dir.exists() {
        return Err(homeboy_core::Error::other(format!(
            "Module '{module_id}' not found",
        )));
    }

    confirm_dangerous_action(force, "This will permanently remove the module")?;

    fs::remove_dir_all(&module_dir).map_err(|err| {
        homeboy_core::Error::internal_io(
            err.to_string(),
            Some("remove module directory".to_string()),
        )
    })?;

    Ok((
        ModuleOutput {
            command: "module.uninstall".to_string(),
            project_id: None,
            module_id: Some(module_id.to_string()),
            modules: None,
            runtime_type: None,
            installed: None,
            updated: None,
            uninstalled: Some(ModuleUninstallOutput {
                path: module_dir.to_string_lossy().to_string(),
            }),
        },
        0,
    ))
}

fn setup_module(module_id: &str) -> CmdResult<ModuleOutput> {
    let module = load_module(module_id)
        .ok_or_else(|| homeboy_core::Error::other(format!("Module '{}' not found", module_id)))?;

    let runtime = match module.runtime.as_ref() {
        Some(r) => r,
        None => {
            return Ok((
                ModuleOutput {
                    command: "module.setup".to_string(),
                    project_id: None,
                    module_id: Some(module_id.to_string()),
                    modules: None,
                    runtime_type: Some("none".to_string()),
                    installed: None,
                    updated: None,
                    uninstalled: None,
                },
                0,
            ));
        }
    };

    if runtime.runtime_type != RuntimeType::Python {
        return Ok((
            ModuleOutput {
                command: "module.setup".to_string(),
                project_id: None,
                module_id: Some(module_id.to_string()),
                modules: None,
                runtime_type: Some(format!("{:?}", runtime.runtime_type).to_lowercase()),
                installed: None,
                updated: None,
                uninstalled: None,
            },
            0,
        ));
    }

    let module_path = module
        .module_path
        .as_ref()
        .ok_or_else(|| homeboy_core::Error::other("module_path not set".to_string()))?;

    #[cfg(windows)]
    let venv_path = format!("{}\\venv", module_path);
    #[cfg(not(windows))]
    let venv_path = format!("{}/venv", module_path);

    let system_python = find_system_python().ok_or_else(|| {
        homeboy_core::Error::other(
            "Python3 not found. Install Python3 and ensure it's in your PATH.".to_string(),
        )
    })?;

    let venv_status = Command::new(&system_python)
        .args(["-m", "venv", "--copies", &venv_path])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| homeboy_core::Error::other(e.to_string()))?;

    if !venv_status.success() {
        return Err(homeboy_core::Error::other(
            "Failed to create virtual environment".to_string(),
        ));
    }

    if let Some(deps) = runtime.dependencies.as_ref() {
        if !deps.is_empty() {
            #[cfg(windows)]
            let pip_path = format!("{}\\Scripts\\pip.exe", venv_path);
            #[cfg(not(windows))]
            let pip_path = format!("{}/bin/pip", venv_path);
            let mut pip_args = vec!["install".to_string()];
            pip_args.extend(deps.clone());

            let pip_status = Command::new(&pip_path)
                .args(&pip_args)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .map_err(|e| homeboy_core::Error::other(e.to_string()))?;

            if !pip_status.success() {
                return Err(homeboy_core::Error::other(
                    "Failed to install dependencies".to_string(),
                ));
            }
        }
    }

    if let Some(browsers) = runtime.playwright_browsers.as_ref() {
        if !browsers.is_empty() {
            #[cfg(windows)]
            let venv_python = format!("{}\\Scripts\\python.exe", venv_path);
            #[cfg(not(windows))]
            let venv_python = format!("{}/bin/python3", venv_path);
            let playwright_path = AppPaths::playwright_browsers()?
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
                .map_err(|e| homeboy_core::Error::other(e.to_string()))?;

            if !pw_status.success() {
                return Err(homeboy_core::Error::other(
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
            installed: None,
            updated: None,
            uninstalled: None,
        },
        0,
    ))
}

fn is_module_ready(module: &ModuleManifest) -> bool {
    let Some(runtime) = module.runtime.as_ref() else {
        // Modules without runtime (platform modules) are always ready
        return true;
    };

    match runtime.runtime_type {
        RuntimeType::Python => {
            // Python modules need venv to be ready
            if let Some(ref path) = module.module_path {
                #[cfg(windows)]
                let venv_python = format!("{}\\venv\\Scripts\\python.exe", path);
                #[cfg(not(windows))]
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

    // Check required modules
    for required_module in &requires.modules {
        if !project.has_module(required_module) {
            return false;
        }
    }

    // Check components
    for component in &requires.components {
        if !project.component_ids.contains(component) {
            return false;
        }
    }

    // For CLI modules, check local environment is configured
    if let Some(runtime) = module.runtime.as_ref() {
        if runtime.runtime_type == RuntimeType::Cli && !project.local_environment.is_configured() {
            return false;
        }
    }

    true
}
