use clap::{ArgMatches, Args, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::commands::{
    activity, agent_task, api, bench, cleanup, component, config, contract, daemon, db,
    deferred_workload, deploy, extension, file, fleet, fuzz, git, harvest, logs, project, refactor,
    release, review, rig, runner, runs, runtime, schedule, self_cmd, server, source, ssh, stack,
    status, topology, trace, tunnel, upgrade, worktree,
};

mod argument_provenance;
pub use argument_provenance::{
    ArgumentSource, ArgumentSourcePolicyError, ArgumentSourceViolation, CommandArgumentProvenance,
    CompiledCommand, TrackerCookArgumentAdapter,
};

const VERSION: &str = homeboy_product_identity::product_version();
const DEFAULT_COMMAND_SURFACE_DEPTH: usize = 8;

// Placement lives in the below-core `homeboy-lab-runner-contract` crate so both
// `core` routing and runner selection can use it without depending on the full
// CLI definition. Re-exported here to keep existing `cli_surface::Placement`
// call sites working.
pub use homeboy_lab_runner_contract::Placement;

#[derive(Parser)]
#[command(name = "homeboy")]
#[command(version = VERSION)]
#[command(about = "Headless automation for agentic software engineering workflows")]
pub struct Cli {
    /// Write structured JSON output to a file path (in addition to stdout).
    /// Bare format names like `json` are rejected; use `./output.json`.
    #[arg(long, global = true, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Installed notification transport for this run. Pair with
    /// `--notification-route`; the route is opaque, non-secret data owned by
    /// that transport.
    #[arg(
        long,
        global = true,
        requires = "notification_route",
        value_name = "TRANSPORT"
    )]
    pub notification_transport: Option<String>,

    /// Opaque, non-secret destination for `--notification-transport`.
    #[arg(
        long,
        global = true,
        requires = "notification_transport",
        value_name = "ROUTE"
    )]
    pub notification_route: Option<String>,

    /// Select where eligible work executes. `auto` (default) follows command
    /// policy; `lab` selects an eligible ready runner; `local` is an explicit
    /// authorized override. Use `--runner <id>` instead to pin one runner.
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t = Placement::Auto,
        conflicts_with = "runner"
    )]
    pub placement: Placement,

    /// Submit to Lab and return after durable controller handoff. Omit it to
    /// keep observing the remote lifecycle, which remains the default.
    #[arg(long, global = true)]
    pub detach_after_handoff: bool,

    /// Directory where persisted run artifacts are copied.
    /// Overrides HOMEBOY_ARTIFACT_ROOT and global config /artifact_root.
    #[arg(long, global = true, value_name = "DIR")]
    pub artifact_root: Option<PathBuf>,

    /// Pin portable work to a connected Lab runner. This implies Lab placement;
    /// use `--placement <policy>` instead to select placement without pinning.
    #[arg(
        long,
        global = true,
        value_name = "RUNNER_ID",
        conflicts_with = "placement"
    )]
    pub runner: Option<String>,

    /// Permit Lab git workspace materialization to overwrite a dirty runner-side checkout.
    #[arg(long, global = true)]
    pub allow_dirty_lab_workspace: bool,

    /// Skip post-materialization dependency hydration for Lab offloads.
    /// When set, Homeboy does not run the detected provider install (e.g.
    /// `composer install`, `npm ci`) in the materialized runner workspace before
    /// the command starts.
    #[arg(long, global = true)]
    pub skip_deps_hydration: bool,

    /// Preserve a failed Lab workspace for bounded TTL-based inspection.
    #[arg(long, global = true)]
    pub preserve_workspace_on_failure: bool,

    /// Add a job-scoped environment variable to a Lab offload without mutating runner config.
    #[arg(long, global = true, value_name = "KEY=VALUE")]
    pub runner_env: Vec<String>,

    /// Reference a runner-owned secret environment variable for a Lab offload.
    /// The runner resolves this identity; Homeboy never accepts its value here.
    #[arg(long, global = true, value_name = "NAME")]
    pub runner_secret_env: Vec<String>,

    /// Add job-scoped Lab offload environment from a JSON object without mutating runner config.
    #[arg(long, global = true, value_name = "JSON")]
    pub lab_env_json: Option<String>,

    /// Override the selected runner workspace root for this Lab offload only.
    #[arg(long, global = true, value_name = "DIR")]
    pub runner_workspace_root: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    /// Builds the user-facing command tree with Lab options shown only where
    /// the Lab portability contract can honor them.
    pub(crate) fn command_with_scoped_lab_args() -> Command {
        crate::command_contract::scope_lab_cli_arguments(Self::command())
    }

    pub(crate) fn from_registered_arg_matches(
        matches: &ArgMatches,
    ) -> Result<(Self, &'static crate::command_contract::CommandSpec), clap::Error> {
        let cli = Self::from_arg_matches(matches)?;
        let spec = matches
            .subcommand_name()
            .and_then(crate::command_contract::registered_command)
            .expect("built-in top-level command should be registered");
        Ok((cli, spec))
    }

    /// Compiles parser matches into typed values plus durable source metadata.
    pub(crate) fn compile_registered_arg_matches(
        matches: &ArgMatches,
    ) -> Result<
        (
            CompiledCommand<Self>,
            &'static crate::command_contract::CommandSpec,
        ),
        clap::Error,
    > {
        let (cli, spec) = Self::from_registered_arg_matches(matches)?;
        Ok((
            CompiledCommand::new(cli, CommandArgumentProvenance::from_matches(matches)),
            spec,
        ))
    }
}

