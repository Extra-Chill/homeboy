use clap::{ArgMatches, Command};
use std::io::IsTerminal;
use std::process::Command as ProcessCommand;
use std::sync::OnceLock;

use crate::cli_surface::{
    command_safety_manifest_from_dynamic, command_surface_from, Cli, CommandSafetyManifest,
    Commands, DynamicCommandDescriptor, ExtensionCommandArgContract, ExtensionCommandArgsContract,
    ExtensionCommandHealth, ExtensionCommandManifest,
};
use crate::commands;
use crate::commands::cli;
use crate::commands::output_runtime;
use crate::commands::utils::{args, entity_suggest, resource_policy, response as output};
use crate::commands::GlobalArgs;
use crate::core::extension::{
    list_summaries, load_all_extensions, CliConfig,
    ExtensionManifest as InstalledExtensionManifest, ExtensionSummary,
};
use crate::core::upgrade;

const COOK_PINNED_RUNTIME_ENV: &str = "HOMEBOY_COOK_PINNED_CONTROLLER_RUNTIME";

pub struct CliRuntime {
    extension_discovery: OnceLock<ExtensionCliDiscovery>,
}

struct ExtensionCliCommand {
    tool: String,
    project_id: String,
    args: Vec<String>,
}

struct ExtensionCliInfo {
    tool: String,
    descriptor: DynamicCommandDescriptor,
    project_id_help: Option<String>,
    args_help: Option<String>,
    examples: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct ExtensionCliHealth {
    load_error: Option<String>,
    broken_link_ids: Vec<String>,
}

struct ExtensionCliDiscovery {
    info: Vec<ExtensionCliInfo>,
    health: ExtensionCliHealth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupFastPath {
    Help,
    Version,
}

pub fn run_startup_fast_path(args: &[String]) -> Option<std::process::ExitCode> {
    match startup_fast_path(args)? {
        StartupFastPath::Help => {
            let mut cmd = Cli::command_with_scoped_lab_args();
            cmd.print_help().expect("Failed to print help");
            println!();
        }
        StartupFastPath::Version => println!("{}", upgrade::current_build_version()),
    }

    Some(std::process::ExitCode::SUCCESS)
}

pub fn current_augmented_command_safety_manifest() -> CommandSafetyManifest {
    let discovery = collect_extension_cli_info();
    let dynamic_descriptors = discovery
        .info
        .iter()
        .map(|info| info.descriptor.clone())
        .collect::<Vec<_>>();

    command_safety_manifest_from_dynamic(
        command_surface_from(build_augmented_command(&discovery.info, &discovery.health)),
        &dynamic_descriptors,
    )
}

impl CliRuntime {
    pub fn new() -> Self {
        Self {
            extension_discovery: OnceLock::new(),
        }
    }

    pub fn run_from_args(&self, args: Vec<String>) -> std::process::ExitCode {
        // Register the config-level artifact_root resolver before any command runs
        // so paths::artifact_root() can honor global config without paths depending
        // on the defaults layer (breaks the paths <-> defaults dependency cycle).
        crate::core::paths::set_config_artifact_root_resolver(|| {
            crate::core::defaults::load_config().artifact_root
        });
        // Register optional feature crates' config entities with core so their
        // IDs/aliases participate in cross-entity collision detection. Core owns
        // the collision invariant but must not depend on these optional features.
        homeboy_tunnel::register();
        // Register the audit manifest provider so code_audit can read extension
        // manifests (detector rules, test mappings, provided extensions) without
        // depending on the extension layer's loader — the seam that lets audit
        // become its own crate.
        crate::core::extension::audit_manifest_provider::register();
        // Register the fingerprint-script provider so code_audit can fall back to
        // extension fingerprint scripts (for files the core grammar engine can't
        // handle) without depending on the extension script runner.
        crate::core::extension::audit_fingerprint_script_provider::register();
        // Register the audit recorded-artifact provider so the artifact-portability
        // detector can read past runs' artifacts from the observation store without
        // code_audit depending on observation — the last seam before audit becomes
        // its own crate.
        crate::core::observation::audit_artifact_provider::register();
        // Register the audit fixability provider so code_audit can report how
        // fixable its findings are without calling up into the refactor engine's
        // fix planner — the seam that removes the last code_audit->refactor edge.
        crate::core::refactor::audit_fixability_provider::register();
        // Register the audit component provider so code_audit can resolve the
        // component under audit (path, extension ids, audit rules, scope excludes)
        // without depending on the component layer — the last cross-layer seam
        // before audit becomes its own crate.
        crate::core::component::audit_provider::register();
        // Register the runner-evidence provider so observation::runs_service can
        // enrich run/artifact lookups with live runner + daemon evidence without
        // core depending on runner behavior. (Runner is still in-crate today;
        // this registration is the seam that lets it become its own crate.)
        crate::runner::register_runner_evidence_provider();
        // Register the runner job-preparation provider so api_jobs can compute
        // the secret-env plan and validate workload dispatch for remote-runner
        // jobs without core depending on runner behavior.
        crate::runner::register_runner_job_preparation_provider();
        // Register the lab-workspace provenance provider so the agent-task
        // scheduler can verify lab-materialized workspaces without core depending
        // on runner behavior.
        crate::runner::register_lab_workspace_provenance_provider();
        // Register the runner-continuation provider so the agent-task lifecycle
        // can reconcile and resume runs dispatched to a remote runner without
        // core depending on runner behavior.
        crate::runner::register_runner_continuation_provider();
        // Register the runner daemon-exec driver so the daemon's /exec endpoint
        // can prepare and run a runner job as a local child without core
        // depending on runner process-execution behavior.
        crate::runner::register_runner_daemon_exec_driver();
        // Register the runner workspace-root provider so the daemon file API can
        // resolve a runner's configured workspace_root without core depending on
        // the runner config registry.
        crate::runner::register_runner_workspace_root_provider();
        // Register Runner as a config entity so it participates in config
        // id/alias collision detection, mirroring how feature crates register
        // their own entities (moves into the runner crate once extracted).
        crate::runner::register_runner_config_entity();
        // Register the runner-upgrade provider so the core upgrade flow can
        // refresh configured runners without depending on runner behavior.
        crate::runner::register_runner_upgrade();
        // Register the runner-availability provider so the controller action loop
        // can gate execution on a runner's live status.
        crate::runner::register_runner_availability_provider();
        // Register the Lab-offload provider so core's lab_routing can execute an
        // offload without depending on runner behavior.
        crate::runner::register_runner_lab_offload_provider();
        // Register the workspace-snapshot provider so core's hygiene subsystem
        // can materialize an isolated validation-dependency workspace without
        // depending on runner behavior.
        crate::runner::register_workspace_snapshot_provider();
        // Register the agent-task controller pin-reference provider so core's
        // controller-runtime retention report can discover which pinned
        // executables are still referenced by nonterminal durable agent-task
        // records without core depending on the agent-task subsystem. (This is
        // the seam that lets agent-task become its own crate.)
        crate::core::agent_task_lifecycle::controller_pin_reference_provider::register();
        // Register the loop-spec validation provider so core's proof validator
        // can validate a materialized agent-task loop-spec artifact without
        // depending on the agent-task subsystem.
        crate::core::agent_task_controller_service::loop_spec_validation_provider::register();
        // Register the gate-feedback candidate-baseline provider so core's
        // worktree-safety logic can accept a dirty worktree that is a verified
        // agent-task gate-feedback candidate without depending on the agent-task
        // subsystem.
        crate::core::agent_task_candidate_baseline::register();
        // Register the command-label resolver so core::runner can map dispatched
        // argv to a hot-command label without depending on the full CLI parser.
        crate::runner::set_command_label_resolver(|argv| {
            let cli = <crate::cli_surface::Cli as clap::Parser>::try_parse_from(argv).ok()?;
            let route_contract = cli.command.lab_route_contract().ok()??;
            Some(route_contract.command.hot_label.to_string())
        });
        // Register the agent-task dispatch resolver so core::runner can extract a
        // cook dispatch command from argv without depending on the CLI parser.
        crate::runner::set_agent_task_dispatch_resolver(|argv| {
            let cli = <crate::cli_surface::Cli as clap::Parser>::try_parse_from(argv).map_err(
                |error| {
                    crate::core::error::Error::validation_invalid_argument(
                        "agent-task",
                        "failed to parse agent-task arguments while compiling Lab provider policy",
                        Some(error.to_string()),
                        None,
                    )
                },
            )?;
            Ok(match cli.command {
                crate::cli_surface::Commands::AgentTask(agent_task) => match agent_task.command {
                    crate::commands::agent_task::AgentTaskCommand::Cook(cook) => {
                        Some(cook.dispatch.into())
                    }
                    _ => None,
                },
                _ => None,
            })
        });
        // Register the Lab-runner hint provider so core::runner can compose
        // `--runner`/`--placement` unsupported errors from the command-spec table
        // without depending on `command_contract`.
        crate::runner::set_lab_runner_hint_provider(|| {
            let summary = crate::command_contract::lab_runner_support_summary();
            crate::runner::LabRunnerHint {
                hint: summary.hint,
                unsupported_message: summary.unsupported_message,
            }
        });

        if is_top_level_version_request(&args) {
            println!("{}", upgrade::current_build_version());
            return std::process::ExitCode::SUCCESS;
        }

        let normalized = args::normalize(args);
        let matches = self.parse_matches(normalized.clone());
        self.run_matches(matches, normalized)
    }

