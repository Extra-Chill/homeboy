use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::core::agent_runtime_manifest::{
    discover_agent_runtime_catalog, AgentRuntimeDiagnosticsContract,
};
use homeboy::core::extension::{
    self, extension_ready_status, is_extension_linked, load_extension, run_setup, ExtensionSummary,
    UpdateEntry,
};
use homeboy::core::project::{self, Project};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::commands::runner::{declared_tool_diagnostics, RunnerToolDiagnostics};
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
    /// Compare installed extension revisions with their current checkout HEADs
    DiffInstalled {
        /// Optional extension ID to inspect
        extension_id: Option<String>,
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
        /// Run only specific steps (comma-separated, e.g. --step test,lint)
        #[arg(long)]
        step: Option<String>,
        /// Skip specific steps (comma-separated, e.g. --skip analyze,lint)
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
        /// Git ref to check out for URL installs (branch, tag, or commit)
        #[arg(long = "ref")]
        revision: Option<String>,
        /// Replace an existing extension install/link
        #[arg(long)]
        replace: bool,
    },
    /// Refresh an extension: uninstall any existing install, then reinstall
    ///
    /// Idempotent core-owned replacement for CI's hardcoded uninstall/install
    /// sequence. Safe to re-run; a missing prior install is not an error.
    Refresh {
        /// Git URL or local path to extension directory
        source: String,
        /// Override extension id
        #[arg(long)]
        id: Option<String>,
        /// Git ref to check out for URL installs (branch, tag, or commit)
        #[arg(long = "ref")]
        revision: Option<String>,
    },
    /// Relink an installed symlinked extension to a new local source path
    Relink {
        /// Extension ID
        extension_id: String,
        /// Local path to extension directory
        source: String,
    },
    /// Install every extension configured by a component
    InstallForComponent {
        /// Git URL or local path to extension repository/directory
        #[arg(long)]
        source: String,
        /// Component path containing homeboy.json (defaults to current directory)
        #[arg(long)]
        path: Option<String>,
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
        ExtensionCommand::DiffInstalled { extension_id } => diff_installed(extension_id.as_deref()),
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
        ExtensionCommand::Install {
            source,
            id,
            revision,
            replace,
        } => install_extension(&source, id, revision, replace),
        ExtensionCommand::Refresh {
            source,
            id,
            revision,
        } => refresh_extension(&source, id.as_deref(), revision.as_deref()),
        ExtensionCommand::Relink {
            extension_id,
            source,
        } => relink_extension(&extension_id, &source),
        ExtensionCommand::InstallForComponent { source, path } => {
            install_for_component(&source, path.as_deref())
        }
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

impl ExtensionArgs {
    pub(crate) fn is_update_command(&self) -> bool {
        matches!(
            self.command,
            ExtensionCommand::Update { .. } | ExtensionCommand::Refresh { .. }
        )
    }

    pub(crate) fn update_command_label(&self) -> &'static str {
        match self.command {
            ExtensionCommand::Refresh { .. } => "extension refresh",
            _ => "extension update",
        }
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
        extensions: Vec<ExtensionSummary>,
    },
    #[serde(rename = "extension.diff_installed")]
    DiffInstalled {
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_id: Option<String>,
        extensions: Vec<InstalledExtensionDiff>,
    },
    #[serde(rename = "extension.show")]
    Show { extension: ExtensionDetail },
    #[serde(rename = "extension.run")]
    Run {
        extension_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", flatten)]
        output: Option<homeboy::core::engine::command::CapturedOutput>,
    },
    #[serde(rename = "extension.setup")]
    Setup {
        extension_id: String,
        runtime_diagnostics: ExtensionRuntimeDiagnostics,
    },
    #[serde(rename = "extension.install")]
    Install {
        extension_id: String,
        source: String,
        path: String,
        manifest_path: String,
        linked: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_revision: Option<String>,
    },
    #[serde(rename = "extension.refresh")]
    Refresh {
        extension_id: String,
        source: String,
        path: String,
        manifest_path: String,
        linked: bool,
        uninstalled_previous: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_revision: Option<String>,
        runtime_diagnostics: ExtensionRuntimeDiagnostics,
    },
    #[serde(rename = "extension.replace")]
    Replace {
        extension_id: String,
        old_path: String,
        new_path: String,
        manifest_path: String,
        source: String,
        linked: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_revision: Option<String>,
    },
    #[serde(rename = "extension.install_for_component")]
    InstallForComponent {
        component_id: String,
        source: String,
        installed: Vec<InstallEntry>,
        skipped: Vec<String>,
    },
    #[serde(rename = "extension.update")]
    Update {
        extension_id: String,
        url: String,
        path: String,
        linked: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        git_root: Option<String>,
        #[serde(flatten)]
        source_update: homeboy::core::extension::ExtensionSourceUpdate,
        #[serde(skip_serializing_if = "Option::is_none")]
        old_version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        repaired_source_metadata: Option<homeboy::core::extension::SourceMetadataRepair>,
    },
    #[serde(rename = "extension.update_all")]
    UpdateAll {
        updated: Vec<UpdateEntry>,
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
        output: Option<homeboy::core::engine::command::CapturedOutput>,
    },
    #[serde(rename = "extension.set")]
    SetBatch { batch: homeboy::core::BatchResult },
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
    pub runtime_requirements: Option<homeboy::core::extension::RuntimeRequirementsConfig>,
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
    pub source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliDetail>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionDetail>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<homeboy::core::extension::InputConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<homeboy::core::extension::SettingConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub structured_sidecars: Vec<homeboy::core::extension::StructuredSidecarDeclaration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<RequiresDetail>,
}

