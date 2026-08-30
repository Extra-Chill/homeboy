use clap::{Args, Subcommand};
use homeboy_core::extension;
use serde::{Deserialize, Serialize};

use homeboy::agents::agent_tasks::provider::AgentTaskProviderCatalog;
use homeboy::core::agent_runtime_manifest::{
    discover_agent_runtime_catalog, AgentRuntimeDiagnosticsContract,
};
use homeboy::core::git;
use homeboy::core::project::{self, Project};
use homeboy::core::server::{self, SshClient};
use homeboy::runner::runners::{self, RunnerKind};
use homeboy_core::{
    self, extension_ready_status_with, is_extension_linked, load_extension, run_setup,
    ExtensionReadinessMode, ExtensionSummary, UpdateEntry,
};
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
        /// Run live readiness probes concurrently and refresh cached readiness
        #[arg(long, conflicts_with = "skip_ready_check")]
        live_readiness: bool,
        /// Deprecated compatibility flag; inventory is metadata-only by default
        #[arg(long, hide = true)]
        skip_ready_check: bool,
    },
    /// Compare installed extension revisions with their current checkout HEADs
    DiffInstalled {
        /// Optional extension ID to inspect
        extension_id: Option<String>,
        /// Inspect the extension install visible to a configured runner
        #[arg(long)]
        runner: Option<String>,
    },
    /// Show detailed information about a extension
    Show {
        /// Extension ID
        extension_id: String,
        /// Run the live readiness probe and refresh cached readiness
        #[arg(long, conflicts_with = "skip_ready_check")]
        live_readiness: bool,
        /// Deprecated compatibility flag; inspection is metadata-only by default
        #[arg(long, hide = true)]
        skip_ready_check: bool,
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
    /// Sync local extension source to a runner, refresh it there, then run a command
    DevRun {
        /// Extension ID
        extension_id: String,
        /// Local extension source directory to sync to the runner
        #[arg(long)]
        source: String,
        /// Runner ID
        #[arg(long)]
        runner: String,
        /// Command and arguments to execute on the runner after refresh
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
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
    /// Converge installed extensions without replacing the controller binary
    Converge,
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

pub fn run(args: ExtensionArgs) -> CmdResult<ExtensionOutput> {
    match args.command {
        ExtensionCommand::List {
            project,
            live_readiness,
            skip_ready_check,
        } => list(project, readiness_mode(live_readiness, skip_ready_check)),
        ExtensionCommand::DiffInstalled {
            extension_id,
            runner,
        } => diff_installed(extension_id.as_deref(), runner.as_deref()),
        ExtensionCommand::Show {
            extension_id,
            live_readiness,
            skip_ready_check,
        } => show_extension(
            &extension_id,
            readiness_mode(live_readiness, skip_ready_check),
        ),
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
        ExtensionCommand::DevRun {
            extension_id,
            source,
            runner,
            command,
        } => dev_run_extension(&extension_id, &source, &runner, &command),
        ExtensionCommand::InstallForComponent { source, path } => {
            install_for_component(&source, path.as_deref())
        }
        ExtensionCommand::Update {
            extension_id,
            all,
            force,
        } => update_extension(extension_id.as_deref(), all, force),
        ExtensionCommand::Converge => converge_extensions(),
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
    pub(crate) fn owns_runner_execution(&self) -> bool {
        matches!(self.command, ExtensionCommand::DevRun { .. })
    }

    pub(crate) fn is_runner_resident_read_command(&self) -> bool {
        matches!(self.command, ExtensionCommand::Show { .. })
    }

    pub(crate) fn runner_resident_read_command_label(&self) -> &'static str {
        match self.command {
            ExtensionCommand::Show { .. } => "extension show",
            _ => "extension",
        }
    }

    pub(crate) fn is_update_command(&self) -> bool {
        matches!(
            self.command,
            ExtensionCommand::Update { .. }
                | ExtensionCommand::Converge
                | ExtensionCommand::Refresh { .. }
                | ExtensionCommand::DevRun { .. }
        )
    }

    pub(crate) fn update_command_label(&self) -> &'static str {
        match self.command {
            ExtensionCommand::Refresh { .. } => "extension refresh",
            ExtensionCommand::DevRun { .. } => "extension dev-run",
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
        #[serde(skip_serializing_if = "Option::is_none")]
        runner_id: Option<String>,
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
        source_update: homeboy_core::ExtensionSourceUpdate,
        #[serde(skip_serializing_if = "Option::is_none")]
        old_version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        repaired_source_metadata: Option<homeboy_core::SourceMetadataRepair>,
    },
    #[serde(rename = "extension.update_all")]
    UpdateAll {
        updated: Vec<UpdateEntry>,
        skipped: Vec<String>,
    },
    #[serde(rename = "extension.converge")]
    Converge {
        controller_version: String,
        compatibility: Vec<ExtensionConvergenceCompatibility>,
        updated: Vec<UpdateEntry>,
        skipped: Vec<homeboy_core::UpdateSkippedEntry>,
        revision_evidence: Vec<ExtensionRevisionEvidence>,
        provider_catalog_before: ProviderCatalogEvidence,
        provider_catalog_after: ProviderCatalogEvidence,
        services_restarted: Vec<homeboy_upgrade::upgrade::ServiceRestartEntry>,
        services_pending_restart: Vec<homeboy_upgrade::upgrade::ServiceRestartEntry>,
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
    #[serde(rename = "extension.dev_run")]
    DevRun(homeboy::runner::dev_run::ExtensionDevRunOutput),
    #[serde(rename = "extension.set")]
    SetBatch { batch: homeboy::core::BatchResult },
}

#[derive(Serialize)]
pub struct ExtensionConvergenceCompatibility {
    pub extension_id: String,
    pub core_compatibility: homeboy_core::CoreCompatibilityReport,
}

#[derive(Serialize)]
pub struct ProviderCatalogEvidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub provider_ids: Vec<String>,
    pub diagnostics: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostic_messages: Vec<String>,
}

#[derive(Serialize)]
pub struct ExtensionRevisionEvidence {
    pub extension_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// `changed` is proven only when both revisions are known and differ.
    pub status: &'static str,
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
    pub core_compatibility: homeboy_core::CoreCompatibilityReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_requirements: Option<homeboy_core::RuntimeRequirementsConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_setup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_ready_check: Option<bool>,
    pub readiness: homeboy_core::ExtensionReadinessState,
    pub ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_cache_age_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_probe_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_follow_up_command: Option<String>,
    pub linked: bool,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliDetail>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionDetail>,
    /// Installed transport IDs and schemas, without executable argv.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notification_transports: Vec<homeboy_core::NotificationTransportDescriptor>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<homeboy_core::InputConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<homeboy_core::SettingConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub structured_sidecars: Vec<homeboy_core::StructuredSidecarDeclaration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_cache: Option<homeboy_core::CiCacheSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materialization_source: Option<homeboy_core::ExtensionMaterializationSourceContract>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contract_producers: Vec<homeboy_core::ExtensionContractProducer>,
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
    pub action_type: homeboy_core::ActionType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<homeboy_core::HttpMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Serialize)]
pub struct RequiresDetail {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homeboy: Option<String>,
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

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledExtensionDiff {
    pub extension_id: String,
    pub version: String,
    pub path: String,
    pub manifest_path: String,
    pub linked: bool,
    pub copied: bool,
    pub ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub source_url_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub source_path_available: bool,
    pub has_setup: bool,
    pub setup_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout_head_revision: Option<String>,
    pub status: String,
    pub next_command: String,
}

/// Inventory is static by default; callers must explicitly request live probes.
fn readiness_mode(live_readiness: bool, _skip_ready_check: bool) -> ExtensionReadinessMode {
    if live_readiness {
        ExtensionReadinessMode::Probe
    } else {
        ExtensionReadinessMode::Cached
    }
}

fn list(project: Option<String>, readiness: ExtensionReadinessMode) -> CmdResult<ExtensionOutput> {
    let project_config: Option<Project> = project.as_ref().and_then(|id| project::load(id).ok());
    let summaries = extension::list_summaries_with(project_config.as_ref(), readiness);

    Ok((
        ExtensionOutput::List {
            project_id: project,
            extensions: summaries,
        },
        0,
    ))
}

fn diff_installed(
    extension_id: Option<&str>,
    runner_id: Option<&str>,
) -> CmdResult<ExtensionOutput> {
    if let Some(runner_id) = runner_id {
        return runner_diff_installed(extension_id, runner_id);
    }

    let rows = extension::list_summaries_with(None, ExtensionReadinessMode::Cached)
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
            runner_id: None,
            extensions: rows,
        },
        0,
    ))
}

