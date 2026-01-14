use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::module::{
    self, is_module_compatible, is_module_linked, is_module_ready, load_all_modules, run_setup,
};
use homeboy::project::{self, Project};

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
    /// Install a module from a git URL or local path
    Install {
        /// Git URL or local path to module directory
        source: String,
        /// Override module id
        #[arg(long)]
        id: Option<String>,
    },
    /// Update an installed module (git pull)
    Update {
        /// Module ID
        module_id: String,
    },
    /// Uninstall a module
    Uninstall {
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
    /// Update module manifest fields
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        /// Module ID (optional if provided in JSON body)
        module_id: Option<String>,
        /// JSON object to merge into manifest (supports @file and - for stdin)
        #[arg(long, value_name = "JSON")]
        json: String,
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
        ModuleCommand::Install { source, id } => install_module(&source, id),
        ModuleCommand::Update { module_id } => update_module(&module_id),
        ModuleCommand::Uninstall { module_id } => uninstall_module(&module_id),
        ModuleCommand::Action {
            module_id,
            action_id,
            project,
            data,
        } => run_action(&module_id, &action_id, project, data),
        ModuleCommand::Set { module_id, json } => set_module(module_id.as_deref(), &json),
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
        source: String,
        path: String,
        linked: bool,
    },
    #[serde(rename = "module.update")]
    Update {
        module_id: String,
        url: String,
        path: String,
    },
    #[serde(rename = "module.uninstall")]
    Uninstall {
        module_id: String,
        path: String,
        was_linked: bool,
    },
    #[serde(rename = "module.action")]
    Action {
        module_id: String,
        action_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        response: serde_json::Value,
    },
    #[serde(rename = "module.set")]
    Set {
        module_id: String,
        updated_fields: Vec<String>,
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

    let project_config: Option<Project> = project.as_ref().and_then(|id| project::load(id).ok());

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

fn install_module(source: &str, id: Option<String>) -> CmdResult<ModuleOutput> {
    let result = homeboy::module::install(source, id.as_deref())?;
    let linked = is_module_linked(&result.module_id);

    Ok((
        ModuleOutput::Install {
            module_id: result.module_id,
            source: result.url,
            path: result.path.to_string_lossy().to_string(),
            linked,
        },
        0,
    ))
}

fn update_module(module_id: &str) -> CmdResult<ModuleOutput> {
    // Core handles all validation: module existence, linked check, sourceUrl requirement
    let result = module::update(module_id, false)?;

    Ok((
        ModuleOutput::Update {
            module_id: result.module_id,
            url: result.url,
            path: result.path.to_string_lossy().to_string(),
        },
        0,
    ))
}

fn uninstall_module(module_id: &str) -> CmdResult<ModuleOutput> {
    let was_linked = is_module_linked(module_id);
    let path = homeboy::module::uninstall(module_id)?;

    Ok((
        ModuleOutput::Uninstall {
            module_id: module_id.to_string(),
            path: path.to_string_lossy().to_string(),
            was_linked,
        },
        0,
    ))
}

fn setup_module(module_id: &str) -> CmdResult<ModuleOutput> {
    let result = run_setup(module_id)?;

    Ok((
        ModuleOutput::Setup {
            module_id: module_id.to_string(),
        },
        result.exit_code,
    ))
}

fn run_action(
    module_id: &str,
    action_id: &str,
    project_id: Option<String>,
    data: Option<String>,
) -> CmdResult<ModuleOutput> {
    let response =
        homeboy::module::run_action(module_id, action_id, project_id.as_deref(), data.as_deref())?;

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

fn set_module(module_id: Option<&str>, json: &str) -> CmdResult<ModuleOutput> {
    let result = homeboy::module::merge_manifest_from_json(module_id, json)?;
    Ok((
        ModuleOutput::Set {
            module_id: result.id,
            updated_fields: result.updated_fields,
        },
        0,
    ))
}