#[derive(Serialize)]
pub struct InstallEntry {
    pub extension_id: String,
    pub path: String,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
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
    pub action_type: homeboy::core::extension::ActionType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<homeboy::core::extension::HttpMethod>,
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

#[derive(Serialize)]
pub struct ExtensionRuntimeDiagnostics {
    pub extension_id: String,
    pub path: String,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub runtime_manifest_found: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runtime_ids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<RunnerToolDiagnostics>,
    pub freshness: ExtensionRuntimeFreshness,
    pub path_behavior: String,
    pub commands: ExtensionRuntimeDiagnosticCommands,
}

#[derive(Serialize)]
pub struct ExtensionRuntimeFreshness {
    pub source_revision_source: String,
    pub refresh_behavior: String,
}

#[derive(Serialize)]
pub struct ExtensionRuntimeDiagnosticCommands {
    pub show: String,
    pub refresh: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct InstalledExtensionDiff {
    pub extension_id: String,
    pub path: String,
    pub linked: bool,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout_head_revision: Option<String>,
    pub status: String,
    pub next_command: String,
}

fn list(project: Option<String>) -> CmdResult<ExtensionOutput> {
    let project_config: Option<Project> = project.as_ref().and_then(|id| project::load(id).ok());
    let summaries = extension::list_summaries(project_config.as_ref());

    Ok((
        ExtensionOutput::List {
            project_id: project,
            extensions: summaries,
        },
        0,
    ))
}

fn diff_installed(extension_id: Option<&str>) -> CmdResult<ExtensionOutput> {
    let rows = extension::list_summaries(None)
        .into_iter()
        .filter(|summary| extension_id.is_none_or(|id| summary.id == id))
        .map(installed_extension_diff)
        .collect::<Vec<_>>();

    if rows.is_empty() {
        if let Some(id) = extension_id {
            load_extension(id)?;
        }
    }

    Ok((
        ExtensionOutput::DiffInstalled {
            extension_id: extension_id.map(str::to_string),
            extensions: rows,
        },
        0,
    ))
}

fn installed_extension_diff(summary: ExtensionSummary) -> InstalledExtensionDiff {
    let checkout_head_revision = git_head_revision(Path::new(&summary.path));
    let status = installed_extension_diff_status(
        summary.ready,
        summary.source_revision.as_deref(),
        checkout_head_revision.as_deref(),
    );
    let next_command = installed_extension_diff_next_command(&summary, &status);

    InstalledExtensionDiff {
        extension_id: summary.id,
        path: summary.path,
        linked: summary.linked,
        ready: summary.ready,
        ready_reason: summary.ready_reason,
        installed_source_revision: summary.source_revision,
        checkout_head_revision,
        status,
        next_command,
    }
}

fn installed_extension_diff_status(
    ready: bool,
    installed_revision: Option<&str>,
    checkout_revision: Option<&str>,
) -> String {
    if !ready {
        return "unready".to_string();
    }
    match (installed_revision, checkout_revision) {
        (Some(installed), Some(checkout)) if installed == checkout => "current".to_string(),
        (Some(_), Some(_)) => "stale".to_string(),
        _ => "unknown".to_string(),
    }
}

fn installed_extension_diff_next_command(summary: &ExtensionSummary, status: &str) -> String {
    match status {
        "current" => format!("homeboy extension show {}", shell_arg(&summary.id)),
        "stale" if summary.linked => format!(
            "homeboy extension relink {} {}",
            shell_arg(&summary.id),
            shell_arg(&summary.path)
        ),
        "stale" => format!("homeboy extension update {}", shell_arg(&summary.id)),
        "unready" => format!("homeboy extension setup {}", shell_arg(&summary.id)),
        _ => format!("homeboy extension show {}", shell_arg(&summary.id)),
    }
}

fn git_head_revision(path: &Path) -> Option<String> {
    if path.as_os_str().is_empty() || !path.exists() {
        return None;
    }
    let output = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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

    let source_revision = homeboy::core::extension::read_source_revision(&extension.id);

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
        runtime_requirements: extension.runtime.clone(),
        has_setup,
        has_ready_check,
        ready: ready_status.ready,
        ready_reason: ready_status.reason,
        ready_detail: ready_status.detail,
        linked,
        path: extension.extension_path.clone().unwrap_or_default(),
        source_revision,
        cli,
        actions,
        inputs: extension.inputs().to_vec(),
        settings: extension.settings.clone(),
        structured_sidecars: extension.structured_sidecars(),
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
    use homeboy::core::extension::{ExtensionExecutionMode, ExtensionStepFilter};

    let mode = if no_stream {
        ExtensionExecutionMode::Captured
    } else if stream || crate::commands::utils::tty::is_stdout_tty() {
        ExtensionExecutionMode::Interactive
    } else {
        ExtensionExecutionMode::Captured
    };

    let filter = ExtensionStepFilter { step, skip };

    let result = homeboy::core::extension::run_extension(
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

fn install_extension(
    source: &str,
    id: Option<String>,
    revision: Option<String>,
    replace: bool,
) -> CmdResult<ExtensionOutput> {
    if replace {
        let result = homeboy::core::extension::replace_with_revision(
            source,
            id.as_deref(),
            revision.as_deref(),
        )?;
        return Ok((
            ExtensionOutput::Replace {
                extension_id: result.extension_id,
                old_path: result.old_path.to_string_lossy().to_string(),
                new_path: result.new_path.to_string_lossy().to_string(),
                manifest_path: result.manifest_path.to_string_lossy().to_string(),
                source: result.source,
                linked: result.linked,
                source_revision: result.source_revision,
            },
            0,
        ));
    }

    let result = homeboy::core::extension::install_with_revision(
        source,
        id.as_deref(),
        revision.as_deref(),
    )?;
    let linked = is_extension_linked(&result.extension_id);

    Ok((
        ExtensionOutput::Install {
            extension_id: result.extension_id,
            source: result.url,
            path: result.path.to_string_lossy().to_string(),
            manifest_path: result.manifest_path.to_string_lossy().to_string(),
            linked,
            source_revision: result.source_revision,
        },
        0,
    ))
}

fn refresh_extension(
    source: &str,
    id: Option<&str>,
    revision: Option<&str>,
) -> CmdResult<ExtensionOutput> {
    let result = homeboy::core::extension::refresh(source, id, revision)?;
    let linked = is_extension_linked(&result.extension_id);

    Ok((
        ExtensionOutput::Refresh {
            runtime_diagnostics: extension_runtime_diagnostics(
                &result.extension_id,
                result.source_revision.clone(),
            ),
            extension_id: result.extension_id,
            source: result.url,
            path: result.path.to_string_lossy().to_string(),
            manifest_path: result.manifest_path.to_string_lossy().to_string(),
            linked,
            uninstalled_previous: result.uninstalled_previous,
            source_revision: result.source_revision,
        },
        0,
    ))
}

fn relink_extension(extension_id: &str, source: &str) -> CmdResult<ExtensionOutput> {
    let result = homeboy::core::extension::relink(extension_id, source)?;

    Ok((
        ExtensionOutput::Replace {
            extension_id: result.extension_id,
            old_path: result.old_path.to_string_lossy().to_string(),
            new_path: result.new_path.to_string_lossy().to_string(),
            manifest_path: result.manifest_path.to_string_lossy().to_string(),
            source: result.source,
            linked: result.linked,
            source_revision: result.source_revision,
        },
        0,
    ))
}

fn install_for_component(source: &str, path: Option<&str>) -> CmdResult<ExtensionOutput> {
    let component = resolve_install_component(path)?;
    let result = homeboy::core::extension::install_for_component(&component, source)?;

    let installed = result
        .installed
        .into_iter()
        .map(|entry| InstallEntry {
            linked: is_extension_linked(&entry.extension_id),
            extension_id: entry.extension_id,
            path: entry.path.to_string_lossy().to_string(),
            source_revision: entry.source_revision,
        })
        .collect();

    Ok((
        ExtensionOutput::InstallForComponent {
            component_id: result.component_id,
            source: result.source,
            installed,
            skipped: result.skipped,
        },
        0,
    ))
}

fn resolve_install_component(
    path: Option<&str>,
) -> homeboy::core::Result<homeboy::core::component::Component> {
    if let Some(path) = path {
        return homeboy::core::component::discover_from_portable(Path::new(path)).ok_or_else(
            || {
                homeboy::core::Error::validation_invalid_argument(
                    "path",
                    format!("No homeboy.json found at {}", path),
                    Some(path.to_string()),
                    None,
                )
            },
        );
    }

    homeboy::core::component::resolve(None)
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
        homeboy::core::Error::validation_invalid_argument(
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
            linked: result.linked,
            source_path: result
                .source_path
                .map(|path| path.to_string_lossy().to_string()),
            git_root: result
                .git_root
                .map(|path| path.to_string_lossy().to_string()),
            source_update: result.source_update,
            old_version,
            new_version,
            repaired_source_metadata: result.repaired_source_metadata,
        },
        0,
    ))
}

fn update_all_extensions(force: bool) -> CmdResult<ExtensionOutput> {
    let result = extension::update_all(force);

    Ok((
        ExtensionOutput::UpdateAll {
            updated: result.updated,
            skipped: result.skipped,
        },
        0,
    ))
}

fn uninstall_extension(extension_id: &str) -> CmdResult<ExtensionOutput> {
    let was_linked = is_extension_linked(extension_id);
    let path = homeboy::core::extension::uninstall(extension_id)?;

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
            runtime_diagnostics: extension_runtime_diagnostics(extension_id, None),
        },
        result.exit_code,
    ))
}