    fn parse_matches(&self, normalized: Vec<String>) -> ArgMatches {
        match Cli::command_with_scoped_lab_args().try_get_matches_from(normalized.clone()) {
            Ok(matches) => matches,
            Err(static_err) => match self
                .build_augmented_command()
                .try_get_matches_from(normalized)
            {
                Ok(matches) => matches,
                Err(err) => {
                    if let Some(output) =
                        try_augment_clap_error(&err, &self.extension_discovery().health)
                    {
                        eprintln!("{}", output);
                        std::process::exit(2);
                    }

                    if let Some(output) =
                        try_augment_clap_error(&static_err, &self.extension_discovery().health)
                    {
                        eprintln!("{}", output);
                        std::process::exit(2);
                    }

                    err.exit();
                }
            },
        }
    }

    fn run_matches(&self, matches: ArgMatches, normalized: Vec<String>) -> std::process::ExitCode {
        let global = GlobalArgs {};

        // Extract --output early so it's available for all code paths (including
        // extension CLI commands which exit before Cli::from_arg_matches).
        let mut output_file: Option<String> = matches
            .try_get_one::<std::path::PathBuf>("output")
            .ok()
            .flatten()
            .map(|path| path.to_string_lossy().to_string());

        let artifact_root_override = matches
            .try_get_one::<std::path::PathBuf>("artifact_root")
            .ok()
            .flatten()
            .cloned();
        crate::core::set_artifact_root_override(artifact_root_override.clone());

        if let Some(extension_cmd) = self.try_parse_extension_cli_command(&matches) {
            if let Some(path) = output_file.as_deref() {
                if let Some(exit) = output_file_path_exit_code(path) {
                    return exit;
                }
            }

            let cli_args = cli::CliArgs {
                tool: extension_cmd.tool,
                identifier: extension_cmd.project_id,
                args: extension_cmd.args,
            };
            let result = cli::run(cli_args, &global);

            let (json_result, exit_code) = output::map_cmd_result_to_json(result);
            output_runtime::emit_json_result(json_result, output_file.as_deref(), exit_code);
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }

        let (mut cli, command_spec) = match Cli::from_registered_arg_matches(&matches) {
            Ok(parsed) => parsed,
            Err(err) => err.exit(),
        };
        let notification_route = match crate::core::notification_route::from_cli_or_env(
            cli.notification_transport.as_deref(),
            cli.notification_route.as_deref(),
        ) {
            Ok(route) => route,
            Err(err) => {
                output_runtime::emit_json_result(Err(err), output_file.as_deref(), 2);
                return std::process::ExitCode::from(2);
            }
        };
        commands::set_skip_deps_hydration(cli.skip_deps_hydration);
        normalize_runs_runner_options(&mut cli, &normalized);

        if matches!(&cli.command, Commands::Runs(args) if args.is_bundle_export()) {
            output_file = None;
        }

        if cli.command.consumes_output_file_as_command_arg() {
            // This command owns `--output/-o`; it is not the global JSON envelope.
            output_file = None;
        } else if let Some(path) = output_file.as_deref() {
            if let Some(exit) = output_file_path_exit_code(path) {
                return exit;
            }
        }

        match delegate_agent_task_cook_to_pinned_runtime(&cli, &normalized) {
            Ok(Some(exit_code)) => return std::process::ExitCode::from(exit_code_to_u8(exit_code)),
            Ok(None) => {}
            Err(err) => {
                output_runtime::emit_json_result(Err(err), output_file.as_deref(), 2);
                return std::process::ExitCode::from(2);
            }
        }

        match delegate_agent_task_lifecycle_to_pinned_runtime(&cli, &normalized) {
            Ok(Some(exit_code)) => return std::process::ExitCode::from(exit_code_to_u8(exit_code)),
            Ok(None) => {}
            Err(err) => {
                output_runtime::emit_json_result(Err(err), output_file.as_deref(), 2);
                return std::process::ExitCode::from(2);
            }
        }

        // Capture controller pressure once before placement routing. The route
        // and persisted evidence reuse this preflight decision rather than
        // probing the host a second time.
        let managed_runner_placement = resource_policy::is_managed_runner_placement_context();
        if let Some(exit_code) = preflight_hot_command(&cli, output_file.as_deref()) {
            if managed_runner_placement {
                resource_policy::clear_managed_runner_placement_context();
            }
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }

        let route_result =
            crate::commands::route::route_after_parse(&cli, &normalized, output_file.as_deref());
        if managed_runner_placement {
            resource_policy::clear_managed_runner_placement_context();
        }
        match route_result {
            Ok(None) => {}
            Ok(Some(exit_code)) => {
                return std::process::ExitCode::from(exit_code_to_u8(exit_code));
            }
            Err(err) => {
                output_runtime::emit_json_result(Err(err), output_file.as_deref(), 2);
                return std::process::ExitCode::from(exit_code_to_u8(2));
            }
        }

        crate::core::set_artifact_root_override(
            cli.artifact_root.clone().or(artifact_root_override),
        );

        if let Some(exit_code) = run_raw_agent_tool_dispatch(&cli.command) {
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }

        run_startup_update_checks(&cli.command);

        let exit_code = crate::core::notification_route::with_current(notification_route, || {
            #[cfg(test)]
            record_marker_context_before_run_command();
            commands::output_runtime::run_command(
                cli.command,
                command_spec,
                &global,
                output_file.as_deref(),
            )
        });
        std::process::ExitCode::from(exit_code_to_u8(exit_code))
    }

