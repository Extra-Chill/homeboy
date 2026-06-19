use clap::{Command, CommandFactory, Parser, Subcommand};
use serde::Serialize;
use std::path::PathBuf;

use crate::commands::{
    agent_task, api, audit, audit_baseline, auth, bench, build, changelog, changes, ci, cleanup,
    component, config, daemon, db, deploy, deps, doctor, extension, file, fleet, git, http, issues,
    lab, lint, logs, observe, project, refactor, refs, release, report, review, rig, runner, runs,
    runtime, self_cmd, server, ssh, stack, status, test, trace, triage, tunnel, undo, upgrade,
    version, worktree,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_COMMAND_SURFACE_DEPTH: usize = 2;

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
    /// Build a component
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
    /// Discover Lab routing and benchmark offload commands
    Lab(lab::LabArgs),
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
    /// List available commands (deprecated alias for --help)
    #[command(hide = true)]
    List {
        /// Print the recursive command safety manifest as JSON.
        #[arg(long)]
        json: bool,
    },
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

        let Some(entry) = self.commands.iter().find(|entry| entry.matches(first)) else {
            return false;
        };

        entry.contains_rest(rest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSurfaceEntry {
    pub name: String,
    pub visible_aliases: Vec<String>,
    pub subcommands: Vec<CommandSurfaceEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSafetyManifest {
    pub commands: Vec<CommandSafetyEntry>,
}

impl CommandSafetyManifest {
    pub fn find_path(&self, path: &[&str]) -> Option<&CommandSafetyEntry> {
        let Some((first, rest)) = path.split_first() else {
            return None;
        };

        self.commands
            .iter()
            .find(|entry| entry.name == *first)
            .and_then(|entry| entry.find_rest(rest))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSafetyEntry {
    pub name: String,
    pub path: Vec<String>,
    pub mutates: bool,
    pub operator: bool,
    pub dry_run: CommandDryRunMetadata,
    pub output: CommandOutputMetadata,
    pub lab: CommandLabMetadata,
    pub docs: CommandDocsMetadata,
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
            .find(|entry| entry.matches(first))
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
            Commands::Lab(_) => "lab",
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
            Commands::List { .. } => "list",
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

pub fn command_safety_manifest_from(surface: CommandSurface) -> CommandSafetyManifest {
    CommandSafetyManifest {
        commands: surface
            .commands
            .iter()
            .map(|entry| command_safety_entry(entry, &[]))
            .collect(),
    }
}

fn command_safety_entry(entry: &CommandSurfaceEntry, parent_path: &[String]) -> CommandSafetyEntry {
    let mut path = parent_path.to_vec();
    path.push(entry.name.clone());
    let safety = command_safety_metadata(&path);

    CommandSafetyEntry {
        name: entry.name.clone(),
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
            path: docs_path(&path),
        },
        dangerous_flags: safety
            .dangerous_flags
            .into_iter()
            .map(str::to_string)
            .collect(),
        subcommands: entry
            .subcommands
            .iter()
            .map(|subcommand| command_safety_entry(subcommand, &path))
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

    match path
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["deploy"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dry_run_flag = Some("--dry-run");
            metadata.dangerous_flags = vec!["--head", "--force"];
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
        ["file", "mkdir"] | ["file", "rename"] => {
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
            metadata.lab_notes = "local-only: depends on local fleet/project/server configuration before SSH fan-out";
        }
        ["api", "post"] | ["api", "put"] | ["api", "patch"] | ["api", "delete"] => {
            metadata.mutates = true;
            metadata.operator = true;
        }
        ["http", "request"] => {
            metadata.mutates = true;
            metadata.operator = true;
            metadata.dangerous_flags = vec!["METHOD!=GET", "METHOD!=HEAD", "METHOD!=OPTIONS"];
        }
        ["bench"] | ["test"] | ["lint"] | ["audit"] | ["trace"] => {
            metadata.lab_supported = true;
            metadata.lab_notes =
                "portable Lab offload may be available for resource-heavy workflows";
        }
        ["list"] => {
            metadata.structured_output = false;
            metadata.output_notes = "hidden raw Markdown help alias";
        }
        _ => {}
    }

    metadata
}

fn docs_path(path: &[String]) -> Option<String> {
    path.first()
        .filter(|command| command.as_str() != "list")
        .map(|command| format!("docs/commands/{command}.md"))
}

fn visible_subcommands(command: &Command, remaining_depth: usize) -> Vec<CommandSurfaceEntry> {
    command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .map(|subcommand| CommandSurfaceEntry {
            name: subcommand.get_name().to_string(),
            visible_aliases: subcommand
                .get_visible_aliases()
                .map(str::to_string)
                .collect(),
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

    fn command_doc(command: &str) -> String {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(root.join("docs/commands").join(format!("{command}.md")))
            .unwrap_or_else(|error| panic!("failed to read docs for {command}: {error}"))
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

    #[test]
    fn test_current_command_surface() {
        let surface = current_command_surface();

        assert!(surface.contains_path(&["self"]));
        assert!(surface.contains_path(&["self", "status"]));
        assert!(surface.contains_path(&["doctor", "resources"]));
        assert!(surface.contains_path(&["ci", "list"]));
        assert!(surface.contains_path(&["observe"]));
    }

    #[test]
    fn test_command_surface_from() {
        let surface = command_surface_from(Cli::command());

        assert!(surface.contains_path(&["self"]));
        assert!(surface.contains_path(&["self", "status"]));
        assert!(surface.contains_path(&["doctor", "resources"]));
        assert!(surface.contains_path(&["ci", "list"]));
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
    }

    #[test]
    fn command_safety_manifest_classifies_known_dangerous_paths() {
        let manifest = current_command_safety_manifest();

        for path in [
            ["db", "delete-row"].as_slice(),
            ["db", "drop-table"].as_slice(),
            ["file", "write"].as_slice(),
            ["file", "delete"].as_slice(),
            ["api", "post"].as_slice(),
            ["api", "put"].as_slice(),
            ["api", "patch"].as_slice(),
            ["api", "delete"].as_slice(),
            ["http", "request"].as_slice(),
        ] {
            let entry = manifest
                .find_path(path)
                .unwrap_or_else(|| panic!("missing safety entry for {path:?}"));

            assert!(entry.mutates, "{path:?} should be marked mutating");
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

        let fleet_exec = manifest.find_path(&["fleet", "exec"]).unwrap();
        assert_eq!(fleet_exec.dry_run.flag.as_deref(), Some("--check"));
        assert!(fleet_exec.lab.notes.contains("local-only"));

        let db_delete_row = manifest.find_path(&["db", "delete-row"]).unwrap();
        assert!(db_delete_row.output.notes.contains("--apply"));

        let file_write = manifest.find_path(&["file", "write"]).unwrap();
        assert!(file_write.output.notes.contains("--apply"));
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
