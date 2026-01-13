use clap::{Args, Subcommand};
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use homeboy::config::{AppPaths, ConfigManager, ProjectConfiguration};
use homeboy::module::{
    is_module_compatible, is_module_linked, is_module_ready, load_all_modules, load_module,
    ModuleManifest,
};
use homeboy::ssh::execute_local_command_interactive;
use homeboy::template;

use crate::commands::CmdResult;

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
    /// Symlink a local module for development
    Link {
        /// Path to local module directory
        path: String,
        /// Override module id (defaults to manifest id)
        #[arg(long)]
        id: Option<String>,
    },
    /// Remove a symlinked module (preserves source directory)
    Unlink {
        /// Module ID
        module_id: String,
    },
    /// Execute a module action (API call or builtin)
    Action {
        /// Module ID
        module_id: String,
        /// Action ID
        action_id: String,
        /// Project ID (required for API actions)
        #[arg(short, long)]
        project: Option<String>,
        /// JSON array of selected data rows
        #[arg(long)]
        data: Option<String>,
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
        ModuleCommand::Link { path, id } => link_module(&path, id),
        ModuleCommand::Unlink { module_id } => unlink_module(&module_id),
        ModuleCommand::Action {
            module_id,
            action_id,
            project,
            data,
        } => run_action(&module_id, &action_id, project, data),
    }
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "camelCase")]
pub enum ModuleOutput {
    #[serde(rename = "module.list")]
    List {
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        modules: Vec<ModuleEntry>,
    },
    #[serde(rename = "module.run")]
    Run {
        module_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
    },
    #[serde(rename = "module.setup")]
    Setup { module_id: String },
    #[serde(rename = "module.install")]
    Install {
        module_id: String,
        url: String,
        path: String,
    },
    #[serde(rename = "module.update")]
    Update {
        module_id: String,
        url: String,
        path: String,
    },
    #[serde(rename = "module.uninstall")]
    Uninstall { module_id: String, path: String },
    #[serde(rename = "module.link")]
    Link {
        module_id: String,
        source_path: String,
        symlink_path: String,
    },
    #[serde(rename = "module.unlink")]
    Unlink { module_id: String, path: String },
    #[serde(rename = "module.action")]
    Action {
        module_id: String,
        action_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        response: serde_json::Value,
    },
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
    pub linked: bool,
    pub path: String,
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
            let compatible = is_module_compatible(module, project_config.as_ref());
            let linked = is_module_linked(&module.id);

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
                configured: true,
                linked,
                path: module.module_path.clone().unwrap_or_default(),
            }
        })
        .collect();

    Ok((
        ModuleOutput::List {
            project_id: project,
            modules: entries,
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
    let result = homeboy::module::run_module(
        module_id,
        project.as_deref(),
        component.as_deref(),
        inputs,
        args,
    )?;

    Ok((
        ModuleOutput::Run {
            module_id: module_id.to_string(),
            project_id: result.project_id,
        },
        result.exit_code,
    ))
}

fn slugify_module_id(value: &str) -> homeboy::Result<String> {
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
        return Err(homeboy::Error::other(
            "Unable to derive module id".to_string(),
        ));
    }

    Ok(output)
}

#[derive(Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModuleInstallMetadata {
    source_url: String,
    #[serde(default)]
    linked: bool,
}

fn install_metadata_path(module_id: &str) -> homeboy::Result<std::path::PathBuf> {
    Ok(AppPaths::module(module_id)?.join(".install.json"))
}

fn write_install_metadata(module_id: &str, url: &str) -> homeboy::Result<()> {
    let path = install_metadata_path(module_id)?;
    let content = serde_json::to_string_pretty(&ModuleInstallMetadata {
        source_url: url.to_string(),
        linked: false,
    })
    .map_err(|err| {
        homeboy::Error::internal_json(
            err.to_string(),
            Some("serialize module install metadata".to_string()),
        )
    })?;

    fs::write(path, content).map_err(|err| {
        homeboy::Error::internal_io(
            err.to_string(),
            Some("write module install metadata".to_string()),
        )
    })?;
    Ok(())
}