    fn build_augmented_command(&self) -> Command {
        let discovery = self.extension_discovery();
        build_augmented_command(&discovery.info, &discovery.health)
    }

    fn try_parse_extension_cli_command(&self, matches: &ArgMatches) -> Option<ExtensionCliCommand> {
        let (tool, _) = matches.subcommand()?;
        if is_builtin_subcommand(tool) {
            return None;
        }

        try_parse_extension_cli_command(matches, &self.extension_discovery().info)
    }

    fn extension_discovery(&self) -> &ExtensionCliDiscovery {
        self.extension_discovery
            .get_or_init(collect_extension_cli_info)
    }
}

/// A cook has no durable run record until controller admission. Re-exec before
/// routing so every subsequent local phase uses the immutable controller that
/// started the cook rather than a globally replaced executable.
fn delegate_agent_task_cook_to_pinned_runtime(
    cli: &Cli,
    normalized_args: &[String],
) -> homeboy::core::Result<Option<i32>> {
    if !matches!(
        &cli.command,
        Commands::AgentTask(agent_task)
            if matches!(
                &agent_task.command,
                crate::commands::agent_task::AgentTaskCommand::Cook(_)
            )
    ) {
        return Ok(None);
    }

    if let Some(expected) = std::env::var_os(COOK_PINNED_RUNTIME_ENV) {
        let current = std::env::current_exe().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("resolve current controller executable".to_string()),
            )
        })?;
        if current == std::path::PathBuf::from(expected) {
            return Ok(None);
        }
    }

    let pinned = crate::core::agent_tasks::lifecycle::pin_current_controller_runtime()?;
    let status = ProcessCommand::new(&pinned)
        .args(&normalized_args[1..])
        .env(COOK_PINNED_RUNTIME_ENV, &pinned)
        .status()
        .map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(format!(
                    "execute pinned controller runtime {}",
                    pinned.display()
                )),
            )
        })?;
    Ok(Some(status.code().unwrap_or(1)))
}

/// Durable lifecycle mutations remain owned by the runtime that admitted the
/// record. Re-exec before Lab routing so recovery cannot create a replacement
/// handoff under the promoted controller.
fn delegate_agent_task_lifecycle_to_pinned_runtime(
    cli: &Cli,
    normalized_args: &[String],
) -> homeboy::core::Result<Option<i32>> {
    let run_id = match &cli.command {
        Commands::AgentTask(agent_task) => match &agent_task.command {
            crate::commands::agent_task::AgentTaskCommand::Run(args) => Some(&args.run_id),
            crate::commands::agent_task::AgentTaskCommand::Resume(args) => Some(&args.run_id),
            crate::commands::agent_task::AgentTaskCommand::Retry(args) => Some(&args.run_id),
            _ => None,
        },
        _ => None,
    };
    let Some(run_id) = run_id else {
        return Ok(None);
    };
    let Some(pinned) = crate::core::agent_tasks::lifecycle::pinned_runtime_for_mutation(run_id)?
    else {
        return Ok(None);
    };
    let status = ProcessCommand::new(&pinned)
        .args(&normalized_args[1..])
        .status()
        .map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(format!(
                    "execute pinned controller runtime {}",
                    pinned.display()
                )),
            )
        })?;
    Ok(Some(status.code().unwrap_or(1)))
}

