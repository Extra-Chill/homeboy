use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::extension::{
    self, extension_ready_status, is_extension_compatible, is_extension_linked,
    load_all_extensions, load_extension, run_setup,
};
use homeboy::project::{self, Project};

use crate::commands::CmdResult;

#[derive(Args)]
pub struct ExtensionArgs {
    #[command(subcommand)]
    command: ExtensionCommand,
}

#[derive(Subcommand)]
enum ExtensionCommand {
    /// Show available extensions with compatibility status
    List {
        /// Project ID to filter compatible extensions
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Show detailed information about a extension
    Show {
        /// Extension ID
        extension_id: String,
    },
    /// Execute a extension
    Run {
        /// Extension ID
        extension_id: String,
        /// Project ID (defaults to active project)
        #[arg(short, long)]
        project: Option<String>,
        /// Component ID (required when ambiguous)
        #[arg(short, long)]
        component: Option<String>,
        /// Input values as key=value pairs
        #[arg(short, long, value_parser = super::parse_key_val)]
        input: Vec<(String, String)>,
        /// Run only specific steps (comma-separated, e.g. --step phpunit,phpcs)
        #[arg(long)]
        step: Option<String>,
        /// Skip specific steps (comma-separated, e.g. --skip phpstan,lint)
        #[arg(long)]
        skip: Option<String>,
        /// Arguments to pass to the extension (for CLI extensions)
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        /// Stream output directly to terminal (default: auto-detect based on TTY)
        #[arg(long)]
        stream: bool,
        /// Disable streaming and capture output (default: auto-detect based on TTY)
        #[arg(long)]
        no_stream: bool,
    },
    /// Run the extension's setup command (if defined)
    Setup {
        /// Extension ID
        extension_id: String,
    },
    /// Install a extension from a git URL or local path
    Install {
        /// Git URL or local path to extension directory
        source: String,
        /// Override extension id
        #[arg(long)]
        id: Option<String>,
    },
    /// Update an installed extension (git pull)
    Update {
        /// Extension ID (omit with --all to update everything)
        extension_id: Option<String>,
        /// Update all installed extensions
        #[arg(long)]
        all: bool,
        /// Force update even with uncommitted changes
        #[arg(long)]
        force: bool,
    },
    /// Uninstall a extension
    Uninstall {
        /// Extension ID
        extension_id: String,
    },
    /// Execute a extension action (API call or builtin)
    Action {
        /// Extension ID
        extension_id: String,
        /// Action ID
        action_id: String,
        /// Project ID (required for API actions)
        #[arg(short, long)]
        project: Option<String>,
        /// JSON array of selected data rows
        #[arg(long)]
        data: Option<String>,
    },
    /// Run a tool from a extension's vendor directory
    Exec {
        /// Extension ID
        extension_id: String,
        /// Component ID (sets working directory to component path)
        #[arg(short, long)]
        component: Option<String>,
        /// Command and arguments to run
        #[arg(trailing_var_arg = true, required = true)]
        args: Vec<String>,
    },
    /// Update extension manifest fields
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        /// Extension ID (optional if provided in JSON body)
        extension_id: Option<String>,
        /// JSON object to merge into manifest (supports @file and - for stdin)
        #[arg(long, value_name = "JSON")]
        json: String,
        /// Replace these fields instead of merging arrays
        #[arg(long, value_name = "FIELD")]
        replace: Vec<String>,
    },
}

