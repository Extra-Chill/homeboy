use clap::{Command, CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::{
    api, audit, auth, bench, build, changelog, changes, ci, component, config, daemon, db, deploy,
    deps, doctor, extension, file, fleet, git, http, issues, lint, logs, observe, project,
    refactor, release, report, review, rig, runner, runs, self_cmd, server, ssh, stack, status,
    test, trace, triage, undo, upgrade, version,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "homeboy")]
#[command(version = VERSION)]
#[command(about = "CLI tool for development and deployment automation")]
pub struct Cli {
    /// Write structured JSON output to a file (in addition to stdout).
    /// The file contains command-specific JSON — no log text.
    #[arg(long, global = true, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Suppress resource policy warnings for intentionally hot commands.
    #[arg(long, global = true)]
    pub force_hot: bool,

    /// Directory where persisted run artifacts are copied.
    /// Overrides HOMEBOY_ARTIFACT_ROOT and global config /artifact_root.
    #[arg(long, global = true, value_name = "DIR")]
    pub artifact_root: Option<PathBuf>,

    /// Offload supported hot commands to a connected Homeboy Lab runner.
    #[arg(long, global = true, hide = true, value_name = "RUNNER_ID")]
    pub runner: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Manage project configuration
    #[command(visible_alias = "projects")]
    Project(project::ProjectArgs),
    /// SSH into a project server or configured server
    Ssh(ssh::SshArgs),
    /// Manage SSH server configurations
    #[command(visible_alias = "servers")]
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
    #[command(visible_alias = "dependencies")]
    Deps(deps::DepsArgs),
    /// Inspect CI reproduction profiles and discovered CI surfaces
    Ci(ci::CiArgs),
    /// Read-only local diagnostics for Homeboy-adjacent work
    Doctor(doctor::DoctorArgs),
    /// Remote file operations
    File(file::FileArgs),
    /// Manage fleets (groups of projects)
    #[command(visible_alias = "fleets")]
    Fleet(fleet::FleetArgs),
    /// Remote log viewing
    Logs(logs::LogsArgs),
    /// Read-only attention report for components, projects, fleets, and rigs
    Triage(triage::TriageArgs),
    /// Deploy components to remote server
    Deploy(deploy::DeployArgs),
    /// Manage standalone component configurations
    #[command(visible_alias = "components")]
    Component(component::ComponentArgs),
    /// Manage global Homeboy configuration
    Config(config::ConfigArgs),
    /// Run the local-only HTTP API daemon
    Daemon(daemon::DaemonArgs),
    /// Execute CLI-compatible extensions
    #[command(visible_alias = "extensions")]
    Extension(extension::ExtensionArgs),
    /// Actionable component status overview
    Status(status::StatusArgs),
    /// Display CLI documentation
    Docs(crate::commands::docs::DocsArgs),
    /// Changelog operations
    Changelog(changelog::ChangelogArgs),
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
    /// Structural refactoring (rename terms across codebase)
    Refactor(refactor::RefactorArgs),
    /// Manage local dev rigs (reproducible multi-component environments)
    #[command(visible_alias = "rigs")]
    Rig(rig::RigArgs),
    /// Manage local and SSH execution runners
    #[command(visible_alias = "runners")]
    Runner(runner::RunnerArgs),
    /// Inspect persisted observation runs and artifacts
    Runs(runs::RunsArgs),
    /// Inspect the active Homeboy binary and install signals
    #[command(name = "self")]
    SelfCmd(self_cmd::SelfArgs),
    /// Manage stacks (combined-fixes branches built from base + cherry-picked PRs)
    #[command(visible_alias = "stacks")]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandResponseMode {
    Json,
    Raw(CommandRawOutputMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRawOutputMode {
    InteractivePassthrough,
    Markdown,
    PlainText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStdoutMode {
    JsonEnvelope,
    Raw(CommandRawOutputMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutputFileMode {
    None,
    GenericEnvelope,
    ReviewStableArtifact,
    TraceJsonSummaryArtifact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandResponsePlan {
    pub stdout: CommandStdoutMode,
    pub output_file: CommandOutputFileMode,
}

impl Commands {
    pub fn response_plan(&self, has_output_file: bool) -> CommandResponsePlan {
        let mode = self.response_mode(has_output_file);

        CommandResponsePlan {
            stdout: match mode {
                CommandResponseMode::Json => CommandStdoutMode::JsonEnvelope,
                CommandResponseMode::Raw(raw_mode) => CommandStdoutMode::Raw(raw_mode),
            },
            output_file: self.output_file_mode(has_output_file),
        }
    }

    pub fn supports_lab_runner(&self) -> bool {
        match self {
            Commands::Audit(_) => true,
            Commands::Bench(args) if args.is_run_command() => true,
            Commands::Lint(_) => true,
            Commands::Test(_) => true,
            Commands::Trace(_) => true,
            _ => false,
        }
    }

    pub fn lab_offload_mutation_flag(&self) -> Option<&'static str> {
        match self {
            Commands::Bench(args) if args.lab_offload_writes_local_state() => {
                Some("--baseline/--ratchet")
            }
            Commands::Lint(args) if args.fix => Some("--fix"),
            Commands::Test(args) if args.write => Some("--write"),
            Commands::Trace(args) if args.keep_overlay => Some("--keep-overlay"),
            _ => None,
        }
    }

    pub fn response_mode(&self, has_output_file: bool) -> CommandResponseMode {
        match self {
            Commands::Ssh(args) if args.subcommand.is_none() && args.command.is_empty() => {
                CommandResponseMode::Raw(CommandRawOutputMode::InteractivePassthrough)
            }
            Commands::Logs(args) if logs::is_interactive(args) => {
                CommandResponseMode::Raw(CommandRawOutputMode::InteractivePassthrough)
            }
            Commands::File(args) if file::is_raw_read(args) => {
                CommandResponseMode::Raw(CommandRawOutputMode::PlainText)
            }
            Commands::Docs(args) if crate::commands::docs::is_json_mode(args) => {
                CommandResponseMode::Json
            }
            Commands::Docs(_) => CommandResponseMode::Raw(CommandRawOutputMode::Markdown),
            Commands::Changelog(args) if changelog::is_show_markdown(args) => {
                CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
            }
            Commands::Review(args) if review::is_markdown_mode(args) => {
                CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
            }
            Commands::Trace(args) if trace::is_markdown_mode(args) => {
                CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
            }
            Commands::Runs(args) if !has_output_file && args.is_markdown_mode() => {
                CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
            }
            Commands::Report(args) if report::is_markdown_mode(args) => {
                CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
            }
            Commands::List => CommandResponseMode::Raw(CommandRawOutputMode::Markdown),
            _ => CommandResponseMode::Json,
        }
    }

    pub fn output_file_mode(&self, has_output_file: bool) -> CommandOutputFileMode {
        if !has_output_file {
            return CommandOutputFileMode::None;
        }

        match self {
            Commands::Review(_) => CommandOutputFileMode::ReviewStableArtifact,
            Commands::Trace(args) if args.json_summary => {
                CommandOutputFileMode::TraceJsonSummaryArtifact
            }
            _ => CommandOutputFileMode::GenericEnvelope,
        }
    }
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

    fn parsed_command(args: &[&str]) -> Commands {
        Cli::try_parse_from(args)
            .expect("CLI args should parse")
            .command
    }

    fn parsed_cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("CLI args should parse")
    }

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
    fn test_response_mode() {
        assert_eq!(
            parsed_command(&["homeboy", "status"]).response_mode(false),
            CommandResponseMode::Json
        );
        assert_eq!(
            parsed_command(&["homeboy", "review", "--report", "pr-comment"]).response_mode(false),
            CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
        );
        assert_eq!(
            parsed_command(&["homeboy", "trace", "--report", "markdown"]).response_mode(false),
            CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
        );
        assert_eq!(
            Commands::List.response_mode(false),
            CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
        );
    }

    #[test]
    fn test_response_plan() {
        assert_eq!(
            parsed_command(&["homeboy", "status"]).response_plan(false),
            CommandResponsePlan {
                stdout: CommandStdoutMode::JsonEnvelope,
                output_file: CommandOutputFileMode::None,
            }
        );

        assert_eq!(
            parsed_command(&["homeboy", "review", "--report", "pr-comment"]).response_plan(false),
            CommandResponsePlan {
                stdout: CommandStdoutMode::Raw(CommandRawOutputMode::Markdown),
                output_file: CommandOutputFileMode::None,
            }
        );

        assert_eq!(
            parsed_command(&["homeboy", "review", "--report", "pr-comment"]).response_plan(true),
            CommandResponsePlan {
                stdout: CommandStdoutMode::Raw(CommandRawOutputMode::Markdown),
                output_file: CommandOutputFileMode::ReviewStableArtifact,
            }
        );

        assert_eq!(
            parsed_command(&["homeboy", "trace", "--report", "markdown", "--json-summary",])
                .response_plan(true),
            CommandResponsePlan {
                stdout: CommandStdoutMode::Raw(CommandRawOutputMode::Markdown),
                output_file: CommandOutputFileMode::TraceJsonSummaryArtifact,
            }
        );
    }

    #[test]
    fn test_supports_lab_runner() {
        assert!(parsed_command(&["homeboy", "lint"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "test"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "audit"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "bench"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "trace"]).supports_lab_runner());
        assert!(!parsed_command(&["homeboy", "status"]).supports_lab_runner());
        assert!(!parsed_command(&["homeboy", "bench", "list"]).supports_lab_runner());

        let cli = parsed_cli(&["homeboy", "lint", "--runner", "lab-a"]);
        assert_eq!(cli.runner.as_deref(), Some("lab-a"));
        assert!(cli.command.supports_lab_runner());
    }

    #[test]
    fn test_lab_runner_flag_is_hidden_from_help() {
        let root_help = Cli::command()
            .try_get_matches_from(["homeboy", "--help"])
            .expect_err("help exits")
            .to_string();
        assert!(!root_help.contains("--runner"));

        for args in [
            ["homeboy", "rig", "check", "--help"].as_slice(),
            ["homeboy", "build", "--help"].as_slice(),
            ["homeboy", "bench", "list", "--help"].as_slice(),
        ] {
            let help = Cli::command()
                .try_get_matches_from(args)
                .expect_err("help exits")
                .to_string();
            assert!(
                !help.contains("--runner"),
                "{args:?} help advertised --runner"
            );
        }
    }

    #[test]
    fn test_lab_offload_mutation_flag() {
        assert_eq!(
            parsed_command(&["homeboy", "lint", "--fix"]).lab_offload_mutation_flag(),
            Some("--fix")
        );
        assert_eq!(
            parsed_command(&["homeboy", "test", "--write"]).lab_offload_mutation_flag(),
            Some("--write")
        );
        assert_eq!(
            parsed_command(&["homeboy", "bench", "--baseline"]).lab_offload_mutation_flag(),
            Some("--baseline/--ratchet")
        );
        assert_eq!(
            parsed_command(&["homeboy", "trace", "--keep-overlay"]).lab_offload_mutation_flag(),
            Some("--keep-overlay")
        );
        assert_eq!(
            parsed_command(&["homeboy", "audit"]).lab_offload_mutation_flag(),
            None
        );
    }

    #[test]
    fn test_output_artifact_policy() {
        assert_eq!(
            parsed_command(&["homeboy", "status"]).output_file_mode(true),
            CommandOutputFileMode::GenericEnvelope
        );
        assert_eq!(
            parsed_command(&["homeboy", "review"]).output_file_mode(true),
            CommandOutputFileMode::ReviewStableArtifact
        );
        assert_eq!(
            parsed_command(&["homeboy", "trace", "--json-summary"]).output_file_mode(true),
            CommandOutputFileMode::TraceJsonSummaryArtifact
        );
        assert_eq!(
            parsed_command(&["homeboy", "trace", "--json-summary"]).output_file_mode(false),
            CommandOutputFileMode::None
        );
    }
}