fn run_raw_agent_tool_dispatch(command: &Commands) -> Option<i32> {
    let Commands::AgentTask(args) = command else {
        return None;
    };
    let crate::commands::agent_task::AgentTaskCommand::Tool(tool_args) = &args.command else {
        return None;
    };
    match &tool_args.command {
        crate::commands::agent_task::tool::AgentTaskToolCommand::Dispatch(_args) => {
            Some(crate::commands::agent_task::tool::dispatch_raw(
                crate::commands::agent_task::tool::AgentTaskToolDispatchArgs {},
            ))
        }
    }
}

#[cfg(test)]
fn record_marker_context_before_run_command() {
    *marker_context_before_run_command()
        .lock()
        .expect("marker test state") = Some(resource_policy::is_managed_runner_placement_context());
}

#[cfg(test)]
fn marker_context_before_run_command() -> &'static std::sync::Mutex<Option<bool>> {
    static STATE: OnceLock<std::sync::Mutex<Option<bool>>> = OnceLock::new();
    STATE.get_or_init(|| std::sync::Mutex::new(None))
}

fn is_top_level_version_request(args: &[String]) -> bool {
    matches!(args, [_, flag] if flag == "--version" || flag == "-V")
}

fn startup_fast_path(args: &[String]) -> Option<StartupFastPath> {
    match args {
        [_, flag] if flag == "--help" || flag == "-h" => Some(StartupFastPath::Help),
        [_, flag] if flag == "--version" || flag == "-V" => Some(StartupFastPath::Version),
        _ => None,
    }
}

impl Default for CliRuntime {
    fn default() -> Self {
        Self::new()
    }
}

fn collect_extension_cli_info() -> ExtensionCliDiscovery {
    let summaries = list_summaries(None);
    let mut broken_link_ids: Vec<String> = summaries
        .iter()
        .filter(|summary| summary.error.as_deref() == Some("target_missing"))
        .map(|summary| summary.id.clone())
        .collect();
    broken_link_ids.sort();

    let (extensions, load_error) = match load_all_extensions() {
        Ok(extensions) => (extensions, None),
        Err(error) => (Vec::new(), Some(error.message)),
    };

    let info = extensions
        .into_iter()
        .filter_map(|m| {
            let cli = m.cli.clone()?;
            Some({
                let help = cli.help.clone().unwrap_or_default();
                let project_id_help = help.project_id_help.clone();
                let args_help = help.args_help.clone();
                let examples = help.examples.clone();
                let about = format!("Run {} commands via {}", cli.display_name, m.name);
                let extension_manifest = extension_command_manifest(
                    &m,
                    &cli,
                    project_id_help.clone(),
                    args_help.clone(),
                    examples.clone(),
                    &summaries,
                );
                ExtensionCliInfo {
                    descriptor: DynamicCommandDescriptor::installed_extension_command(
                        cli.tool.clone(),
                        about,
                        extension_command_docs_path(&m, &cli.tool),
                        extension_manifest,
                    ),
                    tool: cli.tool,
                    project_id_help,
                    args_help,
                    examples,
                }
            })
        })
        .collect();

    ExtensionCliDiscovery {
        info,
        health: ExtensionCliHealth {
            load_error,
            broken_link_ids,
        },
    }
}

fn extension_command_manifest(
    extension: &InstalledExtensionManifest,
    cli: &CliConfig,
    project_id_help: Option<String>,
    args_help: Option<String>,
    examples: Vec<String>,
    summaries: &[ExtensionSummary],
) -> ExtensionCommandManifest {
    let project_id_help = project_id_help.unwrap_or_else(|| "Project ID".to_string());
    let args_help = args_help.unwrap_or_else(|| "Command arguments".to_string());
    let summary = summaries.iter().find(|summary| summary.id == extension.id);
    let health = summary
        .map(extension_command_health_from_summary)
        .unwrap_or_else(|| ExtensionCommandHealth {
            status: "unknown".to_string(),
            ready: false,
            compatible: false,
            linked: false,
            reason: Some("summary_missing".to_string()),
            detail: Some("Extension loaded, but no extension summary was available".to_string()),
        });

    ExtensionCommandManifest {
        extension_id: extension.id.clone(),
        extension_name: extension.name.clone(),
        extension_version: extension.version.clone(),
        tool_name: cli.tool.clone(),
        display_name: cli.display_name.clone(),
        args_contract: ExtensionCommandArgsContract {
            project_id: ExtensionCommandArgContract {
                name: "project_id".to_string(),
                help: project_id_help,
                required: true,
                multiple: false,
            },
            args: ExtensionCommandArgContract {
                name: "args".to_string(),
                help: args_help,
                required: false,
                multiple: true,
            },
            trailing_var_arg: true,
            allow_hyphen_values: true,
            examples,
        },
        health,
    }
}

fn extension_command_health_from_summary(summary: &ExtensionSummary) -> ExtensionCommandHealth {
    let status = if summary.error.is_some() {
        "error"
    } else if summary.ready && summary.compatible {
        "ready"
    } else if !summary.compatible {
        "incompatible"
    } else {
        "not_ready"
    };

    ExtensionCommandHealth {
        status: status.to_string(),
        ready: summary.ready,
        compatible: summary.compatible,
        linked: summary.linked,
        reason: summary
            .error
            .clone()
            .or_else(|| summary.ready_reason.clone()),
        detail: summary.ready_detail.clone(),
    }
}

fn extension_command_docs_path(
    extension: &InstalledExtensionManifest,
    tool: &str,
) -> Option<String> {
    let docs_path = format!("docs/commands/{tool}.md");
    let extension_path = extension.extension_path.as_ref()?;

    std::path::Path::new(extension_path)
        .join(&docs_path)
        .exists()
        .then_some(docs_path)
}

