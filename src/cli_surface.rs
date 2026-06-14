use clap::{Command, CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::{
    agent_task, api, audit, audit_baseline, auth, bench, build, changelog, changes, ci, cleanup,
    component, config, daemon, db, deploy, deps, doctor, extension, file, fleet, git, http, issues,
    lab, lint, logs, observe, project, refactor, refs, release, report, review, rig, runner, runs,
    runtime, self_cmd, server, ssh, stack, status, test, trace, triage, tunnel, undo, upgrade,
    version, worktree,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

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

    /// Allow --force-hot portable Lab commands to stay local even when a default Lab runner exists.
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
    /// Read-only attention report for components, projects, fleets, and rigs
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
    /// List available commands (alias for --help)
    List,
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

        match rest {
            [] => true,
            [second] => entry.subcommands.iter().any(|sub| sub.matches(second)),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSurfaceEntry {
    pub name: String,
    pub visible_aliases: Vec<String>,
    pub subcommands: Vec<CommandSurfaceEntry>,
}

impl CommandSurfaceEntry {
    fn matches(&self, name: &str) -> bool {
        self.name == name || self.visible_aliases.iter().any(|alias| alias == name)
    }
}

pub fn current_command_surface() -> CommandSurface {
    command_surface_from(Cli::command())
}

pub fn command_surface_from(command: Command) -> CommandSurface {
    CommandSurface {
        commands: visible_subcommands(&command, 1),
    }
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