pub fn run(
    args: ExtensionArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ExtensionOutput> {
    match args.command {
        ExtensionCommand::List { project } => list(project),
        ExtensionCommand::Show { extension_id } => show_extension(&extension_id),
        ExtensionCommand::Run {
            extension_id,
            project,
            component,
            input,
            step,
            skip,
            args,
            stream,
            no_stream,
        } => run_extension(
            &extension_id,
            project,
            component,
            input,
            args,
            stream,
            no_stream,
            step,
            skip,
        ),
        ExtensionCommand::Setup { extension_id } => setup_extension(&extension_id),
        ExtensionCommand::Install { source, id } => install_extension(&source, id),
        ExtensionCommand::Update {
            extension_id,
            all,
            force,
        } => update_extension(extension_id.as_deref(), all, force),
        ExtensionCommand::Uninstall { extension_id } => uninstall_extension(&extension_id),
        ExtensionCommand::Action {
            extension_id,
            action_id,
            project,
            data,
        } => run_action(&extension_id, &action_id, project, data),
        ExtensionCommand::Exec {
            extension_id,
            component,
            args,
        } => exec_extension_tool(&extension_id, component, args),
        ExtensionCommand::Set {
            extension_id,
            json,
            replace,
        } => set_extension(extension_id.as_deref(), &json, &replace),
    }
}

#[derive(Serialize)]
#[serde(tag = "command")]
#[allow(clippy::large_enum_variant)]
pub enum ExtensionOutput {
    #[serde(rename = "extension.list")]
    List {
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        extensions: Vec<ExtensionEntry>,
    },
    #[serde(rename = "extension.show")]
    Show { extension: ExtensionDetail },
    #[serde(rename = "extension.run")]
    Run {
        extension_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", flatten)]
        output: Option<homeboy::utils::command::CapturedOutput>,
    },
    #[serde(rename = "extension.setup")]
    Setup { extension_id: String },
    #[serde(rename = "extension.install")]
    Install {
        extension_id: String,
        source: String,
        path: String,
        linked: bool,
    },
    #[serde(rename = "extension.update")]
    Update {
        extension_id: String,
        url: String,
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        old_version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_version: Option<String>,
    },
    #[serde(rename = "extension.update_all")]
    UpdateAll {
        updated: Vec<ExtensionUpdateEntry>,
        skipped: Vec<String>,
    },
    #[serde(rename = "extension.uninstall")]
    Uninstall {
        extension_id: String,
        path: String,
        was_linked: bool,
    },
    #[serde(rename = "extension.action")]
    Action {
        extension_id: String,
        action_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        response: serde_json::Value,
    },
    #[serde(rename = "extension.set")]
    Set {
        extension_id: String,
        updated_fields: Vec<String>,
    },
    #[serde(rename = "extension.exec")]
    Exec {
        extension_id: String,
        #[serde(skip_serializing_if = "Option::is_none", flatten)]
        output: Option<homeboy::utils::command::CapturedOutput>,
    },
    #[serde(rename = "extension.set")]
    SetBatch { batch: homeboy::BatchResult },
}

#[derive(Serialize)]
pub struct ExtensionUpdateEntry {
    pub extension_id: String,
    pub old_version: String,
    pub new_version: String,
}

#[derive(Serialize)]
pub struct ActionSummary {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub action_type: homeboy::extension::ActionType,
}

#[derive(Serialize)]

pub struct ExtensionEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub runtime: String,
    pub compatible: bool,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_detail: Option<String>,
    pub configured: bool,
    pub linked: bool,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_display_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_setup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_ready_check: Option<bool>,
}

#[derive(Serialize)]
pub struct ExtensionDetail {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub runtime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_setup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_ready_check: Option<bool>,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_detail: Option<String>,
    pub linked: bool,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliDetail>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionDetail>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<homeboy::extension::InputConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<homeboy::extension::SettingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<RequiresDetail>,
}

#[derive(Serialize)]
pub struct CliDetail {
    pub tool: String,
    pub display_name: String,
    pub command_template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_cli_path: Option<String>,
}

#[derive(Serialize)]
pub struct ActionDetail {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub action_type: homeboy::extension::ActionType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<homeboy::extension::HttpMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Serialize)]
pub struct RequiresDetail {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
}

fn list(project: Option<String>) -> CmdResult<ExtensionOutput> {
    let extensions = load_all_extensions().unwrap_or_default();

    let project_config: Option<Project> = project.as_ref().and_then(|id| project::load(id).ok());

    let entries: Vec<ExtensionEntry> = extensions
        .iter()
        .map(|extension| {
            let ready_status = extension_ready_status(extension);
            let compatible = is_extension_compatible(extension, project_config.as_ref());
            let linked = is_extension_linked(&extension.id);

            let (cli_tool, cli_display_name) = extension
                .cli
                .as_ref()
                .map(|cli| (Some(cli.tool.clone()), Some(cli.display_name.clone())))
                .unwrap_or((None, None));

            let actions: Vec<ActionSummary> = extension
                .actions
                .iter()
                .map(|a| ActionSummary {
                    id: a.id.clone(),
                    label: a.label.clone(),
                    action_type: a.action_type.clone(),
                })
                .collect();

            let has_setup = extension
                .runtime()
                .and_then(|r| r.setup_command.as_ref())
                .map(|_| true);
            let has_ready_check = extension
                .runtime()
                .and_then(|r| r.ready_check.as_ref())
                .map(|_| true);

            ExtensionEntry {
                id: extension.id.clone(),
                name: extension.name.clone(),
                version: extension.version.clone(),
                description: extension
                    .description
                    .as_ref()
                    .and_then(|d| d.lines().next())
                    .unwrap_or("")
                    .to_string(),
                runtime: if extension.executable.is_some() {
                    "executable".to_string()
                } else {
                    "platform".to_string()
                },
                compatible,
                ready: ready_status.ready,
                ready_reason: ready_status.reason,
                ready_detail: ready_status.detail,
                configured: true,
                linked,
                path: extension.extension_path.clone().unwrap_or_default(),
                cli_tool,
                cli_display_name,
                actions,
                has_setup,
                has_ready_check,
            }
        })
        .collect();

    Ok((
        ExtensionOutput::List {
            project_id: project,
            extensions: entries,
        },
        0,
    ))
}