fn build_augmented_command(
    extension_info: &[ExtensionCliInfo],
    extension_health: &ExtensionCliHealth,
) -> Command {
    let mut cmd = Cli::command_with_scoped_lab_args();

    for info in extension_info {
        let project_id_help = info
            .project_id_help
            .clone()
            .unwrap_or_else(|| "Project ID".to_string());
        let args_help = info
            .args_help
            .clone()
            .unwrap_or_else(|| "Command arguments".to_string());

        let mut subcommand = Command::new(info.descriptor.name.clone())
            .about(info.descriptor.about.clone())
            .arg(
                clap::Arg::new("project_id")
                    .help(project_id_help)
                    .required(true)
                    .index(1),
            )
            .arg(
                clap::Arg::new("args")
                    .help(args_help)
                    .index(2)
                    .num_args(0..)
                    .allow_hyphen_values(true),
            )
            .trailing_var_arg(true);

        if !info.examples.is_empty() {
            let examples_text = format!("Examples:\n  {}", info.examples.join("\n  "));
            subcommand = subcommand.after_help(examples_text);
        }

        cmd = cmd.subcommand(subcommand);
    }

    if let Some(after_help) = extension_after_help(extension_info, extension_health) {
        cmd = cmd.after_help(after_help);
    }

    cmd
}