#[derive(Subcommand)]
// Clap derives this public transport enum directly. Boxing variants would
// cascade through typed command dispatch without improving the CLI boundary.
#[allow(clippy::large_enum_variant)]
pub enum Commands {
    /// Unified view of active and recently finished Homeboy work
    Activity(activity::ActivityArgs),
    /// Run generic agent task plans
    #[command(name = "agent-task")]
    AgentTask(agent_task::AgentTaskArgs),
    /// Resume portable workloads deferred until a runner is ready
    #[command(name = "deferred-workload")]
    DeferredWorkload(deferred_workload::DeferredWorkloadArgs),
    /// Manage project configuration
    Project(project::ProjectArgs),
    /// SSH into a project server or configured server
    Ssh(ssh::SshArgs),
    /// Manage SSH server configurations
    Server(server::ServerArgs),
    /// Run performance benchmarks for a component
    Bench(bench::BenchArgs),
    /// Run generic fuzz workloads for a component
    Fuzz(fuzz::FuzzArgs),
    /// Capture black-box behavioral traces for a component
    #[command(
        after_help = "Command-shaped trace modes:\n  homeboy trace list --profiles\n  homeboy trace <component> list\n  homeboy trace compare before.json after.json\n  homeboy trace compare <component> <scenario> --baseline-target <target> --candidate <target>\n  homeboy trace matrix <component> <scenario> --axis name=value1,value2\n  homeboy trace compare-variant --rig <rig-id> --scenario <scenario>\n  homeboy trace compare-bundle --component <component> --scenario <scenario>\n  homeboy trace overlay-locks --stale"
    )]
    Trace(trace::TraceArgs),
    /// Database operations
    Db(db::DbArgs),
    /// Manage component dependencies
    Deps(DepsArgs),
    /// Remote file operations
    File(file::FileArgs),
    /// Manage fleets (groups of projects)
    Fleet(fleet::FleetArgs),
    /// Remote log viewing
    Logs(logs::LogsArgs),
    /// Deploy components to remote server
    Deploy(deploy::DeployArgs),
    /// Recover remote component content into local Git history
    Harvest(harvest::HarvestArgs),
    /// Manage standalone component configurations
    Component(component::ComponentArgs),
    /// Manage global Homeboy configuration
    Config(config::ConfigArgs),
    /// Inspect, export, validate, and normalize Homeboy contract metadata
    Contract(contract::ContractArgs),
    /// Run the local-only HTTP API daemon
    Daemon(daemon::DaemonArgs),
    /// Execute CLI-compatible extensions
    Extension(extension::ExtensionArgs),
    /// Declare homeboy commands that run on a cadence
    Schedule(schedule::ScheduleArgs),
    /// Actionable component status overview
    Status(status::StatusArgs),
    /// Remove declared reconstructable artifacts from managed worktrees
    Cleanup(cleanup::CleanupArgs),
    /// Git operations for components
    Git(git::GitArgs),
    /// Plan release workflows
    Release(release::ReleaseArgs),
    /// Run scoped audit + lint + test umbrella against PR-style changes
    Review(review::ReviewArgs),
    /// Structural refactoring (rename terms across codebase)
    Refactor(refactor::RefactorArgs),
    /// Manage local dev rigs (reproducible multi-component environments)
    Rig(rig::RigArgs),
    /// Manage local and SSH execution runners
    Runner(runner::RunnerArgs),
    /// Inspect sealed source-package admissibility without staging resources
    Source(source::SourceArgs),
    /// Inspect core-owned runtime helper assets
    Runtime(runtime::RuntimeArgs),
    /// Manage component-backed task worktrees
    Worktree(worktree::WorktreeArgs),
    /// Manage private service tunnel declarations
    Tunnel(tunnel::TunnelArgs),
    /// Inspect declared resource relationships without resolving effective configuration
    Topology(topology::TopologyArgs),
    /// Inspect persisted observation runs, artifacts, and typed evidence projections
    Runs(runs::RunsArgs),
    /// Inspect the active Homeboy binary; `self identity` reports its local build identity
    #[command(name = "self")]
    SelfCmd(self_cmd::SelfArgs),
    /// Manage stacks (combined-fixes branches built from base + cherry-picked PRs)
    Stack(stack::StackArgs),
    /// Make API requests to a project
    Api(api::ApiArgs),
    /// Upgrade Homeboy to the latest version
    Upgrade(upgrade::UpgradeArgs),
}

