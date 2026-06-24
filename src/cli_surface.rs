use clap::{Command, CommandFactory, Parser, Subcommand};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::command_contract::registered_command;
use crate::commands::{
    agent_task, api, audit, audit_baseline, auth, bench, build, changelog, changes, ci, cleanup,
    component, config, daemon, db, deploy, deps, doctor, extension, file, fleet, fuzz, git, http,
    issues, lint, logs, manifest, observe, project, refactor, refs, release, report, review, rig,
    runner, runs, runtime, self_cmd, server, ssh, stack, status, test, trace, triage, tunnel, undo,
    upgrade, version, worktree,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_COMMAND_SURFACE_DEPTH: usize = 8;

#[derive(Parser)]
#[command(name = "homeboy")]
#[command(version = VERSION)]
#[command(about = "Headless automation for agentic software engineering workflows")]
pub struct Cli {
    /// Write structured JSON output to a file path (in addition to stdout).
    /// Bare format names like `json` are rejected; use `./output.json`.
    #[arg(long, global = true, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Suppress resource policy warnings for intentionally hot commands.
    #[arg(long, global = true)]
    pub force_hot: bool,

    /// Permit --force-hot portable Lab commands to stay local even when a default Lab runner exists.
    /// This flag does not disable automatic Lab offload unless --force-hot is also set.
    #[arg(long, global = true)]
    pub allow_local_hot: bool,

    /// Require Lab routing and fail instead of executing locally.
    #[arg(long, visible_alias = "no-local-execution", global = true)]
    pub lab_only: bool,

    /// Return after a runner daemon accepts the job instead of waiting for remote completion.
    #[arg(long, global = true)]
    pub detach_after_handoff: bool,

    /// Directory where persisted run artifacts are copied.
    /// Overrides HOMEBOY_ARTIFACT_ROOT and global config /artifact_root.
    #[arg(long, global = true, value_name = "DIR")]
    pub artifact_root: Option<PathBuf>,

    /// Route commands with portable Lab offload support to a connected runner.
    #[arg(long, global = true, value_name = "RUNNER_ID")]
    pub runner: Option<String>,

    /// Permit a selected Lab runner to fall back to local execution after offload preflight fails.
    #[arg(long, global = true)]
    pub allow_local_fallback: bool,

    /// Permit Lab git workspace materialization to overwrite a dirty runner-side checkout.
    #[arg(long, global = true)]
    pub allow_dirty_lab_workspace: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run generic agent task plans
    #[command(name = "agent-task")]
    AgentTask(agent_task::AgentTaskArgs),
    /// Manage project configuration
    Project(project::ProjectArgs),
    /// SSH into a project server or configured server
    Ssh(ssh::SshArgs),
    /// Manage SSH server configurations
    Server(server::ServerArgs),
    /// Run tests for a component
    Test(test::TestArgs),
    /// Run performance benchmarks for a component
    Bench(bench::BenchArgs),
    /// Run generic fuzz workloads for a component
    Fuzz(fuzz::FuzzArgs),
    /// Capture black-box behavioral traces for a component
    #[command(
        after_help = "Command-shaped trace modes:\n  homeboy trace list --profiles\n  homeboy trace <component> list\n  homeboy trace compare before.json after.json\n  homeboy trace compare <component> <scenario> --baseline-target <target> --candidate <target>\n  homeboy trace matrix <component> <scenario> --axis name=value1,value2\n  homeboy trace compare-variant --rig <rig-id> --scenario <scenario>\n  homeboy trace compare-bundle --component <component> --scenario <scenario>\n  homeboy trace overlay-locks --stale"
    )]
    Trace(trace::TraceArgs),
    /// Passively observe a running system and persist timeline evidence
    Observe(observe::ObserveArgs),
    /// Lint a component
    Lint(lint::LintArgs),
    /// Database operations
    Db(db::DbArgs),
    /// Manage component dependencies
    Deps(deps::DepsArgs),
    /// Inspect CI reproduction profiles and discovered CI surfaces
    Ci(ci::CiArgs),
    /// Read-only local diagnostics for Homeboy-adjacent work
    Doctor(doctor::DoctorArgs),
    /// Remote file operations
    File(file::FileArgs),
    /// Manage fleets (groups of projects)
    Fleet(fleet::FleetArgs),
    /// Remote log viewing
    Logs(logs::LogsArgs),
    /// Attention reports and watch utilities for components, projects, fleets, and rigs
    Triage(triage::TriageArgs),
    /// Deploy components to remote server
    Deploy(deploy::DeployArgs),
    /// Manage standalone component configurations
    Component(component::ComponentArgs),
    /// Manage global Homeboy configuration
    Config(config::ConfigArgs),
    /// Run the local-only HTTP API daemon
    Daemon(daemon::DaemonArgs),
    /// Execute CLI-compatible extensions
    Extension(extension::ExtensionArgs),
    /// Actionable component status overview
    Status(status::StatusArgs),
    /// Display CLI documentation
    Docs(crate::commands::docs::DocsArgs),
    /// Print the recursive command safety, docs, and output manifest
    Manifest(manifest::ManifestArgs),
    /// Changelog operations
    Changelog(changelog::ChangelogArgs),
    /// Remove declared reconstructable artifacts from managed worktrees
    Cleanup(cleanup::CleanupArgs),
    /// Git operations for components
    Git(git::GitArgs),
    /// Reconcile findings against an issue tracker
    Issues(issues::IssuesArgs),
    /// Version management for components
    Version(version::VersionArgs),
    /// Run a local build quality gate for a component
    Build(build::BuildArgs),
    /// Show changes since last version tag
    Changes(changes::ChangesArgs),
    /// Plan release workflows
    Release(release::ReleaseArgs),
    /// Render reports from Homeboy structured output artifacts
    Report(report::ReportArgs),
    /// Run scoped audit + lint + test umbrella against PR-style changes
    Review(review::ReviewArgs),
    /// Audit code conventions and detect architectural drift
    Audit(audit::AuditArgs),
    /// Refresh and inspect generated audit baseline data
    #[command(name = "audit-baseline")]
    AuditBaseline(audit_baseline::AuditBaselineArgs),
    /// Structural refactoring (rename terms across codebase)
    Refactor(refactor::RefactorArgs),
    /// Read-only reference discovery for a symbol or term
    Refs(refs::RefsArgs),
    /// Manage local dev rigs (reproducible multi-component environments)
    Rig(rig::RigArgs),
    /// Manage local and SSH execution runners
    Runner(runner::RunnerArgs),
    /// Inspect core-owned runtime helper assets
    Runtime(runtime::RuntimeArgs),
    /// Manage component-backed task worktrees
    Worktree(worktree::WorktreeArgs),
    /// Manage private service tunnel declarations
    Tunnel(tunnel::TunnelArgs),
    /// Inspect persisted observation runs and artifacts
    Runs(runs::RunsArgs),
    /// Inspect the active Homeboy binary and install signals
    #[command(name = "self")]
    SelfCmd(self_cmd::SelfArgs),
    /// Manage stacks (combined-fixes branches built from base + cherry-picked PRs)
    Stack(stack::StackArgs),
    /// Undo the last write operation (audit fix, refactor, etc.)
    Undo(undo::UndoArgs),
    /// Authenticate with a project's API
    Auth(auth::AuthArgs),
    /// Make API requests to a project
    Api(api::ApiArgs),
    /// Make generic HTTP requests
    Http(http::HttpArgs),
    /// Upgrade Homeboy to the latest version
    Upgrade(upgrade::UpgradeArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSurface {
    pub commands: Vec<CommandSurfaceEntry>,
}

impl CommandSurface {
    pub fn contains_path(&self, path: &[&str]) -> bool {
        let Some((first, rest)) = path.split_first() else {
            return false;
        };

        let Some(entry) = self
            .commands
            .iter()
            .find(|entry| !entry.hidden && entry.matches(first))
        else {
            return false;
        };

        entry.contains_rest(rest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSurfaceEntry {
    pub name: String,
    pub visible_aliases: Vec<String>,
    pub hidden: bool,
    pub subcommands: Vec<CommandSurfaceEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSafetyManifest {
    pub commands: Vec<CommandSafetyEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSurfaceDoctorReport {
    pub agrees: bool,
    pub source_registry_commands: Vec<String>,
    pub docs_index_commands: Vec<String>,
    pub help_commands: Vec<String>,
    pub runtime_extension_docs: Vec<String>,
    pub missing_from_docs_index: Vec<String>,
    pub stale_docs_index: Vec<String>,
    pub missing_from_help: Vec<String>,
    pub missing_from_source_registry: Vec<String>,
    pub drift_notes: Vec<String>,
}

impl CommandSafetyManifest {
    pub fn find_path(&self, path: &[&str]) -> Option<&CommandSafetyEntry> {
        let (first, rest) = path.split_first()?;

        self.commands
            .iter()
            .find(|entry| entry.name == *first)
            .and_then(|entry| entry.find_rest(rest))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSafetyEntry {
    pub name: String,
    pub aliases: Vec<String>,
    pub hidden: bool,
    pub path: Vec<String>,
    pub mutates: bool,
    pub operator: bool,
    pub dry_run: CommandDryRunMetadata,
    pub output: CommandOutputMetadata,
    pub lab: CommandLabMetadata,
    pub docs: CommandDocsMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<ExtensionCommandManifest>,
    pub dangerous_flags: Vec<String>,
    pub subcommands: Vec<CommandSafetyEntry>,
}

impl CommandSafetyEntry {
    fn find_rest(&self, path: &[&str]) -> Option<&CommandSafetyEntry> {
        let Some((first, rest)) = path.split_first() else {
            return Some(self);
        };

        self.subcommands
            .iter()
            .find(|entry| entry.name == *first)
            .and_then(|entry| entry.find_rest(rest))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandDryRunMetadata {
    pub supported: bool,
    pub flag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandOutputMetadata {
    pub structured: bool,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandLabMetadata {
    pub supported: bool,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandDocsMetadata {
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtensionCommandManifest {
    pub extension_id: String,
    pub extension_name: String,
    pub extension_version: String,
    pub tool_name: String,
    pub display_name: String,
    pub args_contract: ExtensionCommandArgsContract,
    pub health: ExtensionCommandHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtensionCommandArgsContract {
    pub project_id: ExtensionCommandArgContract,
    pub args: ExtensionCommandArgContract,
    pub trailing_var_arg: bool,
    pub allow_hyphen_values: bool,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtensionCommandArgContract {
    pub name: String,
    pub help: String,
    pub required: bool,
    pub multiple: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtensionCommandHealth {
    pub status: String,
    pub ready: bool,
    pub compatible: bool,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CommandSurfaceEntry {
    fn matches(&self, name: &str) -> bool {
        self.name == name || self.visible_aliases.iter().any(|alias| alias == name)
    }

    fn contains_rest(&self, path: &[&str]) -> bool {
        let Some((first, rest)) = path.split_first() else {
            return true;
        };

        self.subcommands
            .iter()
            .find(|entry| !entry.hidden && entry.matches(first))
            .is_some_and(|entry| entry.contains_rest(rest))
    }
}

impl Commands {
    pub fn top_level_name(&self) -> &'static str {
        match self {
            Commands::AgentTask(_) => "agent-task",
            Commands::Project(_) => "project",
            Commands::Ssh(_) => "ssh",
            Commands::Server(_) => "server",
            Commands::Test(_) => "test",
            Commands::Bench(_) => "bench",
            Commands::Fuzz(_) => "fuzz",
            Commands::Trace(_) => "trace",
            Commands::Observe(_) => "observe",
            Commands::Lint(_) => "lint",
            Commands::Db(_) => "db",
            Commands::Deps(_) => "deps",
            Commands::Ci(_) => "ci",
            Commands::Doctor(_) => "doctor",
            Commands::File(_) => "file",
            Commands::Fleet(_) => "fleet",
            Commands::Logs(_) => "logs",
            Commands::Triage(_) => "triage",
            Commands::Deploy(_) => "deploy",
            Commands::Component(_) => "component",
            Commands::Config(_) => "config",
            Commands::Daemon(_) => "daemon",
            Commands::Extension(_) => "extension",
            Commands::Status(_) => "status",
            Commands::Docs(_) => "docs",
            Commands::Manifest(_) => "manifest",
            Commands::Changelog(_) => "changelog",
            Commands::Cleanup(_) => "cleanup",
            Commands::Git(_) => "git",
            Commands::Issues(_) => "issues",
            Commands::Version(_) => "version",
            Commands::Build(_) => "build",
            Commands::Changes(_) => "changes",
            Commands::Release(_) => "release",
            Commands::Report(_) => "report",
            Commands::Review(_) => "review",
            Commands::Audit(_) => "audit",
            Commands::AuditBaseline(_) => "audit-baseline",
            Commands::Refactor(_) => "refactor",
            Commands::Refs(_) => "refs",
            Commands::Rig(_) => "rig",
            Commands::Runner(_) => "runner",
            Commands::Runtime(_) => "runtime",
            Commands::Worktree(_) => "worktree",
            Commands::Tunnel(_) => "tunnel",
            Commands::Runs(_) => "runs",
            Commands::SelfCmd(_) => "self",
            Commands::Stack(_) => "stack",
            Commands::Undo(_) => "undo",
            Commands::Auth(_) => "auth",
            Commands::Api(_) => "api",
            Commands::Http(_) => "http",
            Commands::Upgrade(_) => "upgrade",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicCommandDescriptor {
    pub name: String,
    pub about: String,
    pub docs_path: Option<String>,
    pub extension: Option<ExtensionCommandManifest>,
    pub safety: Option<DynamicCommandSafety>,
}

impl DynamicCommandDescriptor {
    pub fn extension_command(name: String, about: String) -> Self {
        Self {
            docs_path: Some(format!("docs/commands/{name}.md")),
            name,
            about,
            extension: None,
            safety: None,
        }
    }

    pub fn installed_extension_command(
        name: String,
        about: String,
        docs_path: Option<String>,
        extension: ExtensionCommandManifest,
    ) -> Self {
        Self {
            name,
            about,
            docs_path,
            extension: Some(extension),
            safety: Some(DynamicCommandSafety::extension_cli_passthrough()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicCommandSafety {
    pub mutates: bool,
    pub operator: bool,
    pub output_notes: &'static str,
    pub lab_notes: &'static str,
    pub dangerous_flags: Vec<&'static str>,
}

impl DynamicCommandSafety {
    fn extension_cli_passthrough() -> Self {
        Self {
            mutates: true,
            operator: true,
            output_notes: "extension-provided CLI passthrough; forwarded arguments may mutate the target system",
            lab_notes: "not declared as Lab-routable in the safety manifest",
            dangerous_flags: vec!["passthrough args"],
        }
    }
}

pub fn current_command_surface() -> CommandSurface {
    command_surface_from(Cli::command())
}

pub fn command_surface_from(command: Command) -> CommandSurface {
    command_surface_from_with_depth(command, DEFAULT_COMMAND_SURFACE_DEPTH)
}

pub fn command_surface_from_with_depth(command: Command, depth: usize) -> CommandSurface {
    CommandSurface {
        commands: visible_subcommands(&command, depth),
    }
}

pub fn current_command_safety_manifest() -> CommandSafetyManifest {
    command_safety_manifest_from(current_command_surface())
}

pub fn current_command_surface_doctor_report() -> CommandSurfaceDoctorReport {
    let surface = current_command_surface();
    let manifest = command_safety_manifest_from(surface.clone());
    let source_registry_commands = manifest
        .commands
        .iter()
        .filter(|entry| visible_manifest_entry_with_docs_path(entry))
        .map(|entry| entry.name.clone())
        .collect();
    let help_commands = surface
        .commands
        .iter()
        .filter(|entry| !entry.hidden)
        .map(|entry| entry.name.clone())
        .collect();
    let docs_index_commands =
        documented_command_index_entries(include_str!("../docs/commands/commands-index.md"));

    command_surface_doctor_report(
        source_registry_commands,
        docs_index_commands,
        help_commands,
        runtime_extension_doc_commands(),
    )
}

pub fn command_surface_doctor_report(
    source_registry_commands: BTreeSet<String>,
    docs_index_commands: BTreeSet<String>,
    help_commands: BTreeSet<String>,
    runtime_extension_docs: BTreeSet<String>,
) -> CommandSurfaceDoctorReport {
    let documented_core_commands: BTreeSet<String> = docs_index_commands
        .difference(&runtime_extension_docs)
        .cloned()
        .collect();

    let missing_from_docs_index =
        sorted_difference(&source_registry_commands, &docs_index_commands);
    let stale_docs_index = sorted_difference(&documented_core_commands, &source_registry_commands);
    let missing_from_help = sorted_difference(&source_registry_commands, &help_commands);
    let missing_from_source_registry = sorted_difference(&help_commands, &source_registry_commands);

    let mut drift_notes = Vec::new();
    push_drift_note(
        &mut drift_notes,
        &missing_from_docs_index,
        "source registry commands missing from docs/commands/commands-index.md",
    );
    push_drift_note(
        &mut drift_notes,
        &stale_docs_index,
        "docs/commands/commands-index.md lists stale commands",
    );
    push_drift_note(
        &mut drift_notes,
        &missing_from_help,
        "source registry commands missing from help-facing command surface",
    );
    push_drift_note(
        &mut drift_notes,
        &missing_from_source_registry,
        "help-facing commands missing from source registry",
    );

    CommandSurfaceDoctorReport {
        agrees: drift_notes.is_empty(),
        source_registry_commands: source_registry_commands.into_iter().collect(),
        docs_index_commands: docs_index_commands.into_iter().collect(),
        help_commands: help_commands.into_iter().collect(),
        runtime_extension_docs: runtime_extension_docs.into_iter().collect(),
        missing_from_docs_index,
        stale_docs_index,
        missing_from_help,
        missing_from_source_registry,
        drift_notes,
    }
}

fn visible_manifest_entry_with_docs_path(entry: &CommandSafetyEntry) -> bool {
    !entry.hidden && entry.docs.path.is_some()
}

fn documented_command_index_entries(index: &str) -> BTreeSet<String> {
    let command_section = index.split("Related:").next().unwrap_or(index);

    command_section
        .lines()
        .filter_map(|line| line.strip_prefix("- ["))
        .filter_map(|rest| rest.split(']').next())
        .map(str::to_string)
        .collect()
}

fn runtime_extension_doc_commands() -> BTreeSet<String> {
    BTreeSet::from(["cargo".to_string(), "wp".to_string()])
}

fn sorted_difference(left: &BTreeSet<String>, right: &BTreeSet<String>) -> Vec<String> {
    left.difference(right).cloned().collect()
}

fn push_drift_note(notes: &mut Vec<String>, commands: &[String], label: &str) {
    if !commands.is_empty() {
        notes.push(format!("{label}: {}", commands.join(", ")));
    }
}

pub fn command_safety_manifest_from(surface: CommandSurface) -> CommandSafetyManifest {
    command_safety_manifest_from_dynamic(surface, &[])
}

pub fn command_safety_manifest_from_dynamic(
    surface: CommandSurface,
    dynamic_commands: &[DynamicCommandDescriptor],
) -> CommandSafetyManifest {
    CommandSafetyManifest {
        commands: surface
            .commands
            .iter()
            .map(|entry| command_safety_entry(entry, &[], dynamic_commands))
            .collect(),
    }
}

fn command_safety_entry(
    entry: &CommandSurfaceEntry,
    parent_path: &[String],
    dynamic_commands: &[DynamicCommandDescriptor],
) -> CommandSafetyEntry {
    let mut path = parent_path.to_vec();
    path.push(entry.name.clone());
    let mut safety = command_safety_metadata(&path);
    let dynamic_command = dynamic_command_for_path(&path, dynamic_commands);

    if let Some(dynamic_safety) = dynamic_command.and_then(|command| command.safety.as_ref()) {
        safety.mutates = dynamic_safety.mutates;
        safety.operator = dynamic_safety.operator;
        safety.output_notes = dynamic_safety.output_notes;
        safety.lab_notes = dynamic_safety.lab_notes;
        safety.dangerous_flags = dynamic_safety.dangerous_flags.clone();
    }

    CommandSafetyEntry {
        name: entry.name.clone(),
        aliases: entry.visible_aliases.clone(),
        hidden: entry.hidden,
        path: path.clone(),
        mutates: safety.mutates,
        operator: safety.operator,
        dry_run: CommandDryRunMetadata {
            supported: safety.dry_run_flag.is_some(),
            flag: safety.dry_run_flag.map(str::to_string),
        },
        output: CommandOutputMetadata {
            structured: safety.structured_output,
            notes: safety.output_notes.to_string(),
        },
        lab: CommandLabMetadata {
            supported: safety.lab_supported,
            notes: safety.lab_notes.to_string(),
        },
        docs: CommandDocsMetadata {
            path: docs_path(&path, dynamic_commands),
        },
        extension: dynamic_command.and_then(|command| command.extension.clone()),
        dangerous_flags: safety
            .dangerous_flags
            .into_iter()
            .map(str::to_string)
            .collect(),
        subcommands: entry
            .subcommands
            .iter()
            .map(|subcommand| command_safety_entry(subcommand, &path, dynamic_commands))
            .collect(),
    }
}

struct CommandSafetyMetadata {
    mutates: bool,
    operator: bool,
    dry_run_flag: Option<&'static str>,
    structured_output: bool,
    output_notes: &'static str,
    lab_supported: bool,
    lab_notes: &'static str,
    dangerous_flags: Vec<&'static str>,
}

impl Default for CommandSafetyMetadata {
    fn default() -> Self {
        Self {
            mutates: false,
            operator: false,
            dry_run_flag: None,
            structured_output: true,
            output_notes: "standard CLI output contract",
            lab_supported: false,
            lab_notes: "not declared as Lab-routable in the safety manifest",
            dangerous_flags: Vec::new(),
        }
    }
}

fn command_safety_metadata(path: &[String]) -> CommandSafetyMetadata {
    let mut metadata = CommandSafetyMetadata::default();

    if let Some(top_level) = path.first().and_then(|name| registered_command(name)) {
        metadata.structured_output =
            top_level.json_family != crate::command_contract::CommandJsonFamily::RawOnly;
        metadata.output_notes = top_level.output_notes;
        metadata.lab_supported = top_level.lab_supported;
        metadata.lab_notes = top_level.lab_notes;
    }

    let path = path.iter().map(String::as_str).collect::<Vec<_>>();
    match path.as_slice() {
        ["manifest"] => {
            metadata.output_notes =
                "recursive command safety, docs, output, and Lab metadata in the standard JSON envelope";
        }
        ["docs", "map"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "default JSON output is non-mutating; pass --write to write markdown docs to disk";
            metadata.dangerous_flags = vec!["--write"];
        }
        ["deploy"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.dangerous_flags = vec!["--head", "--force"];
        }
        ["release"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "release execution mutates git tags/releases and may deploy; use --dry-run to plan and --apply for risky modes";
            metadata.dangerous_flags = vec![
                "--apply",
                "--deploy",
                "--recover",
                "--retag",
                "--head",
                "--skip-checks",
                "--force-lower-bump",
            ];
        }
        ["upgrade"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "upgrades the active Homeboy binary, extensions, runners, and services unless --check or skip flags are used";
            metadata.dangerous_flags = vec!["--force", "--upgrade-runner"];
        }
        ["trace"] => {
            metadata.mutates = true;
            metadata.output_notes = "runs trace workflows and records observation artifacts unless using read-only subcommands";
        }
        ["lint"] => {
            metadata.output_notes =
                "runs lint workflows; pass --fix to apply auto-fixable findings in place";
            metadata.dangerous_flags = vec!["--fix", "--force"];
        }
        ["deps", "install"] | ["deps", "update"] | ["deps", "stack", "apply"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates dependency manifests, lockfiles, or installed dependency trees";
        }
        ["ci", "autofix"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "commits and pushes prepared CI autofix changes";
        }
        ["cleanup"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "cleanup subcommands report plans by default and require --apply for removals";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["cleanup", "artifacts"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "default output is a non-mutating cleanup plan; pass --apply to remove artifacts";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["self", "cleanup-runtime-tmp"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "default output is a non-mutating cleanup plan; pass --apply to delete runtime temp entries";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["config", "set"] | ["config", "remove"] | ["config", "reset"] => {
            metadata.mutates = true;
        }
        ["project", "create"]
        | ["project", "set"]
        | ["project", "remove"]
        | ["project", "rename"]
        | ["project", "delete"]
        | ["project", "init"]
        | ["project", "components", "set"]
        | ["project", "components", "attach-path"]
        | ["project", "components", "remove"]
        | ["project", "components", "clear"]
        | ["project", "pin", "add"]
        | ["project", "pin", "remove"]
        | ["project", "pin", "rename"]
        | ["project", "pin", "update"] => {
            metadata.mutates = true;
        }
        ["component", "create"]
        | ["component", "set"]
        | ["component", "delete"]
        | ["component", "rename"]
        | ["component", "setup"] => {
            metadata.mutates = true;
        }
        ["component", "reconcile"] | ["component", "artifacts"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "default output is non-mutating; pass --apply to repair or remove artifacts";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["server", "create"]
        | ["server", "set"]
        | ["server", "delete"]
        | ["server", "connect"]
        | ["server", "disconnect"]
        | ["server", "key", "generate"]
        | ["server", "key", "import"]
        | ["server", "key", "use"]
        | ["server", "key", "unset"] => {
            metadata.mutates = true;
            metadata.operator = true;
        }
        ["extension", "setup"]
        | ["extension", "refresh"]
        | ["extension", "relink"]
        | ["extension", "install-for-component"]
        | ["extension", "set"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates installed extension files or extension manifest metadata";
        }
        ["extension", "install"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates installed extension files or extension manifest metadata";
            metadata.dangerous_flags = vec!["--replace"];
        }
        ["extension", "update"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates installed extension files or extension manifest metadata";
            metadata.dangerous_flags = vec!["--force"];
        }
        ["extension", "uninstall"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates installed extension files or extension manifest metadata";
            metadata.dangerous_flags = vec!["uninstall"];
        }
        ["runs", "reconcile"] => {
            metadata.mutates = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "marks orphaned running records stale unless --dry-run is passed";
        }
        ["runs", "import"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "imports observation bundle or GitHub Actions artifacts into the local run store";
        }
        ["runs", "loop-sync"] => {
            metadata.mutates = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "syncs copied loop archives into observation runs/artifacts unless --dry-run is passed";
        }
        ["runs", "artifact", "cleanup-downloads"] | ["runs", "artifact", "cleanup-persisted"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "default output is a non-mutating cleanup plan; pass --apply to delete artifacts";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["agent-task", "promote"] => {
            metadata.mutates = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "applies a selected patch artifact into a managed worktree unless --dry-run is passed";
        }
        ["agent-task", "active"] => {
            metadata.mutates = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "reads active runs by default; --reconcile cancels stale active records unless --dry-run is passed";
        }
        ["agent-task", "controller", "init"]
        | ["agent-task", "controller", "from-spec"]
        | ["agent-task", "controller", "run-from-spec"]
        | ["agent-task", "controller", "materialize"]
        | ["agent-task", "controller", "events"]
        | ["agent-task", "controller", "apply-event"]
        | ["agent-task", "controller", "run-next"]
        | ["agent-task", "controller", "run"]
        | ["agent-task", "controller", "resume"]
        | ["agent-task", "controller", "mark-human-ready"] => {
            metadata.mutates = true;
            metadata.output_notes = "mutates durable agent-task loop controller state";
        }
        ["agent-task", "auth", "remove"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "removes one agent-task provider secret source mapping";
        }
        ["agent-task", "prompts", "remove"] => {
            metadata.mutates = true;
            metadata.output_notes = "removes one stored agent-task prompt";
        }
        ["fuzz", "replay"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "replays a persisted fuzz case against local code and may write run artifacts";
        }
        ["db", "delete-row"] | ["db", "drop-table"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "default output is a non-mutating plan; pass --apply to mutate";
        }
        ["file", "write"] | ["file", "delete"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "default output is a non-mutating plan; pass --apply to mutate";
        }
        ["file", "copy"]
        | ["file", "edit"]
        | ["file", "mkdir"]
        | ["file", "rename"]
        | ["file", "sync"] => {
            metadata.mutates = true;
        }
        ["triage"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dangerous_flags = vec!["--auto-merge"];
        }
        ["fleet", "exec"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--check");
            metadata.output_notes = "default output is blocked for remote execution; pass --check to plan or --apply to execute";
            metadata.dangerous_flags = vec!["--apply"];
            metadata.lab_notes = "local-only: depends on local fleet/project/server configuration before SSH fan-out";
        }
        ["fleet", "create"]
        | ["fleet", "set"]
        | ["fleet", "delete"]
        | ["fleet", "add"]
        | ["fleet", "remove"] => {
            metadata.mutates = true;
            metadata.output_notes = "mutates local fleet configuration";
        }
        ["api", "post"] | ["api", "put"] | ["api", "patch"] | ["api", "delete"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutating API requests require --apply";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["git", "issue", "create"]
        | ["git", "issue", "comment"]
        | ["git", "issue", "close"]
        | ["git", "issue", "edit"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates GitHub issue state through the configured repository";
        }
        ["git", "pr", "create"]
        | ["git", "pr", "edit"]
        | ["git", "pr", "comment"]
        | ["git", "pr", "refresh"]
        | ["git", "pr", "policy", "open"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates GitHub pull request state or branch state";
        }
        ["git", "pr", "fleet"] | ["git", "pr", "land"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes =
                "reports by default or with --dry-run; apply/merge flags mutate PR state";
            metadata.dangerous_flags = vec!["--apply", "--delete-branch"];
        }
        ["issues", "reconcile"] | ["issues", "reconcile-run"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "default output is a non-mutating issue reconciliation plan; pass --apply to mutate tracker state";
            metadata.dangerous_flags = vec!["--apply"];
        }
        ["audit-baseline", "refresh"] | ["audit-baseline", "merge"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "mutates persisted audit baseline data in component configuration";
        }
        ["refactor"] => {
            metadata.mutates = true;
            metadata.output_notes = "refactor subcommands can rewrite source files; use planning/dry-run modes where available";
            metadata.dangerous_flags = vec!["--write", "--commit"];
        }
        ["refactor", "rename"]
        | ["refactor", "add"]
        | ["refactor", "move"]
        | ["refactor", "propagate"]
        | ["refactor", "transform"]
        | ["refactor", "decompose"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "reports a plan by default; pass --write to rewrite source files";
            metadata.dangerous_flags = vec!["--write"];
        }
        ["rig", "up"]
        | ["rig", "down"]
        | ["rig", "repair"]
        | ["rig", "install"]
        | ["rig", "update"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates local rig runtime state or installed rig packages";
        }
        ["rig", "sync"]
        | ["rig", "app", "install"]
        | ["rig", "app", "update"]
        | ["rig", "app", "uninstall"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "mutates rig-managed files unless --dry-run is passed";
        }
        ["rig", "sources", "remove"] | ["rig", "sources", "refresh"] => {
            metadata.mutates = true;
            metadata.output_notes = "mutates installed rig source metadata";
        }
        ["runner", "add"]
        | ["runner", "enable"]
        | ["runner", "set"]
        | ["runner", "trust"]
        | ["runner", "pair"]
        | ["runner", "remove"]
        | ["runner", "connect"]
        | ["runner", "disconnect"]
        | ["runner", "work"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes =
                "mutates runner configuration, trust policy, or runner lifecycle state";
        }
        ["runner", "doctor"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes =
                "diagnoses runners by default; --repair mutates runner lifecycle state";
            metadata.dangerous_flags = vec!["--repair"];
        }
        ["runner", "exec"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "executes commands on a runner unless --dry-run is passed";
        }
        ["runner", "workspace", "sync"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "materializes a local worktree into runner workspace state";
            metadata.dangerous_flags = vec!["--allow-dirty-lab-workspace"];
        }
        ["runner", "workspace", "apply"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "applies a Lab-generated workspace patch to a local worktree";
            metadata.dangerous_flags = vec!["--force"];
        }
        ["http", "request"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutating HTTP methods require --apply; GET, HEAD, and OPTIONS are allowed without it";
            metadata.dangerous_flags =
                vec!["--apply", "METHOD!=GET", "METHOD!=HEAD", "METHOD!=OPTIONS"];
        }
        ["worktree", "queue-create"] => {
            metadata.mutates = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "default output creates DMC worktrees one-at-a-time; pass --dry-run to plan without creating";
        }
        ["worktree", "create"] => {
            metadata.mutates = true;
            metadata.output_notes = "creates a task worktree from a registered component checkout";
        }
        ["worktree", "remove"] => {
            metadata.mutates = true;
            metadata.output_notes = "removes a task worktree after safety checks";
            metadata.dangerous_flags = vec!["--force"];
        }
        ["worktree", "cleanup"] => {
            metadata.mutates = true;
            metadata.output_notes =
                "removes cleanup-eligible task worktrees and rebuildable artifacts";
            metadata.dangerous_flags = vec!["--force"];
        }
        ["tunnel", "service", "expose"]
        | ["tunnel", "service", "set"]
        | ["tunnel", "service", "remove"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates private service tunnel declarations";
        }
        ["tunnel", "service", "start"] | ["tunnel", "service", "stop"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates private service tunnel runtime state";
        }
        ["tunnel", "preview-client", "start"]
        | ["tunnel", "preview-consumer", "run"]
        | ["tunnel", "preview-ingress", "serve"]
        | ["tunnel", "artifact-origin", "serve"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "starts or supervises tunnel preview runtime state";
        }
        ["tunnel", "preview-ingress", "route"] | ["tunnel", "preview-ingress", "unroute"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates preview ingress route state";
        }
        ["tunnel", "preview-ingress", "install"] => {
            metadata.operator = true;
            metadata.output_notes = "renders a non-destructive operator install plan";
        }
        ["stack", "create"] | ["stack", "add-pr"] | ["stack", "remove-pr"] => {
            metadata.mutates = true;
            metadata.output_notes = "mutates persisted stack specification metadata";
        }
        ["stack", "apply"] | ["stack", "rebase"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates the configured stack target branch";
        }
        ["stack", "sync"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.output_notes = "mutates the configured stack target branch and may update the stack spec unless --dry-run is passed";
        }
        ["stack", "push"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "pushes the configured stack target branch to its remote";
        }
        ["undo"] => {
            metadata.mutates = true;
            metadata.output_notes = "restores files from the latest or selected undo snapshot";
        }
        ["undo", "delete"] => {
            metadata.mutates = true;
            metadata.output_notes = "deletes an undo snapshot without restoring it";
        }
        ["auth", "login"]
        | ["auth", "set"]
        | ["auth", "remove"]
        | ["auth", "logout"]
        | ["auth", "profile", "set-basic"]
        | ["auth", "profile", "set-bearer"]
        | ["auth", "profile", "remove"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.output_notes = "mutates keychain-backed authentication state";
        }
        _ => {}
    }

    metadata
}

fn docs_path(path: &[String], dynamic_commands: &[DynamicCommandDescriptor]) -> Option<String> {
    if let Some(dynamic) = dynamic_command_for_path(path, dynamic_commands) {
        return dynamic.docs_path.clone();
    }

    let command = path.first()?;

    registered_command(command).and_then(|entry| entry.docs_path())
}

fn dynamic_command_for_path<'a>(
    path: &[String],
    dynamic_commands: &'a [DynamicCommandDescriptor],
) -> Option<&'a DynamicCommandDescriptor> {
    let command = path.first()?;

    if path.len() == 1 {
        dynamic_commands.iter().find(|entry| entry.name == *command)
    } else {
        None
    }
}

fn visible_subcommands(command: &Command, remaining_depth: usize) -> Vec<CommandSurfaceEntry> {
    command
        .get_subcommands()
        .map(|subcommand| CommandSurfaceEntry {
            name: subcommand.get_name().to_string(),
            visible_aliases: subcommand
                .get_visible_aliases()
                .map(str::to_string)
                .collect(),
            hidden: subcommand.is_hide_set(),
            subcommands: if remaining_depth == 0 {
                Vec::new()
            } else {
                visible_subcommands(subcommand, remaining_depth - 1)
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn command_doc(command: &str) -> String {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(root.join("docs/commands").join(format!("{command}.md")))
            .unwrap_or_else(|error| panic!("failed to read docs for {command}: {error}"))
    }

    fn commands_index() -> String {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(root.join("docs/commands/commands-index.md"))
            .unwrap_or_else(|error| panic!("failed to read docs command index: {error}"))
    }

    fn root_command(command: &str) -> clap::Command {
        Cli::command()
            .find_subcommand(command)
            .unwrap_or_else(|| panic!("missing command {command}"))
            .clone()
    }

    fn visible_child_names(command: &clap::Command) -> Vec<String> {
        command
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
            .map(|subcommand| subcommand.get_name().to_string())
            .collect()
    }

    fn visible_long_flags(command: &clap::Command) -> Vec<String> {
        let mut flags: Vec<String> = command
            .get_arguments()
            .filter(|arg| !arg.is_hide_set())
            .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
            .collect();
        flags.sort();
        flags.dedup();
        flags
    }

    fn assert_docs_cover_subcommands(command_name: &str) {
        let command = root_command(command_name);
        let docs = command_doc(command_name);

        for subcommand in visible_child_names(&command) {
            assert!(
                docs.contains(&format!("`{subcommand}")),
                "docs/commands/{command_name}.md does not document `{subcommand}` from live help"
            );
        }
    }

    fn assert_docs_cover_flags(command_name: &str) {
        let command = root_command(command_name);
        let docs = command_doc(command_name);

        for flag in visible_long_flags(&command) {
            assert!(
                docs.contains(&flag),
                "docs/commands/{command_name}.md does not document `{flag}` from live help"
            );
        }
    }

    fn manifest_path<'a>(
        manifest: &'a CommandSafetyManifest,
        path: &[&str],
    ) -> &'a CommandSafetyEntry {
        manifest
            .find_path(path)
            .unwrap_or_else(|| panic!("missing safety entry for {path:?}"))
    }

    fn flatten_manifest_entries<'a>(
        entries: &'a [CommandSafetyEntry],
        flattened: &mut Vec<&'a CommandSafetyEntry>,
    ) {
        for entry in entries {
            flattened.push(entry);
            flatten_manifest_entries(&entry.subcommands, flattened);
        }
    }

    fn command_has_visible_flag(command: &clap::Command, flag: &str) -> bool {
        command
            .get_arguments()
            .any(|arg| !arg.is_hide_set() && arg.get_long().is_some_and(|long| long == flag))
    }

    fn command_has_visible_risk_flag(command: &clap::Command) -> bool {
        ["apply", "dry-run", "force"]
            .iter()
            .any(|flag| command_has_visible_flag(command, flag))
    }

    fn path_has_visible_risk_flag(command: &clap::Command, path: &[String]) -> bool {
        let Some((first, rest)) = path.split_first() else {
            return false;
        };
        let Some(subcommand) = command.find_subcommand(first) else {
            return false;
        };
        if rest.is_empty() {
            return command_has_visible_risk_flag(subcommand);
        }
        path_has_visible_risk_flag(subcommand, rest)
    }

    fn is_suspicious_path(entry: &CommandSafetyEntry) -> bool {
        let Some(name) = entry.path.last().map(String::as_str) else {
            return false;
        };

        matches!(
            name,
            "apply"
                | "cleanup"
                | "cleanup-downloads"
                | "cleanup-persisted"
                | "connect"
                | "create"
                | "delete"
                | "disconnect"
                | "generate"
                | "import"
                | "init"
                | "install"
                | "install-for-component"
                | "refresh"
                | "relink"
                | "release"
                | "remove"
                | "rename"
                | "reset"
                | "set"
                | "uninstall"
                | "unset"
                | "update"
                | "upgrade"
                | "use"
        )
    }

    fn entry_has_safety_classification(entry: &CommandSafetyEntry) -> bool {
        entry.mutates
            || entry.operator
            || entry.dry_run.supported
            || !entry.dangerous_flags.is_empty()
            || entry.output.notes.contains("--apply")
            || entry.output.notes.contains("--dry-run")
    }

    #[test]
    fn test_current_command_surface() {
        let surface = current_command_surface();

        assert!(surface.contains_path(&["self"]));
        assert!(surface.contains_path(&["self", "status"]));
        assert!(surface.contains_path(&["doctor", "resources"]));
        assert!(surface.contains_path(&["ci", "list"]));
        assert!(surface.contains_path(&["agent-task", "controller", "run-next"]));
        assert!(surface.contains_path(&["observe"]));
    }

    #[test]
    fn test_command_surface_from() {
        let surface = command_surface_from(Cli::command());

        assert!(surface.contains_path(&["self"]));
        assert!(surface.contains_path(&["self", "status"]));
        assert!(surface.contains_path(&["doctor", "resources"]));
        assert!(surface.contains_path(&["ci", "list"]));
        assert!(surface.contains_path(&["agent-task", "controller", "run-next"]));
        assert!(surface.contains_path(&["observe"]));
    }

    #[test]
    fn test_contains_path() {
        let surface = current_command_surface();

        assert!(surface.contains_path(&["self"]));
        assert!(!surface.contains_path(&["self", "missing"]));
    }

    #[test]
    fn command_safety_manifest_covers_surface_paths() {
        let manifest = current_command_safety_manifest();

        assert!(manifest.find_path(&["db"]).is_some());
        assert!(manifest.find_path(&["db", "delete-row"]).is_some());
        assert!(manifest.find_path(&["file", "write"]).is_some());
        assert!(manifest.find_path(&["api", "post"]).is_some());
        assert!(manifest
            .find_path(&["agent-task", "controller", "run-next"])
            .is_some());
    }

    #[test]
    fn command_safety_manifest_records_clap_visibility_metadata() {
        let manifest = current_command_safety_manifest();

        let command_manifest = manifest_path(&manifest, &["manifest"]);
        assert!(!command_manifest.hidden);
        assert!(command_manifest.output.structured);
        assert!(command_manifest
            .output
            .notes
            .contains("recursive command safety"));

        let visible_status = manifest.find_path(&["status"]).unwrap();
        assert!(!visible_status.hidden);
        assert!(visible_status.aliases.is_empty());
    }

    #[test]
    fn command_safety_manifest_uses_registry_metadata() {
        let manifest = current_command_safety_manifest();

        let bench = manifest.find_path(&["bench"]).unwrap();
        assert!(bench.output.structured);
        assert!(bench.lab.supported);
        assert!(bench.lab.notes.contains("portable Lab offload"));
        assert_eq!(bench.docs.path.as_deref(), Some("docs/commands/bench.md"));
    }

    #[test]
    fn command_registry_docs_paths_exist_and_are_indexed() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let index = commands_index();

        for entry in crate::command_contract::COMMAND_REGISTRY {
            let Some(path) = entry.docs_path() else {
                continue;
            };
            let Some(slug) = entry.docs_slug else {
                continue;
            };

            assert!(
                root.join(&path).is_file(),
                "registered command `{}` points at missing docs path {path}",
                entry.name
            );
            assert!(
                index.contains(&format!("[{slug}]({slug}.md)")),
                "docs/commands/commands-index.md is missing registered command `{}`",
                entry.name
            );
        }
    }

    #[test]
    fn command_registry_manifest_and_docs_metadata_align() {
        let parser_names = Cli::command()
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
            .map(|subcommand| subcommand.get_name().to_string())
            .collect::<BTreeSet<_>>();
        let registry_names = crate::command_contract::COMMAND_REGISTRY
            .iter()
            .map(|entry| entry.name.to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(registry_names, parser_names);

        let manifest = current_command_safety_manifest();
        for entry in crate::command_contract::COMMAND_REGISTRY {
            let manifest_entry = manifest
                .find_path(&[entry.name])
                .unwrap_or_else(|| panic!("manifest missing registered command `{}`", entry.name));
            let output_notes_overridden_for_safety = matches!(entry.name, "release" | "upgrade");

            assert_eq!(
                manifest_entry.docs.path,
                entry.docs_path(),
                "manifest docs path drifted from registry for `{}`",
                entry.name
            );
            if !output_notes_overridden_for_safety {
                assert_eq!(
                    manifest_entry.output.notes, entry.output_notes,
                    "manifest output notes drifted from registry for `{}`",
                    entry.name
                );
            }
            assert_eq!(
                manifest_entry.lab.supported, entry.lab_supported,
                "manifest Lab support drifted from registry for `{}`",
                entry.name
            );
            assert_eq!(
                manifest_entry.lab.notes, entry.lab_notes,
                "manifest Lab notes drifted from registry for `{}`",
                entry.name
            );
        }
    }

    #[test]
    fn core_command_docs_do_not_drift_from_registry() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let registered_docs = crate::command_contract::COMMAND_REGISTRY
            .iter()
            .filter_map(|entry| entry.docs_slug)
            .collect::<BTreeSet<_>>();
        let extension_or_support_docs =
            BTreeSet::from(["audit-rules", "cargo", "commands-index", "rig-spec", "wp"]);

        for doc in
            std::fs::read_dir(root.join("docs/commands")).expect("failed to read docs/commands")
        {
            let doc = doc.expect("failed to read docs/commands entry");
            let path = doc.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                continue;
            }
            let slug = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("command docs filename should be valid UTF-8");
            assert!(
                registered_docs.contains(slug) || extension_or_support_docs.contains(slug),
                "docs/commands/{slug}.md is not registered as a core command doc or known extension/support doc"
            );
        }
    }

    #[test]
    fn command_safety_manifest_includes_dynamic_command_descriptors() {
        let dynamic_command = DynamicCommandDescriptor::extension_command(
            "ext-tool".to_string(),
            "Run extension tool commands".to_string(),
        );
        let command = Cli::command().subcommand(clap::Command::new("ext-tool"));
        let manifest = command_safety_manifest_from_dynamic(
            command_surface_from(command),
            std::slice::from_ref(&dynamic_command),
        );

        let ext_tool = manifest.find_path(&["ext-tool"]).unwrap();
        assert_eq!(
            ext_tool.docs.path.as_deref(),
            Some("docs/commands/ext-tool.md")
        );
        assert!(ext_tool.output.structured);
    }

    #[test]
    fn command_safety_manifest_classifies_known_dangerous_paths() {
        let manifest = current_command_safety_manifest();

        for path in [
            ["release"].as_slice(),
            ["upgrade"].as_slice(),
            ["cleanup", "artifacts"].as_slice(),
            ["self", "cleanup-runtime-tmp"].as_slice(),
            ["db", "delete-row"].as_slice(),
            ["db", "drop-table"].as_slice(),
            ["file", "write"].as_slice(),
            ["file", "delete"].as_slice(),
            ["docs", "map"].as_slice(),
            ["runs", "reconcile"].as_slice(),
            ["runs", "import"].as_slice(),
            ["runs", "artifact", "cleanup-downloads"].as_slice(),
            ["runs", "artifact", "cleanup-persisted"].as_slice(),
            ["agent-task", "promote"].as_slice(),
            ["extension", "install"].as_slice(),
            ["extension", "update"].as_slice(),
            ["extension", "uninstall"].as_slice(),
            ["config", "set"].as_slice(),
            ["config", "remove"].as_slice(),
            ["project", "set"].as_slice(),
            ["project", "delete"].as_slice(),
            ["component", "set"].as_slice(),
            ["component", "delete"].as_slice(),
            ["server", "set"].as_slice(),
            ["server", "delete"].as_slice(),
            ["api", "post"].as_slice(),
            ["api", "put"].as_slice(),
            ["api", "patch"].as_slice(),
            ["api", "delete"].as_slice(),
            ["http", "request"].as_slice(),
        ] {
            let entry = manifest_path(&manifest, path);

            assert!(entry.mutates, "{path:?} should be marked mutating");
        }

        for path in [
            ["release"].as_slice(),
            ["upgrade"].as_slice(),
            ["self", "cleanup-runtime-tmp"].as_slice(),
            ["db", "delete-row"].as_slice(),
            ["file", "delete"].as_slice(),
            ["server", "set"].as_slice(),
            ["api", "post"].as_slice(),
            ["http", "request"].as_slice(),
        ] {
            let entry = manifest_path(&manifest, path);
            assert!(entry.operator, "{path:?} should be marked operator-gated");
        }
    }

    #[test]
    fn command_safety_manifest_records_guard_flags_and_dry_run_flags() {
        let manifest = current_command_safety_manifest();

        let deploy = manifest.find_path(&["deploy"]).unwrap();
        assert!(deploy.operator);
        assert_eq!(deploy.dry_run.flag.as_deref(), Some("--dry-run"));
        assert_eq!(deploy.dangerous_flags, vec!["--head", "--force"]);

        let triage = manifest.find_path(&["triage"]).unwrap();
        assert_eq!(triage.dangerous_flags, vec!["--auto-merge"]);

        let docs_map = manifest.find_path(&["docs", "map"]).unwrap();
        assert!(docs_map.output.notes.contains("--write"));
        assert_eq!(docs_map.dangerous_flags, vec!["--write"]);

        let fleet_exec = manifest.find_path(&["fleet", "exec"]).unwrap();
        assert_eq!(fleet_exec.dry_run.flag.as_deref(), Some("--check"));
        assert!(fleet_exec.output.notes.contains("--apply"));
        assert!(fleet_exec.dangerous_flags.contains(&"--apply".to_string()));
        assert!(fleet_exec.lab.notes.contains("local-only"));

        let db_delete_row = manifest.find_path(&["db", "delete-row"]).unwrap();
        assert!(db_delete_row.output.notes.contains("--apply"));

        let file_write = manifest.find_path(&["file", "write"]).unwrap();
        assert!(file_write.output.notes.contains("--apply"));

        let api_post = manifest.find_path(&["api", "post"]).unwrap();
        assert!(api_post.output.notes.contains("--apply"));
        assert!(api_post.dangerous_flags.contains(&"--apply".to_string()));

        let http_request = manifest.find_path(&["http", "request"]).unwrap();
        assert!(http_request.output.notes.contains("--apply"));
        assert!(http_request
            .dangerous_flags
            .contains(&"METHOD!=GET".to_string()));

        let release = manifest_path(&manifest, &["release"]);
        assert_eq!(release.dry_run.flag.as_deref(), Some("--dry-run"));
        assert!(release.dangerous_flags.contains(&"--apply".to_string()));
        assert!(release.dangerous_flags.contains(&"--head".to_string()));

        let cleanup_artifacts = manifest_path(&manifest, &["cleanup", "artifacts"]);
        assert!(cleanup_artifacts.output.notes.contains("--apply"));

        let self_cleanup = manifest_path(&manifest, &["self", "cleanup-runtime-tmp"]);
        assert!(self_cleanup.output.notes.contains("--apply"));

        let runs_reconcile = manifest_path(&manifest, &["runs", "reconcile"]);
        assert_eq!(runs_reconcile.dry_run.flag.as_deref(), Some("--dry-run"));

        let runs_cleanup = manifest_path(&manifest, &["runs", "artifact", "cleanup-persisted"]);
        assert!(runs_cleanup.output.notes.contains("--apply"));

        let agent_task_promote = manifest_path(&manifest, &["agent-task", "promote"]);
        assert_eq!(
            agent_task_promote.dry_run.flag.as_deref(),
            Some("--dry-run")
        );
    }

    #[test]
    fn suspicious_command_paths_require_safety_classification() {
        let manifest = current_command_safety_manifest();
        let command = Cli::command();
        let mut entries = Vec::new();
        flatten_manifest_entries(&manifest.commands, &mut entries);

        for entry in entries {
            let suspicious = !entry.hidden
                && (is_suspicious_path(entry) || path_has_visible_risk_flag(&command, &entry.path));
            if suspicious {
                assert!(
                    entry_has_safety_classification(entry),
                    "suspicious command path {:?} lacks explicit safety metadata",
                    entry.path
                );
            }
        }
    }

    #[test]
    fn allow_dirty_lab_workspace_global_flag_parses() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "trace",
            "--runner",
            "homeboy-lab",
            "--allow-dirty-lab-workspace",
        ])
        .expect("global dirty Lab workspace override should parse");

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert!(cli.allow_dirty_lab_workspace);
    }

    #[test]
    fn docs_cover_high_use_command_surfaces() {
        for command in ["runner", "rig"] {
            assert_docs_cover_subcommands(command);
        }

        assert_docs_cover_flags("audit");
    }

    #[test]
    fn documented_command_forms_parse() {
        for args in [
            ["homeboy", "refactor", "homeboy", "--all"].as_slice(),
            [
                "homeboy",
                "report",
                "failure-digest",
                "--output-dir",
                ".",
                "--results",
                "{\"review\":\"fail\"}",
            ]
            .as_slice(),
            ["homeboy", "rig", "repair", "studio"].as_slice(),
            ["homeboy", "runner", "doctor", "local"].as_slice(),
            ["homeboy", "runner", "connect", "homeboy-lab"].as_slice(),
            ["homeboy", "runner", "status", "homeboy-lab"].as_slice(),
            ["homeboy", "runner", "disconnect", "homeboy-lab"].as_slice(),
            [
                "homeboy",
                "db",
                "delete-row",
                "mysite",
                "--apply",
                "wp_posts",
                "1",
            ]
            .as_slice(),
            ["homeboy", "db", "drop-table", "mysite", "--apply", "wp_tmp"].as_slice(),
            ["homeboy", "file", "delete", "mysite", "tmp.txt", "--apply"].as_slice(),
            ["homeboy", "file", "write", "mysite", "tmp.txt", "--apply"].as_slice(),
            [
                "homeboy",
                "api",
                "mysite",
                "post",
                "/wp/v2/posts",
                "--apply",
            ]
            .as_slice(),
            [
                "homeboy",
                "http",
                "request",
                "POST",
                "--apply",
                "https://example.test/api",
            ]
            .as_slice(),
        ] {
            Cli::try_parse_from(args).unwrap_or_else(|error| {
                panic!("documented command form failed to parse: {args:?}\n{error}")
            });
        }
    }

    #[test]
    fn dynamic_set_commands_require_canonical_update_inputs() {
        for args in [
            [
                "homeboy",
                "server",
                "set",
                "sandbox",
                "auth.mode=key_plus_password_controlmaster",
            ]
            .as_slice(),
            [
                "homeboy",
                "project",
                "set",
                "sandbox",
                r#"{"base_path":"/srv/site"}"#,
            ]
            .as_slice(),
            [
                "homeboy",
                "runner",
                "set",
                "sandbox",
                "--",
                "--concurrency_limit",
                "4",
            ]
            .as_slice(),
        ] {
            assert!(
                Cli::try_parse_from(args).is_err(),
                "dynamic set compatibility form should not parse: {args:?}"
            );
        }

        for args in [
            [
                "homeboy",
                "server",
                "set",
                "sandbox",
                "--json",
                r#"{"host":"example.com"}"#,
            ]
            .as_slice(),
            ["homeboy", "project", "set", "sandbox", "--base64", "e30="].as_slice(),
            [
                "homeboy",
                "component",
                "set",
                "sandbox",
                "--changelog-target",
                "CHANGELOG.md",
            ]
            .as_slice(),
        ] {
            Cli::try_parse_from(args).unwrap_or_else(|error| {
                panic!("canonical dynamic set form failed to parse: {args:?}\n{error}")
            });
        }
    }
}