fn extension_after_help(
    extension_info: &[ExtensionCliInfo],
    extension_health: &ExtensionCliHealth,
) -> Option<String> {
    let mut lines = Vec::new();

    if !extension_info.is_empty() {
        let commands = extension_info
            .iter()
            .map(|info| info.tool.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Extension-provided commands: {commands}"));
    }

    if let Some(error) = &extension_health.load_error {
        lines.push(format!(
            "Extension discovery warning: {error}. Run `homeboy extension list` for details."
        ));
    }

    if !extension_health.broken_link_ids.is_empty() {
        lines.push(format!(
            "Extension health warning: {} broken extension link(s): {}. Run `homeboy extension list` for details or `homeboy extension relink <id> <path>` to repair.",
            extension_health.broken_link_ids.len(),
            extension_health.broken_link_ids.join(", ")
        ));
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn try_parse_extension_cli_command(
    matches: &ArgMatches,
    extension_info: &[ExtensionCliInfo],
) -> Option<ExtensionCliCommand> {
    let (tool, sub_matches) = matches.subcommand()?;

    if !extension_info.iter().any(|m| m.tool == tool) {
        return None;
    }

    let project_id = sub_matches.get_one::<String>("project_id")?.clone();
    let args: Vec<String> = sub_matches
        .get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Some(ExtensionCliCommand {
        tool: tool.to_string(),
        project_id,
        args,
    })
}

fn is_builtin_subcommand(name: &str) -> bool {
    crate::command_contract::registered_command(name).is_some()
}

fn preflight_hot_command(cli: &Cli, output_file: Option<&str>) -> Option<i32> {
    if let Some(hot_command) = resource_policy::hot_command(&cli.command) {
        if let Ok((resources, _)) = crate::commands::resources::run_preflight() {
            let default_lab_runner = if hot_command.lab_offload_supported {
                crate::runner::resolve_default_lab_runner().ok().flatten()
            } else {
                None
            };
            // An explicit runner is a routing decision, not a default-runner
            // fallback. Let Lab offload report any runner-specific readiness or
            // capability failure rather than blocking it at controller preflight.
            let selected_lab_runner =
                resource_policy_runner_hint(cli, default_lab_runner.as_deref());
            let warning = resource_policy::evaluate_with_runner_hint(
                hot_command,
                &resources,
                selected_lab_runner,
            );
            let runner_hosted = resource_policy::is_runner_hosted_exec();
            if let Some(warning) = warning.as_ref() {
                if !matches!(cli.placement, crate::cli_surface::Placement::Local) && !runner_hosted
                {
                    eprintln!("{}", warning.message);
                }
            }
            // Persist the preflight resource policy decision so observation
            // runs (bench, lint, test, etc.) can record it in their metadata
            // for later interpretation. This stays generic to Homeboy core.
            let mut resource_policy_context =
                resource_policy::resource_policy_context_from_evaluation(
                    hot_command,
                    &resources,
                    if runner_hosted {
                        None
                    } else {
                        warning.as_ref()
                    },
                    matches!(cli.placement, crate::cli_surface::Placement::Local),
                    selected_lab_runner,
                    runner_hosted,
                );
            if cli.runner.is_some()
                && hot_command.lab_offload_supported
                && !matches!(cli.placement, crate::cli_surface::Placement::Local)
            {
                resource_policy_context.runner_selection.reason = "explicit_lab_runner".to_string();
            }
            resource_policy::capture_context(resource_policy_context);
            if let Some(warning) = warning.as_ref() {
                if let Some(err) = resource_policy::non_interactive_preflight_error(
                    warning,
                    !matches!(cli.placement, crate::cli_surface::Placement::Auto) || runner_hosted,
                    is_interactive_shell(),
                    resource_policy::rerun_command(
                        hot_command,
                        &std::env::args().collect::<Vec<_>>(),
                        selected_lab_runner,
                    ),
                    default_lab_runner.as_deref(),
                ) {
                    output_runtime::emit_json_result(Err(err), output_file, 2);
                    return Some(2);
                }
            }
        }
    }

    None
}

fn resource_policy_runner_hint<'a>(
    cli: &'a Cli,
    default_runner: Option<&'a str>,
) -> Option<&'a str> {
    cli.runner.as_deref().or(default_runner)
}

fn run_startup_update_checks(command: &Commands) {
    // Startup update checks — skip for upgrade (it handles this itself).
    if !matches!(
        command,
        Commands::Upgrade(_) | Commands::Daemon(_) | Commands::SelfCmd(_)
    ) {
        crate::core::upgrade::update_check::run_startup_check();
        crate::core::extension::update_check::run_startup_check();
    }
}

/// Validate the JSON-envelope output-file path. When the path is invalid,
/// emit the error envelope and return the process `ExitCode` the caller
/// should return; otherwise return `None` to continue.
fn output_file_path_exit_code(path: &str) -> Option<std::process::ExitCode> {
    if let Some(err) = output_runtime::validate_output_file_path(path) {
        output_runtime::emit_json_result(Err(err), None, 2);
        return Some(std::process::ExitCode::from(exit_code_to_u8(2)));
    }
    None
}

fn exit_code_to_u8(code: i32) -> u8 {
    if code <= 0 {
        0
    } else if code >= 255 {
        255
    } else {
        code as u8
    }
}

fn is_interactive_shell() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn normalize_runs_runner_options(cli: &mut Cli, normalized_args: &[String]) {
    if is_runs_list_runner_option(normalized_args)
        || is_runs_artifact_get_runner_option(normalized_args)
        || matches!(&cli.command, Commands::Runs(args) if args.is_artifact_get())
    {
        if let Commands::Runs(args) = &mut cli.command {
            cli.runner = args.absorb_global_runner_for_command_option(cli.runner.take());
        }
    }
}

fn is_runs_list_runner_option(args: &[String]) -> bool {
    let Some(runs_index) = args.iter().position(|arg| arg == "runs") else {
        return false;
    };
    let Some(list_index) = args.iter().position(|arg| arg == "list") else {
        return false;
    };

    list_index > runs_index
        && args.iter().enumerate().any(|(index, arg)| {
            index > list_index && (arg == "--runner" || arg.starts_with("--runner="))
        })
}

fn is_runs_artifact_get_runner_option(args: &[String]) -> bool {
    let Some(runs_index) = args.iter().position(|arg| arg == "runs") else {
        return false;
    };
    let Some(artifact_index) = args.iter().position(|arg| arg == "artifact") else {
        return false;
    };
    let Some(get_index) = args.iter().position(|arg| arg == "get") else {
        return false;
    };

    artifact_index > runs_index
        && get_index > artifact_index
        && args.iter().enumerate().any(|(index, arg)| {
            index > get_index && (arg == "--runner" || arg.starts_with("--runner="))
        })
}

/// Attempt to augment a clap error with entity suggestions.
/// Returns Some(augmented_message) if the unrecognized string matches a known entity.
fn try_augment_clap_error(
    e: &clap::Error,
    extension_health: &ExtensionCliHealth,
) -> Option<String> {
    // Extract unrecognized subcommand and parent command from error.
    let unrecognized = extract_unrecognized_from_error(e)?;
    let parent_command = extract_parent_command_from_error(e)?;

    let mut hints = entity_suggest::find_entity_match(&unrecognized)
        .map(|entity_match| {
            entity_suggest::generate_entity_hints(&entity_match, &parent_command, &unrecognized)
        })
        .unwrap_or_default();

    append_extension_health_hints(&mut hints, extension_health);

    if hints.is_empty() {
        return None;
    }

    // Build augmented output.
    let mut output = format!("error: unrecognized subcommand '{}'\n\n", unrecognized);
    for hint in hints {
        output.push_str(&format!("hint: {}\n", hint));
    }
    if parent_command.is_empty() {
        output.push_str("\nFor more information, try 'homeboy --help'");
    } else {
        output.push_str(&format!(
            "\nFor more information, try 'homeboy {} --help'",
            parent_command
        ));
    }

    Some(output)
}

fn append_extension_health_hints(hints: &mut Vec<String>, extension_health: &ExtensionCliHealth) {
    if extension_health.load_error.is_some() || !extension_health.broken_link_ids.is_empty() {
        hints.push(
            "extension-provided commands may be unavailable; run `homeboy extension list` to inspect extension health".to_string(),
        );
    }

    if !extension_health.broken_link_ids.is_empty() {
        hints.push(format!(
            "broken extension link(s): {}; repair with `homeboy extension relink <id> <path>`",
            extension_health.broken_link_ids.join(", ")
        ));
    }
}

/// Extract the unrecognized subcommand string from a clap error.
fn extract_unrecognized_from_error(e: &clap::Error) -> Option<String> {
    use clap::error::ContextKind;

    // clap 4.x provides context via e.context().
    for (kind, value) in e.context() {
        if matches!(kind, ContextKind::InvalidSubcommand) {
            return Some(value.to_string());
        }
    }

    // Fallback: parse from error message. Clap wording varies between
    // contexts and versions.
    let msg = e.to_string();
    for marker in ["unrecognized subcommand '", "subcommand '"] {
        if let Some(start) = msg.find(marker) {
            let rest = &msg[start + marker.len()..];
            if let Some(end) = rest.find('\'') {
                return Some(rest[..end].to_string());
            }
        }
    }
    for marker in ["unrecognized subcommand `", "subcommand `"] {
        if let Some(start) = msg.find(marker) {
            let rest = &msg[start + marker.len()..];
            if let Some(end) = rest.find('`') {
                return Some(rest[..end].to_string());
            }
        }
    }

    None
}

/// Extract the parent command from a clap error's usage string.
fn extract_parent_command_from_error(e: &clap::Error) -> Option<String> {
    use clap::error::ContextKind;

    // clap 4.x: look for Usage context which contains "homeboy <command> ...".
    for (kind, value) in e.context() {
        if matches!(kind, ContextKind::Usage) {
            let usage = value.to_string();
            // Format: "Usage: homeboy <command> [OPTIONS] ...".
            if let Some(rest) = usage.strip_prefix("Usage: homeboy ") {
                // Get first word after "homeboy ".
                if let Some(cmd) = rest.split_whitespace().next() {
                    // Skip if it's a placeholder like "[OPTIONS]" or "<COMMAND>".
                    if !cmd.starts_with('[') && !cmd.starts_with('<') {
                        return Some(cmd.to_string());
                    }
                }
            }
        }
    }

    // Fallback: parse from error message which includes usage.
    let msg = e.to_string();
    if let Some(start) = msg.find("Usage: homeboy ") {
        let rest = &msg[start + 15..];
        if let Some(cmd) = rest.split_whitespace().next() {
            if !cmd.starts_with('[') && !cmd.starts_with('<') {
                return Some(cmd.to_string());
            }
        }
    }

    Some(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    struct EnvGuard {
        name: &'static str,
        previous: Option<String>,
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn remove(name: &'static str) -> Self {
            let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
            let previous = std::env::var(name).ok();
            std::env::remove_var(name);
            Self {
                name,
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                std::env::set_var(self.name, value);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn sample_extension_info(tool: &str) -> ExtensionCliInfo {
        ExtensionCliInfo {
            tool: tool.to_string(),
            descriptor: DynamicCommandDescriptor::extension_command(
                tool.to_string(),
                "Run Sample CLI commands via Sample Extension".to_string(),
            ),
            project_id_help: None,
            args_help: None,
            examples: Vec::new(),
        }
    }

    fn write_cli_extension(home: &std::path::Path, id: &str, tool: &str) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": "Sample Runtime Extension",
                "version": "0.0.0",
                "cli": {
                    "tool": tool,
                    "display_name": "Sample CLI",
                    "command_template": "{{cliPath}} {{args}}"
                }
            })
            .to_string(),
        )
        .expect("extension manifest");
    }

    fn write_extension_command_docs(home: &std::path::Path, id: &str, tool: &str) {
        let docs_dir = home
            .join(".config/homeboy/extensions")
            .join(id)
            .join("docs/commands");
        std::fs::create_dir_all(&docs_dir).expect("extension docs dir");
        std::fs::write(docs_dir.join(format!("{tool}.md")), "# Extension command")
            .expect("extension command docs");
    }

    #[test]
    fn output_format_names_are_rejected_as_global_output_paths() {
        let err = output_runtime::validate_output_file_path("json")
            .expect("format-like path should be rejected");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("--output json"));
    }

    #[test]
    fn normal_output_file_paths_are_allowed() {
        assert!(output_runtime::validate_output_file_path("./homeboy-output.json").is_none());
    }

    #[test]
    fn startup_fast_path_only_matches_root_help_and_version_flags() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            startup_fast_path(&args(&["homeboy", "--help"])),
            Some(StartupFastPath::Help)
        );
        assert_eq!(
            startup_fast_path(&args(&["homeboy", "-h"])),
            Some(StartupFastPath::Help)
        );
        assert_eq!(
            startup_fast_path(&args(&["homeboy", "--version"])),
            Some(StartupFastPath::Version)
        );
        assert_eq!(
            startup_fast_path(&args(&["homeboy", "-V"])),
            Some(StartupFastPath::Version)
        );
        assert_eq!(
            startup_fast_path(&args(&["homeboy", "status", "--help"])),
            None
        );
        assert_eq!(
            startup_fast_path(&args(&["homeboy", "sample-cli", "--help"])),
            None
        );
    }

    #[test]
    fn root_help_lists_extension_provided_commands() {
        let mut command = build_augmented_command(
            &[sample_extension_info("sample-cli")],
            &ExtensionCliHealth::default(),
        );

        let help = command.render_help().to_string();

        assert!(help.contains("Extension-provided commands: sample-cli"));
    }

    #[cfg(unix)]
    #[test]
    fn root_help_warns_about_broken_extension_links_without_paths() {
        let health = ExtensionCliHealth {
            load_error: None,
            broken_link_ids: vec!["sample-runtime".to_string()],
        };
        let mut command = build_augmented_command(&[], &health);

        let help = command.render_help().to_string();

        assert!(
            help.contains("Extension health warning: 1 broken extension link(s): sample-runtime")
        );
        assert!(help.contains("homeboy extension list"));
        assert!(help.contains("homeboy extension relink <id> <path>"));
        assert!(!help.contains("/missing-sample-runtime"));
    }

    #[cfg(unix)]
    #[test]
    fn invalid_dynamic_command_points_to_extension_health_when_links_are_broken() {
        let command = build_augmented_command(&[], &ExtensionCliHealth::default());
        let err = command
            .try_get_matches_from(["homeboy", "sample-cli"])
            .expect_err("sample-cli should not parse without extension command metadata");
        let health = ExtensionCliHealth {
            load_error: None,
            broken_link_ids: vec!["sample-runtime".to_string()],
        };

        let output = try_augment_clap_error(&err, &health).expect("extension health hint");

        assert!(output.contains("extension-provided commands may be unavailable"));
        assert!(output.contains("broken extension link(s): sample-runtime"));
        assert!(output.contains("homeboy extension list"));
    }

    #[cfg(unix)]
    #[test]
    fn extension_discovery_reports_dynamic_commands_and_broken_links() {
        crate::test_support::with_isolated_home(|home| {
            write_cli_extension(home.path(), "sample-runtime", "sample-cli");
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            let link = extensions_dir.join("stale-runtime");
            let target = extensions_dir.join("missing-stale-runtime");
            std::os::unix::fs::symlink(&target, &link).unwrap();

            let discovery = collect_extension_cli_info();

            assert_eq!(discovery.info.len(), 1);
            assert_eq!(discovery.info[0].tool, "sample-cli");
            assert_eq!(discovery.health.broken_link_ids, vec!["stale-runtime"]);
        });
    }

    #[test]
    fn augmented_manifest_includes_extension_command_contract_and_health() {
        crate::test_support::with_isolated_home(|home| {
            write_cli_extension(home.path(), "sample-runtime", "sample-cli");
            write_extension_command_docs(home.path(), "sample-runtime", "sample-cli");

            let manifest = current_augmented_command_safety_manifest();
            let sample_cli = manifest
                .find_path(&["sample-cli"])
                .expect("sample-cli command manifest");

            assert!(sample_cli.mutates);
            assert!(sample_cli.operator);
            assert_eq!(
                sample_cli.docs.path.as_deref(),
                Some("docs/commands/sample-cli.md")
            );
            assert!(sample_cli
                .dangerous_flags
                .contains(&"passthrough args".to_string()));
            assert!(sample_cli
                .output
                .notes
                .contains("extension-provided CLI passthrough"));

            let extension = sample_cli.extension.as_ref().expect("extension metadata");
            assert_eq!(extension.extension_id, "sample-runtime");
            assert_eq!(extension.tool_name, "sample-cli");
            assert_eq!(extension.args_contract.project_id.name, "project_id");
            assert!(extension.args_contract.project_id.required);
            assert_eq!(extension.args_contract.args.name, "args");
            assert!(extension.args_contract.args.multiple);
            assert!(extension.args_contract.trailing_var_arg);
            assert!(extension.args_contract.allow_hyphen_values);
            assert_eq!(extension.health.status, "ready");
            assert!(extension.health.ready);
            assert!(extension.health.compatible);
        });
    }

    #[test]
    fn runs_list_runner_after_subcommand_is_not_treated_as_global_runner() {
        let _env = EnvGuard::remove(crate::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let mut cli = Cli::parse_from([
            "homeboy",
            "runs",
            "list",
            "--runner",
            "homeboy-lab",
            "--status",
            "running",
        ]);

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));

        normalize_runs_runner_options(
            &mut cli,
            &[
                "homeboy".into(),
                "runs".into(),
                "list".into(),
                "--runner".into(),
                "homeboy-lab".into(),
                "--status".into(),
                "running".into(),
            ],
        );

        assert_eq!(cli.runner, None);
        let Commands::Runs(args) = &cli.command else {
            panic!("expected runs command");
        };
        assert_eq!(args.list_runner(), Some("homeboy-lab"));
    }

    #[test]
    fn global_runner_for_runs_show_is_preserved_for_guidance_error() {
        let _env = EnvGuard::remove(crate::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let mut cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "runs",
            "show",
            "run-123",
        ]);

        normalize_runs_runner_options(
            &mut cli,
            &[
                "homeboy".into(),
                "--runner".into(),
                "homeboy-lab".into(),
                "runs".into(),
                "show".into(),
                "run-123".into(),
            ],
        );

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        let err = crate::commands::route::route_after_parse(
            &cli,
            &[
                "homeboy".into(),
                "--runner".into(),
                "homeboy-lab".into(),
                "runs".into(),
                "show".into(),
                "run-123".into(),
            ],
            None,
        )
        .expect_err("runs show still rejects global runner");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("without --runner"));
    }

    #[test]
    fn runs_artifact_get_runner_after_subcommand_is_not_treated_as_global_runner() {
        let _env = EnvGuard::remove(crate::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let mut cli = Cli::parse_from([
            "homeboy",
            "runs",
            "artifact",
            "get",
            "run-123",
            "report-json",
            "--runner",
            "homeboy-lab",
        ]);

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));

        normalize_runs_runner_options(
            &mut cli,
            &[
                "homeboy".into(),
                "runs".into(),
                "artifact".into(),
                "get".into(),
                "run-123".into(),
                "report-json".into(),
                "--runner".into(),
                "homeboy-lab".into(),
            ],
        );

        assert_eq!(cli.runner, None);
        let Commands::Runs(args) = &cli.command else {
            panic!("expected runs command");
        };
        assert!(args.is_artifact_get());
        assert_eq!(args.artifact_get_runner(), Some("homeboy-lab"));
        crate::commands::route::route_after_parse(
            &cli,
            &[
                "homeboy".into(),
                "runs".into(),
                "artifact".into(),
                "get".into(),
                "run-123".into(),
                "report-json".into(),
                "--runner".into(),
                "homeboy-lab".into(),
            ],
            None,
        )
        .expect("runs artifact get command-local runner is allowed");
    }

    #[test]
    fn global_runner_for_runs_artifact_get_is_absorbed_into_command_option() {
        let _env = EnvGuard::remove(crate::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let mut cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "runs",
            "artifact",
            "get",
            "run-123",
            "report-json",
        ]);

        normalize_runs_runner_options(
            &mut cli,
            &[
                "homeboy".into(),
                "--runner".into(),
                "homeboy-lab".into(),
                "runs".into(),
                "artifact".into(),
                "get".into(),
                "run-123".into(),
                "report-json".into(),
            ],
        );

        assert_eq!(cli.runner, None);
        let Commands::Runs(args) = &cli.command else {
            panic!("expected runs command");
        };
        assert_eq!(args.artifact_get_runner(), Some("homeboy-lab"));
        crate::commands::route::route_after_parse(
            &cli,
            &[
                "homeboy".into(),
                "--runner".into(),
                "homeboy-lab".into(),
                "runs".into(),
                "artifact".into(),
                "get".into(),
                "run-123".into(),
                "report-json".into(),
            ],
            None,
        )
        .expect("runs artifact get accepts global runner for command-local fetch");
    }

    #[test]
    fn wrapper_global_runner_preserves_trailing_output_request() {
        let matches = Cli::command_with_scoped_lab_args()
            .try_get_matches_from([
                "homeboy",
                "--runner",
                "homeboy-lab",
                "agent-task",
                "controller",
                "run-from-spec",
                "loop.json",
                "--max-actions",
                "1",
                "--output",
                "/tmp/controller-result.json",
            ])
            .expect("parse wrapper-style lab offload command");

        assert_eq!(
            matches
                .try_get_one::<std::path::PathBuf>("output")
                .expect("output arg")
                .map(|path| path.to_string_lossy().to_string())
                .as_deref(),
            Some("/tmp/controller-result.json")
        );

        let (cli, _) = Cli::from_registered_arg_matches(&matches).expect("typed cli");
        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    }

    #[test]
    fn resource_policy_uses_explicit_runner_before_default_runner() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "selected-lab",
            "agent-task",
            "cook",
            "--to-worktree",
            "homeboy@fix-explicit-runner",
            "--prompt",
            "fix the issue",
        ]);

        assert_eq!(
            resource_policy_runner_hint(&cli, Some("default-lab")),
            Some("selected-lab")
        );
    }

    #[test]
    fn managed_runner_context_clears_before_production_run_command() {
        let _lock = env_lock().lock().unwrap_or_else(|err| err.into_inner());
        let previous = [
            crate::runner::RUNNER_HOSTED_EXEC_ENV,
            crate::runner::RUNNER_PLACEMENT_RESOLVED_ENV,
            crate::runner::RUNNER_ID_ENV,
        ]
        .map(|name| (name, std::env::var(name).ok()));
        std::env::set_var(crate::runner::RUNNER_HOSTED_EXEC_ENV, "1");
        std::env::set_var(crate::runner::RUNNER_PLACEMENT_RESOLVED_ENV, "1");
        std::env::set_var(crate::runner::RUNNER_ID_ENV, "homeboy-lab");

        *marker_context_before_run_command()
            .lock()
            .expect("marker test state") = None;
        let runtime = CliRuntime::new();
        let exit = runtime.run_from_args(vec!["homeboy".to_string(), "status".to_string()]);

        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        assert_eq!(
            *marker_context_before_run_command()
                .lock()
                .expect("marker test state"),
            Some(false),
            "run_command must not inherit managed placement markers"
        );
        assert!(!resource_policy::is_managed_runner_placement_context());

        for (name, value) in previous {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}
