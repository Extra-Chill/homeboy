use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use homeboy_core::config::ModuleScope;
use homeboy_core::config::{AppPaths, ConfigManager, InstalledModuleConfig, ProjectConfiguration};
use homeboy_core::module::{load_all_modules, load_module, ModuleManifest};
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
    /// Execute a module
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
    /// Run the module's setup command (if defined)
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
                runtime: if module.runtime.is_some() {
                    "executable".to_string()
                } else {
                    "platform".to_string()
                },
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

    let run_command = runtime.run_command.as_ref().ok_or_else(|| {
        homeboy_core::Error::other(format!(
            "Module '{}' does not have a runCommand defined",
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

    let module_path = module
        .module_path
        .as_ref()
        .ok_or_else(|| homeboy_core::Error::other("module_path not set".to_string()))?;

    let input_values: HashMap<String, String> = inputs.into_iter().collect();

    // Build args string from inputs and trailing args
    let mut argv = Vec::new();
    for input in &module.inputs {
        if let Some(value) = input_values.get(&input.id) {
            if !value.is_empty() {
                argv.push(input.arg.clone());
                argv.push(value.clone());
            }
        }
    }
    argv.extend(args);
    let args_str = argv.join(" ");

    // Check if project context is required
    let requires_project = module.requires.is_some()
        || template::is_present(run_command, "projectId")
        || template::is_present(run_command, "sitePath")
        || template::is_present(run_command, "cliPath")
        || template::is_present(run_command, "domain");

    let mut resolved_project_id: Option<String> = None;
    let mut resolved_component_id: Option<String> = None;
    let mut project_config: Option<ProjectConfiguration> = None;
    let mut component_config = None;

    if requires_project {
        let project_id = project.ok_or_else(|| {
            homeboy_core::Error::other(
                "This module requires a project; pass --project <id>".to_string(),
            )
        })?;

        let loaded_project = ConfigManager::load_project(&project_id)?;
        ModuleScope::validate_project_compatibility(&module, &loaded_project)?;

        resolved_component_id = ModuleScope::resolve_component_scope(
            &module,
            &loaded_project,
            component.as_deref(),
        )?;

        if let Some(ref comp_id) = resolved_component_id {
            component_config = Some(ConfigManager::load_component(comp_id).map_err(|_| {
                homeboy_core::Error::config(format!(
                    "Component '{}' required by module '{}' is not configured",
                    comp_id, module.id
                ))
            })?);
        }

        resolved_project_id = Some(project_id);
        project_config = Some(loaded_project);
    }

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
        project_id: resolved_project_id.clone(),
        component_id: resolved_component_id.clone(),
        settings_json,
    };

    // Build template variables
    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let local_domain: String;
    let cli_path: String;
    let site_path: String;

    let vars: Vec<(&str, &str)> = if let Some(ref proj) = project_config {
        local_domain = if proj.local_environment.domain.is_empty() {
            "localhost".to_string()
        } else {
            proj.local_environment.domain.clone()
        };
        cli_path = proj.local_environment.cli_path.clone().unwrap_or_default();
        site_path = proj.local_environment.site_path.clone();

        vec![
            ("modulePath", module_path.as_str()),
            ("entrypoint", entrypoint.as_str()),
            ("args", args_str.as_str()),
            ("projectId", resolved_project_id.as_deref().unwrap_or("")),
            ("domain", local_domain.as_str()),
            ("sitePath", site_path.as_str()),
            ("cliPath", cli_path.as_str()),
        ]
    } else {
        vec![
            ("modulePath", module_path.as_str()),
            ("entrypoint", entrypoint.as_str()),
            ("args", args_str.as_str()),
        ]
    };

    let command = template::render(run_command, &vars);

    // Build environment with module-defined env vars + exec context
    let mut env = module_exec_context_env(&exec_context);
    if let Some(ref module_env) = runtime.env {
        for (key, value) in module_env {
            let rendered_value = template::render(value, &vars);
            env.push((Box::leak(key.clone().into_boxed_str()), rendered_value));
        }
    }
    let env_pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();

    let exit_code = execute_local_command_interactive(&command, Some(module_path), Some(&env_pairs));

    Ok((
        ModuleOutput {
            command: "module.run".to_string(),
            project_id: resolved_project_id,
            module_id: Some(module_id.to_string()),
            modules: None,
            runtime_type: None,
            installed: None,
            updated: None,
            uninstalled: None,
        },
        exit_code,
    ))
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

    // Auto-run setup if module defines a setup_command
    if let Some(module) = load_module(&module_id) {
        if module
            .runtime
            .as_ref()
            .is_some_and(|r| r.setup_command.is_some())
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

    // Auto-run setup if module defines a setup_command
    if let Some(module) = load_module(module_id) {
        if module
            .runtime
            .as_ref()
            .is_some_and(|r| r.setup_command.is_some())
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
                    runtime_type: None,
                    installed: None,
                    updated: None,
                    uninstalled: None,
                },
                0,
            ));
        }
    };

    let setup_command = match &runtime.setup_command {
        Some(cmd) => cmd,
        None => {
            // No setup command defined - nothing to do
            return Ok((
                ModuleOutput {
                    command: "module.setup".to_string(),
                    project_id: None,
                    module_id: Some(module_id.to_string()),
                    modules: None,
                    runtime_type: None,
                    installed: None,
                    updated: None,
                    uninstalled: None,
                },
                0,
            ));
        }
    };

    let module_path = module
        .module_path
        .as_ref()
        .ok_or_else(|| homeboy_core::Error::other("module_path not set".to_string()))?;

    // Build template variables
    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let vars: Vec<(&str, &str)> = vec![
        ("modulePath", module_path.as_str()),
        ("entrypoint", entrypoint.as_str()),
    ];

    let command = template::render(setup_command, &vars);

    let exit_code = execute_local_command_interactive(&command, Some(module_path), None);

    if exit_code != 0 {
        return Err(homeboy_core::Error::other(format!(
            "Setup command failed with exit code {}",
            exit_code
        )));
    }

    Ok((
        ModuleOutput {
            command: "module.setup".to_string(),
            project_id: None,
            module_id: Some(module_id.to_string()),
            modules: None,
            runtime_type: None,
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

    // If module has a ready_check command, run it
    if let Some(ref ready_check) = runtime.ready_check {
        if let Some(ref module_path) = module.module_path {
            let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
            let vars: Vec<(&str, &str)> = vec![
                ("modulePath", module_path.as_str()),
                ("entrypoint", entrypoint.as_str()),
            ];
            let command = template::render(ready_check, &vars);
            let exit_code = execute_local_command_interactive(&command, Some(module_path), None);
            return exit_code == 0;
        }
        return false;
    }

    // No ready_check defined - assume ready
    true
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

    true
}