fn extension_runtime_diagnostics(
    extension_id: &str,
    source_revision: Option<String>,
) -> ExtensionRuntimeDiagnostics {
    let extension = load_extension(extension_id).ok();
    let linked = is_extension_linked(extension_id);
    let path = extension
        .as_ref()
        .and_then(|extension| extension.extension_path.clone())
        .unwrap_or_default();
    let source_revision =
        source_revision.or_else(|| homeboy::core::extension::read_source_revision(extension_id));
    let matching_manifests = discover_agent_runtime_catalog()
        .manifests
        .into_iter()
        .filter(|manifest| manifest.extension_id.as_deref() == Some(extension_id))
        .collect::<Vec<_>>();
    let runtime_ids = matching_manifests
        .iter()
        .map(|manifest| manifest.id.clone())
        .collect::<Vec<_>>();
    let env = runtime_diagnostic_env(
        matching_manifests
            .iter()
            .map(|manifest| &manifest.materialization.diagnostics),
    );
    let tools = matching_manifests
        .iter()
        .flat_map(|manifest| manifest.materialization.diagnostics.tools.iter())
        .map(|declaration| declared_tool_diagnostics(declaration, None, &env))
        .collect::<Vec<_>>();

    ExtensionRuntimeDiagnostics {
        extension_id: extension_id.to_string(),
        path,
        linked,
        source_revision,
        runtime_manifest_found: !runtime_ids.is_empty(),
        runtime_ids,
        tools,
        freshness: ExtensionRuntimeFreshness {
            source_revision_source: "installed extension source metadata".to_string(),
            refresh_behavior: "extension refresh replaces the installed extension from the supplied source/ref and reports the installed source revision when available".to_string(),
        },
        path_behavior: "Shared agent runtime paths come from the extension manifest and generic runtime materialization declarations; Homeboy core does not special-case individual providers.".to_string(),
        commands: ExtensionRuntimeDiagnosticCommands {
            show: format!("homeboy extension show {}", shell_arg(extension_id)),
            refresh: format!("homeboy extension refresh <source> --id {}", shell_arg(extension_id)),
        },
    }
}