fn runner_diff_installed(
    extension_id: Option<&str>,
    runner_id: &str,
) -> CmdResult<ExtensionOutput> {
    let runner = runners::load(runner_id)?;
    let homeboy_path = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let mut command = format!("{} extension diff-installed", shell_arg(homeboy_path));
    if let Some(extension_id) = extension_id {
        command.push(' ');
        command.push_str(&shell_arg(extension_id));
    }

    let output = match runner.kind {
        RunnerKind::Local => {
            let output = Command::new(homeboy_path)
                .args(["extension", "diff-installed"])
                .args(extension_id)
                .output()
                .map_err(|err| {
                    homeboy::core::Error::internal_io(
                        err.to_string(),
                        Some("run local runner extension diff-installed".to_string()),
                    )
                })?;
            server::CommandOutput {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: output.status.code().unwrap_or(1),
                timed_out: false,
                observation: Default::default(),
                child_resource: None,
            }
        }
        RunnerKind::Ssh => {
            let server_id = runner.server_id.as_deref().ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "runner",
                    format!("Runner '{runner_id}' is missing server_id"),
                    Some(runner_id.to_string()),
                    None,
                )
            })?;
            let server = server::load(server_id)?;
            let mut client = SshClient::from_server(&server, server_id)?;
            client.env.extend(runner.env);
            client.execute(&command)
        }
    };

    if !output.success {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "runner",
            format!("Runner '{runner_id}' extension health probe failed"),
            Some(runner_id.to_string()),
            Some(vec![runner_diff_diagnostic_tail(
                &output.stderr,
                &output.stdout,
            )]),
        ));
    }

    let rows = parse_runner_diff_installed(&output.stdout)?;
    Ok((
        ExtensionOutput::DiffInstalled {
            extension_id: extension_id.map(str::to_string),
            runner_id: Some(runner_id.to_string()),
            extensions: rows,
        },
        0,
    ))
}

fn parse_runner_diff_installed(stdout: &str) -> homeboy::core::Result<Vec<InstalledExtensionDiff>> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|err| {
        homeboy::core::Error::validation_invalid_argument(
            "runner_output",
            format!("Runner extension health output was not valid JSON: {err}"),
            None,
            None,
        )
    })?;
    let extensions = value
        .pointer("/data/extensions")
        .or_else(|| value.get("extensions"))
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "runner_output",
                "Runner extension health output did not contain data.extensions",
                None,
                None,
            )
        })?;
    serde_json::from_value(extensions.clone()).map_err(|err| {
        homeboy::core::Error::validation_invalid_argument(
            "runner_output",
            format!("Runner extension health extensions payload was invalid: {err}"),
            None,
            None,
        )
    })
}

fn installed_extension_diff(summary: ExtensionSummary) -> InstalledExtensionDiff {
    let checkout_head_revision = git::head_sha_short(Path::new(&summary.path));
    let manifest_path = manifest_path_for_summary(&summary);
    let source_url = read_source_url_metadata(&summary.path);
    let source_path = source_url.as_deref().and_then(local_source_path);
    let has_setup = summary.has_setup.unwrap_or(false);
    let status = installed_extension_diff_status(
        summary.ready,
        summary.source_revision.as_deref(),
        checkout_head_revision.as_deref(),
    );
    let next_command = installed_extension_diff_next_command(&summary, &status);

    InstalledExtensionDiff {
        extension_id: summary.id,
        version: summary.version,
        path: summary.path,
        manifest_path,
        linked: summary.linked,
        copied: !summary.linked,
        ready: summary.ready,
        ready_reason: summary.ready_reason,
        ready_detail: summary.ready_detail,
        source_url_available: source_url.is_some(),
        source_path_available: source_path
            .as_ref()
            .is_some_and(|path| Path::new(path).exists()),
        source_url,
        source_path,
        has_setup,
        setup_status: if summary.ready == Some(true) {
            "ready".to_string()
        } else if has_setup {
            "setup_required".to_string()
        } else {
            "unready_no_setup".to_string()
        },
        installed_source_revision: summary.source_revision,
        checkout_head_revision,
        status,
        next_command,
    }
}