fn show_extension(extension_id: &str) -> CmdResult<ExtensionOutput> {
    let extension = load_extension(extension_id)?;
    let ready_status = extension_ready_status(&extension);
    let linked = is_extension_linked(&extension.id);

    let has_setup = extension
        .runtime()
        .and_then(|r| r.setup_command.as_ref())
        .map(|_| true);
    let has_ready_check = extension
        .runtime()
        .and_then(|r| r.ready_check.as_ref())
        .map(|_| true);

    let cli = extension.cli.as_ref().map(|c| CliDetail {
        tool: c.tool.clone(),
        display_name: c.display_name.clone(),
        command_template: c.command_template.clone(),
        default_cli_path: c.default_cli_path.clone(),
    });

    let actions: Vec<ActionDetail> = extension
        .actions
        .iter()
        .map(|a| ActionDetail {
            id: a.id.clone(),
            label: a.label.clone(),
            action_type: a.action_type.clone(),
            endpoint: a.endpoint.clone(),
            method: a.method.clone(),
            command: a.command.clone(),
        })
        .collect();

    let requires = extension.requires.as_ref().map(|r| RequiresDetail {
        extensions: r.extensions.clone(),
        components: r.components.clone(),
    });

    let detail = ExtensionDetail {
        id: extension.id.clone(),
        name: extension.name.clone(),
        version: extension.version.clone(),
        description: extension.description.clone(),
        author: extension.author.clone(),
        homepage: extension.homepage.clone(),
        source_url: extension.source_url.clone(),
        runtime: if extension.executable.is_some() {
            "executable".to_string()
        } else {
            "platform".to_string()
        },
        has_setup,
        has_ready_check,
        ready: ready_status.ready,
        ready_reason: ready_status.reason,
        ready_detail: ready_status.detail,
        linked,
        path: extension.extension_path.clone().unwrap_or_default(),
        cli,
        actions,
        inputs: extension.inputs().to_vec(),
        settings: extension.settings.clone(),
        requires,
    };

    Ok((ExtensionOutput::Show { extension: detail }, 0))
}

#[allow(clippy::too_many_arguments)]
fn run_extension(
    extension_id: &str,
    project: Option<String>,
    component: Option<String>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
    stream: bool,
    no_stream: bool,
    step: Option<String>,
    skip: Option<String>,
) -> CmdResult<ExtensionOutput> {
    use homeboy::extension::{ExtensionExecutionMode, ExtensionStepFilter};

    let mode = if no_stream {
        ExtensionExecutionMode::Captured
    } else if stream || crate::tty::is_stdout_tty() {
        ExtensionExecutionMode::Interactive
    } else {
        ExtensionExecutionMode::Captured
    };

    let filter = ExtensionStepFilter { step, skip };

    let result = homeboy::extension::run_extension(
        extension_id,
        project.as_deref(),
        component.as_deref(),
        inputs,
        args,
        mode,
        filter,
    )?;

    Ok((
        ExtensionOutput::Run {
            extension_id: extension_id.to_string(),
            project_id: result.project_id,
            output: result.output,
        },
        result.exit_code,
    ))
}

fn install_extension(source: &str, id: Option<String>) -> CmdResult<ExtensionOutput> {
    let result = homeboy::extension::install(source, id.as_deref())?;
    let linked = is_extension_linked(&result.extension_id);

    Ok((
        ExtensionOutput::Install {
            extension_id: result.extension_id,
            source: result.url,
            path: result.path.to_string_lossy().to_string(),
            linked,
        },
        0,
    ))
}

fn update_extension(
    extension_id: Option<&str>,
    all: bool,
    force: bool,
) -> CmdResult<ExtensionOutput> {
    if all {
        return update_all_extensions(force);
    }

    let extension_id = extension_id.ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "extension_id",
            "Provide a extension ID or use --all to update all extensions",
            None,
            None,
        )
    })?;

    // Capture version before update
    let old_version = load_extension(extension_id).ok().map(|m| m.version.clone());

    let result = extension::update(extension_id, force)?;

    // Capture version after update
    let new_version = load_extension(&result.extension_id)
        .ok()
        .map(|m| m.version.clone());

    Ok((
        ExtensionOutput::Update {
            extension_id: result.extension_id,
            url: result.url,
            path: result.path.to_string_lossy().to_string(),
            old_version,
            new_version,
        },
        0,
    ))
}