fn runtime_diagnostic_env<'a>(
    contracts: impl Iterator<Item = &'a AgentRuntimeDiagnosticsContract>,
) -> BTreeMap<String, String> {
    let mut names = Vec::new();
    for contract in contracts {
        for declaration in &contract.tools {
            names.extend(declaration.configured_binary_env.iter().cloned());
            if let Some(name) = &declaration.install_dir_env {
                names.push(name.clone());
            }
        }
        for declaration in &contract.runtimes {
            names.extend(declaration.configured_binary_env.iter().cloned());
            if let Some(name) = &declaration.install_dir_env {
                names.push(name.clone());
            }
            for package in &declaration.packages {
                if let Some(name) = &package.env_override {
                    names.push(name.clone());
                }
            }
            for diagnostic in &declaration.source_consistency {
                if !diagnostic.path.contains("${") && diagnostic.path != "configured_binary" {
                    names.push(diagnostic.path.clone());
                }
            }
        }
    }

    names
        .into_iter()
        .filter_map(|name| std::env::var(&name).ok().map(|value| (name, value)))
        .collect()
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '='))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn run_action(
    extension_id: &str,
    action_id: &str,
    project_id: Option<String>,
    data: Option<String>,
) -> CmdResult<ExtensionOutput> {
    let response = homeboy::core::extension::run_action(
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
    match homeboy::core::extension::merge(extension_id, json, replace_fields)? {
        homeboy::core::MergeOutput::Single(result) => Ok((
            ExtensionOutput::Set {
                extension_id: result.id,
                updated_fields: result.updated_fields,
            },
            0,
        )),
        homeboy::core::MergeOutput::Bulk(batch) => {
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
    let exit_code = extension::exec_tool(extension_id, component.as_deref(), &args)?;

    Ok((
        ExtensionOutput::Exec {
            extension_id: extension_id.to_string(),
            output: None,
        },
        exit_code,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use std::fs;

    #[test]
    fn extension_runtime_diagnostics_reports_generic_materialization_guidance() {
        with_isolated_home(|home| {
            let extension_id = "generic-runtime";
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id);
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join(format!("{extension_id}.json")),
                r#"{
  "name": "Generic runtime extension",
  "version": "1.0.0",
  "agent_runtimes": [{
    "id": "generic-runtime/v1",
    "agent_task_executors": [{
      "id": "generic-runtime.default",
      "backend": "generic-runtime"
    }],
    "materialization": {
      "diagnostics": {
        "tools": [{
          "tool": "generic-tool",
          "configured_binary_env": ["HOMEBOY_GENERIC_TOOL_BIN"],
          "install_dir_env": "HOMEBOY_GENERIC_TOOL_INSTALL_DIR",
          "default_install_dir": "/tmp/homeboy/generic-tool",
          "managed_cache_source": "${install_dir}/source",
          "managed_cache_binary": "${managed_cache_source}/bin/generic-tool",
          "effective_binary_rule": "managed cache binary, configured binary, then PATH",
          "diagnostic_script": "generic-tool --version"
        }]
      }
    }
  }]
}"#,
            )
            .expect("extension manifest");
            std::env::set_var("HOMEBOY_GENERIC_TOOL_BIN", "/custom/bin/generic-tool");

            let diagnostics =
                extension_runtime_diagnostics(extension_id, Some("abc1234".to_string()));

            assert_eq!(diagnostics.extension_id, extension_id);
            assert_eq!(diagnostics.source_revision.as_deref(), Some("abc1234"));
            assert!(diagnostics.runtime_manifest_found);
            assert_eq!(diagnostics.runtime_ids, vec!["generic-runtime/v1"]);
            assert_eq!(diagnostics.tools.len(), 1);
            assert_eq!(diagnostics.tools[0].tool, "generic-tool");
            assert_eq!(
                diagnostics.tools[0].configured_binary.as_deref(),
                Some("/custom/bin/generic-tool")
            );
            assert!(diagnostics
                .path_behavior
                .contains("does not special-case individual providers"));
            assert!(diagnostics.freshness.refresh_behavior.contains("reports"));

            std::env::remove_var("HOMEBOY_GENERIC_TOOL_BIN");
        });
    }

    #[test]
    fn installed_extension_diff_status_reports_stale_current_and_unknown() {
        assert_eq!(
            installed_extension_diff_status(true, Some("abc1234"), Some("abc1234")),
            "current"
        );
        assert_eq!(
            installed_extension_diff_status(true, Some("abc1234"), Some("def5678")),
            "stale"
        );
        assert_eq!(
            installed_extension_diff_status(true, Some("abc1234"), None),
            "unknown"
        );
        assert_eq!(
            installed_extension_diff_status(false, Some("abc1234"), Some("abc1234")),
            "unready"
        );
    }

    #[test]
    fn installed_extension_diff_next_command_guides_stale_local_iteration() {
        let mut summary = ExtensionSummary {
            id: "rust".to_string(),
            name: "Rust".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            runtime: "platform".to_string(),
            compatible: true,
            ready: true,
            ready_reason: None,
            ready_detail: None,
            linked: true,
            path: "/tmp/homeboy-extensions/rust".to_string(),
            error: None,
            symlink_target: None,
            source_revision: Some("abc1234".to_string()),
            cli_tool: None,
            cli_display_name: None,
            actions: Vec::new(),
            has_setup: None,
            has_ready_check: None,
        };

        assert_eq!(
            installed_extension_diff_next_command(&summary, "stale"),
            "homeboy extension relink rust /tmp/homeboy-extensions/rust"
        );

        summary.linked = false;
        assert_eq!(
            installed_extension_diff_next_command(&summary, "stale"),
            "homeboy extension update rust"
        );

        assert_eq!(
            installed_extension_diff_next_command(&summary, "unready"),
            "homeboy extension setup rust"
        );
    }
}