fn read_install_metadata(module_id: &str) -> homeboy::Result<ModuleInstallMetadata> {
    let path = install_metadata_path(module_id)?;
    if !path.exists() {
        return Err(homeboy::Error::other(format!(
            "No .install.json found for module '{module_id}'. Reinstall it with `homeboy module install`.",
        )));
    }

    let content = fs::read_to_string(path).map_err(|err| {
        homeboy::Error::internal_io(
            err.to_string(),
            Some("read module install metadata".to_string()),
        )
    })?;

    serde_json::from_str(&content).map_err(|err| {
        homeboy::Error::internal_json(
            err.to_string(),
            Some("parse module install metadata".to_string()),
        )
    })
}

fn derive_module_id_from_url(url: &str) -> homeboy::Result<String> {
    let trimmed = url.trim_end_matches('/');
    let segment = trimmed
        .split('/')
        .next_back()
        .unwrap_or(trimmed)
        .trim_end_matches(".git");

    slugify_module_id(segment)
}

fn confirm_dangerous_action(force: bool, message: &str) -> homeboy::Result<()> {
    if force {
        return Ok(());
    }

    Err(homeboy::Error::other(format!(
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
        return Err(homeboy::Error::other(format!(
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
        .map_err(|e| homeboy::Error::other(e.to_string()))?;

    if !status.success() {
        return Err(homeboy::Error::other("git clone failed".to_string()));
    }

    write_install_metadata(&module_id, url)?;

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
        ModuleOutput::Install {
            module_id: module_id.clone(),
            url: url.to_string(),
            path: module_dir.to_string_lossy().to_string(),
        },
        0,
    ))
}

fn update_module(module_id: &str, force: bool) -> CmdResult<ModuleOutput> {
    let module_dir = AppPaths::module(module_id)?;
    if !module_dir.exists() {
        return Err(homeboy::Error::other(format!(
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
        .map_err(|e| homeboy::Error::other(e.to_string()))?;

    if !status.success() {
        return Err(homeboy::Error::other("git pull failed".to_string()));
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
        ModuleOutput::Update {
            module_id: module_id.to_string(),
            url: metadata.source_url,
            path: module_dir.to_string_lossy().to_string(),
        },
        0,
    ))
}

fn uninstall_module(module_id: &str, force: bool) -> CmdResult<ModuleOutput> {
    let module_dir = AppPaths::module(module_id)?;
    if !module_dir.exists() {
        return Err(homeboy::Error::other(format!(
            "Module '{module_id}' not found",
        )));
    }

    confirm_dangerous_action(force, "This will permanently remove the module")?;

    fs::remove_dir_all(&module_dir).map_err(|err| {
        homeboy::Error::internal_io(
            err.to_string(),
            Some("remove module directory".to_string()),
        )
    })?;

    Ok((
        ModuleOutput::Uninstall {
            module_id: module_id.to_string(),
            path: module_dir.to_string_lossy().to_string(),
        },
        0,
    ))
}

fn setup_module(module_id: &str) -> CmdResult<ModuleOutput> {
    let module = load_module(module_id)
        .ok_or_else(|| homeboy::Error::other(format!("Module '{}' not found", module_id)))?;

    let runtime = match module.runtime.as_ref() {
        Some(r) => r,
        None => {
            return Ok((
                ModuleOutput::Setup {
                    module_id: module_id.to_string(),
                },
                0,
            ));
        }
    };

    let setup_command = match &runtime.setup_command {
        Some(cmd) => cmd,
        None => {
            return Ok((
                ModuleOutput::Setup {
                    module_id: module_id.to_string(),
                },
                0,
            ));
        }
    };

    let module_path = module
        .module_path
        .as_ref()
        .ok_or_else(|| homeboy::Error::other("module_path not set".to_string()))?;

    let entrypoint = runtime.entrypoint.clone().unwrap_or_default();
    let vars: Vec<(&str, &str)> = vec![
        ("modulePath", module_path.as_str()),
        ("entrypoint", entrypoint.as_str()),
    ];

    let command = template::render(setup_command, &vars);

    let exit_code = execute_local_command_interactive(&command, Some(module_path), None);

    if exit_code != 0 {
        return Err(homeboy::Error::other(format!(
            "Setup command failed with exit code {}",
            exit_code
        )));
    }

    Ok((
        ModuleOutput::Setup {
            module_id: module_id.to_string(),
        },
        0,
    ))
}

fn link_module(path: &str, id: Option<String>) -> CmdResult<ModuleOutput> {
    let source_path = Path::new(path);

    // Resolve to absolute path
    let source_path = if source_path.is_absolute() {
        source_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| homeboy::Error::other(e.to_string()))?
            .join(source_path)
    };

    if !source_path.exists() {
        return Err(homeboy::Error::other(format!(
            "Source path does not exist: {}",
            source_path.display()
        )));
    }

    // Validate homeboy.json exists
    let manifest_path = source_path.join("homeboy.json");
    if !manifest_path.exists() {
        return Err(homeboy::Error::other(format!(
            "No homeboy.json found at {}",
            source_path.display()
        )));
    }

    // Read manifest to get module id if not provided
    let manifest_content = fs::read_to_string(&manifest_path).map_err(|e| {
        homeboy::Error::internal_io(e.to_string(), Some("read module manifest".to_string()))
    })?;
    let manifest: ModuleManifest = serde_json::from_str(&manifest_content).map_err(|e| {
        homeboy::Error::config_invalid_json(manifest_path.to_string_lossy().to_string(), e)
    })?;

    let module_id = match id {
        Some(id) => slugify_module_id(&id)?,
        None => manifest.id.clone(),
    };

    if module_id.is_empty() {
        return Err(homeboy::Error::other(
            "Module id is empty. Provide --id or ensure manifest has an id field.".to_string(),
        ));
    }

    let module_dir = AppPaths::module(&module_id)?;
    if module_dir.exists() {
        return Err(homeboy::Error::other(format!(
            "Module '{}' already exists at {}",
            module_id,
            module_dir.display()
        )));
    }

    AppPaths::ensure_directories()?;

    // Create symlink
    #[cfg(unix)]
    std::os::unix::fs::symlink(&source_path, &module_dir).map_err(|e| {
        homeboy::Error::internal_io(e.to_string(), Some("create symlink".to_string()))
    })?;

    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&source_path, &module_dir).map_err(|e| {
        homeboy::Error::internal_io(e.to_string(), Some("create symlink".to_string()))
    })?;

    // Write install metadata with linked: true
    let metadata_path = module_dir.join(".install.json");
    let metadata = ModuleInstallMetadata {
        source_url: source_path.to_string_lossy().to_string(),
        linked: true,
    };
    let metadata_content = serde_json::to_string_pretty(&metadata).map_err(|e| {
        homeboy::Error::internal_json(
            e.to_string(),
            Some("serialize module install metadata".to_string()),
        )
    })?;
    fs::write(&metadata_path, metadata_content).map_err(|e| {
        homeboy::Error::internal_io(
            e.to_string(),
            Some("write module install metadata".to_string()),
        )
    })?;

    Ok((
        ModuleOutput::Link {
            module_id,
            source_path: source_path.to_string_lossy().to_string(),
            symlink_path: module_dir.to_string_lossy().to_string(),
        },
        0,
    ))
}

fn unlink_module(module_id: &str) -> CmdResult<ModuleOutput> {
    let module_dir = AppPaths::module(module_id)?;

    if !module_dir.exists() {
        return Err(homeboy::Error::other(format!(
            "Module '{}' not found",
            module_id
        )));
    }

    if !module_dir.is_symlink() {
        return Err(homeboy::Error::other(format!(
            "Module '{}' is not a symlink. Use `uninstall` to remove git-cloned modules.",
            module_id
        )));
    }

    // Remove the symlink (this does not delete the source directory)
    fs::remove_file(&module_dir).map_err(|e| {
        homeboy::Error::internal_io(e.to_string(), Some("remove symlink".to_string()))
    })?;

    Ok((
        ModuleOutput::Unlink {
            module_id: module_id.to_string(),
            path: module_dir.to_string_lossy().to_string(),
        },
        0,
    ))
}

fn run_action(
    module_id: &str,
    action_id: &str,
    project_id: Option<String>,
    data: Option<String>,
) -> CmdResult<ModuleOutput> {
    let response = homeboy::module::run_action(
        module_id,
        action_id,
        project_id.as_deref(),
        data.as_deref(),
    )?;

    Ok((
        ModuleOutput::Action {
            module_id: module_id.to_string(),
            action_id: action_id.to_string(),
            project_id,
            response,
        },
        0,
    ))
}