fn manifest_path_for_summary(summary: &ExtensionSummary) -> String {
    if summary.path.is_empty() {
        return String::new();
    }
    Path::new(&summary.path)
        .join(format!("{}.json", summary.id))
        .to_string_lossy()
        .to_string()
}

fn read_source_url_metadata(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    homeboy_core::extension_update_check::read_source_url(Path::new(path))
}

fn local_source_path(source: &str) -> Option<String> {
    if looks_like_remote_source(source) {
        return None;
    }
    Some(
        homeboy::core::expand_tilde_path(source)
            .to_string_lossy()
            .into_owned(),
    )
}

fn looks_like_remote_source(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    lower.contains("://")
        || lower.starts_with("git@")
        || source.contains('@') && source.contains(':')
}

fn runner_diff_diagnostic_tail(stderr: &str, stdout: &str) -> String {
    let output = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

fn installed_extension_diff_status(
    ready: Option<bool>,
    installed_revision: Option<&str>,
    checkout_revision: Option<&str>,
) -> String {
    if ready == Some(false) {
        return "unready".to_string();
    }
    if ready.is_none() {
        return "unknown".to_string();
    }
    match (installed_revision, checkout_revision) {
        (Some(installed), Some(checkout)) if installed == checkout => "current".to_string(),
        (Some(_), Some(_)) => "stale".to_string(),
        (Some(_), None) => "current".to_string(),
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

fn show_extension(
    extension_id: &str,
    readiness: ExtensionReadinessMode,
) -> CmdResult<ExtensionOutput> {
    let extension = load_extension(extension_id)?;
    let ready_status = extension_ready_status_with(&extension, readiness);
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
        homeboy: r.homeboy.clone(),
        extensions: r.extensions.clone(),
        components: r.components.clone(),
    });

    let source_revision = homeboy_core::extension_update_check::read_source_revision(&extension.id);
    let core_compatibility = homeboy_core::evaluate_core_compatibility(
        extension
            .requires
            .as_ref()
            .and_then(|requires| requires.homeboy.as_deref()),
        source_revision.clone(),
    )?;

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
        core_compatibility,
        runtime_requirements: extension.runtime.clone(),
        has_setup,
        has_ready_check,
        readiness: ready_status.state,
        ready: ready_status.ready,
        ready_reason: ready_status.reason,
        ready_detail: ready_status.detail,
        readiness_cache_age_seconds: ready_status.cache_age_seconds,
        readiness_probe_duration_ms: ready_status.probe_duration_ms,
        readiness_timeout_ms: ready_status.timeout_ms,
        readiness_follow_up_command: ready_status.follow_up_command,
        linked,
        path: extension.extension_path.clone().unwrap_or_default(),
        source_revision,
        cli,
        actions,
        notification_transports: extension
            .notification_transports
            .iter()
            .map(|transport| transport.descriptor())
            .collect(),
        inputs: extension.inputs().to_vec(),
        settings: extension.settings.clone(),
        structured_sidecars: homeboy_core::structured_sidecars(&extension),
        ci_cache: extension.ci.as_ref().and_then(|ci| ci.cache.clone()),
        materialization_source: extension.materialization_source.clone(),
        contract_producers: extension.contract_producers.clone(),
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
    use homeboy_core::{ExtensionExecutionMode, ExtensionStepFilter};

    let mode = if no_stream {
        ExtensionExecutionMode::Captured
    } else if stream || crate::commands::utils::tty::is_stdout_tty() {
        ExtensionExecutionMode::Interactive
    } else {
        ExtensionExecutionMode::Captured
    };

    let filter = ExtensionStepFilter { step, skip };

    let result = homeboy_core::run_extension(
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
        let result =
            homeboy_core::replace_with_revision(source, id.as_deref(), revision.as_deref())?;
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

    let result = homeboy_core::install_with_revision(source, id.as_deref(), revision.as_deref())?;
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
    let result = homeboy_core::refresh(source, id, revision)?;
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
    let result = homeboy_core::relink(extension_id, source)?;

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

fn dev_run_extension(
    extension_id: &str,
    source: &str,
    runner: &str,
    command: &[String],
) -> CmdResult<ExtensionOutput> {
    let (output, exit_code) =
        homeboy::runner::dev_run::run_extension_dev_run(extension_id, runner, source, command)?;

    Ok((ExtensionOutput::DevRun(output), exit_code))
}

fn install_for_component(source: &str, path: Option<&str>) -> CmdResult<ExtensionOutput> {
    let component = resolve_install_component(path)?;
    let result = homeboy_core::install_for_component(&component, source)?;

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

/// Extension-only convergence intentionally has no controller-upgrade admission
/// or runtime-promotion lease: it never replaces the controller binary.
fn converge_extensions() -> CmdResult<ExtensionOutput> {
    let extension_ids = extension::available_extension_ids();
    let compatibility = extension_ids
        .iter()
        .map(|id| {
            let manifest = extension::load_extension(id)?;
            let source_revision = homeboy_core::extension_update_check::read_source_revision(id);
            let report = extension::evaluate_core_compatibility(
                manifest
                    .requires
                    .as_ref()
                    .and_then(|requires| requires.homeboy.as_deref()),
                source_revision,
            )?;
            if report.status == "incompatible" {
                return Err(extension::core_incompatible_error("extension", id, report));
            }
            Ok(ExtensionConvergenceCompatibility {
                extension_id: id.clone(),
                core_compatibility: report,
            })
        })
        .collect::<homeboy::core::Result<Vec<_>>>()?;

    let provider_catalog_before = provider_catalog_evidence(AgentTaskProviderCatalog::discover());
    let result = extension::update_all(false);
    let changed_extension_ids = changed_extension_ids(&result.updated);
    let provider_catalog_after = provider_catalog_evidence(AgentTaskProviderCatalog::refresh());
    let (services_restarted, services_pending_restart) =
        homeboy_upgrade::upgrade::restart_extension_services(&changed_extension_ids);
    let revision_evidence = revision_evidence(&result.updated);

    Ok((
        ExtensionOutput::Converge {
            controller_version: extension::installed_homeboy_version(),
            compatibility,
            updated: result.updated,
            skipped: result.skipped_details,
            revision_evidence,
            provider_catalog_before,
            provider_catalog_after,
            services_restarted,
            services_pending_restart,
        },
        0,
    ))
}

fn provider_catalog_evidence(catalog: AgentTaskProviderCatalog) -> ProviderCatalogEvidence {
    let mut provider_ids = catalog
        .providers()
        .iter()
        .map(|provider| provider.id.clone())
        .collect::<Vec<_>>();
    provider_ids.sort();
    let diagnostics = catalog.diagnostics().len();
    let diagnostic_messages = catalog
        .diagnostics()
        .iter()
        .take(8)
        .map(|diagnostic| {
            bounded_diagnostic(&format!("{}: {}", diagnostic.class, diagnostic.message))
        })
        .collect();
    ProviderCatalogEvidence {
        version: catalog.version,
        provider_ids,
        diagnostics,
        diagnostic_messages,
    }
}

fn changed_extension_ids(entries: &[UpdateEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| revision_status(entry) == "changed")
        .map(|entry| entry.extension_id.clone())
        .collect()
}

fn revision_evidence(entries: &[UpdateEntry]) -> Vec<ExtensionRevisionEvidence> {
    entries
        .iter()
        .map(|entry| ExtensionRevisionEvidence {
            extension_id: entry.extension_id.clone(),
            before: entry.source_update.old_source_revision.clone(),
            after: entry.source_update.new_source_revision.clone(),
            status: revision_status(entry),
        })
        .collect()
}

fn revision_status(entry: &UpdateEntry) -> &'static str {
    match (
        entry.source_update.old_source_revision.as_deref(),
        entry.source_update.new_source_revision.as_deref(),
    ) {
        (Some(before), Some(after)) if before != after => "changed",
        (Some(_), Some(_)) => "unchanged",
        _ => "unknown",
    }
}

fn bounded_diagnostic(message: &str) -> String {
    const LIMIT: usize = 512;
    if message.len() <= LIMIT {
        return message.to_string();
    }
    let mut end = LIMIT;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... [truncated]", &message[..end])
}

fn uninstall_extension(extension_id: &str) -> CmdResult<ExtensionOutput> {
    let was_linked = is_extension_linked(extension_id);
    let path = homeboy_core::uninstall(extension_id)?;

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
    let source_revision = source_revision
        .or_else(|| homeboy_core::extension_update_check::read_source_revision(extension_id));
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
    let response = homeboy_core::run_action(
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
    match homeboy_core::merge(extension_id, json, replace_fields)? {
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
    use homeboy_core::{ExtensionSourceUpdate, UpdateEntry};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[test]
    fn convergence_restarts_only_extensions_with_changed_source_revisions() {
        let entries = vec![
            update_entry("unchanged", Some("abc"), Some("abc")),
            update_entry("changed", Some("abc"), Some("def")),
            update_entry("unverifiable", None, None),
        ];

        assert_eq!(changed_extension_ids(&entries), vec!["changed"]);
    }

    #[test]
    fn convergence_keeps_unknown_revisions_out_of_restart_selection() {
        let entries = vec![update_entry("unknown", None, Some("def"))];

        assert!(changed_extension_ids(&entries).is_empty());
        assert_eq!(revision_evidence(&entries)[0].status, "unknown");
    }

    #[test]
    fn provider_diagnostics_are_bounded() {
        let diagnostic = bounded_diagnostic(&"x".repeat(600));

        assert!(diagnostic.len() < 540);
        assert!(diagnostic.ends_with("... [truncated]"));
    }

    #[cfg(unix)]
    #[test]
    fn converge_acceptance_preserves_dirty_sources_reports_evidence_restarts_targeted_service_and_rolls_back(
    ) {
        with_isolated_home(|home| {
            homeboy_upgrade::upgrade::register_controller_upgrade_admission_provider(Box::new(
                BlockedControllerAdmission,
            ));
            let root = home.path();
            let remote = root.join("remote.git");
            let seed = root.join("seed");
            let linked = root.join("linked");
            assert!(git(
                &root,
                &["init", "--bare", "--quiet", remote.to_str().unwrap()]
            ));
            assert!(git(&root, &["init", "--quiet", seed.to_str().unwrap()]));
            assert!(git(&seed, &["checkout", "--quiet", "-b", "main"]));
            write_convergence_manifest(&seed, ">=0.0.0");
            assert!(git(
                &seed,
                &["remote", "add", "origin", remote.to_str().unwrap()]
            ));
            commit_and_push(&seed, "initial");
            assert!(git(
                &root,
                &[
                    "--git-dir",
                    remote.to_str().unwrap(),
                    "symbolic-ref",
                    "HEAD",
                    "refs/heads/main"
                ]
            ));
            assert!(git(
                &root,
                &[
                    "clone",
                    "--quiet",
                    remote.to_str().unwrap(),
                    linked.to_str().unwrap()
                ]
            ));

            let extensions = root.join(".config/homeboy/extensions");
            fs::create_dir_all(&extensions).expect("extensions dir");
            symlink(&linked, extensions.join("fixture")).expect("linked extension");
            let sentinel = root.join("service-restarted");
            let mut config = homeboy::core::defaults::HomeboyConfig::default();
            config
                .resident_services
                .push(homeboy::core::defaults::ResidentServiceConfig {
                    id: "fixture-provider".to_string(),
                    systemd_unit: None,
                    restart_command: Some(format!("touch {}", sentinel.display())),
                    extension_ids: vec!["fixture".to_string()],
                });
            config
                .resident_services
                .push(homeboy::core::defaults::ResidentServiceConfig {
                    id: "controller-only".to_string(),
                    systemd_unit: None,
                    restart_command: Some("false".to_string()),
                    extension_ids: Vec::new(),
                });
            homeboy::core::defaults::save_config(&config).expect("save service config");
            homeboy::core::defaults::reset_config_cache_for_test();

            fs::write(linked.join("user-notes.txt"), "preserve me").expect("dirty source");
            let (dirty, _) = converge_extensions().expect("dirty convergence reports skip");
            let ExtensionOutput::Converge { skipped, .. } = dirty else {
                panic!("converge output")
            };
            assert_eq!(skipped.len(), 1);
            assert!(skipped[0].reason.contains("uncommitted changes"));
            assert!(
                linked.join("user-notes.txt").exists(),
                "dirty source preserved"
            );
            fs::remove_file(linked.join("user-notes.txt")).expect("clean source");

            write_convergence_manifest(&seed, ">=0.0.0");
            fs::write(seed.join("provider-change"), "new provider").expect("provider change");
            commit_and_push(&seed, "compatible refresh");
            let (converged, _) = converge_extensions().expect("compatible convergence");
            let ExtensionOutput::Converge {
                updated,
                revision_evidence,
                provider_catalog_before,
                provider_catalog_after,
                services_restarted,
                ..
            } = converged
            else {
                panic!("converge output")
            };
            assert_eq!(updated.len(), 1);
            assert_eq!(revision_evidence[0].status, "changed");
            assert!(revision_evidence[0].before.is_some());
            assert!(revision_evidence[0].after.is_some());
            assert!(provider_catalog_before.version.is_some());
            assert!(provider_catalog_after.version.is_some());
            assert_eq!(
                services_restarted
                    .iter()
                    .map(|entry| entry.service_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["fixture-provider"]
            );
            assert!(sentinel.exists(), "configured extension service restarted");

            let before_rollback =
                homeboy_core::extension_update_check::read_source_revision("fixture")
                    .expect("current revision");
            write_convergence_manifest(&seed, ">=999.0.0");
            commit_and_push(&seed, "incompatible refresh");
            let (rolled_back, _) =
                converge_extensions().expect("rollback is a reported extension skip");
            let ExtensionOutput::Converge {
                skipped,
                services_restarted,
                ..
            } = rolled_back
            else {
                panic!("converge output")
            };
            assert_eq!(skipped.len(), 1);
            assert!(skipped[0].reason.contains("requires homeboy"));
            assert_eq!(
                homeboy_core::extension_update_check::read_source_revision("fixture").as_deref(),
                Some(before_rollback.as_str())
            );
            assert!(
                services_restarted.is_empty(),
                "failed refresh has no service effects"
            );
        });
    }

    #[cfg(unix)]
    fn write_convergence_manifest(repo: &Path, requirement: &str) {
        fs::write(
            repo.join("fixture.json"),
            format!(
                r#"{{"name":"Fixture","version":"1.0.0","requires":{{"homeboy":"{requirement}"}}}}"#
            ),
        )
        .expect("fixture manifest");
    }

    #[cfg(unix)]
    fn commit_and_push(repo: &Path, message: &str) {
        assert!(git(repo, &["add", "."]));
        assert!(git(
            repo,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=test",
                "commit",
                "--quiet",
                "-m",
                message
            ]
        ));
        assert!(git(repo, &["push", "--quiet", "origin", "HEAD:main"]));
    }

    #[cfg(unix)]
    fn git(path: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .is_ok_and(|status| status.success())
    }

    struct BlockedControllerAdmission;

    impl homeboy_upgrade::upgrade::ControllerUpgradeAdmissionProvider for BlockedControllerAdmission {
        fn controller_upgrade_admission(
            &self,
        ) -> homeboy::core::Result<homeboy_upgrade::upgrade::ControllerUpgradeAdmission> {
            Ok(homeboy_upgrade::upgrade::ControllerUpgradeAdmission {
                schema: "homeboy/controller-upgrade-admission/v1",
                active: 1,
                stale: 0,
                suspect: 0,
                unreconciled: 0,
                reconcilable: 0,
                record_health: serde_json::Value::Null,
                blockers: vec![homeboy_upgrade::upgrade::ControllerUpgradeBlocker {
                    run_id: "blocked-controller".to_string(),
                    owner: "fixture".to_string(),
                    scope: "fixture controller ownership".to_string(),
                    postcondition: "fixture controller admission is allowed".to_string(),
                    liveness: "live",
                    reason: "fixture controller ownership".to_string(),
                    action: "fixture recover".to_string(),
                    recovery_command: "fixture recover".to_string(),
                }],
            })
        }
    }

    #[test]
    fn extension_only_convergence_does_not_require_controller_upgrade_admission() {
        with_isolated_home(|_| {
            let (output, exit_code) = converge_extensions().expect("extension-only convergence");

            assert_eq!(exit_code, 0);
            let ExtensionOutput::Converge {
                compatibility,
                updated,
                skipped,
                services_restarted,
                services_pending_restart,
                ..
            } = output
            else {
                panic!("expected extension convergence output");
            };
            assert!(compatibility.is_empty());
            assert!(updated.is_empty());
            assert!(skipped.is_empty());
            assert!(services_restarted.is_empty());
            assert!(services_pending_restart.is_empty());
        });
    }

    fn update_entry(
        id: &str,
        old_revision: Option<&str>,
        new_revision: Option<&str>,
    ) -> UpdateEntry {
        UpdateEntry {
            extension_id: id.to_string(),
            old_version: "1.0.0".to_string(),
            new_version: "1.0.0".to_string(),
            linked: true,
            source_path: None,
            git_root: None,
            source_update: ExtensionSourceUpdate {
                old_source_revision: old_revision.map(str::to_string),
                new_source_revision: new_revision.map(str::to_string),
                ..Default::default()
            },
            repaired_source_metadata: None,
        }
    }

    /// Installs an extension whose `ready_check` leaves a sentinel behind, so a
    /// test can prove whether the probe ran rather than trusting the reported
    /// reason string.
    #[cfg(unix)]
    fn write_extension_with_observable_ready_check(home: &Path, id: &str, sentinel: &Path) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::to_string(&serde_json::json!({
                "name": "Readiness fixture",
                "version": "1.0.0",
                "executable": {
                    "runtime": {
                        "ready_check": format!("touch {}", sentinel.display())
                    }
                }
            }))
            .expect("manifest json"),
        )
        .expect("extension manifest");
    }

    /// #10517: inventory must not be gated on an operator-authored shell
    /// command. The sentinel is the evidence — a reason string alone could be
    /// produced by a probe that ran anyway.
    #[cfg(unix)]
    #[test]
    fn cached_extension_list_never_spawns_the_ready_check() {
        with_isolated_home(|home| {
            let sentinel = home.path().join("ready-check-ran");
            write_extension_with_observable_ready_check(home.path(), "readiness", &sentinel);

            let (output, exit_code) =
                list(None, ExtensionReadinessMode::Cached).expect("extension list");

            assert_eq!(exit_code, 0);
            assert!(
                !sentinel.exists(),
                "metadata-only inventory must not execute the ready_check"
            );
            let ExtensionOutput::List { extensions, .. } = output else {
                panic!("expected extension list output");
            };
            let entry = extensions
                .iter()
                .find(|summary| summary.id == "readiness")
                .expect("fixture extension");
            assert_eq!(entry.ready_reason.as_deref(), Some("ready_check_skipped"));
            // Metadata is the whole point of the fast path; it must still be here.
            assert_eq!(entry.version, "1.0.0");
            assert_eq!(entry.has_ready_check, Some(true));
        });
    }

    /// The explicit live mode is the control for the cached inventory test.
    #[cfg(unix)]
    #[test]
    fn live_extension_list_probes_readiness() {
        with_isolated_home(|home| {
            let sentinel = home.path().join("ready-check-ran");
            write_extension_with_observable_ready_check(home.path(), "readiness", &sentinel);

            let (output, _) = list(None, ExtensionReadinessMode::Probe).expect("extension list");

            assert!(
                sentinel.exists(),
                "live inventory must report live readiness"
            );
            let ExtensionOutput::List { extensions, .. } = output else {
                panic!("expected extension list output");
            };
            let entry = extensions
                .iter()
                .find(|summary| summary.id == "readiness")
                .expect("fixture extension");
            assert_eq!(entry.ready, Some(true));
            assert_eq!(entry.ready_reason, None);
        });
    }

    #[test]
    fn extension_inventory_defaults_to_cached_readiness() {
        assert_eq!(readiness_mode(false, false), ExtensionReadinessMode::Cached);
        assert_eq!(readiness_mode(false, true), ExtensionReadinessMode::Cached);
        assert_eq!(readiness_mode(true, false), ExtensionReadinessMode::Probe);
    }

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
    fn extension_show_emits_materialization_source_contract() {
        with_isolated_home(|home| {
            let extension_id = "local-iteration";
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id);
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join(format!("{extension_id}.json")),
                r#"{
  "name": "Local iteration extension",
  "version": "1.0.0",
  "materialization_source": {
    "source_kind": "git",
    "revision": "abc1234",
    "runner_ref": "refs/heads/feat/local-iteration",
    "helper_manifest_refs": [{
      "id": "local-runtime",
      "path": "runtime/local-runtime.json",
      "schema": "homeboy/agent-runtime-manifest/v1"
    }]
  }
}"#,
            )
            .expect("extension manifest");

            let (output, exit_code) = show_extension(extension_id, ExtensionReadinessMode::Probe)
                .expect("show extension");
            assert_eq!(exit_code, 0);
            let ExtensionOutput::Show { extension } = output else {
                panic!("expected extension show output");
            };
            let source = extension
                .materialization_source
                .expect("materialization source");

            assert_eq!(
                source.source_kind,
                homeboy_core::ExtensionMaterializationSourceKind::Git
            );
            assert_eq!(source.revision.as_deref(), Some("abc1234"));
            assert_eq!(
                source.runner_ref.as_deref(),
                Some("refs/heads/feat/local-iteration")
            );
            assert_eq!(
                source.helper_manifest_refs[0].path,
                "runtime/local-runtime.json"
            );
        });
    }

    #[test]
    fn extension_inspection_discovers_transport_metadata_without_argv() {
        with_isolated_home(|home| {
            let extension_id = "notification-provider";
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id);
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join(format!("{extension_id}.json")),
                r#"{
  "name": "Notification provider",
  "version": "1.0.0",
  "notification_transports": [{
    "schema": "homeboy/notification-transport/v1",
    "id": "example.completed",
    "command": ["private-notify", "--token", "secret"]
  }]
}"#,
            )
            .expect("extension manifest");

            let (output, exit_code) = show_extension(extension_id, ExtensionReadinessMode::Cached)
                .expect("show extension");
            assert_eq!(exit_code, 0);
            let ExtensionOutput::Show { extension } = output else {
                panic!("expected extension show output");
            };
            assert_eq!(extension.notification_transports.len(), 1);
            assert_eq!(extension.notification_transports[0].id, "example.completed");
            assert_eq!(
                extension.notification_transports[0].schema,
                homeboy_core::NOTIFICATION_TRANSPORT_SCHEMA
            );
            assert!(
                serde_json::to_value(&extension)
                    .expect("serialize extension detail")
                    .pointer("/notification_transports/0/command")
                    .is_none(),
                "extension inspection must not expose transport argv"
            );

            let (output, exit_code) =
                list(None, ExtensionReadinessMode::Cached).expect("list extensions");
            assert_eq!(exit_code, 0);
            let ExtensionOutput::List { extensions, .. } = output else {
                panic!("expected extension list output");
            };
            assert_eq!(
                extensions[0].notification_transports[0].id,
                "example.completed"
            );
        });
    }

    #[test]
    fn extension_show_emits_contract_producers() {
        with_isolated_home(|home| {
            let extension_id = "contract-producer";
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id);
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join(format!("{extension_id}.json")),
                r#"{
  "name": "Contract producer extension",
  "version": "1.0.0",
  "contract_producers": [{
    "id": "handoff-envelope",
    "phase": "handoff",
    "invocation": {
      "script": "contracts/handoff.sh",
      "output_schema": "homeboy/runner-envelope-additions/v1"
    },
    "produces": [{
      "kind": "runner_envelope_addition",
      "schema": "homeboy/runner-envelope-additions/v1"
    }]
  }]
}"#,
            )
            .expect("extension manifest");

            let (output, exit_code) = show_extension(extension_id, ExtensionReadinessMode::Probe)
                .expect("show extension");
            assert_eq!(exit_code, 0);
            let ExtensionOutput::Show { extension } = output else {
                panic!("expected extension show output");
            };

            assert_eq!(extension.contract_producers.len(), 1);
            assert_eq!(extension.contract_producers[0].id, "handoff-envelope");
            assert_eq!(
                extension.contract_producers[0].phase,
                homeboy_core::ExtensionContractProducerPhase::Handoff
            );
            assert_eq!(
                extension.contract_producers[0].produces[0].kind,
                homeboy_core::ExtensionContractProducerOutputKind::RunnerEnvelopeAddition
            );
        });
    }

    #[test]
    fn extension_show_emits_ci_cache_contract() {
        with_isolated_home(|home| {
            let extension_id = "cached-runtime";
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id);
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join(format!("{extension_id}.json")),
                r#"{
  "name": "Cached runtime",
  "version": "1.0.0",
  "ci": {
    "cache": {
      "namespace": "toolchain",
      "key_files": ["toolchain.lock"],
      "paths": [
        {"root": "home", "path": ".tool/registry"},
        {"root": "homeboy-data", "path": "build-targets"},
        {"root": "component", "path": "target", "env": "BUILD_TARGET_DIR"}
      ]
    }
  }
}"#,
            )
            .expect("extension manifest");

            let (output, exit_code) = show_extension(extension_id, ExtensionReadinessMode::Cached)
                .expect("show extension");
            assert_eq!(exit_code, 0);
            let ExtensionOutput::Show { extension } = output else {
                panic!("expected extension show output");
            };
            let cache = extension.ci_cache.expect("CI cache contract");
            assert_eq!(cache.namespace, "toolchain");
            assert_eq!(cache.key_files, vec!["toolchain.lock"]);
            assert_eq!(cache.paths.len(), 3);
            assert_eq!(
                cache.paths[1].root,
                homeboy_core::CiCachePathRoot::HomeboyData
            );
            assert_eq!(cache.paths[1].path, "build-targets");
            assert_eq!(cache.paths[2].env.as_deref(), Some("BUILD_TARGET_DIR"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn extension_show_reports_symlink_sidecar_source_revision_before_copied_marker() {
        with_isolated_home(|home| {
            let extension_id = "nodejs";
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            let source_dir = home
                .path()
                .join(".config/homeboy/extension-sources/nodejs/nodejs");
            fs::create_dir_all(&source_dir).expect("source dir");
            fs::write(
                source_dir.join("nodejs.json"),
                r#"{"name":"Node.js extension","version":"1.0.0"}"#,
            )
            .expect("manifest");
            fs::write(source_dir.join(".source-revision"), "stale-marker\n")
                .expect("copied stale marker");
            fs::create_dir_all(&extensions_dir).expect("extensions dir");
            symlink(&source_dir, extensions_dir.join(extension_id)).expect("extension symlink");
            fs::write(
                extensions_dir.join(".nodejs.source-revision"),
                "fresh-revision\n",
            )
            .expect("sidecar revision");

            let (output, exit_code) = show_extension(extension_id, ExtensionReadinessMode::Probe)
                .expect("show extension");

            assert_eq!(exit_code, 0);
            let ExtensionOutput::Show { extension } = output else {
                panic!("expected extension show output");
            };
            assert_eq!(extension.source_revision.as_deref(), Some("fresh-revision"));
        });
    }

    #[test]
    fn installed_extension_diff_status_reports_stale_current_and_unknown() {
        assert_eq!(
            installed_extension_diff_status(Some(true), Some("abc1234"), Some("abc1234")),
            "current"
        );
        assert_eq!(
            installed_extension_diff_status(Some(true), Some("abc1234"), Some("def5678")),
            "stale"
        );
        assert_eq!(
            installed_extension_diff_status(Some(true), Some("abc1234"), None),
            "current"
        );
        assert_eq!(
            installed_extension_diff_status(Some(false), Some("abc1234"), Some("abc1234")),
            "unready"
        );
        assert_eq!(
            installed_extension_diff_status(None, Some("abc1234"), Some("abc1234")),
            "unknown"
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
            core_compatibility: homeboy_core::CoreCompatibilityReport::undeclared(None),
            readiness: homeboy_core::ExtensionReadinessState::Ready,
            ready: Some(true),
            ready_reason: None,
            ready_detail: None,
            readiness_cache_age_seconds: None,
            readiness_probe_duration_ms: None,
            readiness_timeout_ms: None,
            readiness_follow_up_command: None,
            linked: true,
            path: "/tmp/homeboy-extensions/rust".to_string(),
            manifest_path: None,
            error: None,
            diagnostic: None,
            symlink_target: None,
            source_revision: Some("abc1234".to_string()),
            cli_tool: None,
            cli_display_name: None,
            actions: Vec::new(),
            repair_actions: Vec::new(),
            notification_transports: Vec::new(),
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

    #[test]
    fn installed_extension_diff_reports_copied_install_health_and_update_hint() {
        with_isolated_home(|home| {
            let extension_id = "copied-runtime";
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id);
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join(".source-url"),
                "https://example.com/runtime.git\n",
            )
            .expect("source metadata");

            let summary = ExtensionSummary {
                id: extension_id.to_string(),
                name: "Copied Runtime".to_string(),
                version: "1.2.3".to_string(),
                description: String::new(),
                runtime: "platform".to_string(),
                compatible: true,
                core_compatibility: homeboy_core::CoreCompatibilityReport::undeclared(None),
                readiness: homeboy_core::ExtensionReadinessState::Ready,
                ready: Some(true),
                ready_reason: None,
                ready_detail: None,
                readiness_cache_age_seconds: None,
                readiness_probe_duration_ms: None,
                readiness_timeout_ms: None,
                readiness_follow_up_command: None,
                linked: false,
                path: extension_dir.to_string_lossy().to_string(),
                manifest_path: None,
                error: None,
                diagnostic: None,
                symlink_target: None,
                source_revision: Some("abc1234".to_string()),
                cli_tool: None,
                cli_display_name: None,
                actions: Vec::new(),
                repair_actions: Vec::new(),
                notification_transports: Vec::new(),
                has_setup: Some(true),
                has_ready_check: Some(true),
            };

            let diff = installed_extension_diff(summary);

            assert_eq!(diff.extension_id, extension_id);
            assert_eq!(diff.version, "1.2.3");
            assert!(!diff.linked);
            assert!(diff.copied);
            assert_eq!(
                diff.source_url.as_deref(),
                Some("https://example.com/runtime.git")
            );
            assert!(diff.source_url_available);
            assert!(!diff.source_path_available);
            assert_eq!(diff.setup_status, "ready");
            assert!(diff
                .manifest_path
                .ends_with("copied-runtime/copied-runtime.json"));
            assert_eq!(diff.next_command, "homeboy extension show copied-runtime");

            let stale_summary = ExtensionSummary {
                source_revision: Some("abc1234".to_string()),
                linked: false,
                path: extension_dir.to_string_lossy().to_string(),
                id: extension_id.to_string(),
                name: "Copied Runtime".to_string(),
                version: "1.2.3".to_string(),
                description: String::new(),
                runtime: "platform".to_string(),
                compatible: true,
                core_compatibility: homeboy_core::CoreCompatibilityReport::undeclared(None),
                readiness: homeboy_core::ExtensionReadinessState::Ready,
                ready: Some(true),
                ready_reason: None,
                ready_detail: None,
                readiness_cache_age_seconds: None,
                readiness_probe_duration_ms: None,
                readiness_timeout_ms: None,
                readiness_follow_up_command: None,
                manifest_path: None,
                error: None,
                diagnostic: None,
                symlink_target: None,
                cli_tool: None,
                cli_display_name: None,
                actions: Vec::new(),
                repair_actions: Vec::new(),
                notification_transports: Vec::new(),
                has_setup: Some(true),
                has_ready_check: Some(true),
            };
            assert_eq!(
                installed_extension_diff_next_command(&stale_summary, "stale"),
                "homeboy extension update copied-runtime"
            );
        });
    }

    #[test]
    fn extension_show_reports_the_safe_manifest_failure_for_an_installed_extension() {
        with_isolated_home(|home| {
            let extension_id = "broken";
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id);
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(extension_dir.join("broken.json"), "{secret-value").expect("manifest");

            let error = match show_extension(extension_id, ExtensionReadinessMode::Cached) {
                Err(error) => error,
                Ok(_) => panic!("show must report the manifest failure"),
            };

            assert_eq!(error.code, homeboy::core::ErrorCode::ConfigInvalidValue);
            assert_eq!(error.details["id"], extension_id);
            assert_eq!(error.details["category"], "manifest_json_malformed");
            assert_eq!(
                error.details["diagnostic"],
                "The extension manifest contains malformed JSON."
            );
            assert!(!error.details.to_string().contains("secret-value"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn broken_extension_list_and_show_emit_the_same_typed_repairs() {
        with_isolated_home(|home| {
            let extension_id = "swift";
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            fs::create_dir_all(&extensions_dir).expect("extensions dir");
            symlink(
                home.path().join("removed-swift-extension"),
                extensions_dir.join(extension_id),
            )
            .expect("broken extension link");

            let (output, _) =
                list(None, ExtensionReadinessMode::Cached).expect("list broken extension");
            let ExtensionOutput::List { extensions, .. } = output else {
                panic!("expected extension list output");
            };
            let list_actions = &extensions[0].repair_actions;

            let show_error = match show_extension(extension_id, ExtensionReadinessMode::Cached) {
                Err(error) => error,
                Ok(_) => panic!("show should report the broken link"),
            };
            let show_actions: Vec<homeboy::core::error::ExecutableAction> = serde_json::from_value(
                show_error.details[homeboy::core::error::ACTIONS_DETAILS_KEY].clone(),
            )
            .expect("show repair actions");

            assert_eq!(list_actions, &show_actions);
            assert_eq!(list_actions[0].id, "extension.relink");
            assert_eq!(
                list_actions[0].args,
                ["extension", "relink", "swift", "<path>"]
            );
            assert_eq!(list_actions[1].id, "extension.uninstall");
            assert_eq!(list_actions[1].args, ["extension", "uninstall", "swift"]);
        });
    }

    #[test]
    fn runner_diff_installed_parses_runner_json_payload() {
        let stdout = r#"{
  "success": true,
  "data": {
    "command": "extension.diff_installed",
    "extensions": [{
      "extension_id": "generic-runtime",
      "version": "1.0.0",
      "path": "/runner/extensions/generic-runtime",
      "manifest_path": "/runner/extensions/generic-runtime/generic-runtime.json",
      "linked": false,
      "copied": true,
      "ready": false,
      "ready_reason": "ready_check_failed",
      "ready_detail": "missing dependency",
      "source_url": "https://example.com/generic-runtime.git",
      "source_url_available": true,
      "source_path_available": false,
      "has_setup": true,
      "setup_status": "setup_required",
      "installed_source_revision": "abc1234",
      "checkout_head_revision": "def5678",
      "status": "unready",
      "next_command": "homeboy extension setup generic-runtime"
    }]
  }
}"#;

        let rows = parse_runner_diff_installed(stdout).expect("runner diff payload");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].extension_id, "generic-runtime");
        assert!(rows[0].copied);
        assert_eq!(rows[0].setup_status, "setup_required");
        assert_eq!(rows[0].ready_detail.as_deref(), Some("missing dependency"));
        assert_eq!(
            rows[0].next_command,
            "homeboy extension setup generic-runtime"
        );
    }
}