#[derive(Args)]
pub struct DepsArgs {
    #[command(subcommand)]
    command: DepsCommand,
}

#[derive(Subcommand)]
enum DepsCommand {
    /// Inspect dependency constraints and locked package versions
    Status {
        /// Component ID. When omitted, auto-detected from CWD.
        component: Option<String>,

        /// Limit output to one package.
        #[arg(long, value_name = "PACKAGE")]
        package: Option<String>,

        /// Workspace path to operate on directly.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Install a component's dependencies through its detected providers
    Install {
        /// Component ID. When omitted, auto-detected from CWD.
        component: Option<String>,

        /// Workspace path to operate on directly.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Update one package through its dependency provider
    Update {
        /// Package name, e.g. example-org/block-format-bridge.
        package: String,

        /// Component ID. When omitted, auto-detected from CWD.
        component: Option<String>,

        /// New manifest constraint, e.g. ^0.4.
        #[arg(long, value_name = "CONSTRAINT")]
        to: Option<String>,

        /// Workspace path to operate on directly.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,

        /// Skip provider-owned install/lockfile refresh after the manifest update.
        #[arg(long)]
        no_install: bool,

        /// Rebuild the component through its generic build capability after updating.
        #[arg(long)]
        rebuild: bool,
    },
    /// Work with declared downstream dependency stacks
    Stack {
        #[command(subcommand)]
        command: DepsStackCommand,
    },
}

#[derive(Subcommand)]
enum DepsStackCommand {
    /// List declared dependency stack edges
    Status,
    /// Plan downstream updates for an upstream component/repo
    Plan {
        /// Upstream component or repository identifier from dependency_stack[].upstream.
        upstream: String,
    },
    /// Run downstream update commands for an upstream component/repo
    Apply {
        /// Upstream component or repository identifier from dependency_stack[].upstream.
        upstream: String,

        /// New manifest constraint to pass to provider-backed default update steps.
        #[arg(long, value_name = "CONSTRAINT")]
        to: Option<String>,

        /// Print the command plan without running commands.
        #[arg(long)]
        dry_run: bool,

        /// Skip provider-owned install/lockfile refresh after each manifest update.
        #[arg(long)]
        no_install: bool,

        /// Rebuild each downstream component through its generic build capability.
        #[arg(long)]
        rebuild: bool,
    },
}

impl DepsArgs {
    pub(crate) fn run(self) -> homeboy::core::Result<(serde_json::Value, i32)> {
        match self.command {
            DepsCommand::Status {
                component,
                package,
                path,
            } => {
                let output = homeboy::core::deps::status_value(
                    component.as_deref(),
                    path.as_deref(),
                    package.as_deref(),
                )?;
                Ok((output, 0))
            }
            DepsCommand::Install { component, path } => {
                let output =
                    homeboy::core::deps::install_value(component.as_deref(), path.as_deref())?;
                Ok((output, 0))
            }
            DepsCommand::Update {
                package,
                component,
                to,
                path,
                no_install,
                rebuild,
            } => {
                let output = homeboy::core::deps::update_value(
                    component.as_deref(),
                    path.as_deref(),
                    &package,
                    to.as_deref(),
                    !no_install,
                    rebuild,
                )?;
                Ok((output, 0))
            }
            DepsCommand::Stack { command } => match command {
                DepsStackCommand::Status => {
                    let output = homeboy::core::deps::stack_status_value()?;
                    Ok((output, 0))
                }
                DepsStackCommand::Plan { upstream } => {
                    let output = homeboy::core::deps::stack_plan_value(&upstream)?;
                    Ok((output, 0))
                }
                DepsStackCommand::Apply {
                    upstream,
                    to,
                    dry_run,
                    no_install,
                    rebuild,
                } => {
                    let output = homeboy::core::deps::stack_apply_value(
                        &upstream,
                        to.as_deref(),
                        dry_run,
                        !no_install,
                        rebuild,
                    )?;
                    Ok((output, 0))
                }
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSurface {
    pub commands: Vec<CommandSurfaceEntry>,
}

impl CommandSurface {}

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
pub struct CommandSafetyAuditReport {
    pub report_only: bool,
    pub missing_action_metadata: Vec<CommandSafetyAuditFinding>,
}

impl CommandSafetyAuditReport {
    pub fn has_findings(&self) -> bool {
        !self.missing_action_metadata.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSafetyAuditFinding {
    pub path: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSurfaceDoctorReport {
    pub agrees: bool,
    pub source_registry_commands: Vec<String>,
    pub command_provenance: Vec<CommandSurfaceCommandProvenance>,
    pub docs_index_commands: Vec<String>,
    pub help_commands: Vec<String>,
    pub runtime_extension_docs: Vec<String>,
    pub missing_from_docs_index: Vec<String>,
    pub stale_docs_index: Vec<String>,
    pub missing_from_help: Vec<String>,
    pub missing_from_source_registry: Vec<String>,
    pub drift_evidence: Vec<CommandSurfaceDriftEvidence>,
    pub drift_notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSurfaceRegistry {
    Core,
    Descriptor,
    Extension,
    DocsIndex,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSurfaceCommandProvenance {
    pub command: String,
    pub registry: CommandSurfaceRegistry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSurfaceDriftEvidence {
    pub command: String,
    pub drift: String,
    pub registry: CommandSurfaceRegistry,
}

impl CommandSafetyManifest {}

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
    pub risk_exemption: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<ExtensionCommandManifest>,
    pub dangerous_flags: Vec<String>,
    pub subcommands: Vec<CommandSafetyEntry>,
}

impl CommandSafetyEntry {}

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

mod entry_command_impls {
    use super::*;

    impl CommandSurfaceEntry {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicCommandDescriptor {
    pub name: String,
    pub about: String,
    pub docs_path: Option<String>,
    pub extension: Option<ExtensionCommandManifest>,
    pub safety: Option<DynamicCommandSafety>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicCommandSafety {
    pub mutates: bool,
    pub operator: bool,
    pub output_notes: &'static str,
    pub lab_notes: &'static str,
    pub dangerous_flags: Vec<&'static str>,
}

mod dynamic_impls {
    use super::*;

    impl DynamicCommandDescriptor {
        pub(crate) fn installed_extension_command(
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

    impl DynamicCommandSafety {
        pub(super) fn extension_cli_passthrough() -> Self {
            Self {
                mutates: true,
                operator: true,
                output_notes: "extension-provided CLI passthrough; forwarded arguments may mutate the target system",
                lab_notes: "not declared as Lab-routable in the safety manifest",
                dangerous_flags: vec!["passthrough args"],
            }
        }
    }
}

// Command-safety-manifest derivation lives in
// `crate::command_contract::safety_manifest`. Re-export the public
// entry points here so existing call sites keep importing them from
// `crate::cli_surface` unchanged while this module leans toward clap shapes.
mod safety_manifest;
pub(crate) use safety_manifest::command_safety_manifest_from_dynamic;

mod surface {
    use super::*;

    thread_local! {
        static CURRENT_DOCTOR_REPORT: std::cell::RefCell<Option<CommandSurfaceDoctorReport>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(crate) fn current_command_surface() -> CommandSurface {
        command_surface_from(Cli::command())
    }

    pub(crate) fn command_surface_from(command: Command) -> CommandSurface {
        command_surface_from_with_depth(command, DEFAULT_COMMAND_SURFACE_DEPTH)
    }

    fn command_surface_from_with_depth(command: Command, depth: usize) -> CommandSurface {
        CommandSurface {
            commands: visible_subcommands(&command, depth),
        }
    }

    pub(crate) fn current_command_surface_doctor_report() -> CommandSurfaceDoctorReport {
        if let Some(report) = CURRENT_DOCTOR_REPORT.with(|current| current.borrow().clone()) {
            return report;
        }

        let surface = current_command_surface();
        let command_provenance = surface
            .commands
            .iter()
            .filter(|entry| !entry.hidden)
            .map(|entry| CommandSurfaceCommandProvenance {
                command: entry.name.clone(),
                registry: CommandSurfaceRegistry::Core,
            })
            .collect();
        let help_commands = surface
            .commands
            .iter()
            .filter(|entry| !entry.hidden)
            .map(|entry| entry.name.clone())
            .collect();
        let docs_index_commands = documented_command_index_entries(include_str!(
            "../../../../docs/commands/commands-index.md"
        ));

        command_surface_doctor_report(
            command_provenance,
            docs_index_commands,
            help_commands,
            runtime_extension_doc_commands(),
        )
    }

    pub(crate) fn with_command_surface_doctor_report<T>(
        report: Option<CommandSurfaceDoctorReport>,
        run: impl FnOnce() -> T,
    ) -> T {
        struct Reset(Option<CommandSurfaceDoctorReport>);

        impl Drop for Reset {
            fn drop(&mut self) {
                CURRENT_DOCTOR_REPORT.with(|current| {
                    current.replace(self.0.take());
                });
            }
        }

        let previous = CURRENT_DOCTOR_REPORT.with(|current| current.replace(report));
        let _reset = Reset(previous);
        run()
    }

    pub(crate) fn command_surface_doctor_report_from_composed(
        command: Command,
        command_provenance: Vec<CommandSurfaceCommandProvenance>,
    ) -> CommandSurfaceDoctorReport {
        let help_commands = command_surface_from(command)
            .commands
            .into_iter()
            .filter(|entry| !entry.hidden)
            .map(|entry| entry.name)
            .collect();
        let docs_index_commands = documented_command_index_entries(include_str!(
            "../../../../docs/commands/commands-index.md"
        ));

        command_surface_doctor_report(
            command_provenance,
            docs_index_commands,
            help_commands,
            runtime_extension_doc_commands(),
        )
    }

    pub(crate) fn command_surface_doctor_report(
        mut command_provenance: Vec<CommandSurfaceCommandProvenance>,
        docs_index_commands: BTreeSet<String>,
        help_commands: BTreeSet<String>,
        runtime_extension_docs: BTreeSet<String>,
    ) -> CommandSurfaceDoctorReport {
        command_provenance.sort_by(|left, right| left.command.cmp(&right.command));
        let provenance_by_command: BTreeMap<_, _> = command_provenance
            .iter()
            .map(|entry| (entry.command.clone(), entry.registry))
            .collect();
        let source_registry_commands = provenance_by_command.keys().cloned().collect();
        let docs_required_commands = command_provenance
            .iter()
            .filter(|entry| entry.registry != CommandSurfaceRegistry::Extension)
            .map(|entry| entry.command.clone())
            .collect();
        let documented_core_commands: BTreeSet<String> = docs_index_commands
            .difference(&runtime_extension_docs)
            .cloned()
            .collect();

        let missing_from_docs_index =
            sorted_difference(&docs_required_commands, &docs_index_commands);
        let stale_docs_index =
            sorted_difference(&documented_core_commands, &docs_required_commands);
        let missing_from_help = sorted_difference(&source_registry_commands, &help_commands);
        let missing_from_source_registry =
            sorted_difference(&help_commands, &source_registry_commands);

        let mut drift_evidence = Vec::new();
        extend_drift_evidence(
            &mut drift_evidence,
            &missing_from_docs_index,
            "missing_from_docs_index",
            |command| provenance_by_command[command],
        );
        extend_drift_evidence(
            &mut drift_evidence,
            &stale_docs_index,
            "stale_docs_index",
            |_| CommandSurfaceRegistry::DocsIndex,
        );
        extend_drift_evidence(
            &mut drift_evidence,
            &missing_from_help,
            "missing_from_help",
            |command| provenance_by_command[command],
        );
        extend_drift_evidence(
            &mut drift_evidence,
            &missing_from_source_registry,
            "missing_from_source_registry",
            |_| CommandSurfaceRegistry::Help,
        );

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
            command_provenance,
            docs_index_commands: docs_index_commands.into_iter().collect(),
            help_commands: help_commands.into_iter().collect(),
            runtime_extension_docs: runtime_extension_docs.into_iter().collect(),
            missing_from_docs_index,
            stale_docs_index,
            missing_from_help,
            missing_from_source_registry,
            drift_evidence,
            drift_notes,
        }
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
        crate::command_contract::runtime_extension_command_doc_slugs()
            .map(str::to_string)
            .collect()
    }

    fn sorted_difference(left: &BTreeSet<String>, right: &BTreeSet<String>) -> Vec<String> {
        left.difference(right).cloned().collect()
    }

    fn push_drift_note(notes: &mut Vec<String>, commands: &[String], label: &str) {
        if !commands.is_empty() {
            notes.push(format!("{label}: {}", commands.join(", ")));
        }
    }

    fn extend_drift_evidence(
        evidence: &mut Vec<CommandSurfaceDriftEvidence>,
        commands: &[String],
        drift: &str,
        registry: impl Fn(&String) -> CommandSurfaceRegistry,
    ) {
        evidence.extend(commands.iter().map(|command| CommandSurfaceDriftEvidence {
            command: command.clone(),
            drift: drift.to_string(),
            registry: registry(command),
        }));
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
}
pub(crate) use surface::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// Repo-root-relative paths (docs/, README.md) resolve from the workspace
    /// root, not this crate's manifest dir. After homeboy-cli was extracted into
    /// `crates/homeboy-cli`, `CARGO_MANIFEST_DIR` points at the crate, so these
    /// surface tests must climb back to the workspace root.
    fn workspace_root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    fn command_doc(command: &str) -> String {
        let root = workspace_root();
        std::fs::read_to_string(root.join("docs/commands").join(format!("{command}.md")))
            .unwrap_or_else(|error| panic!("failed to read docs for {command}: {error}"))
    }

    fn commands_index() -> String {
        command_doc("commands-index")
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

    fn provenance(
        command: &str,
        registry: CommandSurfaceRegistry,
    ) -> CommandSurfaceCommandProvenance {
        CommandSurfaceCommandProvenance {
            command: command.to_string(),
            registry,
        }
    }

    #[test]
    fn doctor_reports_actual_missing_docs_with_registry_provenance() {
        let report = command_surface_doctor_report(
            vec![provenance("composed", CommandSurfaceRegistry::Descriptor)],
            BTreeSet::new(),
            BTreeSet::from(["composed".to_string()]),
            BTreeSet::new(),
        );

        assert!(!report.agrees);
        assert_eq!(report.missing_from_docs_index, ["composed"]);
        assert_eq!(
            report.drift_evidence,
            [CommandSurfaceDriftEvidence {
                command: "composed".to_string(),
                drift: "missing_from_docs_index".to_string(),
                registry: CommandSurfaceRegistry::Descriptor,
            }]
        );
    }

    #[test]
    fn doctor_reports_actual_stale_docs_with_docs_registry_provenance() {
        let report = command_surface_doctor_report(
            vec![provenance("core", CommandSurfaceRegistry::Core)],
            BTreeSet::from(["core".to_string(), "removed".to_string()]),
            BTreeSet::from(["core".to_string()]),
            BTreeSet::new(),
        );

        assert!(!report.agrees);
        assert_eq!(report.stale_docs_index, ["removed"]);
        assert_eq!(
            report.drift_evidence,
            [CommandSurfaceDriftEvidence {
                command: "removed".to_string(),
                drift: "stale_docs_index".to_string(),
                registry: CommandSurfaceRegistry::DocsIndex,
            }]
        );
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

    #[test]
    fn command_registry_docs_paths_exist_and_are_indexed() {
        let root = workspace_root();
        let index = commands_index();

        for entry in crate::command_contract::COMMAND_SPECS {
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

    /// Every declared safety path must resolve to a real node in the
    /// clap-derived command surface.
    ///
    /// This is the structural guard for the class of bug in #10313: safety was
    /// declared for `review audit baseline refresh`, a spelling that only
    /// exists in pre-parse argv rewriting, while clap exposes
    /// `review audit-baseline refresh`. The declaration could never match, so
    /// the manifest reported a command that mutates persisted baseline data as
    /// non-mutating. A declared path that clap does not expose is always a bug.
    #[test]
    fn command_path_safety_specs_resolve_to_clap_surface_nodes() {
        fn resolves(entries: &[CommandSurfaceEntry], path: &[&str]) -> bool {
            let Some((first, rest)) = path.split_first() else {
                return true;
            };

            entries
                .iter()
                .find(|entry| entry.name == *first)
                .is_some_and(|entry| resolves(&entry.subcommands, rest))
        }

        let surface = current_command_surface();

        for spec in crate::command_contract::COMMAND_SPECS {
            for path_safety in spec.subcommand_safety {
                for declared in path_safety.paths {
                    let path = std::iter::once(spec.name)
                        .chain(declared.split_whitespace())
                        .collect::<Vec<_>>();

                    assert!(
                        resolves(&surface.commands, &path),
                        "command `{}` declares safety metadata for `{}`, which is not a path in the clap command surface",
                        spec.name,
                        path.join(" ")
                    );
                }
            }
        }
    }

    #[test]
    fn generated_quality_remediation_commands_parse() {
        for argv in [
            ["homeboy", "review", "audit", "fixture"].as_slice(),
            ["homeboy", "review", "lint", "fixture"].as_slice(),
            ["homeboy", "review", "test", "fixture"].as_slice(),
            [
                "homeboy", "refactor", "fixture", "--from", "lint", "--write",
            ]
            .as_slice(),
        ] {
            Cli::try_parse_from(argv).unwrap_or_else(|error| {
                panic!("generated quality remediation failed to parse: {argv:?}\n{error}")
            });
        }
    }

    #[test]
    fn generated_cook_continuation_commands_parse_against_the_live_cli() {
        use homeboy_agents::agent_task_service::{cook_continue_command, cook_recovery_command};

        for command in [
            cook_continue_command(None, "cook-1", false, None),
            cook_continue_command(None, "cook-1-attempt-2", false, None),
            cook_continue_command(None, "cook-1-attempt-2", true, None),
            cook_continue_command(None, "cook-1-attempt-2", true, Some("selected patch")),
            cook_continue_command(
                Some("/tmp/controller runtimes/homeboy"),
                "cook-1-attempt-2",
                false,
                None,
            ),
            cook_recovery_command(
                "cook-1-attempt-2",
                &["finalize-pr", "--recover", "cook-1-attempt-2"],
            ),
        ] {
            let argv =
                shlex::split(&command).expect("generated continuation command must be shell-safe");
            Cli::try_parse_from(&argv).unwrap_or_else(|error| {
                panic!("generated Cook continuation command failed to parse: {command}\n{error}")
            });
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
    fn skip_deps_hydration_global_flag_parses() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "trace",
            "--runner",
            "homeboy-lab",
            "--skip-deps-hydration",
        ])
        .expect("global skip deps hydration override should parse");

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert!(cli.skip_deps_hydration);
    }

    #[test]
    fn preserve_workspace_on_failure_global_flag_parses() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "trace",
            "--runner",
            "homeboy-lab",
            "--preserve-workspace-on-failure",
        ])
        .expect("Lab failure-retention profile should parse");

        assert!(cli.preserve_workspace_on_failure);
    }

    #[test]
    fn registered_parse_path_accepts_placement_in_every_global_position() {
        for args in [
            ["homeboy", "--placement=local", "bench", "example"].as_slice(),
            ["homeboy", "bench", "--placement", "lab", "example"].as_slice(),
            ["homeboy", "bench", "--placement", "lab-or-local", "example"].as_slice(),
        ] {
            let matches = Cli::command_with_scoped_lab_args()
                .try_get_matches_from(args)
                .expect("registered command parses placement");
            let (cli, _) = Cli::from_registered_arg_matches(&matches)
                .expect("registered parse path accepts placement");
            assert_ne!(cli.placement, Placement::Auto);
        }
    }

    #[test]
    fn registered_cook_parse_preserves_placement_from_compact_help_position() {
        for args in [
            [
                "homeboy",
                "--placement",
                "lab",
                "agent-task",
                "cook",
                "--to-worktree",
                "repo@slug",
            ]
            .as_slice(),
            [
                "homeboy",
                "agent-task",
                "cook",
                "--placement",
                "lab",
                "--to-worktree",
                "repo@slug",
            ]
            .as_slice(),
        ] {
            let matches = Cli::command_with_scoped_lab_args()
                .try_get_matches_from(args)
                .expect("Cook placement parses from its documented positions");
            let (cli, _) = Cli::from_registered_arg_matches(&matches)
                .expect("registered Cook parse retains placement");

            assert_eq!(cli.placement, Placement::Lab);
        }
    }

    #[test]
    fn placement_exposes_explicit_lab_or_local_fallback() {
        let cli =
            Cli::try_parse_from(["homeboy", "bench", "example", "--placement", "lab-or-local"])
                .expect("lab-or-local placement should parse");

        assert_eq!(cli.placement, Placement::LabOrLocal);
        assert!(cli.placement.allows_local_fallback());
    }

    #[test]
    fn runner_and_placement_are_mutually_exclusive() {
        for placement in ["lab", "local"] {
            let result = Cli::command_with_scoped_lab_args().try_get_matches_from([
                "homeboy",
                "bench",
                "example",
                "--runner",
                "homeboy-lab",
                "--placement",
                placement,
            ]);
            let Err(error) = result else {
                panic!("runner selection and placement policy must not be combined");
            };

            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
            let message = error.to_string();
            assert!(message.contains("--runner"));
            assert!(message.contains("--placement"));
        }
    }

    #[test]
    fn runner_only_preserves_auto_placement() {
        let matches = Cli::command_with_scoped_lab_args()
            .try_get_matches_from(["homeboy", "bench", "example", "--runner", "homeboy-lab"])
            .expect("runner-only selection should parse");
        let (cli, _) = Cli::from_registered_arg_matches(&matches)
            .expect("runner-only selection should deserialize");

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert_eq!(cli.placement, Placement::Auto);
    }

    #[test]
    fn placement_does_not_consume_passthrough_arguments() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "runner",
            "exec",
            "lab",
            "--",
            "tool",
            "--placement",
            "local",
        ])
        .expect("passthrough placement is owned by the child command");
        assert_eq!(cli.placement, Placement::Auto);
    }

    #[test]
    fn lab_flags_are_hidden_from_non_portable_command_help() {
        let help = scoped_help(&["contract", "manifest"]);

        for flag in [
            "--placement",
            "--runner",
            "--detach-after-handoff",
            "--allow-dirty-lab-workspace",
            "--skip-deps-hydration",
            "--preserve-workspace-on-failure",
            "--runner-env",
            "--lab-env-json",
            "--runner-workspace-root",
            "--artifact-root",
        ] {
            assert!(
                !help.contains(flag),
                "contract manifest must not advertise {flag}"
            );
        }
    }

    #[test]
    fn lab_flags_remain_visible_for_portable_command_help() {
        let help = scoped_help(&["bench"]);

        for flag in [
            "--placement",
            "--runner",
            "--detach-after-handoff",
            "--allow-dirty-lab-workspace",
            "--skip-deps-hydration",
            "--preserve-workspace-on-failure",
            "--runner-env",
            "--lab-env-json",
            "--runner-workspace-root",
        ] {
            assert!(help.contains(flag), "bench must advertise {flag}");
        }
        // Both halves of the placement/runner choice stay documented in compact
        // help: `--placement` selects where work executes, and `--runner` pins it.
        // `c05305b09` reworded the placement help without updating this assertion,
        // so it named prose the tree no longer contained.
        assert!(help.contains("Select where eligible work executes"));
        assert!(help.contains("This implies Lab placement"));
    }

    #[test]
    fn compact_cook_help_explains_lab_selection_without_a_runner_pin() {
        let help = scoped_help(&["agent-task", "cook"]);

        assert!(
            help.contains("`lab` selects an eligible ready runner"),
            "{help}"
        );
        assert!(
            help.contains("Use `--runner <id>` instead to pin one runner"),
            "{help}"
        );
    }

    #[test]
    fn portable_review_subcommand_help_keeps_lab_flags() {
        let help = scoped_help(&["review", "lint"]);
        assert!(help.contains("--placement"));
        assert!(help.contains("--runner"));
    }

    #[test]
    fn ssh_help_routes_persisted_artifacts_to_file_commands() {
        let help = scoped_help(&["ssh"]);

        assert!(help.contains("Persisted artifact and file transfers use `homeboy file`"));
        assert!(help.contains("homeboy file copy ./artifact.tar.gz prod:/var/tmp/artifact.tar.gz"));
        assert!(help.contains("homeboy file copy prod:/var/tmp/result.json ./result.json"));
        assert!(help.contains("homeboy file sync ./artifacts prod:/var/tmp/artifacts"));
        assert!(help.contains("For a persisted remote artifact rather than stdout"));
    }

    #[test]
    fn reconciliation_help_names_each_state_plane_and_mutation_boundary() {
        let runs = scoped_help(&["runs", "reconcile"]);
        assert!(runs.contains("observation records"));
        assert!(runs.contains("Runner generations and durable agent-task records have"));

        let runner = scoped_help(&["runner", "reconcile"]);
        assert!(runner.contains("persisted daemon generations"));
        assert!(runner.contains("accepts jobs with no unresolved generation projection"));

        let agent_task = scoped_help(&["agent-task", "reconcile"]);
        assert!(agent_task.contains("preview by default"));
        assert!(agent_task.contains("--apply"));
        assert!(agent_task.contains("authoritative provider state"));

        let self_help = scoped_help(&["self"]);
        assert!(!self_help.contains("upgrade-admission"));
    }

    fn scoped_help(path: &[&str]) -> String {
        let mut command = Cli::command_with_scoped_lab_args();
        for segment in path {
            command = command
                .find_subcommand(segment)
                .unwrap_or_else(|| panic!("missing command path segment `{segment}`"))
                .clone();
        }
        command.render_help().to_string()
    }

    #[test]
    fn docs_cover_high_use_command_surfaces() {
        for command in ["runner", "rig"] {
            assert_docs_cover_subcommands(command);
        }
    }

    #[test]
    fn documented_command_forms_parse() {
        for args in [
            ["homeboy", "refactor", "homeboy", "--all"].as_slice(),
            [
                "homeboy",
                "runs",
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
                "runner",
                "workspace",
                "pull",
                "homeboy-lab",
                "--remote-path",
                "/srv/homeboy/workspace",
                "--include",
                "fixtures/*.fig",
                "--to",
                "fixtures",
                "--dry-run",
            ]
            .as_slice(),
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
                "post",
                "mysite",
                "/wp/v2/posts",
                "--apply",
            ]
            .as_slice(),
            [
                "homeboy",
                "api",
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

#[cfg(test)]
mod global_flag_surface_tests;

pub mod reference_docs;
/// Reject `--runner` combined with an explicit `--placement`.
///
/// Both carry `conflicts_with`, but clap only enforces that when they are
/// supplied *after* the subcommand. Given at the root — `homeboy --placement
/// local --runner lab agent-task cook …` — the conflict silently does not fire,
/// so a contradictory invocation parsed cleanly and resolved to some placement
/// nobody asked for. `--runner` implies Lab, so pairing it with `--placement
/// local` is not a preference, it is two different answers to one question.
///
/// `placement` carries a default, so presence is not enough to detect: this
/// keys on argument provenance and fires only when both were actually typed.
pub(crate) fn reject_conflicting_placement_selection(
    matches: &clap::ArgMatches,
) -> Result<(), clap::Error> {
    use crate::cli_surface::argument_provenance::{ArgumentSource, CommandArgumentProvenance};

    let provenance = CommandArgumentProvenance::from_matches(matches);
    let typed = |name: &str| provenance.source(name) == Some(ArgumentSource::CommandLine);

    if typed("placement") && typed("runner") {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            "the argument '--placement <PLACEMENT>' cannot be used with '--runner <RUNNER_ID>'\n\n             '--runner' already selects Lab placement by pinning a runner. Use one or the other.\n",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod reference_docs_tests;