fn update_all_extensions(force: bool) -> CmdResult<ExtensionOutput> {
    let extension_ids = extension::available_extension_ids();
    let mut updated = Vec::new();
    let mut skipped = Vec::new();

    for id in &extension_ids {
        // Skip linked extensions (they're managed externally)
        if is_extension_linked(id) {
            skipped.push(id.clone());
            continue;
        }

        let old_version = load_extension(id).ok().map(|m| m.version.clone());

        match extension::update(id, force) {
            Ok(_) => {
                let new_version = load_extension(id)
                    .ok()
                    .map(|m| m.version.clone())
                    .unwrap_or_default();

                updated.push(ExtensionUpdateEntry {
                    extension_id: id.clone(),
                    old_version: old_version.unwrap_or_default(),
                    new_version,
                });
            }
            Err(_) => {
                skipped.push(id.clone());
            }
        }
    }

    Ok((ExtensionOutput::UpdateAll { updated, skipped }, 0))
}

fn uninstall_extension(extension_id: &str) -> CmdResult<ExtensionOutput> {
    let was_linked = is_extension_linked(extension_id);
    let path = homeboy::extension::uninstall(extension_id)?;

    Ok((
        ExtensionOutput::Uninstall {
            extension_id: extension_id.to_string(),
            path: path.to_string_lossy().to_string(),
            was_linked,
        },
        0,
    ))
}

fn setup_extension(extension_id: &str) -> CmdResult<ExtensionOutput> {
    let result = run_setup(extension_id)?;

    Ok((
        ExtensionOutput::Setup {
            extension_id: extension_id.to_string(),
        },
        result.exit_code,
    ))
}

fn run_action(
    extension_id: &str,
    action_id: &str,
    project_id: Option<String>,
    data: Option<String>,
) -> CmdResult<ExtensionOutput> {
    let response = homeboy::extension::run_action(
        extension_id,
        action_id,
        project_id.as_deref(),
        data.as_deref(),
    )?;

    Ok((
        ExtensionOutput::Action {
            extension_id: extension_id.to_string(),
            action_id: action_id.to_string(),
            project_id,
            response,
        },
        0,
    ))
}

fn set_extension(
    extension_id: Option<&str>,
    json: &str,
    replace_fields: &[String],
) -> CmdResult<ExtensionOutput> {
    match homeboy::extension::merge(extension_id, json, replace_fields)? {
        homeboy::MergeOutput::Single(result) => Ok((
            ExtensionOutput::Set {
                extension_id: result.id,
                updated_fields: result.updated_fields,
            },
            0,
        )),
        homeboy::MergeOutput::Bulk(batch) => {
            let exit_code = batch.exit_code();
            Ok((ExtensionOutput::SetBatch { batch }, exit_code))
        }
    }
}

fn exec_extension_tool(
    extension_id: &str,
    component: Option<String>,
    args: Vec<String>,
) -> CmdResult<ExtensionOutput> {
    let extension = load_extension(extension_id)?;
    let extension_path = extension.extension_path.as_deref().ok_or_else(|| {
        homeboy::Error::config_missing_key("extension_path", Some(extension_id.into()))
    })?;

    // Resolve working directory: component path if given, otherwise current dir
    let working_dir = if let Some(ref cid) = component {
        let comp = homeboy::component::load(cid)?;
        comp.local_path.clone()
    } else {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    };

    // Build PATH with extension's vendor/bin prepended
    let vendor_bin = format!("{}/vendor/bin", extension_path);
    let node_bin = format!("{}/node_modules/.bin", extension_path);
    let current_path = std::env::var("PATH").unwrap_or_default();
    let enriched_path = format!("{}:{}:{}", vendor_bin, node_bin, current_path);

    let env = vec![
        ("PATH", enriched_path.as_str()),
        (
            homeboy::extension::exec_context::EXTENSION_PATH,
            extension_path,
        ),
        (homeboy::extension::exec_context::EXTENSION_ID, extension_id),
    ];

    let command = args.join(" ");
    let exit_code =
        homeboy::ssh::execute_local_command_interactive(&command, Some(&working_dir), Some(&env));

    Ok((
        ExtensionOutput::Exec {
            extension_id: extension_id.to_string(),
            output: None,
        },
        exit_code,
    ))
}
