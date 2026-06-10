use clap::{Command, CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::{
    agent_task, api, audit, audit_baseline, auth, bench, build, changelog, changes, ci, cleanup,
    component, config, daemon, db, deploy, deps, doctor, extension, file, fleet, git, http, issues,
    lab, lint, logs, observe, project, refactor, refs, release, report, review, rig, runner, runs,
    runtime, self_cmd, server, ssh, stack, status, test, trace, triage, tunnel, undo, upgrade,
    version, worktree,
};

mod lab_contract;
pub use lab_contract::{
    LabCommandContract, LabCommandPortability, LabCommandRequiredTool, LabSourcePathMode,
    LabWorkspaceModePolicy, LAB_TRACE_EXTRA_TOOLS,
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
pub enum CommandJsonFamily {
    Quality,
    Workspace,
    Ops,
    RawOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutputContractKind {
    JsonEnvelope,
    RawOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDescriptor {
    pub response_mode: CommandResponseMode,
    pub output_file_mode: CommandOutputFileMode,
    pub json_family: CommandJsonFamily,
    pub supports_lab_runner: bool,
    pub lab_runner_unsupported_reason: Option<&'static str>,
    pub lab_offload_mutation_flag: Option<&'static str>,
    pub output_contract: CommandOutputContractKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicOutputVariantContract {
    pub command: &'static str,
    pub variant: &'static str,
    pub discriminator_field: Option<&'static str>,
    pub discriminator_value: Option<&'static str>,
    pub golden_fixture: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandResponsePlan {
    pub stdout: CommandStdoutMode,
    pub output_file: CommandOutputFileMode,
}

impl Commands {
    pub fn descriptor(&self, has_output_file: bool) -> CommandDescriptor {
        let output_file_mode = if !has_output_file {
            CommandOutputFileMode::None
        } else {
            match self {
                Commands::Review(_) => CommandOutputFileMode::ReviewStableArtifact,
                Commands::Trace(args) if args.json_summary => {
                    CommandOutputFileMode::TraceJsonSummaryArtifact
                }
                _ => CommandOutputFileMode::GenericEnvelope,
            }
        };

        let mut descriptor = match self {
            Commands::Ssh(args) if args.subcommand.is_none() && args.command.is_empty() => {
                raw_ops_descriptor(CommandRawOutputMode::InteractivePassthrough, output_file_mode)
            }
            Commands::Logs(args) if logs::is_interactive(args) => {
                raw_ops_descriptor(CommandRawOutputMode::InteractivePassthrough, output_file_mode)
            }
            Commands::File(args) if file::is_raw_read(args) => {
                raw_ops_descriptor(CommandRawOutputMode::PlainText, output_file_mode)
            }
            Commands::Docs(args) => workspace_descriptor(
                if crate::commands::docs::is_json_mode(args) {
                    CommandResponseMode::Json
                } else {
                    CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
                },
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Changelog(args) if changelog::is_show_markdown(args) => workspace_descriptor(
                CommandResponseMode::Raw(CommandRawOutputMode::Markdown),
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Review(args) => CommandDescriptor {
                response_mode: markdown_or_json_response(review::is_markdown_mode(args)),
                output_file_mode,
                json_family: CommandJsonFamily::Quality,
                supports_lab_runner: false,
                lab_runner_unsupported_reason: None,
                lab_offload_mutation_flag: None,
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Trace(args) => CommandDescriptor {
                response_mode: markdown_or_json_response(trace::is_markdown_mode(args)),
                output_file_mode,
                json_family: CommandJsonFamily::Quality,
                supports_lab_runner: true,
                lab_runner_unsupported_reason: None,
                lab_offload_mutation_flag: args.keep_overlay.then_some("--keep-overlay"),
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Runs(args) => workspace_descriptor(
                if !has_output_file && args.is_markdown_mode() {
                    CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
                } else {
                    CommandResponseMode::Json
                },
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Report(args) if report::is_markdown_mode(args) => workspace_descriptor(
                CommandResponseMode::Raw(CommandRawOutputMode::Markdown),
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::List => CommandDescriptor {
                response_mode: CommandResponseMode::Raw(CommandRawOutputMode::Markdown),
                output_file_mode,
                json_family: CommandJsonFamily::RawOnly,
                supports_lab_runner: false,
                lab_runner_unsupported_reason: None,
                lab_offload_mutation_flag: None,
                output_contract: CommandOutputContractKind::RawOnly,
            },
            Commands::Test(args) => quality_json_descriptor(
                output_file_mode,
                true,
                args.write.then_some("--write"),
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Bench(args) => quality_json_descriptor(
                output_file_mode,
                args.is_lab_offload_command(),
                args.lab_offload_writes_local_state()
                    .then_some("--baseline/--ratchet"),
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Lint(args) => quality_json_descriptor(
                output_file_mode,
                true,
                args.fix.then_some("--fix"),
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Audit(_) | Commands::Observe(_) => quality_json_descriptor(
                output_file_mode,
                matches!(self, Commands::Audit(_)),
                None,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::AuditBaseline(_) => quality_json_descriptor(
                output_file_mode,
                false,
                None,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Refactor(args) => CommandDescriptor {
                response_mode: CommandResponseMode::Json,
                output_file_mode,
                json_family: CommandJsonFamily::Workspace,
                supports_lab_runner: args.is_hot_resource_command(),
                lab_runner_unsupported_reason: None,
                lab_offload_mutation_flag: args
                    .lab_offload_writes_local_state()
                    .then_some("--write/--commit"),
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Refs(_) => workspace_descriptor(
                CommandResponseMode::Json,
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::AgentTask(_)
            | Commands::Project(_)
            | Commands::Component(_)
            | Commands::Config(_)
            | Commands::Extension(_)
            | Commands::Changelog(_)
            | Commands::Cleanup(_)
            | Commands::Version(_)
            | Commands::Build(_)
            | Commands::Changes(_)
            | Commands::Release(_)
            | Commands::Report(_)
            | Commands::Lab(_)
            | Commands::Runner(_)
            | Commands::Runtime(_)
            | Commands::Worktree(_)
            | Commands::Tunnel(_)
            | Commands::Stack(_)
            | Commands::Undo(_) => CommandDescriptor {
                response_mode: CommandResponseMode::Json,
                output_file_mode,
                json_family: CommandJsonFamily::Workspace,
                supports_lab_runner: false,
                lab_runner_unsupported_reason: None,
                lab_offload_mutation_flag: None,
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Rig(args) => CommandDescriptor {
                response_mode: CommandResponseMode::Json,
                output_file_mode,
                json_family: CommandJsonFamily::Workspace,
                supports_lab_runner: false,
                lab_runner_unsupported_reason: args.is_hot_resource_command().then_some(
                    "`rig up` stays local because rig pipelines manage local services, leases, ports, and declared filesystem paths that the current single-workspace Lab snapshot cannot safely mirror.",
                ),
                lab_offload_mutation_flag: None,
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Status(_)
            | Commands::Ci(_)
            | Commands::Server(_)
            | Commands::Db(_)
            | Commands::Deps(_)
            | Commands::Doctor(_)
            | Commands::File(_)
            | Commands::Logs(_)
            | Commands::Deploy(_)
            | Commands::Daemon(_)
            | Commands::Git(_)
            | Commands::Issues(_)
            | Commands::SelfCmd(_)
            | Commands::Auth(_)
            | Commands::Api(_)
            | Commands::Http(_)
            | Commands::Upgrade(_)
            | Commands::Ssh(_) => ops_json_descriptor(output_file_mode, None),
            Commands::Fleet(args) => ops_json_descriptor(
                output_file_mode,
                args.is_hot_resource_command().then_some(
                    "`fleet exec` stays local because it depends on local fleet, project, and server configuration before opening SSH sessions to each project; runner-side config parity is not guaranteed.",
                ),
            ),
            Commands::Triage(_) => ops_json_descriptor(output_file_mode, None),
        };

        lab_contract::apply_lab_contract_to_descriptor(&mut descriptor, self.lab_contract());
        descriptor
    }

    pub fn response_plan(&self, has_output_file: bool) -> CommandResponsePlan {
        let descriptor = self.descriptor(has_output_file);

        CommandResponsePlan {
            stdout: match descriptor.response_mode {
                CommandResponseMode::Json => CommandStdoutMode::JsonEnvelope,
                CommandResponseMode::Raw(raw_mode) => CommandStdoutMode::Raw(raw_mode),
            },
            output_file: descriptor.output_file_mode,
        }
    }

    pub fn supports_lab_runner(&self) -> bool {
        self.lab_contract()
            .is_some_and(|contract| matches!(contract.portability, LabCommandPortability::Portable))
    }

    pub fn lab_runner_unsupported_reason(&self) -> Option<&'static str> {
        self.lab_contract()
            .and_then(|contract| match contract.portability {
                LabCommandPortability::Portable => None,
                LabCommandPortability::LocalOnly(reason) => Some(reason),
            })
    }

    pub fn lab_offload_mutation_flag(&self) -> Option<&'static str> {
        self.lab_contract()
            .and_then(|contract| contract.mutation_flag)
    }

    pub fn response_mode(&self, has_output_file: bool) -> CommandResponseMode {
        self.descriptor(has_output_file).response_mode
    }

    pub fn output_file_mode(&self, has_output_file: bool) -> CommandOutputFileMode {
        self.descriptor(has_output_file).output_file_mode
    }

    pub fn consumes_output_file_as_command_arg(&self) -> bool {
        matches!(self, Commands::Runs(args) if args.is_artifact_get())
    }
}

fn raw_ops_descriptor(
    raw_mode: CommandRawOutputMode,
    output_file_mode: CommandOutputFileMode,
) -> CommandDescriptor {
    CommandDescriptor {
        response_mode: CommandResponseMode::Raw(raw_mode),
        output_file_mode,
        json_family: CommandJsonFamily::Ops,
        supports_lab_runner: false,
        lab_runner_unsupported_reason: None,
        lab_offload_mutation_flag: None,
        output_contract: CommandOutputContractKind::JsonEnvelope,
    }
}

fn workspace_descriptor(
    response_mode: CommandResponseMode,
    output_file_mode: CommandOutputFileMode,
    output_contract: CommandOutputContractKind,
) -> CommandDescriptor {
    CommandDescriptor {
        response_mode,
        output_file_mode,
        json_family: CommandJsonFamily::Workspace,
        supports_lab_runner: false,
        lab_runner_unsupported_reason: None,
        lab_offload_mutation_flag: None,
        output_contract,
    }
}

fn markdown_or_json_response(markdown: bool) -> CommandResponseMode {
    if markdown {
        CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
    } else {
        CommandResponseMode::Json
    }
}

fn quality_json_descriptor(
    output_file_mode: CommandOutputFileMode,
    supports_lab_runner: bool,
    lab_offload_mutation_flag: Option<&'static str>,
    output_contract: CommandOutputContractKind,
) -> CommandDescriptor {
    CommandDescriptor {
        response_mode: CommandResponseMode::Json,
        output_file_mode,
        json_family: CommandJsonFamily::Quality,
        supports_lab_runner,
        lab_runner_unsupported_reason: None,
        lab_offload_mutation_flag,
        output_contract,
    }
}

fn ops_json_descriptor(
    output_file_mode: CommandOutputFileMode,
    lab_runner_unsupported_reason: Option<&'static str>,
) -> CommandDescriptor {
    CommandDescriptor {
        response_mode: CommandResponseMode::Json,
        output_file_mode,
        json_family: CommandJsonFamily::Ops,
        supports_lab_runner: false,
        lab_runner_unsupported_reason,
        lab_offload_mutation_flag: None,
        output_contract: CommandOutputContractKind::JsonEnvelope,
    }
}

pub const PUBLIC_OUTPUT_VARIANT_CONTRACTS: &[PublicOutputVariantContract] = &[
    PublicOutputVariantContract {
        command: "bench",
        variant: "single",
        discriminator_field: Some("variant"),
        discriminator_value: Some("single"),
        golden_fixture: Some("bench_contract.json"),
    },
    PublicOutputVariantContract {
        command: "bench",
        variant: "comparison",
        discriminator_field: Some("variant"),
        discriminator_value: Some("comparison"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "bench",
        variant: "comparison_summary",
        discriminator_field: Some("variant"),
        discriminator_value: Some("comparison_summary"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "bench",
        variant: "list",
        discriminator_field: Some("variant"),
        discriminator_value: Some("list"),
        golden_fixture: Some("bench_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "list",
        discriminator_field: Some("variant"),
        discriminator_value: Some("list"),
        golden_fixture: Some("runs_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "show",
        discriminator_field: Some("variant"),
        discriminator_value: Some("show"),
        golden_fixture: Some("runs_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "artifacts",
        discriminator_field: Some("variant"),
        discriminator_value: Some("artifacts"),
        golden_fixture: Some("runs_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "query",
        discriminator_field: Some("variant"),
        discriminator_value: Some("query"),
        golden_fixture: Some("runs_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "drift",
        discriminator_field: Some("variant"),
        discriminator_value: Some("drift"),
        golden_fixture: Some("runs_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "loop_sync",
        discriminator_field: Some("variant"),
        discriminator_value: Some("loop_sync"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "list",
        discriminator_field: Some("variant"),
        discriminator_value: Some("list"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "show",
        discriminator_field: Some("variant"),
        discriminator_value: Some("show"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "up",
        discriminator_field: Some("variant"),
        discriminator_value: Some("up"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "check",
        discriminator_field: Some("variant"),
        discriminator_value: Some("check"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "down",
        discriminator_field: Some("variant"),
        discriminator_value: Some("down"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "repair",
        discriminator_field: Some("variant"),
        discriminator_value: Some("repair"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "sync",
        discriminator_field: Some("variant"),
        discriminator_value: Some("sync"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "status",
        discriminator_field: Some("variant"),
        discriminator_value: Some("status"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "install",
        discriminator_field: Some("variant"),
        discriminator_value: Some("install"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "update",
        discriminator_field: Some("variant"),
        discriminator_value: Some("update"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "sources",
        discriminator_field: Some("variant"),
        discriminator_value: Some("sources"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "app",
        discriminator_field: Some("variant"),
        discriminator_value: Some("app"),
        golden_fixture: None,
    },
];

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
    fn test_command_descriptor_drives_behavioral_routing() {
        let bench = parsed_command(&["homeboy", "bench"]);
        let bench_descriptor = bench.descriptor(false);
        assert_eq!(bench_descriptor.json_family, CommandJsonFamily::Quality);
        assert!(bench_descriptor.supports_lab_runner);
        assert_eq!(
            bench_descriptor.output_contract,
            CommandOutputContractKind::JsonEnvelope
        );

        let runs = parsed_command(&["homeboy", "runs", "list"]);
        let runs_descriptor = runs.descriptor(false);
        assert_eq!(runs_descriptor.json_family, CommandJsonFamily::Workspace);
        assert_eq!(runs_descriptor.response_mode, CommandResponseMode::Json);
        assert_eq!(
            runs_descriptor.output_contract,
            CommandOutputContractKind::JsonEnvelope
        );

        let list_descriptor = Commands::List.descriptor(false);
        assert_eq!(list_descriptor.json_family, CommandJsonFamily::RawOnly);
        assert_eq!(
            list_descriptor.response_mode,
            CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
        );
    }

    #[test]
    fn rig_check_supports_lab_runner_but_rig_up_stays_local_only() {
        let rig_check = parsed_command(&["homeboy", "rig", "check", "studio"]);
        let rig_check_descriptor = rig_check.descriptor(false);
        assert!(rig_check_descriptor.supports_lab_runner);
        assert!(rig_check_descriptor.lab_runner_unsupported_reason.is_none());

        let rig_up = parsed_command(&["homeboy", "rig", "up", "studio"]);
        let rig_up_descriptor = rig_up.descriptor(false);
        assert!(!rig_up_descriptor.supports_lab_runner);
        assert!(rig_up_descriptor
            .lab_runner_unsupported_reason
            .is_some_and(|reason| reason.contains("rig up")));
    }

    #[test]
    fn public_variant_contracts_have_discriminators_or_fixtures() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixtures = root.join("tests/fixtures/golden_json_contracts");

        for contract in PUBLIC_OUTPUT_VARIANT_CONTRACTS {
            assert!(
                contract.discriminator_field.is_some() || contract.golden_fixture.is_some(),
                "{}.{} needs a discriminator or golden fixture",
                contract.command,
                contract.variant
            );

            if let Some(fixture) = contract.golden_fixture {
                assert!(
                    fixtures.join(fixture).exists(),
                    "{}.{} references missing fixture {fixture}",
                    contract.command,
                    contract.variant
                );
            }
        }
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
    fn artifact_get_output_flag_is_command_payload_destination() {
        assert!(parsed_command(&[
            "homeboy",
            "runs",
            "artifact",
            "get",
            "run-1",
            "artifact-1",
            "-o",
            "artifact.bin",
        ])
        .consumes_output_file_as_command_arg());

        assert!(!parsed_command(&["homeboy", "status"]).consumes_output_file_as_command_arg());
    }

    #[test]
    fn test_supports_lab_runner() {
        assert!(parsed_command(&["homeboy", "lint"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "test"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "audit"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "refactor", "--from", "audit"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "refactor", "--all"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "bench"]).supports_lab_runner());
        assert!(parsed_command(&[
            "homeboy",
            "bench",
            "matrix",
            "--setting-matrix",
            "clients=10,100"
        ])
        .supports_lab_runner());
        assert!(parsed_command(&["homeboy", "bench", "history", "homeboy"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "trace"]).supports_lab_runner());
        assert!(
            parsed_command(&["homeboy", "agent-task", "dispatch", "--prompt", "cook"])
                .supports_lab_runner()
        );
        assert!(
            parsed_command(&["homeboy", "agent-task", "run-plan", "--plan", "@plan.json"])
                .supports_lab_runner()
        );
        assert!(parsed_command(&[
            "homeboy",
            "agent-task",
            "loop",
            "--to-worktree",
            "homeboy@smoke",
            "--verify",
            "true",
            "--prompt",
            "cook"
        ])
        .supports_lab_runner());
        assert!(!parsed_command(&[
            "homeboy", "refactor", "rename", "--from", "old", "--to", "new",
        ])
        .supports_lab_runner());
        assert!(!parsed_command(&["homeboy", "rig", "up", "studio"]).supports_lab_runner());
        assert!(
            !parsed_command(&["homeboy", "fleet", "exec", "prod", "wp", "plugin", "list"])
                .supports_lab_runner()
        );
        assert!(!parsed_command(&["homeboy", "status"]).supports_lab_runner());
        assert!(!parsed_command(&["homeboy", "bench", "list"]).supports_lab_runner());
        assert!(
            !parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"])
                .supports_lab_runner()
        );
        assert!(
            !parsed_command(&["homeboy", "test", "--changed-since", "origin/main"])
                .supports_lab_runner()
        );

        let cli = parsed_cli(&["homeboy", "lint", "--runner", "lab-a"]);
        assert_eq!(cli.runner.as_deref(), Some("lab-a"));
        assert!(cli.command.supports_lab_runner());

        let cli = parsed_cli(&[
            "homeboy",
            "trace",
            "--runner",
            "homeboy-lab",
            "--allow-local-fallback",
        ]);
        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert!(cli.allow_local_fallback);

        let cli = parsed_cli(&["homeboy", "--force-hot", "--allow-local-hot", "bench"]);
        assert!(cli.force_hot);
        assert!(cli.allow_local_hot);
        assert!(cli.command.supports_lab_runner());
    }

    #[test]
    fn test_lab_command_contracts_cover_hot_commands() {
        let supported = [
            (parsed_command(&["homeboy", "lint"]), "lint"),
            (parsed_command(&["homeboy", "test"]), "test"),
            (parsed_command(&["homeboy", "audit"]), "audit"),
            (parsed_command(&["homeboy", "bench"]), "bench"),
            (
                parsed_command(&[
                    "homeboy",
                    "bench",
                    "matrix",
                    "--setting-matrix",
                    "clients=10,100",
                ]),
                "bench",
            ),
            (
                parsed_command(&["homeboy", "bench", "history", "homeboy"]),
                "bench",
            ),
            (parsed_command(&["homeboy", "trace"]), "trace"),
            (
                parsed_command(&["homeboy", "refactor", "--from", "audit"]),
                "refactor",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "dispatch", "--prompt", "cook"]),
                "agent-task dispatch/cook/loop/run-plan",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "cook", "--prompt", "cook"]),
                "agent-task dispatch/cook/loop/run-plan",
            ),
            (
                parsed_command(&[
                    "homeboy",
                    "agent-task",
                    "loop",
                    "--to-worktree",
                    "homeboy@smoke",
                    "--verify",
                    "true",
                    "--prompt",
                    "cook",
                ]),
                "agent-task dispatch/cook/loop/run-plan",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "run-plan", "--plan", "@plan.json"]),
                "agent-task dispatch/cook/loop/run-plan",
            ),
        ];

        for (command, label) in supported {
            let contract = command.lab_contract().expect("hot contract");
            assert_eq!(contract.hot_label, label);
            assert_eq!(contract.portability, LabCommandPortability::Portable);
            assert_eq!(contract.source_path_mode, LabSourcePathMode::CwdOrPathFlag);
            assert_eq!(
                contract.workspace_mode_policy,
                LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot
            );
        }

        let trace = parsed_command(&["homeboy", "trace"])
            .lab_contract()
            .expect("trace contract");
        assert_eq!(trace.extra_required_tools, LAB_TRACE_EXTRA_TOOLS);
        assert!(!trace.requires_extension_parity);
        assert!(!trace.infer_source_path_tools);
        assert_eq!(
            trace.workspace_mode_policy,
            LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot
        );

        let trace_compare_refs = parsed_command(&[
            "homeboy",
            "trace",
            "compare",
            "woocommerce-gateway-stripe",
            "ece-product-page-waterfall",
            "--baseline-target",
            "origin/develop",
            "--candidate",
            "32f68bb07ac0efa1d754f78e2adc8de115ddca6f",
        ])
        .lab_contract()
        .expect("trace compare contract");
        assert_eq!(
            trace_compare_refs.workspace_mode_policy,
            LabWorkspaceModePolicy::Git
        );

        let lint = parsed_command(&["homeboy", "lint"])
            .lab_contract()
            .expect("lint contract");
        assert!(lint.requires_extension_parity);
        assert!(lint.infer_source_path_tools);

        let rig = parsed_command(&["homeboy", "rig", "up", "studio"])
            .lab_contract()
            .expect("rig up contract");
        assert_eq!(rig.hot_label, "rig up");
        assert!(matches!(
            rig.portability,
            LabCommandPortability::LocalOnly(reason) if reason.contains("single-workspace Lab snapshot")
        ));

        let fleet = parsed_command(&["homeboy", "fleet", "exec", "prod", "wp", "plugin", "list"])
            .lab_contract()
            .expect("fleet exec contract");
        assert_eq!(fleet.hot_label, "fleet exec");
        assert!(matches!(
            fleet.portability,
            LabCommandPortability::LocalOnly(reason) if reason.contains("config parity")
        ));

        for args in [
            ["homeboy", "audit", "--changed-since", "origin/main"].as_slice(),
            ["homeboy", "lint", "--changed-since", "origin/main"].as_slice(),
            ["homeboy", "lint", "--changed-only"].as_slice(),
            ["homeboy", "test", "--changed-since", "origin/main"].as_slice(),
        ] {
            let contract = parsed_command(args)
                .lab_contract()
                .expect("scoped hot command should have a Lab plan contract");
            assert!(matches!(
                contract.portability,
                LabCommandPortability::LocalOnly(_)
            ));
        }

        assert!(parsed_command(&["homeboy", "status"])
            .lab_contract()
            .is_none());
        assert!(parsed_command(&["homeboy", "bench", "list"])
            .lab_contract()
            .is_none());
        assert!(parsed_command(&["homeboy", "audit", "--conventions"])
            .lab_contract()
            .is_none());
        assert!(
            parsed_command(&["homeboy", "lint", "--file", "src/main.rs"])
                .lab_contract()
                .is_none()
        );
    }

    #[test]
    fn test_lab_runner_unsupported_hot_command_reasons() {
        assert!(parsed_command(&["homeboy", "rig", "up", "studio"])
            .lab_runner_unsupported_reason()
            .expect("rig up reason")
            .contains("single-workspace Lab snapshot"));
        assert!(
            parsed_command(&["homeboy", "fleet", "exec", "prod", "wp", "plugin", "list"])
                .lab_runner_unsupported_reason()
                .expect("fleet exec reason")
                .contains("config parity")
        );
        assert!(
            parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"])
                .lab_runner_unsupported_reason()
                .expect("changed-scope lint reason")
                .contains("Changed-scope lint runs stay local")
        );
        assert!(
            parsed_command(&["homeboy", "test", "--changed-since", "origin/main"])
                .lab_runner_unsupported_reason()
                .expect("changed-since test reason")
                .contains("test --changed-since")
        );
        assert!(parsed_command(&["homeboy", "status"])
            .lab_runner_unsupported_reason()
            .is_none());
    }

    #[test]
    fn test_lab_runner_flag_is_visible_in_help() {
        let root_help = Cli::command()
            .try_get_matches_from(["homeboy", "--help"])
            .expect_err("help exits")
            .to_string();
        assert!(root_help.contains("--runner"));

        for args in [
            ["homeboy", "rig", "check", "--help"].as_slice(),
            ["homeboy", "build", "--help"].as_slice(),
            ["homeboy", "bench", "list", "--help"].as_slice(),
        ] {
            let help = Cli::command()
                .try_get_matches_from(args)
                .expect_err("help exits")
                .to_string();
            assert!(help.contains("--runner"), "{args:?} help omitted --runner");
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
            parsed_command(&["homeboy", "refactor", "--from", "audit", "--write"])
                .lab_offload_mutation_flag(),
            Some("--write/--commit")
        );
        assert_eq!(
            parsed_command(&["homeboy", "audit"]).lab_offload_mutation_flag(),
            None
        );
        assert_eq!(
            parsed_command(&["homeboy", "audit", "--baseline"]).lab_offload_mutation_flag(),
            Some("--baseline/--ratchet")
        );
        assert_eq!(
            parsed_command(&["homeboy", "audit", "--ratchet"]).lab_offload_mutation_flag(),
            Some("--baseline/--ratchet")
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
