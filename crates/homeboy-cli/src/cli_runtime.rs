use clap::{ArgMatches, Command};
use std::io::IsTerminal;
use std::process::Command as ProcessCommand;
use std::sync::OnceLock;
use uuid::Uuid;

use crate::cli_surface::{
    command_safety_manifest_from_dynamic, command_surface_from, Cli, CommandSafetyManifest,
    Commands, DynamicCommandDescriptor, ExtensionCommandArgContract, ExtensionCommandArgsContract,
    ExtensionCommandHealth, ExtensionCommandManifest,
};
use crate::command_capability::{
    classify as classify_command_capability, homeboy_owned_args, requires_startup_reconciliation,
    CommandCapability,
};
use crate::commands;
use crate::commands::cli;
use crate::commands::output_runtime;
use crate::commands::utils::{args, entity_suggest, resource_policy, response as output};
use homeboy::extension::{
    list_summaries_with, load_all_extensions, CliConfig,
    ExtensionManifest as InstalledExtensionManifest, ExtensionReadinessMode, ExtensionSummary,
};
use homeboy_agents::agent_task_service::cook_continue_command;
use homeboy_core::extension_readiness::READY_CHECK_SKIPPED_REASON;
use homeboy_upgrade::upgrade;

const COOK_PINNED_RUNTIME_ENV: &str = "HOMEBOY_COOK_PINNED_CONTROLLER_RUNTIME";
const RUNNER_EXEC_RECOVERY_OWNER_ENV: &str = "HOMEBOY_RUNNER_EXEC_RECOVERY_OWNER";
const RUNNER_EXEC_RECOVERY_CHILD_ENV: &str = "HOMEBOY_RUNNER_EXEC_RECOVERY_CHILD";
const CONTROLLER_FALLBACK_RECONCILIATION_ENV: &str = "HOMEBOY_CONTROLLER_FALLBACK_RECONCILIATION";

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
    Identity,
}

/// Payload a startup fast path emits. Resolving the payload separately from
/// printing it keeps the user-visible `--help` surface reachable from tests:
/// rendering help inline is why the root-help tests could only assert against a
/// builder the binary never called.
enum StartupFastPathOutput {
    Help(String),
    Version(String),
    Identity(serde_json::Value),
}

fn startup_fast_path_output(args: &[String]) -> Option<StartupFastPathOutput> {
    Some(match startup_fast_path(args)? {
        // Root help renders the augmented command because extension-provided
        // subcommands are part of the user-visible surface; the static derive
        // command hides every one of them. Discovery here is metadata-only: it
        // reads installed manifests and broken symlinks, and spawns nothing.
        // Rendered help never displays readiness, so probing it was pure cost
        // on the hottest command in the CLI (#10616).
        StartupFastPath::Help => {
            // Fall through to normal parsing rather than asserting. This used
            // to `expect_err`, so any argv the fast path read as help but clap
            // parsed successfully aborted the process instead of running the
            // command (#11577). The fast path is an optimization; disagreeing
            // with clap must cost a slow path, never a panic.
            let Err(error) = CliRuntime::new()
                .build_augmented_command()
                .try_get_matches_from(args)
            else {
                return None;
            };
            if error.kind() != clap::error::ErrorKind::DisplayHelp {
                return None;
            }
            StartupFastPathOutput::Help(error.to_string())
        }
        StartupFastPath::Version => {
            StartupFastPathOutput::Version(upgrade::current_build_version())
        }
        StartupFastPath::Identity => {
            StartupFastPathOutput::Identity(crate::commands::self_cmd::identity_report())
        }
    })
}

pub fn run_startup_fast_path(args: &[String]) -> Option<std::process::ExitCode> {
    match startup_fast_path_output(args)? {
        StartupFastPathOutput::Help(help) => print!("{help}"),
        StartupFastPathOutput::Version(version) => println!("{version}"),
        StartupFastPathOutput::Identity(identity) => {
            output_runtime::emit_json_result_for_identity(
                Ok(identity),
                None,
                0,
                &output::CommandIdentity::with_operation("self", "identity"),
            );
        }
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

/// Installed command/capability metadata for admission checks. Extension
/// discovery intentionally skips readiness probes at this phase.
pub fn current_augmented_command_surface() -> crate::cli_surface::CommandSurface {
    let discovery = collect_extension_cli_info_metadata_only();
    command_surface_from(build_augmented_command(&discovery.info, &discovery.health))
}

pub(crate) fn current_augmented_command_contract() -> clap::Command {
    let discovery = collect_extension_cli_info_metadata_only();
    build_augmented_command(&discovery.info, &discovery.health)
}

/// Register every provider hook the CLI wires before the startup terminal-run
/// reconcile.
///
/// Split out of [`CliRuntime::run_from_args`] so the set of registrations is a
/// named, callable list instead of an inline sequence no test could reach. An
/// omission here is silent at runtime — a boxed provider registry with an empty
/// slot dispatches to its no-op — so `register_all_providers` exists to give the
/// completeness test the exact startup wiring the binary performs.
///
/// Order is load-bearing and preserved verbatim from the original inline block.
pub fn register_startup_providers_before_reconcile() {
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
    // Register the in-core release provider so core's status mechanics
    // (fleet/project/context/git change reporting/tag-gap) get deploy+release
    // behavior through the hook. Moves out with deploy/release when they
    // become the homeboy-release crate.
    crate::release::provider_impl::register();
    homeboy_extension::audit_manifest_provider::register();
    homeboy_extension::component_script::register_component_script_runner();
    homeboy_extension::build::register_component_build_runner();
    homeboy_extension::lifecycle::register_component_install_runner();
    // Register extension-backed audit providers so code_audit can load
    // grammars, run fallback fingerprint scripts, and collect compiler
    // warnings without depending on the extension registry or script runner.
    homeboy_extension::audit_fingerprint_script_provider::register();
    homeboy_extension::audit_grammar_source_provider::register();
    homeboy_extension::audit_compiler_warning_provider::register();
    // Register the audit recorded-artifact provider so the artifact-portability
    // detector can read past runs' artifacts from the observation store without
    // code_audit depending on observation — the last seam before audit becomes
    // its own crate.
    crate::core::observation::audit_artifact_provider::register();
    // Register the audit fixability provider so code_audit can report how
    // fixable its findings are without calling up into the refactor engine's
    // fix planner — the seam that removes the last code_audit->refactor edge.
    // Register the rig toolchain provider so core's extension exec-env
    // builder can prepend the rig toolchain PATH.
    crate::rig::provider::register();
    crate::stack::provider::register();
    crate::refactor::audit_fixability_provider::register();
    // Register the refactor transform provider so core's extension
    // test-drift auto-fixer can apply generated transform rules.
    crate::refactor::transform_provider::register();
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
    crate::runner::register_runner_staging_provider();
    // Register the lab-workspace provenance provider so the agent-task
    // scheduler can verify lab-materialized workspaces without core depending
    // on runner behavior.
    crate::runner::register_lab_workspace_provenance_provider();
    // Register the runner-continuation provider so the agent-task lifecycle
    // can reconcile and resume runs dispatched to a remote runner without
    // core depending on runner behavior.
    crate::runner::register_runner_continuation_provider();
}

/// Register every provider hook the CLI wires after the startup terminal-run
/// reconcile.
///
/// Takes the agent-task config rather than loading it so the completeness test
/// can drive the full registration sequence without touching the ambient home
/// directory.
pub fn register_startup_providers_after_reconcile(
    agent_task: &crate::core::defaults::AgentTaskConfig,
) -> Result<(), crate::core::error::Error> {
    // Register the runner daemon-exec driver so the daemon's /exec endpoint
    // can prepare and run a runner job as a local child without core
    // depending on runner process-execution behavior.
    crate::runner::register_runner_daemon_exec_driver();
    // Lab owns its durable staging/dispatch controller-job interpretation;
    // core only owns the generic daemon lifecycle.
    crate::runner::register_lab_staging_controller_driver();
    crate::agents::agent_task_service::register_promotion_job_driver();
    // Register the agent-task orchestration driver so the daemon's
    // orchestration tick can reconcile orphaned `running` records and resolve
    // controller waits from durable state without core depending on the
    // agent-task subsystem. Both mechanisms previously had no automatic
    // caller: a detached cook whose owner died stayed `running` forever, and a
    // controller parked in `Waiting` never left it.
    crate::agents::agent_task_service::register_orchestration_driver();
    crate::agents::agent_task_service::register_controller_upgrade_admission_provider();
    // A locally-placed detached Cook is a daemon-owned durable job: the daemon
    // owns its record, checkpointing, cancellation and HTTP inspection, while
    // the launcher-spawned child keeps the operator's execution environment.
    crate::agents::agent_task_service::register_cook_job_driver();
    // A locally-placed detached fanout wave is daemon-owned on the same terms:
    // the daemon supervises a coordinator it did not spawn, so no branch of its
    // lifecycle can re-run a child that already completed.
    crate::agents::agent_task_service::register_cook_batch_job_driver();
    crate::commands::cleanup::register_cleanup_job_driver();
    // The configured acceptance verifier is the one registration that is
    // conditional: `register_acceptance_verifier_from_config` is a no-op when
    // no verifier is configured, which is why the completeness test treats
    // that registry as deliberately optional.
    crate::agents::agent_task_lifecycle::register_acceptance_verifier_from_config(agent_task)?;
    crate::runner::enable_production_lab_staging();
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
    crate::agents::agent_task_lifecycle::controller_pin_reference_provider::register();
    // Register the loop-spec validation provider so core's proof validator
    // can validate a materialized agent-task loop-spec artifact without
    // depending on the agent-task subsystem.
    crate::agents::agent_task_controller_service::loop_spec_validation_provider::register();
    // Register the gate-feedback candidate-baseline provider so core's
    // worktree-safety logic can accept a dirty worktree that is a verified
    // agent-task gate-feedback candidate without depending on the agent-task
    // subsystem.
    crate::agents::agent_task_candidate_baseline::register();
    // Register the agent-task activity provider so core's activity report
    // includes durable agent-task records and their health summary without
    // depending on the agent-task subsystem.
    crate::agents::agent_task_lifecycle::activity_provider::register();
    // Register the bench agent-task matrix provider so core's cross-rig
    // bench comparison can project rig entries into an agent-task matrix
    // without depending on the agent-task subsystem.
    crate::agents::agent_task::bench_matrix_provider::register();
    // Register the agent-task terminal-recovery provider so core's job store
    // can recover terminal jobs from durable agent-task runs.
    crate::agents::api_jobs_terminal_recovery::register();
    // Register the agent-task secret provider so core's trace secret
    // resolution can consult the agent-task secret store.
    crate::agents::agent_task_secrets::register();
    // Register the extension provider-discovery validator so core's
    // extension install/repair can verify declared agent-runtime providers.
    crate::agents::agent_task_provider::discovery::register();
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
        let cli =
            <crate::cli_surface::Cli as clap::Parser>::try_parse_from(argv).map_err(|error| {
                crate::core::error::Error::validation_invalid_argument(
                    "agent-task",
                    "failed to parse agent-task arguments while compiling Lab provider policy",
                    Some(error.to_string()),
                    None,
                )
            })?;
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

    Ok(())
}

/// Every provider registration the CLI performs at startup, in the exact order
/// [`CliRuntime::run_from_args`] performs them.
///
/// `run_from_args` calls the two halves separately because it interleaves a
/// terminal-run reconcile between them. This is the entry point the
/// registration-completeness test drives; keeping it as the sole other caller is
/// what makes "every declared provider registry is populated afterwards" a
/// meaningful assertion about the real binary.
pub fn register_all_providers(
    agent_task: &crate::core::defaults::AgentTaskConfig,
) -> Result<(), crate::core::error::Error> {
    register_startup_providers_before_reconcile();
    register_startup_providers_after_reconcile(agent_task)
}

impl CliRuntime {
    pub fn new() -> Self {
        Self {
            extension_discovery: OnceLock::new(),
        }
    }

    pub fn run_from_args(&self, args: Vec<String>) -> std::process::ExitCode {
        let normalized = args::normalize(args);
        let command_capability = classify_command_capability(&normalized);
        if let Some(message) = args::runner_exec_option_boundary_error(&normalized) {
            eprintln!("error: {message}");
            return std::process::ExitCode::from(2);
        }

        register_startup_providers_before_reconcile();
        if std::env::var_os(CONTROLLER_FALLBACK_RECONCILIATION_ENV).is_some() {
            let config = crate::core::defaults::load_config();
            if register_startup_providers_after_reconcile(&config.agent_task).is_err() {
                return std::process::ExitCode::from(2);
            }
            let _ =
                crate::runner::controller_fallback_projection::reconcile_on_controller_startup();
            return std::process::ExitCode::SUCCESS;
        }
        if let Some(child_id) = std::env::var_os(RUNNER_EXEC_RECOVERY_CHILD_ENV) {
            let child_token =
                std::env::var_os("HOMEBOY_RUNNER_EXEC_RECOVERY_CHILD_TOKEN").unwrap_or_default();
            if let Ok(Some(diagnostic)) =
                crate::runner::run_scheduled_terminal_runner_exec_recovery_child(
                    &child_id.to_string_lossy(),
                    &child_token.to_string_lossy(),
                )
            {
                eprintln!("{}", format_runner_exec_recovery_diagnostic(&diagnostic));
            }
            return std::process::ExitCode::SUCCESS;
        }
        if let Some(owner_id) = std::env::var_os(RUNNER_EXEC_RECOVERY_OWNER_ENV) {
            let owner_token =
                std::env::var_os("HOMEBOY_RUNNER_EXEC_RECOVERY_OWNER_TOKEN").unwrap_or_default();
            if let Ok(Some(work)) = crate::runner::run_scheduled_terminal_runner_exec_recovery(
                &owner_id.to_string_lossy(),
                &owner_token.to_string_lossy(),
            ) {
                let mut scheduled_count = 0;
                let mut spawn_failed_count = 0;
                let executable = std::env::current_exe();
                for child in &work.children {
                    let spawned = match &executable {
                        Ok(executable) => spawn_runner_exec_recovery_child(executable, child),
                        Err(error) => Err(std::io::Error::new(error.kind(), error.to_string())),
                    };
                    match spawned {
                        Ok(()) => scheduled_count += 1,
                        Err(error) => {
                            spawn_failed_count += 1;
                            let _ = crate::runner::record_scheduled_terminal_runner_exec_recovery_child_spawn_failure(child, &error);
                        }
                    }
                }
                let _ = crate::runner::finish_scheduled_terminal_runner_exec_recovery(
                    &owner_id.to_string_lossy(),
                    &owner_token.to_string_lossy(),
                    scheduled_count,
                    spawn_failed_count,
                    work.deferred_count,
                );
            }
            return std::process::ExitCode::SUCCESS;
        }
        let config = crate::core::defaults::load_config();
        if let Err(error) = register_startup_providers_after_reconcile(&config.agent_task) {
            eprintln!("error: {error}");
            return std::process::ExitCode::from(2);
        }
        // Runner-owned fallback staging may outlive the controller process. A
        // detached bounded pass keeps command startup independent of remote I/O.
        schedule_controller_fallback_reconciliation();
        // Deferred records outlive their worker. Startup restarts the singleton
        // so expired claims recover without another deferral request. The
        // deferred-workload family itself is exempt: the worker would restart
        // itself, and `reconcile` would spawn the worker it is about to judge.
        if command_capability == CommandCapability::Mutation
            && normalized.get(1).map(String::as_str) != Some("deferred-workload")
        {
            let _ = crate::commands::deferred_workload::restart_worker_if_pending();
        }

        if is_top_level_version_request(&normalized) {
            println!("{}", upgrade::current_build_version());
            return std::process::ExitCode::SUCCESS;
        }

        let matches = self.parse_matches(normalized.clone());
        // Global-arg conflicts are only enforced by clap when the flags follow
        // the subcommand, so this must be checked explicitly (#11826).
        if let Err(error) = crate::cli_surface::reject_conflicting_placement_selection(&matches) {
            eprintln!("{error}");
            return std::process::ExitCode::from(2);
        }
        self.run_matches(matches, normalized)
    }

    fn parse_matches(&self, normalized: Vec<String>) -> ArgMatches {
        let diagnostic_args = normalized.clone();
        match Cli::command_with_scoped_lab_args().try_get_matches_from(normalized.clone()) {
            Ok(matches) => matches,
            Err(static_err) => match self
                .build_augmented_command()
                .try_get_matches_from(normalized)
            {
                Ok(matches) => matches,
                Err(err) => {
                    if let Some(output) = try_augment_clap_error(
                        &err,
                        &diagnostic_args,
                        &self.extension_discovery().health,
                    ) {
                        eprintln!("{}", output);
                        std::process::exit(2);
                    }

                    if let Some(output) = try_augment_clap_error(
                        &static_err,
                        &diagnostic_args,
                        &self.extension_discovery().health,
                    ) {
                        eprintln!("{}", output);
                        std::process::exit(2);
                    }

                    err.exit();
                }
            },
        }
    }

    fn run_matches(&self, matches: ArgMatches, normalized: Vec<String>) -> std::process::ExitCode {
        let command_identity = command_identity_from_matches(&matches);

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
                if let Some(exit) = output_file_path_exit_code(path, &command_identity) {
                    return exit;
                }
            }

            let cli_args = cli::CliArgs {
                tool: extension_cmd.tool,
                identifier: extension_cmd.project_id,
                args: extension_cmd.args,
            };
            let result = cli::run(cli_args);

            let (json_result, exit_code) = output::map_cmd_result_to_json(result);
            output_runtime::emit_json_result_for_identity(
                json_result,
                output_file.as_deref(),
                exit_code,
                &command_identity,
            );
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }

        let (compiled, command_spec) = match Cli::compile_registered_arg_matches(&matches) {
            Ok(parsed) => parsed,
            Err(err) => err.exit(),
        };
        let mut cli = compiled.value;
        let command_provenance = compiled.provenance;
        let mut notification_route =
            match crate::core::notification_route_resolver::resolve_from_cli_or_env(
                cli.notification_transport.as_deref(),
                cli.notification_route.as_deref(),
            ) {
                Ok(route) => route,
                Err(err) => {
                    output_runtime::emit_json_result_for_identity(
                        Err(err),
                        output_file.as_deref(),
                        2,
                        &command_identity,
                    );
                    return std::process::ExitCode::from(2);
                }
            };
        if let Some(route) = &notification_route {
            // Placement routing happens before the thread-local command scope.
            // Mirror the selected route into its existing durable handoff input.
            cli.notification_transport = Some(route.transport.clone());
            cli.notification_route = Some(route.route.clone());
        }
        commands::set_skip_deps_hydration(cli.skip_deps_hydration);
        normalize_runs_runner_options(&mut cli, &normalized);
        normalize_cook_runner_option(&mut cli, &normalized);
        if let Commands::Fuzz(args) = &mut cli.command {
            match args.absorb_planning_runner(cli.runner.take()) {
                Ok(runner) => cli.runner = runner,
                Err(err) => {
                    output_runtime::emit_json_result_for_identity(
                        Err(err),
                        output_file.as_deref(),
                        2,
                        &command_identity,
                    );
                    return std::process::ExitCode::from(2);
                }
            }
        }

        if matches!(&cli.command, Commands::Runs(args) if args.is_bundle_export()) {
            output_file = None;
        }

        if cli.command.consumes_output_file_as_command_arg() {
            // This command owns `--output/-o`; it is not the global JSON envelope.
            output_file = None;
        } else if let Some(path) = output_file.as_deref() {
            if let Some(exit) = output_file_path_exit_code(path, &command_identity) {
                return exit;
            }
        }

        if let Commands::AgentTask(agent_task) = &cli.command {
            if let crate::commands::agent_task::AgentTaskCommand::Cook(cook) = &agent_task.command {
                if let Err(err) =
                    crate::commands::agent_task::run::validate_cook_request_with_provenance(
                        cook,
                        Some(&command_provenance),
                    )
                {
                    output_runtime::emit_json_result_for_identity(
                        Err(err),
                        output_file.as_deref(),
                        2,
                        &command_identity,
                    );
                    return std::process::ExitCode::from(2);
                }
            }
        }

        match delegate_agent_task_cook_to_pinned_runtime(&cli, &normalized) {
            Ok(Some(exit_code)) => return std::process::ExitCode::from(exit_code_to_u8(exit_code)),
            Ok(None) => {}
            Err(err) => {
                output_runtime::emit_json_result_for_identity(
                    Err(err),
                    output_file.as_deref(),
                    2,
                    &command_identity,
                );
                return std::process::ExitCode::from(2);
            }
        }

        if matches!(
            &cli.command,
            Commands::AgentTask(agent_task)
                if matches!(
                    &agent_task.command,
                    crate::commands::agent_task::AgentTaskCommand::Cook(_)
                )
        ) && notification_route.is_none()
        {
            notification_route = match crate::core::notification_route_resolver::resolve_installed()
            {
                Ok(route) => route,
                Err(err) => {
                    output_runtime::emit_json_result_for_identity(
                        Err(err),
                        output_file.as_deref(),
                        2,
                        &command_identity,
                    );
                    return std::process::ExitCode::from(2);
                }
            };
            if let Some(route) = &notification_route {
                cli.notification_transport = Some(route.transport.clone());
                cli.notification_route = Some(route.route.clone());
            }
        }

        match delegate_agent_task_lifecycle_to_pinned_runtime(&cli, &normalized) {
            Ok(Some(exit_code)) => return std::process::ExitCode::from(exit_code_to_u8(exit_code)),
            Ok(None) => {}
            Err(err) => {
                output_runtime::emit_json_result_for_identity(
                    Err(err),
                    output_file.as_deref(),
                    2,
                    &command_identity,
                );
                return std::process::ExitCode::from(2);
            }
        }

        match guard_fanout_resume_runtime(&cli) {
            Ok(()) => {}
            Err(err) => {
                output_runtime::emit_json_result_for_identity(
                    Err(err),
                    output_file.as_deref(),
                    2,
                    &command_identity,
                );
                return std::process::ExitCode::from(2);
            }
        }

        // These internal adapters carry their requests on process stdin. Execute
        // them before generic routing or startup work can consume that stream.
        if let Some(exit_code) =
            run_promotion_provider_dispatch(&cli.command, output_file.as_deref(), &command_identity)
        {
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }
        if let Some(exit_code) = run_raw_agent_tool_dispatch(&cli.command) {
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }

        // Capture controller pressure once before placement routing. The route
        // and persisted evidence reuse this preflight decision rather than
        // probing the host a second time.
        let managed_runner_placement = resource_policy::is_managed_runner_placement_context();
        if let Some(exit_code) =
            preflight_hot_command(&cli, output_file.as_deref(), &command_identity)
        {
            if managed_runner_placement {
                resource_policy::clear_managed_runner_placement_context();
            }
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }

        // Persist the actual preflight decision with the command intent before
        // placement routing can consume controller transport markers.
        crate::commands::utils::execution_provenance::capture(&cli, &normalized);

        let route_result = crate::commands::route::route_after_parse_with_provenance(
            &cli,
            &normalized,
            output_file.as_deref(),
            Some(&command_provenance),
        );
        if managed_runner_placement {
            resource_policy::clear_managed_runner_placement_context();
        }
        match route_result {
            Ok(None) => {}
            Ok(Some(exit_code)) => {
                return std::process::ExitCode::from(exit_code_to_u8(exit_code));
            }
            Err(err) => {
                output_runtime::emit_json_result_for_identity(
                    Err(err),
                    output_file.as_deref(),
                    2,
                    &command_identity,
                );
                return std::process::ExitCode::from(exit_code_to_u8(2));
            }
        }

        crate::core::set_artifact_root_override(
            cli.artifact_root.clone().or(artifact_root_override),
        );

        run_startup_update_checks(&cli.command);

        let exit_code = crate::core::notification_route::with_current(notification_route, || {
            #[cfg(test)]
            record_marker_context_before_run_command();
            commands::output_runtime::run_command(
                cli.command,
                command_spec,
                output_file.as_deref(),
                &command_identity,
                command_provenance,
                cli.placement,
            )
        });
        // The command's initial outcome is now durable and returned. Historical
        // runner evidence is a separately owned
        // best-effort recovery concern and cannot delay that boundary.
        if requires_startup_reconciliation(&normalized) {
            schedule_runner_exec_recovery();
        }
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

    /// Runtime discovery never probes readiness.
    ///
    /// Every consumer on this path — [`Self::build_augmented_command`], the
    /// clap-error augmenter, and extension command dispatch — reads only the
    /// command surface and link health. A `ready_check` is an arbitrary
    /// operator-authored shell command, and spawning one per installed
    /// extension to render `--help` or to route `homeboy rust ...` is pure
    /// latency. The safety manifest still probes via
    /// [`collect_extension_cli_info`]. (#10616)
    fn extension_discovery(&self) -> &ExtensionCliDiscovery {
        self.extension_discovery
            .get_or_init(collect_extension_cli_info_metadata_only)
    }
}

fn schedule_runner_exec_recovery() {
    // Unit tests execute this code inside the libtest binary. Re-executing
    // current_exe there recursively launches the complete test harness.
    if cfg!(test) {
        return;
    }
    let Ok(Some(schedule)) = crate::runner::schedule_terminal_runner_exec_recovery() else {
        return;
    };
    if !schedule.is_new_owner {
        return;
    }
    eprintln!(
        "runner-exec recovery scheduled: owner_id={} deferred_count={} inspect=`{}`",
        schedule.owner_id, schedule.deferred_count, schedule.inspection_action
    );
    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => {
            let _ = crate::runner::record_scheduled_terminal_runner_exec_recovery_spawn_failure(
                &schedule.owner_id,
                &schedule.owner_token,
                &error,
            );
            return;
        }
    };
    let mut command = ProcessCommand::new(executable);
    command
        .env(RUNNER_EXEC_RECOVERY_OWNER_ENV, &schedule.owner_id)
        .env(
            "HOMEBOY_RUNNER_EXEC_RECOVERY_OWNER_TOKEN",
            &schedule.owner_token,
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        // Child recovery emits only terminal or action-required diagnostics.
        .stderr(std::process::Stdio::inherit());
    crate::core::process::detach_from_caller_session(&mut command);
    if let Err(error) = command.spawn() {
        let _ = crate::runner::record_scheduled_terminal_runner_exec_recovery_spawn_failure(
            &schedule.owner_id,
            &schedule.owner_token,
            &error,
        );
    }
}

fn format_runner_exec_recovery_diagnostic(
    diagnostic: &crate::runner::RunnerExecRecoveryDiagnostic,
) -> String {
    format!(
        "runner-exec recovery action required: source_run_id={} reason={} inspect=`{}`",
        diagnostic.source_run_id, diagnostic.reason, diagnostic.inspection_action
    )
}

fn spawn_runner_exec_recovery_child(
    executable: &std::path::Path,
    child: &crate::runner::RunnerExecRecoveryChildSchedule,
) -> std::io::Result<()> {
    let mut command = ProcessCommand::new(executable);
    command
        .env(RUNNER_EXEC_RECOVERY_CHILD_ENV, &child.child_id)
        .env(
            "HOMEBOY_RUNNER_EXEC_RECOVERY_CHILD_TOKEN",
            &child.child_token,
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit());
    crate::core::process::detach_from_caller_session(&mut command);
    command.spawn().map(|_| ())
}

fn schedule_controller_fallback_reconciliation() {
    // The production binary can safely re-exec itself; the unit-test binary
    // would recursively launch the complete test harness and escape nextest.
    if cfg!(test) {
        return;
    }
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let mut command = ProcessCommand::new(executable);
    command
        .env(CONTROLLER_FALLBACK_RECONCILIATION_ENV, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::core::process::detach_from_caller_session(&mut command);
    let _ = command.spawn();
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
        // This comparison is the only thing standing between the re-exec below
        // and unbounded recursion, so it must compare file identity rather than
        // path spelling. `current_exe` resolves through `/proc/self/exe` (or the
        // platform equivalent) and is fully canonical; the pinned path is
        // composed from the data store root and can retain symlinked components
        // — a symlinked `$TMPDIR`, `/tmp` -> `/private/tmp`, a bind-mounted CI
        // workspace. A verbatim mismatch there makes the pinned runtime fail to
        // recognize itself and seal-and-re-exec again, forever.
        let expected = std::path::PathBuf::from(expected);
        let is_pinned_runtime = current == expected
            || match (
                std::fs::canonicalize(&current),
                std::fs::canonicalize(&expected),
            ) {
                (Ok(current), Ok(expected)) => current == expected,
                _ => false,
            };
        if is_pinned_runtime {
            return Ok(None);
        }
    }

    let request_id = format!("seal-{}", Uuid::new_v4());
    let pinned =
        crate::agents::agent_tasks::lifecycle::pin_current_controller_runtime(&request_id, || {
            Ok(false)
        })
        .map_err(|error| annotate_cook_seal_failure(error, &request_id, normalized_args))?;
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

/// The runtime seal runs before a cook has any durable run record — controller
/// admission is what creates one — so a failed seal leaves the error itself as
/// the only evidence the operator gets.
///
/// Carry the admission request identity and the exact command to re-submit, so
/// a contended parallel wave produces a resumable instruction instead of a bare
/// retryable failure (#9373). Admission is FIFO, so re-running the identical
/// command queues rather than races.
fn annotate_cook_seal_failure(
    mut error: homeboy::core::Error,
    request_id: &str,
    normalized_args: &[String],
) -> homeboy::core::Error {
    if !error.details.is_object() {
        error.details = serde_json::json!({});
    }
    error.details["controller_admission_phase"] = serde_json::json!("cook_runtime_seal");
    error.details["controller_admission_request_id"] = serde_json::json!(request_id);
    error.details["next_actions"] =
        serde_json::json!([homeboy::core::engine::shell::quote_args(normalized_args)]);
    error.with_hint(
        "Controller admission is FIFO: re-running the identical cook command queues behind the current owner instead of racing it.",
    )
}

/// Durable lifecycle mutations remain owned by the runtime that admitted the
/// record. Re-exec before Lab routing so recovery cannot create a replacement
/// handoff under the promoted controller.
fn delegate_agent_task_lifecycle_to_pinned_runtime(
    cli: &Cli,
    normalized_args: &[String],
) -> homeboy::core::Result<Option<i32>> {
    // Run and Resume mutate the SAME durable record, so they must re-exec under
    // the runtime that admitted it. Retry is intentionally excluded: it reads the
    // source record but creates a NEW replacement run, which must be owned by the
    // runtime that creates it rather than inheriting the source's (possibly stale)
    // pinned runtime. Delegating Retry here stamped the replacement with the
    // obsolete runtime after an upgrade (Extra-Chill/homeboy#8550).
    let run_id = match &cli.command {
        Commands::AgentTask(agent_task) => match &agent_task.command {
            crate::commands::agent_task::AgentTaskCommand::Run(args) => Some(&args.run_id),
            crate::commands::agent_task::AgentTaskCommand::Resume(args)
                if args.bridge
                    && crate::agents::agent_tasks::service::terminal_transport_recovery_required(
                        &args.run_id,
                    ) =>
            {
                None
            }
            crate::commands::agent_task::AgentTaskCommand::Resume(args) => Some(&args.run_id),
            crate::commands::agent_task::AgentTaskCommand::Accept(args) => Some(&args.run_id),
            crate::commands::agent_task::AgentTaskCommand::CookContinue(args) => {
                if matches!(cli.placement, crate::cli_surface::Placement::Local) {
                    return Ok(None);
                }
                return delegate_cook_continue_to_pinned_runtime(&args.cook_or_attempt_id, normalized_args);
            }
            _ => None,
        },
        _ => None,
    };
    let Some(run_id) = run_id else {
        return Ok(None);
    };
    delegate_agent_task_lifecycle_to_resolved_runtime(run_id, normalized_args)
}

fn delegate_cook_continue_to_pinned_runtime(
    cook_or_attempt_id: &str,
    normalized_args: &[String],
) -> homeboy::core::Result<Option<i32>> {
    let run_id =
        crate::agents::agent_tasks::service::resolve_cook_continuation_run_id(cook_or_attempt_id)?;
    if current_runtime_owns_terminal_cook_continuation(&run_id)? {
        return Ok(None);
    }
    delegate_agent_task_lifecycle_to_resolved_runtime(&run_id, normalized_args)
}

enum AgentTaskLifecyclePinnedRuntime {
    Runner(crate::agents::agent_tasks::lifecycle::RunnerPinnedRuntime),
    Controller(std::path::PathBuf),
}

/// Select runner authority before validating a controller-local pin. Historical
/// runner records carry paths that are intentionally unavailable on controller.
fn agent_task_lifecycle_pinned_runtime_for_mutation(
    run_id: &str,
) -> homeboy::core::Result<Option<AgentTaskLifecyclePinnedRuntime>> {
    if let Some(pinned) =
        crate::agents::agent_tasks::lifecycle::runner_pinned_runtime_for_mutation(run_id)?
    {
        return Ok(Some(AgentTaskLifecyclePinnedRuntime::Runner(pinned)));
    }
    Ok(
        crate::agents::agent_tasks::lifecycle::pinned_runtime_for_mutation(run_id)?
            .map(AgentTaskLifecyclePinnedRuntime::Controller),
    )
}

fn delegate_agent_task_lifecycle_to_resolved_runtime(
    run_id: &str,
    normalized_args: &[String],
) -> homeboy::core::Result<Option<i32>> {
    match agent_task_lifecycle_pinned_runtime_for_mutation(run_id)? {
        Some(AgentTaskLifecyclePinnedRuntime::Runner(pinned)) => {
            delegate_agent_task_lifecycle_to_runner_pinned_runtime(&pinned, normalized_args)
                .map(Some)
        }
        Some(AgentTaskLifecyclePinnedRuntime::Controller(pinned)) => {
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
        None => Ok(None),
    }
}

fn delegate_agent_task_lifecycle_to_runner_pinned_runtime(
    pinned: &crate::agents::agent_tasks::lifecycle::RunnerPinnedRuntime,
    normalized_args: &[String],
) -> homeboy::core::Result<i32> {
    let mut command = vec![pinned.executable.display().to_string()];
    command.extend_from_slice(&normalized_args[1..]);
    let (output, exit_code) = homeboy::runner::exec(
        &pinned.runner_id,
        homeboy::runner::RunnerExecOptions {
            command,
            raw_exec: true,
            ..Default::default()
        },
    )
    .map_err(|error| annotate_runner_pinned_runtime_failure(error, &pinned.runner_id))?;
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }
    print!("{}", output.stdout);
    Ok(exit_code)
}

fn annotate_runner_pinned_runtime_failure(
    mut error: homeboy::core::Error,
    runner_id: &str,
) -> homeboy::core::Error {
    error.message = format!(
        "runner `{runner_id}` could not execute its pinned controller runtime: {}",
        error.message
    );
    if !error.details.is_object() {
        error.details = serde_json::json!({});
    }
    error.details["runner_id"] = serde_json::json!(runner_id);
    error.details["next_actions"] = serde_json::json!([
        format!("homeboy runner status {runner_id}"),
        format!("homeboy runner connect {runner_id}")
    ]);
    error.with_hint(format!(
        "Verify runner `{runner_id}` is reachable, then retry the exact durable command."
    ))
}

/// A terminal recipe-bound continuation is controller-owned. Provider execution
/// remains pinned, but harvest, artifact hydration, gates, and finalization must
/// run where the immutable Cook recipe is stored rather than on Lab.
fn current_runtime_owns_terminal_cook_continuation(run_id: &str) -> homeboy::core::Result<bool> {
    let Some(recipe) = crate::agents::agent_tasks::service::load_recipe_for_attempt(run_id)? else {
        return Ok(false);
    };
    let record = crate::agents::agent_tasks::service::persisted_status(run_id)?;
    if !matches!(
        record.state,
        crate::agents::agent_tasks::lifecycle::AgentTaskRunState::Succeeded
            | crate::agents::agent_tasks::lifecycle::AgentTaskRunState::CandidateRecoverable
            | crate::agents::agent_tasks::lifecycle::AgentTaskRunState::PartialRecoverable
    ) {
        return Ok(false);
    }
    crate::agents::agent_tasks::service::validate_recipe_attempt_record(&recipe, run_id, &record)?;
    Ok(true)
}

/// Fanout coordination is controller-owned and may span children from distinct
/// generations. It must not be offloaded wholesale; return exact child-local
/// continuations before any provider or coordinator work can begin.
fn guard_fanout_resume_runtime(cli: &Cli) -> homeboy::core::Result<()> {
    let Commands::AgentTask(agent_task) = &cli.command else {
        return Ok(());
    };
    let crate::commands::agent_task::AgentTaskCommand::Fanout(fanout) = &agent_task.command else {
        return Ok(());
    };
    let crate::commands::agent_task::args::AgentTaskFanoutCommand::Resume(args) = &fanout.command
    else {
        return Ok(());
    };
    let batch = crate::agents::agent_tasks::batch::read_batch_record(&args.batch_id)?;
    let mut actions = Vec::new();
    for child in &batch.child_runs {
        // Children without a recipe never reached cook admission, so preserve
        // the existing per-child recovery report instead of inventing a pin.
        if crate::agents::agent_tasks::service::load_recipe(&child.run_id).is_err() {
            continue;
        }
        let run_id =
            crate::agents::agent_tasks::service::resolve_cook_continuation_run_id(&child.run_id)?;
        if let Some(pinned) =
            crate::agents::agent_tasks::lifecycle::pinned_runtime_for_mutation(&run_id)?
        {
            actions.push(pinned_cook_continue_command(&pinned, &child.run_id));
        }
    }
    if actions.is_empty() {
        return Ok(());
    }
    let mut error = homeboy::core::Error::validation_invalid_argument(
        "batch_id",
        "fanout coordinator cannot resume children pinned to another controller runtime",
        Some(args.batch_id.clone()),
        None,
    );
    error.details["next_actions"] = serde_json::json!(actions);
    Err(error.with_retryable(true))
}

fn pinned_cook_continue_command(pinned: &std::path::Path, cook_id: &str) -> String {
    cook_continue_command(Some(&pinned.to_string_lossy()), cook_id, false, None)
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

fn run_promotion_provider_dispatch(
    command: &Commands,
    output_file: Option<&str>,
    identity: &output::CommandIdentity,
) -> Option<i32> {
    let Commands::AgentTask(args) = command else {
        return None;
    };
    let crate::commands::agent_task::AgentTaskCommand::PromotionProvider(args) = &args.command
    else {
        return None;
    };

    let (result, exit_code) =
        match crate::commands::agent_task::run::promotion_provider(args.clone()) {
            Ok((value, exit_code)) => (Ok(value), exit_code),
            Err(error) => (Err(error), 2),
        };
    output_runtime::emit_json_result_for_identity(result, output_file, exit_code, identity);
    Some(exit_code)
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
    if classify_command_capability(args) != CommandCapability::ReadOnly {
        return None;
    }

    match args {
        // Only Homeboy's own arguments can request help. A forwarded remote
        // command may legitimately contain `-h` (#11577).
        [_, rest @ ..]
            if homeboy_owned_args(rest)
                .iter()
                .any(|arg| arg == "--help" || arg == "-h") =>
        {
            Some(StartupFastPath::Help)
        }
        [_, flag] if flag == "--version" || flag == "-V" => Some(StartupFastPath::Version),
        [_, command, subcommand]
            if command == "self" && matches!(subcommand.as_str(), "identity" | "inspect") =>
        {
            Some(StartupFastPath::Identity)
        }
        _ => None,
    }
}

impl Default for CliRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Discovery with readiness probed. Only the safety manifest needs this: it is
/// the sole consumer of [`ExtensionCommandManifest::health`].
fn collect_extension_cli_info() -> ExtensionCliDiscovery {
    collect_extension_cli_info_with(ExtensionReadinessMode::Probe)
}

/// Discovery in metadata-only mode: no `ready_check` is spawned.
///
/// The extension-provided command surface comes from each installed manifest's
/// `cli` block, and broken-link health comes from `broken_extension_links()`.
/// Neither reads readiness, so the rendered `--help` surface and the augmented
/// parser are byte-identical either way (#10616).
fn collect_extension_cli_info_metadata_only() -> ExtensionCliDiscovery {
    collect_extension_cli_info_with(ExtensionReadinessMode::Skip)
}

fn collect_extension_cli_info_with(readiness: ExtensionReadinessMode) -> ExtensionCliDiscovery {
    let summaries = list_summaries_with(None, readiness);
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
    // An extension whose `ready_check` was never run is `unknown`, not `ready`.
    // `ExtensionReadinessMode::Skip` reports `ready: true` so that inventory
    // output is not mistaken for a *failed* probe, but a command-health
    // contract that copied that through would be asserting a readiness nobody
    // measured — the fail-open defect class in #10685. Report the absence of a
    // measurement as an absence. (#10616)
    let readiness_skipped = summary.ready_reason.as_deref() == Some(READY_CHECK_SKIPPED_REASON);

    let status = if summary.error.is_some() {
        "error"
    } else if !summary.compatible {
        "incompatible"
    } else if readiness_skipped {
        "unknown"
    } else if summary.ready {
        "ready"
    } else {
        "not_ready"
    };

    ExtensionCommandHealth {
        status: status.to_string(),
        ready: summary.ready && !readiness_skipped,
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

#[derive(Debug, serde::Serialize)]
struct LabInventoryAdmissionDiagnostic {
    observed_state: String,
    observed_at_ms: u128,
    source_freshness: Option<String>,
    refresh_attempted: bool,
    refreshed_state: Option<String>,
    refreshed_at_ms: Option<u128>,
    terminal_reason: &'static str,
    refresh_error_code: Option<String>,
    refresh_error: Option<String>,
}

/// Resolve the inventory used by a terminal resource-policy placement decision.
/// A stale projection gets one runner-owned live refresh; a fresh empty result
/// is already authoritative enough to refuse without opening another probe.
fn resolve_terminal_lab_inventory<F>(
    observed: crate::runner::runners::LabRunnerReadiness,
    observed_at_ms: u128,
    refresh: F,
) -> (
    crate::runner::runners::LabRunnerReadiness,
    LabInventoryAdmissionDiagnostic,
)
where
    F: FnOnce() -> crate::core::Result<(crate::runner::runners::LabRunnerReadiness, u128)>,
{
    use crate::runner::runners::LabRunnerReadinessState;

    let observed_state = observed.state.as_str().to_string();
    let source_freshness = Some(observed_state.clone());
    if observed.state != LabRunnerReadinessState::Stale {
        return (
            observed,
            LabInventoryAdmissionDiagnostic {
                observed_state,
                observed_at_ms,
                source_freshness,
                refresh_attempted: false,
                refreshed_state: None,
                refreshed_at_ms: None,
                terminal_reason: "no_ready_capacity",
                refresh_error_code: None,
                refresh_error: None,
            },
        );
    }

    match refresh() {
        Ok((refreshed, refreshed_at_ms)) => {
            let terminal_reason = if refreshed.state == LabRunnerReadinessState::ConnectedReady {
                "ready_after_refresh"
            } else if refreshed.state == LabRunnerReadinessState::Stale {
                "inventory_stale"
            } else {
                "no_ready_capacity"
            };
            let refreshed_state = Some(refreshed.state.as_str().to_string());
            (
                refreshed,
                LabInventoryAdmissionDiagnostic {
                    observed_state,
                    observed_at_ms,
                    source_freshness,
                    refresh_attempted: true,
                    refreshed_state,
                    refreshed_at_ms: Some(refreshed_at_ms),
                    terminal_reason,
                    refresh_error_code: None,
                    refresh_error: None,
                },
            )
        }
        Err(error) => {
            let timed_out = error.code == crate::core::ErrorCode::RemoteCommandTimeout;
            let refresh_error_code = error.code.as_str().to_string();
            (
                observed,
                LabInventoryAdmissionDiagnostic {
                    observed_state,
                    observed_at_ms,
                    source_freshness,
                    refresh_attempted: true,
                    refreshed_state: None,
                    refreshed_at_ms: Some(unix_timestamp_ms()),
                    terminal_reason: if timed_out {
                        "refresh_timeout"
                    } else {
                        "refresh_failed"
                    },
                    refresh_error_code: Some(refresh_error_code),
                    refresh_error: Some(error.message),
                },
            )
        }
    }
}

fn unix_timestamp_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn preflight_hot_command(
    cli: &Cli,
    output_file: Option<&str>,
    command_identity: &output::CommandIdentity,
) -> Option<i32> {
    preflight_hot_command_with(cli, output_file, command_identity, || {
        crate::commands::resources::run_preflight()
    })
}

fn preflight_hot_command_with(
    cli: &Cli,
    output_file: Option<&str>,
    command_identity: &output::CommandIdentity,
    preflight: impl FnOnce() -> crate::commands::CmdResult<crate::commands::resources::DoctorOutput>,
) -> Option<i32> {
    if let Some(hot_command) = resource_policy::hot_command(&cli.command) {
        if let Ok((resources, _)) = preflight() {
            let mut lab_readiness = if hot_command.lab_offload_supported {
                crate::runner::lab_runner_readiness().ok()
            } else {
                None
            };
            // Timestamp the projection after it has completed; it describes the
            // exact inventory consumed by the terminal placement decision.
            let lab_inventory_observed_at_ms = unix_timestamp_ms();
            let mut lab_inventory_diagnostic = None;
            // A cached/projection-based inventory is enough for normal routing,
            // but not for a terminal local-resource refusal. A stale inventory
            // receives exactly one bounded runner-owned refresh before placement.
            if hot_command.lab_offload_supported
                && cli.runner.is_none()
                && !matches!(cli.placement, crate::cli_surface::Placement::Local)
                && resource_policy::evaluate_with_runner_hint(
                    hot_command,
                    &resources,
                    lab_readiness.as_ref(),
                )
                .is_some()
            {
                if let Some(observed) = lab_readiness.take() {
                    let (resolved, diagnostic) = resolve_terminal_lab_inventory(
                        observed,
                        lab_inventory_observed_at_ms,
                        || {
                            let refreshed =
                                crate::runner::refresh_lab_runner_readiness_for_admission()?;
                            Ok((refreshed, unix_timestamp_ms()))
                        },
                    );
                    lab_readiness = Some(resolved);
                    lab_inventory_diagnostic = Some(diagnostic);
                }
            }
            // An explicit runner is a routing decision, not a default-runner
            // fallback. Let Lab offload report any runner-specific readiness or
            // capability failure rather than blocking it at controller preflight.
            let selected_lab_runner = resource_policy_runner_hint(
                cli,
                lab_readiness
                    .as_ref()
                    .and_then(|readiness| readiness.selected_runner_id.as_deref()),
            );
            let runner_admits_offload = if hot_command.allows_warm_runner_coordination {
                resource_policy::admits_warm_runner_coordination(
                    hot_command,
                    &resources,
                    selected_lab_runner
                        .filter(|_| !matches!(cli.placement, crate::cli_surface::Placement::Local)),
                    lab_readiness.as_ref(),
                )
            } else {
                hot_command.lab_offload_supported
                    && selected_lab_runner.is_some_and(|runner_id| {
                        lab_readiness.as_ref().is_some_and(|readiness| {
                            readiness.state
                                == crate::runner::runners::LabRunnerReadinessState::ConnectedReady
                                && readiness
                                    .available_runner_ids
                                    .iter()
                                    .any(|available| available == runner_id)
                        })
                    })
            };
            let runner_admits_offload = runner_admits_offload
                && selected_lab_runner.is_none_or(|runner_id| {
                    review_test_runner_requirements(cli).is_none_or(|requirements| {
                        crate::runner::runners::runner_capability_inventory(runner_id)
                            .map(|inventory| {
                                requirements.is_satisfied_by(
                                    &inventory.runtime_ids,
                                    &inventory.capabilities,
                                )
                            })
                            .unwrap_or(false)
                    })
                });
            // A detached Cook may be durably admitted to a connected reverse
            // runner that is temporarily full. The route rechecks that runner
            // and submits through the broker queue; this only bypasses the
            // controller-local resource refusal, never selects local execution.
            let runner_admits_offload = runner_admits_offload
                || (hot_command.allows_warm_runner_coordination
                    && cli.detach_after_handoff
                    && cli.runner.is_none()
                    && matches!(
                        cli.command,
                        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                            command: crate::commands::agent_task::AgentTaskCommand::Cook(_),
                        })
                    )
                    && crate::runner::refresh_detached_queue_runner()
                        .ok()
                        .flatten()
                        .is_some());
            let auto_local_capacity_fallback = resource_policy::admits_auto_local_capacity_fallback(
                hot_command,
                &resources,
                lab_readiness.as_ref(),
                cli.placement,
            );
            let explicit_runner_placement = explicit_runner_placement(cli, hot_command);
            // An explicit runner resolves workload placement before resource
            // guidance. Controller pressure still matters for handoff overhead,
            // but it must not be presented as a local workload warning.
            let warning = explicit_runner_placement
                .is_none()
                .then(|| {
                    resource_policy::evaluate_with_runner_hint(
                        hot_command,
                        &resources,
                        lab_readiness.as_ref(),
                    )
                })
                .flatten();
            let runner_hosted = resource_policy::is_runner_hosted_exec();
            if let Some(runner_id) = explicit_runner_placement {
                if let Some(notice) = resource_policy::explicit_runner_controller_notice(
                    hot_command,
                    &resources,
                    runner_id,
                ) {
                    eprintln!("{notice}");
                }
            }
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
                    cli.placement.is_explicit_local_override(),
                    auto_local_capacity_fallback,
                    lab_readiness.as_ref(),
                    runner_hosted,
                );
            if cli.runner.is_some()
                && hot_command.lab_offload_supported
                && !matches!(cli.placement, crate::cli_surface::Placement::Local)
            {
                resource_policy_context.runner_selection.reason = "explicit_lab_runner".to_string();
                resource_policy_context.runner_selection.runner_id = cli.runner.clone();
            }
            resource_policy::capture_context(resource_policy_context);
            if let Some(warning) = warning.as_ref() {
                if let Some(mut err) = resource_policy::non_interactive_preflight_error(
                    warning,
                    cli.placement.is_explicit_local_override() || runner_hosted,
                    is_interactive_shell(),
                    resource_policy::rerun_command(
                        hot_command,
                        &std::env::args().collect::<Vec<_>>(),
                        selected_lab_runner,
                    )
                    .or_else(|| {
                        // No ready runner means there is no placement rewrite,
                        // but the original request remains the deterministic
                        // resume input after the targeted recovery action.
                        Some(crate::core::engine::shell::quote_args(
                            &std::env::args().collect::<Vec<_>>(),
                        ))
                    }),
                    runner_admits_offload || auto_local_capacity_fallback,
                ) {
                    if let Some(diagnostic) = lab_inventory_diagnostic {
                        err.details["lab_inventory_admission"] = serde_json::to_value(diagnostic)
                            .expect("Lab inventory admission diagnostic serializes");
                    }
                    if review_test_deferred_workload_eligible(cli, warning, runner_admits_offload) {
                        return None;
                    }
                    output_runtime::emit_json_result_for_identity(
                        Err(err),
                        output_file,
                        2,
                        command_identity,
                    );
                    return Some(2);
                }
            }
        }
    }

    None
}

fn review_test_runner_requirements(
    cli: &Cli,
) -> Option<homeboy::deferred_workload::DeferredWorkloadRequirements> {
    let Commands::Review(review) = &cli.command else {
        return None;
    };
    let crate::commands::review::ReviewCommand::Test(test) = review.command.as_ref()? else {
        return None;
    };
    let contract = test.lab_contract();
    contract.is_portable().then(
        || homeboy::deferred_workload::DeferredWorkloadRequirements {
            required_runtimes: ["homeboy".to_string()].into(),
            required_capabilities: contract
                .extra_required_capabilities
                .iter()
                .map(|capability| (*capability).to_string())
                .collect(),
        },
    )
}

fn review_test_deferred_workload_eligible(
    cli: &Cli,
    warning: &resource_policy::ResourcePolicyWarning,
    runner_admits_offload: bool,
) -> bool {
    cli.runner.is_none()
        && !runner_admits_offload
        && matches!(
            warning.recommendation,
            crate::commands::resources::ResourceRecommendation::Warm
                | crate::commands::resources::ResourceRecommendation::Hot
        )
        && matches!(
            cli.placement,
            crate::cli_surface::Placement::Auto | crate::cli_surface::Placement::LabOrLocal
        )
        && matches!(
            &cli.command,
            Commands::Review(review)
                if matches!(review.command, Some(crate::commands::review::ReviewCommand::Test(_)))
                    && review.lab_contract().is_some_and(|contract| contract.is_portable())
        )
}

fn resource_policy_runner_hint<'a>(
    cli: &'a Cli,
    default_runner: Option<&'a str>,
) -> Option<&'a str> {
    cli.runner.as_deref().or(default_runner)
}

fn explicit_runner_placement(cli: &Cli, hot_command: resource_policy::HotCommand) -> Option<&str> {
    cli.runner.as_deref().filter(|_| {
        hot_command.lab_offload_supported
            && !matches!(cli.placement, crate::cli_surface::Placement::Local)
    })
}

fn run_startup_update_checks(command: &Commands) {
    // Startup update checks — skip for upgrade (it handles this itself).
    if !matches!(
        command,
        Commands::Upgrade(_) | Commands::Daemon(_) | Commands::SelfCmd(_)
    ) {
        homeboy_upgrade::upgrade::update_check::run_startup_check();
        homeboy_extension::update_check::run_startup_check();
    }
}

/// Validate the JSON-envelope output-file path. When the path is invalid,
/// emit the error envelope and return the process `ExitCode` the caller
/// should return; otherwise return `None` to continue.
fn output_file_path_exit_code(
    path: &str,
    command_identity: &output::CommandIdentity,
) -> Option<std::process::ExitCode> {
    if let Some(err) = output_runtime::validate_output_file_path(path) {
        output_runtime::emit_json_result_for_identity(Err(err), None, 2, command_identity);
        return Some(std::process::ExitCode::from(exit_code_to_u8(2)));
    }
    None
}

fn command_identity_from_matches(matches: &ArgMatches) -> output::CommandIdentity {
    let mut current = matches;
    let mut path = Vec::new();
    while let Some((name, subcommand)) = current.subcommand() {
        path.push(name.to_string());
        current = subcommand;
    }

    let Some(command) = path.first() else {
        return output::CommandIdentity::top_level("unknown");
    };
    let operation = path[1..].join(" ");
    if operation.is_empty() {
        output::CommandIdentity::top_level(command)
    } else {
        output::CommandIdentity::with_operation(command, operation)
    }
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
        || matches!(&cli.command, Commands::Runs(args) if args.is_artifacts())
        || is_runs_artifact_get_runner_option(normalized_args)
        || matches!(&cli.command, Commands::Runs(args) if args.is_artifact_get())
    {
        if let Commands::Runs(args) = &mut cli.command {
            cli.runner = args.absorb_global_runner_for_command_option(cli.runner.take());
        }
    }
}

/// Cook is re-executed by a pinned controller binary. Retain an explicit runner
/// from that exact argv even when a command-scoped Clap argument did not hydrate
/// the root global field used by admission and placement routing.
fn normalize_cook_runner_option(cli: &mut Cli, normalized_args: &[String]) {
    if cli.runner.is_some()
        || !matches!(
            &cli.command,
            Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                command: crate::commands::agent_task::AgentTaskCommand::Cook(_),
            })
        )
    {
        return;
    }
    cli.runner = explicit_runner_from_args(normalized_args);
}

fn explicit_runner_from_args(args: &[String]) -> Option<String> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            return None;
        }
        if arg == "--runner" {
            return args.next().cloned();
        }
        if let Some(runner_id) = arg.strip_prefix("--runner=") {
            return Some(runner_id.to_string());
        }
    }
    None
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

struct ArgumentMigration {
    command: &'static str,
    help_flag: &'static str,
    command_path: &'static [&'static str],
    historical_flags: &'static [&'static str],
    diagnostic: &'static str,
}

const ARGUMENT_MIGRATIONS: &[ArgumentMigration] = &[ArgumentMigration {
    command: "homeboy agent-task cook",
    help_flag: "--help-full",
    command_path: &["agent-task", "cook"],
    historical_flags: &["--provider", "--provider-id", "--dispatch-selector"],
    diagnostic: "executor selection now uses `--backend <backend>` and optional `--selector <provider-id>`.\n\
Example: `homeboy agent-task cook --backend opencode --selector opencode.agent-task-executor --to-worktree repo@branch --goal 'Describe the task' --verify 'cargo test' --no-finalize`\n\
List available executor providers: `homeboy agent-task providers`\n\
`--provider-argv` is promotion-only: it configures the deprecated promotion apply-provider invocation and cannot select an executor.",
}];

/// Attempt to augment a clap error with semantic argument migrations or entity suggestions.
/// Returns Some(augmented_message) when an actionable diagnostic is available.
fn try_augment_clap_error(
    e: &clap::Error,
    argv: &[String],
    extension_health: &ExtensionCliHealth,
) -> Option<String> {
    if let Some(output) = semantic_argument_migration_diagnostic(e, argv) {
        return Some(output);
    }

    // Extract unrecognized subcommand and parent command from error.
    let unrecognized = extract_unrecognized_from_error(e)?;
    let parent_command = extract_parent_command_from_error(e)?;

    let mut hints = command_domain_hints(&unrecognized, &parent_command).unwrap_or_else(|| {
        entity_suggest::find_entity_match(&unrecognized)
            .map(|entity_match| {
                entity_suggest::generate_entity_hints(&entity_match, &parent_command, &unrecognized)
            })
            .unwrap_or_default()
    });

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

/// Keep well-known control-plane terms out of persisted entity matching.
///
/// Entity IDs are user-defined, so a component can be a closer textual match
/// than the command domain the operator intended. These hints describe the
/// generic runner surface and deliberately require discovery before selecting
/// a runner ID.
fn command_domain_hints(unrecognized: &str, parent_command: &str) -> Option<Vec<String>> {
    if !parent_command.is_empty()
        || !(unrecognized.eq_ignore_ascii_case("lab")
            || homeboy::core::engine::text::levenshtein(&unrecognized.to_lowercase(), "runner")
                <= 2)
    {
        return None;
    }

    Some(vec![format!(
        "'{}' refers to the runner control plane. Discover runners with `homeboy runner list`, then inspect one with `homeboy runner status <runner-id>`",
        unrecognized
    )])
}

fn semantic_argument_migration_diagnostic(e: &clap::Error, argv: &[String]) -> Option<String> {
    if e.kind() != clap::error::ErrorKind::UnknownArgument {
        return None;
    }

    let migration = ARGUMENT_MIGRATIONS.iter().find(|migration| {
        argv.windows(migration.command_path.len()).any(|command| {
            command
                .iter()
                .map(String::as_str)
                .eq(migration.command_path.iter().copied())
        })
    })?;
    let argument = argv.iter().find_map(|argument| {
        migration
            .historical_flags
            .iter()
            .find(|flag| {
                argument.as_str() == **flag
                    || argument
                        .strip_prefix(**flag)
                        .is_some_and(|suffix| suffix.starts_with('='))
            })
            .map(|flag| (*flag).to_string())
    })?;

    Some(format!(
        "error: historical executor selection flag '{argument}' is not supported\n\nhint: {}\n\nFor more information, try '{} {}'",
        migration.diagnostic, migration.command, migration.help_flag
    ))
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
    use sha2::{Digest, Sha256};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn lab_readiness(
        state: crate::runner::runners::LabRunnerReadinessState,
    ) -> crate::runner::runners::LabRunnerReadiness {
        crate::runner::runners::LabRunnerReadiness {
            state,
            selected_runner_id: (state
                == crate::runner::runners::LabRunnerReadinessState::ConnectedReady)
                .then(|| "lab-a".to_string()),
            available_runner_ids: (state
                == crate::runner::runners::LabRunnerReadinessState::ConnectedReady)
                .then(|| vec!["lab-a".to_string()])
                .unwrap_or_default(),
            reasons: Vec::new(),
            remediation_commands: Vec::new(),
        }
    }

    fn hot_resources() -> crate::commands::resources::DoctorOutput {
        use crate::commands::resources::{
            DoctorOutput, LoadSummary, ProcessSummary, ResourceRecommendation, RigLeaseSummary,
        };

        DoctorOutput {
            command: "self.resources",
            recommendation: ResourceRecommendation::Warm,
            load: LoadSummary {
                one: Some(4.0),
                five: Some(4.0),
                fifteen: Some(4.0),
                cpu_count: 2,
                recommendation: ResourceRecommendation::Warm,
            },
            memory: None,
            processes: ProcessSummary {
                relevant_count: 0,
                top_cpu: Vec::new(),
                top_rss: Vec::new(),
                recommendation: ResourceRecommendation::Ok,
            },
            rig_leases: RigLeaseSummary {
                active_count: 0,
                concurrency_limit: None,
                leases: Vec::new(),
                recommendation: ResourceRecommendation::Ok,
            },
            notes: Vec::new(),
        }
    }

    fn cook_hot_command() -> resource_policy::HotCommand {
        resource_policy::HotCommand {
            label: "agent-task cook/run-plan/retry --run",
            lab_offload_supported: true,
            lab_offload_unsupported_reason: None,
            allows_warm_runner_coordination: true,
            offload_only_when_hot: false,
        }
    }

    #[test]
    fn cook_preview_never_consults_resource_preflight() {
        for placement in [
            vec!["--placement", "lab"],
            vec!["--placement", "auto"],
            vec!["--runner", "homeboy-lab"],
        ] {
            let mut args = vec!["homeboy"];
            args.extend(placement.iter().copied());
            args.extend([
                "agent-task",
                "cook",
                "--preview",
                "--prompt",
                "inspect the task",
                "--to-worktree",
                "fixture@preview",
                "--verify",
                "true",
            ]);
            let cli = Cli::parse_from(args);

            assert_eq!(
                preflight_hot_command_with(
                    &cli,
                    None,
                    &output::CommandIdentity::with_operation("agent-task", "cook"),
                    || -> crate::commands::CmdResult<crate::commands::resources::DoctorOutput> {
                        panic!("Cook preview must not run resource preflight")
                    },
                ),
                None,
                "{placement:?}"
            );
        }
    }

    #[test]
    fn stale_inventory_refreshes_once_and_routes_newly_ready_capacity() {
        let (resolved, diagnostic) = resolve_terminal_lab_inventory(
            lab_readiness(crate::runner::runners::LabRunnerReadinessState::Stale),
            100,
            || {
                Ok((
                    lab_readiness(crate::runner::runners::LabRunnerReadinessState::ConnectedReady),
                    200,
                ))
            },
        );

        assert_eq!(
            resolved.state,
            crate::runner::runners::LabRunnerReadinessState::ConnectedReady
        );
        assert_eq!(resolved.selected_runner_id.as_deref(), Some("lab-a"));
        assert!(diagnostic.refresh_attempted);
        assert_eq!(diagnostic.observed_at_ms, 100);
        assert_eq!(diagnostic.source_freshness.as_deref(), Some("stale"));
        assert_eq!(diagnostic.refreshed_at_ms, Some(200));
        assert_eq!(diagnostic.terminal_reason, "ready_after_refresh");
    }

    #[test]
    fn stale_inventory_refresh_to_empty_reports_confirmed_no_ready_capacity() {
        let (resolved, diagnostic) = resolve_terminal_lab_inventory(
            lab_readiness(crate::runner::runners::LabRunnerReadinessState::Stale),
            100,
            || {
                Ok((
                    lab_readiness(crate::runner::runners::LabRunnerReadinessState::CapacityBlocked),
                    200,
                ))
            },
        );

        assert_eq!(
            resolved.state,
            crate::runner::runners::LabRunnerReadinessState::CapacityBlocked
        );
        assert_eq!(
            diagnostic.refreshed_state.as_deref(),
            Some("capacity_blocked")
        );
        assert_eq!(diagnostic.terminal_reason, "no_ready_capacity");
    }

    #[test]
    fn stale_inventory_refresh_timeout_is_reported_without_local_fallback() {
        let (resolved, diagnostic) = resolve_terminal_lab_inventory(
            lab_readiness(crate::runner::runners::LabRunnerReadinessState::Stale),
            100,
            || {
                Err(crate::core::Error::new(
                    crate::core::ErrorCode::RemoteCommandTimeout,
                    "runner stopped answering",
                    serde_json::Value::Null,
                ))
            },
        );

        assert_eq!(
            resolved.state,
            crate::runner::runners::LabRunnerReadinessState::Stale
        );
        assert_eq!(diagnostic.terminal_reason, "refresh_timeout");
        assert!(diagnostic.refresh_attempted);
        assert_eq!(
            diagnostic.refresh_error_code.as_deref(),
            Some("remote.command_timeout")
        );
        assert!(diagnostic.refresh_error.is_some());
    }

    #[test]
    fn fresh_empty_inventory_does_not_open_a_refresh_probe() {
        let (resolved, diagnostic) = resolve_terminal_lab_inventory(
            lab_readiness(crate::runner::runners::LabRunnerReadinessState::CapacityBlocked),
            100,
            || -> crate::core::Result<_> { panic!("fresh inventory must not refresh") },
        );

        assert_eq!(
            resolved.state,
            crate::runner::runners::LabRunnerReadinessState::CapacityBlocked
        );
        assert!(!diagnostic.refresh_attempted);
        assert_eq!(diagnostic.observed_at_ms, 100);
        assert_eq!(diagnostic.terminal_reason, "no_ready_capacity");
    }

    #[test]
    fn preflight_resource_policy_routes_stale_inventory_to_refreshed_lab_capacity() {
        let resources = hot_resources();
        let (readiness, diagnostic) = resolve_terminal_lab_inventory(
            lab_readiness(crate::runner::runners::LabRunnerReadinessState::Stale),
            100,
            || {
                Ok((
                    lab_readiness(crate::runner::runners::LabRunnerReadinessState::ConnectedReady),
                    200,
                ))
            },
        );

        assert_eq!(diagnostic.terminal_reason, "ready_after_refresh");
        assert!(resource_policy::admits_warm_runner_coordination(
            cook_hot_command(),
            &resources,
            readiness.selected_runner_id.as_deref(),
            Some(&readiness),
        ));

        // This is the exact routing capture boundary used by
        // `preflight_hot_command`: the refreshed selection must survive beyond
        // admission and become the auto-placement input consumed by dispatch.
        resource_policy::reset_captured_context_for_test();
        let warning = resource_policy::evaluate_with_runner_hint(
            cook_hot_command(),
            &resources,
            Some(&readiness),
        );
        resource_policy::capture_context(resource_policy::resource_policy_context_from_evaluation(
            cook_hot_command(),
            &resources,
            warning.as_ref(),
            false,
            false,
            Some(&readiness),
            false,
        ));
        let captured = resource_policy::captured_context().expect("preflight routing capture");
        assert_eq!(
            captured.runner_selection.runner_id.as_deref(),
            Some("lab-a")
        );
        assert_eq!(captured.runner_selection.reason, "default_lab_runner");
        resource_policy::reset_captured_context_for_test();
    }

    #[test]
    fn preflight_resource_policy_keeps_timeout_and_fresh_empty_local_fallback_closed() {
        let resources = hot_resources();
        let (timed_out, timeout_diagnostic) = resolve_terminal_lab_inventory(
            lab_readiness(crate::runner::runners::LabRunnerReadinessState::Stale),
            100,
            || {
                Err(crate::core::Error::new(
                    crate::core::ErrorCode::RemoteCommandTimeout,
                    "runner stopped answering",
                    serde_json::Value::Null,
                ))
            },
        );
        let (fresh_empty, fresh_diagnostic) = resolve_terminal_lab_inventory(
            lab_readiness(crate::runner::runners::LabRunnerReadinessState::CapacityBlocked),
            300,
            || -> crate::core::Result<_> { panic!("fresh inventory must not refresh") },
        );

        assert_eq!(timeout_diagnostic.terminal_reason, "refresh_timeout");
        assert_eq!(fresh_diagnostic.terminal_reason, "no_ready_capacity");
        for readiness in [&timed_out, &fresh_empty] {
            assert!(!resource_policy::admits_warm_runner_coordination(
                cook_hot_command(),
                &resources,
                readiness.selected_runner_id.as_deref(),
                Some(readiness),
            ));
            assert!(!resource_policy::admits_auto_local_capacity_fallback(
                cook_hot_command(),
                &resources,
                Some(readiness),
                crate::cli_surface::Placement::Auto,
            ));
        }
    }

    #[test]
    fn command_identity_uses_the_resolved_top_level_and_nested_path() {
        let matches = Cli::command_with_scoped_lab_args()
            .try_get_matches_from(["homeboy", "agent-task", "cook", "--to-worktree", "fixture"])
            .expect("parse nested command");
        assert_eq!(
            command_identity_from_matches(&matches),
            output::CommandIdentity::with_operation("agent-task", "cook")
        );

        let matches = Cli::command_with_scoped_lab_args()
            .try_get_matches_from(["homeboy", "runner", "status", "local"])
            .expect("parse another nested command");
        assert_eq!(
            command_identity_from_matches(&matches),
            output::CommandIdentity::with_operation("runner", "status")
        );
    }

    #[test]
    fn every_clap_self_subcommand_uses_canonical_command_identity() {
        let command = Cli::command_with_scoped_lab_args();
        let self_command = command
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == "self")
            .expect("self command");

        for subcommand in self_command.get_subcommands() {
            let argv: Vec<&str> = match subcommand.get_name() {
                "status" | "identity" | "doctor" | "cleanup-runtime-tmp" => {
                    vec!["homeboy", "self", subcommand.get_name()]
                }
                "service-supervisor-worker" | "postprocess-worker" => vec![
                    "homeboy",
                    "self",
                    subcommand.get_name(),
                    "--request",
                    "request.json",
                ],
                "upgrade-admission" => vec![
                    "homeboy",
                    "self",
                    "upgrade-admission",
                    "--legacy-identity",
                    "legacy",
                ],
                // A topic is intentionally supplied so this covers the direct
                // markdown `self docs <topic>` surface, not only `docs map`.
                "docs" => vec!["homeboy", "self", "docs", "commands/self"],
                name => panic!("self subcommand `{name}` needs an identity fixture"),
            };
            let matches = Cli::command_with_scoped_lab_args()
                .try_get_matches_from(&argv)
                .expect("parse self subcommand");
            assert_eq!(
                command_identity_from_matches(&matches),
                output::CommandIdentity::with_operation("self", subcommand.get_name()),
                "argv: {argv:?}"
            );
        }

        let matches = Cli::command_with_scoped_lab_args()
            .try_get_matches_from(["homeboy", "self", "inspect"])
            .expect("parse self identity alias");
        assert_eq!(
            command_identity_from_matches(&matches),
            output::CommandIdentity::with_operation("self", "identity")
        );
    }

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

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
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

    #[cfg(unix)]
    fn write_notification_resolver(home: &std::path::Path, sentinel: &std::path::Path) {
        let extension_dir = home.join(".config/homeboy/extensions/resolver-test");
        std::fs::create_dir_all(&extension_dir).expect("resolver extension dir");
        std::fs::write(
            extension_dir.join("resolver-test.json"),
            serde_json::json!({
                "name": "Resolver test",
                "version": "0.0.0",
                "notification_transports": [{
                    "id": "test.completed",
                    "command": ["true"],
                    "route_resolver": {
                        "command": ["sh", "-c", format!("touch '{}'", sentinel.display())]
                    }
                }]
            })
            .to_string(),
        )
        .expect("resolver extension manifest");
    }

    /// A CLI extension whose `ready_check` leaves a durable trace when it runs.
    ///
    /// "no probe happened" is only provable if the probe would have been
    /// observable, so the sentinel file is the control: a test that asserts the
    /// sentinel is absent is worthless unless another assertion shows the same
    /// fixture creates it under [`ExtensionReadinessMode::Probe`].
    fn write_cli_extension_with_ready_check(
        home: &std::path::Path,
        id: &str,
        tool: &str,
        sentinel: &std::path::Path,
    ) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": "Probe Trace Extension",
                "version": "0.0.0",
                "cli": {
                    "tool": tool,
                    "display_name": "Probe CLI",
                    "command_template": "{{cliPath}} {{args}}"
                },
                "executable": {
                    "runtime": {
                        "ready_check": format!("touch '{}'", sentinel.display())
                    }
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

    #[cfg(unix)]
    fn write_audit_extension(home: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let extension_dir = home.join(".config/homeboy/extensions/sample-audit");
        let scripts_dir = extension_dir.join("scripts");
        std::fs::create_dir_all(&scripts_dir).expect("audit extension scripts dir");
        std::fs::write(
            extension_dir.join("sample-audit.json"),
            serde_json::json!({
                "name": "Sample Audit",
                "version": "0.0.0",
                "provides": {
                    "file_extensions": ["sample"],
                    "capabilities": ["fingerprint"]
                },
                "scripts": { "fingerprint": "scripts/fingerprint.sh" }
            })
            .to_string(),
        )
        .expect("audit extension manifest");
        std::fs::write(
            extension_dir.join("grammar.json"),
            serde_json::json!({
                "language": { "id": "sample", "extensions": ["sample"] },
                "comments": { "line": ["//"], "block": [], "doc": [] },
                "strings": { "quotes": ["\""], "escape": "\\", "multiline": [] },
                "patterns": {
                    "function": {
                        "regex": "fn\\s+(\\w+)\\s*\\(([^)]*)\\)",
                        "captures": { "name": 1, "params": 2 },
                        "context": "any",
                        "skip_comments": true,
                        "skip_strings": true
                    }
                }
            })
            .to_string(),
        )
        .expect("audit extension grammar");
        let script = scripts_dir.join("fingerprint.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' '{\"aggregate_literals\":[{\"type_name\":\"Policy\",\"fields\":[\"allow\"],\"line\":1}]}'\n",
        )
        .expect("audit fingerprint script");
        let mut permissions = std::fs::metadata(&script)
            .expect("audit fingerprint script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).expect("executable audit fingerprint script");
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

    #[cfg(unix)]
    #[test]
    fn ambient_notification_discovery_is_gated_to_valid_cook_execution() {
        crate::test_support::with_isolated_home(|home| {
            let sentinel = home.path().join("resolver-invoked");
            write_notification_resolver(home.path(), &sentinel);

            let status = CliRuntime::new().run_from_args(argv(&["homeboy", "status"]));
            assert_eq!(status, std::process::ExitCode::SUCCESS);
            assert!(
                !sentinel.exists(),
                "non-Cook commands must not invoke resolvers"
            );

            let cook = CliRuntime::new().run_from_args(argv(&[
                "homeboy",
                "agent-task",
                "cook",
                "--to-worktree",
                "fixture",
            ]));
            assert_eq!(cook, std::process::ExitCode::from(2));
            assert!(
                !sentinel.exists(),
                "Cook validation failures must precede ambient discovery"
            );
        });
    }

    #[test]
    fn runner_exec_rejects_inherited_options_before_runtime_initialization() {
        for option in [
            "--output",
            "--notification-transport",
            "--notification-route",
        ] {
            let exit = CliRuntime::new().run_from_args(vec![
                "homeboy".to_string(),
                "runner".to_string(),
                "exec".to_string(),
                "lab".to_string(),
                "cp".to_string(),
                "source".to_string(),
                "destination".to_string(),
                option.to_string(),
                "value".to_string(),
            ]);

            assert_eq!(exit, std::process::ExitCode::from(2));
        }
    }

    #[test]
    fn startup_fast_path_matches_explicit_help_at_every_command_depth() {
        assert_eq!(
            startup_fast_path(&argv(&["homeboy", "--help"])),
            Some(StartupFastPath::Help)
        );
        assert_eq!(
            startup_fast_path(&argv(&["homeboy", "-h"])),
            Some(StartupFastPath::Help)
        );
        assert_eq!(
            startup_fast_path(&argv(&["homeboy", "--version"])),
            Some(StartupFastPath::Version)
        );
        assert_eq!(
            startup_fast_path(&argv(&["homeboy", "-V"])),
            Some(StartupFastPath::Version)
        );
        assert_eq!(
            startup_fast_path(&argv(&["homeboy", "status", "--help"])),
            Some(StartupFastPath::Help)
        );
        assert_eq!(
            startup_fast_path(&argv(&["homeboy", "agent-task", "cook", "--help"])),
            Some(StartupFastPath::Help)
        );
    }

    /// Explicit help is rendered before runtime registration, config loading,
    /// controller consultation, or deferred-worker recovery. Clap still decides
    /// whether the flag is actually help rather than an option value.
    #[test]
    fn explicit_subcommand_help_uses_the_startup_fast_path() {
        for values in [
            &["homeboy", "status", "--help"][..],
            &["homeboy", "status", "-h"][..],
            &["homeboy", "extension", "list", "--help"][..],
            &["homeboy", "agent-task", "cook", "--help"][..],
        ] {
            assert!(
                matches!(
                    startup_fast_path_output(&argv(values)),
                    Some(StartupFastPathOutput::Help(_))
                ),
                "{values:?} should render before runtime initialization"
            );
        }

        assert!(startup_fast_path_output(&argv(&["homeboy", "help", "status"])).is_none());
    }

    #[test]
    fn version_fast_path_reports_the_build_version_without_extension_discovery() {
        match startup_fast_path_output(&argv(&["homeboy", "--version"])) {
            Some(StartupFastPathOutput::Version(version)) => {
                assert_eq!(version, upgrade::current_build_version());
            }
            _ => panic!("`homeboy --version` should resolve to the version fast path"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn cli_startup_registers_both_extension_audit_providers() {
        crate::test_support::with_isolated_home(|home| {
            write_audit_extension(home.path());

            let exit = CliRuntime::new()
                .run_from_args(vec!["homeboy".to_string(), "--version".to_string()]);

            assert_eq!(exit, std::process::ExitCode::SUCCESS);
            let grammar = crate::core::code_audit::core_fingerprint::load_grammar_for_ext("sample")
                .expect("CLI startup should make the extension grammar provider reachable");
            assert_eq!(grammar.language.id, "sample");

            let source = home.path().join("policy.sample");
            let fingerprint = crate::core::code_audit::fingerprint::fingerprint_content(
                &source,
                home.path(),
                "fn decide() {}",
            )
            .expect("CLI startup should make both audit providers reachable");
            assert_eq!(fingerprint.methods, vec!["decide"]);
            assert_eq!(fingerprint.aggregate_literals.len(), 1);
            assert_eq!(fingerprint.aggregate_literals[0].type_name, "Policy");
        });
    }

    /// Renders the help the binary actually prints for `argv`, by driving the
    /// real startup entry point. Asserting through `startup_fast_path_output`
    /// (instead of calling `build_augmented_command` directly) is what makes
    /// these tests fail if root help ever regresses to the static command.
    fn root_help_for(argv_values: &[&str]) -> String {
        match startup_fast_path_output(&argv(argv_values)) {
            Some(StartupFastPathOutput::Help(help)) => help,
            _ => panic!("{argv_values:?} should resolve to the root-help startup fast path"),
        }
    }

    #[test]
    fn root_help_lists_extension_provided_commands() {
        crate::test_support::with_isolated_home(|home| {
            write_cli_extension(home.path(), "sample-runtime", "sample-cli");

            for flag in ["--help", "-h"] {
                let help = root_help_for(&["homeboy", flag]);

                assert!(
                    help.contains("Extension-provided commands: sample-cli"),
                    "`homeboy {flag}` should advertise extension-provided commands: {help}"
                );
                assert!(
                    help.contains("sample-cli"),
                    "`homeboy {flag}` should list the extension subcommand: {help}"
                );
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn root_help_warns_about_broken_extension_links_without_paths() {
        crate::test_support::with_isolated_home(|home| {
            write_cli_extension(home.path(), "sample-runtime", "sample-cli");
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            let link = extensions_dir.join("stale-runtime");
            let target = extensions_dir.join("missing-stale-runtime");
            std::os::unix::fs::symlink(&target, &link).expect("broken extension link");

            let help = root_help_for(&["homeboy", "--help"]);

            assert!(
                help.contains(
                    "Extension health warning: 1 broken extension link(s): stale-runtime"
                ),
                "root help should warn about broken extension links: {help}"
            );
            assert!(help.contains("homeboy extension list"));
            assert!(help.contains("homeboy extension relink <id> <path>"));
            assert!(!help.contains("/missing-stale-runtime"));
        });
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

        let output = try_augment_clap_error(
            &err,
            &["homeboy".to_string(), "sample-cli".to_string()],
            &health,
        )
        .expect("extension health hint");

        assert!(output.contains("extension-provided commands may be unavailable"));
        assert!(output.contains("broken extension link(s): sample-runtime"));
        assert!(output.contains("homeboy extension list"));
    }

    #[test]
    fn top_level_lab_prefers_generic_runner_guidance_over_component_matching() {
        crate::test_support::with_isolated_home(|home| {
            entity_suggest::reset_entity_suggestion_cache_for_test();
            homeboy::core::component::write_standalone_registration(
                &homeboy::core::component::Component {
                    id: "chat".to_string(),
                    local_path: home.path().display().to_string(),
                    ..Default::default()
                },
            )
            .expect("register component matching lab typo distance");

            let err = build_augmented_command(&[], &ExtensionCliHealth::default())
                .try_get_matches_from(["homeboy", "lab"])
                .expect_err("lab is not a top-level command");
            let output = try_augment_clap_error(
                &err,
                &argv(&["homeboy", "lab"]),
                &ExtensionCliHealth::default(),
            )
            .expect("Lab should have a control-plane hint");

            assert!(output.contains("homeboy runner list"));
            assert!(output.contains("homeboy runner status <runner-id>"));
            assert!(!output.contains("component 'chat'"));
        });
    }

    #[test]
    fn top_level_runner_misspellings_prefer_runner_guidance() {
        for command in ["runer", "runnr"] {
            let err = build_augmented_command(&[], &ExtensionCliHealth::default())
                .try_get_matches_from(["homeboy", command])
                .expect_err("misspelled runner command should not parse");
            let output = try_augment_clap_error(
                &err,
                &argv(&["homeboy", command]),
                &ExtensionCliHealth::default(),
            )
            .expect("runner typo should have a control-plane hint");

            assert!(
                output.contains("homeboy runner list"),
                "{command}: {output}"
            );
            assert!(
                output.contains("homeboy runner status <runner-id>"),
                "{command}: {output}"
            );
        }
    }

    #[test]
    fn unrelated_top_level_tokens_still_suggest_matching_components() {
        crate::test_support::with_isolated_home(|home| {
            entity_suggest::reset_entity_suggestion_cache_for_test();
            homeboy::core::component::write_standalone_registration(
                &homeboy::core::component::Component {
                    id: "catalog".to_string(),
                    local_path: home.path().display().to_string(),
                    ..Default::default()
                },
            )
            .expect("register component");

            let err = build_augmented_command(&[], &ExtensionCliHealth::default())
                .try_get_matches_from(["homeboy", "catlog"])
                .expect_err("component typo is not a top-level command");
            let output = try_augment_clap_error(
                &err,
                &argv(&["homeboy", "catlog"]),
                &ExtensionCliHealth::default(),
            )
            .expect("component typo should have an entity hint");

            assert!(output.contains("component 'catalog'"));
            assert!(output.contains("homeboy component catalog"));
        });
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

    /// `--help` is the hottest command in the CLI and needs zero readiness
    /// information to render. Discovery on that path must spawn nothing. (#10616)
    #[test]
    fn runtime_discovery_never_spawns_a_ready_check() {
        crate::test_support::with_isolated_home(|home| {
            let sentinel = home.path().join("ready-check-ran");
            write_cli_extension_with_ready_check(
                home.path(),
                "probe-runtime",
                "probe-cli",
                &sentinel,
            );

            let discovery = collect_extension_cli_info_metadata_only();

            assert!(
                !sentinel.exists(),
                "metadata-only discovery must not spawn an extension ready_check"
            );
            // The extension command surface is still fully discovered.
            assert_eq!(discovery.info.len(), 1);
            assert_eq!(discovery.info[0].tool, "probe-cli");

            // Control: the same fixture under Probe *does* run the check, so
            // the assertion above measures a real suppression rather than a
            // ready_check that never worked.
            let _probed = collect_extension_cli_info();
            assert!(
                sentinel.exists(),
                "control: probe mode must actually run the ready_check"
            );
        });
    }

    /// The rendered `--help` surface is a wire protocol: leaseless-recovery
    /// negotiation parses it and a docs gate pins it. Skipping the probe must
    /// therefore change no rendered byte. (#10616)
    #[test]
    fn metadata_only_discovery_renders_identical_help_to_probed_discovery() {
        crate::test_support::with_isolated_home(|home| {
            let sentinel = home.path().join("ready-check-ran");
            write_cli_extension_with_ready_check(
                home.path(),
                "probe-runtime",
                "probe-cli",
                &sentinel,
            );

            let skipped = collect_extension_cli_info_metadata_only();
            let probed = collect_extension_cli_info();

            let skipped_help = build_augmented_command(&skipped.info, &skipped.health)
                .render_long_help()
                .to_string();
            let probed_help = build_augmented_command(&probed.info, &probed.health)
                .render_long_help()
                .to_string();

            assert_eq!(
                skipped_help, probed_help,
                "readiness must not affect the rendered help surface"
            );
            assert!(
                skipped_help.contains("probe-cli"),
                "control: the extension command must appear in help at all"
            );
        });
    }

    /// An extension whose `ready_check` was never run is `unknown`, not `ready`.
    /// Reporting an unmeasured probe as success is the fail-open defect class
    /// tracked in #10685.
    #[test]
    fn unprobed_extension_command_health_is_unknown_rather_than_ready() {
        crate::test_support::with_isolated_home(|home| {
            let sentinel = home.path().join("ready-check-ran");
            write_cli_extension_with_ready_check(
                home.path(),
                "probe-runtime",
                "probe-cli",
                &sentinel,
            );

            let discovery = collect_extension_cli_info_metadata_only();
            let health = &discovery.info[0]
                .descriptor
                .extension
                .as_ref()
                .expect("extension manifest")
                .health;

            assert_eq!(health.status, "unknown");
            assert!(
                !health.ready,
                "an unprobed ready_check must not be reported as ready"
            );
            assert_eq!(health.reason.as_deref(), Some(READY_CHECK_SKIPPED_REASON));
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
    fn runs_artifacts_command_local_runner_is_routable_for_runner_only_and_mirrored_runs() {
        let _env = EnvGuard::remove(crate::core::observation::LAB_OFFLOAD_METADATA_ENV);

        for run_id in ["runner-only-run", "mirrored-run"] {
            let mut cli = Cli::parse_from([
                "homeboy",
                "runs",
                "artifacts",
                run_id,
                "--runner",
                "homeboy-lab",
            ]);
            let argv = vec![
                "homeboy".into(),
                "runs".into(),
                "artifacts".into(),
                run_id.into(),
                "--runner".into(),
                "homeboy-lab".into(),
            ];

            // Clap initially hydrates the global field. Normalize it into the
            // command-local query option before generic Lab routing runs.
            normalize_runs_runner_options(&mut cli, &argv);

            assert_eq!(
                cli.runner, None,
                "{run_id} must not enter Lab offload routing"
            );
            let Commands::Runs(args) = &cli.command else {
                panic!("expected runs command");
            };
            assert!(args.is_artifacts());
            assert_eq!(args.artifacts_runner(), Some("homeboy-lab"));
            crate::commands::route::route_after_parse(&cli, &argv, None)
                .expect("command-local runner query is accepted");
        }
    }

    #[test]
    fn runs_artifacts_without_runner_remains_a_local_query() {
        let _env = EnvGuard::remove(crate::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let mut cli = Cli::parse_from(["homeboy", "runs", "artifacts", "local-run"]);
        let argv = vec![
            "homeboy".into(),
            "runs".into(),
            "artifacts".into(),
            "local-run".into(),
        ];

        normalize_runs_runner_options(&mut cli, &argv);

        assert_eq!(cli.runner, None);
        let Commands::Runs(args) = &cli.command else {
            panic!("expected runs command");
        };
        assert!(args.is_artifacts());
        assert_eq!(args.artifacts_runner(), None);
        assert!(!args.has_command_local_runner_option());
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

    /// One compact-output convention must work across the `review` umbrella and
    /// every phase subcommand. Before #10428 `review lint --summary` parsed while
    /// `review test --summary` failed and clap suggested `-- --summary`, which
    /// would have forwarded the flag to the underlying test runner instead.
    ///
    /// `--summary` is the umbrella's own compact-output selector, and
    /// `review/mod.rs` already maps it onto `json_summary` for the test and audit
    /// phases and onto `summary` for lint — so accepting it directly on each
    /// phase makes the phase surface agree with the umbrella that drives it.
    #[test]
    fn summary_is_accepted_across_the_review_umbrella_and_every_phase() {
        let phases: &[&[&str]] = &[
            &["homeboy", "review", "lint", "homeboy", "--summary"],
            &["homeboy", "review", "test", "homeboy", "--summary"],
            &["homeboy", "review", "audit", "homeboy", "--summary"],
            &["homeboy", "review", "lint", "homeboy", "--json-summary"],
            &["homeboy", "review", "test", "homeboy", "--json-summary"],
            &["homeboy", "review", "audit", "homeboy", "--json-summary"],
        ];

        for args in phases {
            Cli::try_parse_from(*args)
                .unwrap_or_else(|err| panic!("{args:?} must parse, got: {err}"));
        }

        // The umbrella's own canonical spelling keeps working unchanged.
        Cli::try_parse_from(["homeboy", "review", "homeboy", "--summary"])
            .expect("review --summary must parse");
    }

    /// The alias must select Homeboy output formatting, never leak into the
    /// runner passthrough after `--`.
    #[test]
    fn summary_alias_sets_the_same_field_as_json_summary() {
        for spelling in ["--summary", "--json-summary"] {
            let cli = Cli::try_parse_from(["homeboy", "review", "test", "homeboy", spelling])
                .expect("parse review test summary flag");
            let Commands::Review(review) = cli.command else {
                panic!("expected a review command");
            };
            let Some(crate::commands::review::ReviewCommand::Test(args)) = review.command else {
                panic!("expected a review test subcommand");
            };
            assert!(args.json_summary, "{spelling} must set json_summary");
            assert!(
                args.args.is_empty(),
                "{spelling} must not leak into runner passthrough args"
            );
        }
    }

    #[test]
    fn explicit_runner_preserves_lab_routing_for_hot_cook_and_review_commands() {
        let cases: &[(&[&str], &str)] = &[
            (
                &[
                    "homeboy",
                    "--runner",
                    "homeboy-lab",
                    "agent-task",
                    "cook",
                    "--to-worktree",
                    "homeboy@fix-explicit-runner",
                    "--prompt",
                    "fix the issue",
                    "--verify",
                    "true",
                ],
                "agent-task cook/run-plan/retry --run",
            ),
            (
                &["homeboy", "--runner", "homeboy-lab", "review", "audit"],
                "review audit",
            ),
            (
                &[
                    "homeboy",
                    "--runner",
                    "homeboy-lab",
                    "review",
                    "lint",
                    "homeboy",
                ],
                "review lint",
            ),
            (
                &[
                    "homeboy",
                    "--runner",
                    "homeboy-lab",
                    "review",
                    "test",
                    "homeboy",
                ],
                "review test",
            ),
        ];

        for (args, label) in cases {
            let cli = Cli::try_parse_from(*args).expect("parse explicit Lab runner command");
            let hot_command = resource_policy::hot_command(&cli.command)
                .expect("command has a resource policy contract");

            assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
            assert!(hot_command.lab_offload_supported);
            assert_eq!(hot_command.label, *label);
        }
    }

    #[test]
    fn explicit_runner_resolves_worktree_cleanup_placement_for_dry_run_and_apply() {
        for args in [
            [
                "homeboy",
                "--runner",
                "homeboy-lab",
                "worktree",
                "cleanup",
                "--dry-run",
            ]
            .as_slice(),
            [
                "homeboy",
                "--runner",
                "homeboy-lab",
                "worktree",
                "cleanup",
                "--apply",
            ]
            .as_slice(),
        ] {
            let cli = Cli::try_parse_from(args).expect("parse runner-pinned cleanup command");
            let hot_command = resource_policy::hot_command(&cli.command)
                .expect("worktree cleanup is resource managed");

            assert!(hot_command.lab_offload_supported);
            assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
            assert_eq!(
                explicit_runner_placement(&cli, hot_command),
                Some("homeboy-lab"),
                "resource preflight resolves explicit runner placement before warning"
            );
        }
    }

    #[test]
    fn scoped_cook_runner_is_preserved_for_pinned_reexecution() {
        let matches = Cli::command_with_scoped_lab_args()
            .try_get_matches_from([
                "homeboy",
                "agent-task",
                "cook",
                "--to-worktree",
                "homeboy@fix-explicit-runner",
                "--prompt",
                "fix the issue",
                "--verify",
                "true",
                "--runner",
                "selected-lab",
            ])
            .expect("scoped cook accepts an explicit runner");
        let (cli, _) = Cli::from_registered_arg_matches(&matches).expect("typed cli");

        assert_eq!(cli.runner.as_deref(), Some("selected-lab"));
        assert_eq!(
            resource_policy_runner_hint(&cli, Some("default-lab")),
            Some("selected-lab")
        );
    }

    #[test]
    fn pinned_cook_argv_restores_explicit_runner_before_admission() {
        let mut cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--to-worktree",
            "homeboy@fix-explicit-runner",
            "--prompt",
            "fix the issue",
            "--verify",
            "true",
        ]);
        let pinned_argv = vec![
            "/tmp/homeboy/controller-runtimes/homeboy_0_290_0/homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--to-worktree".to_string(),
            "homeboy@fix-explicit-runner".to_string(),
            "--prompt".to_string(),
            "fix the issue".to_string(),
            "--verify".to_string(),
            "true".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
        ];

        normalize_cook_runner_option(&mut cli, &pinned_argv);

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert_eq!(
            resource_policy_runner_hint(&cli, None),
            Some("homeboy-lab"),
            "hot-machine admission must receive the runner selected in pinned argv"
        );
    }

    #[test]
    fn pinned_cook_continuation_command_quotes_the_executable_and_cook_id() {
        assert_eq!(
            pinned_cook_continue_command(
                std::path::Path::new("/tmp/controller runtimes/homeboy's"),
                "cook id's",
            ),
            "'/tmp/controller runtimes/homeboy'\\''s' agent-task cook-continue 'cook id'\\''s'"
        );
    }

    #[test]
    fn explicit_local_cook_continuation_bypasses_runner_runtime_delegation() {
        let cli = Cli::parse_from([
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "cook-continue",
            "controller-owned-cook",
        ]);
        let args = vec![
            "homeboy".to_string(),
            "--placement".to_string(),
            "local".to_string(),
            "agent-task".to_string(),
            "cook-continue".to_string(),
            "controller-owned-cook".to_string(),
        ];

        assert_eq!(
            delegate_agent_task_lifecycle_to_pinned_runtime(&cli, &args)
                .expect("explicit local continuation stays on the controller"),
            None
        );
    }

    #[test]
    fn run_delegates_a_runner_owned_linux_v2_pin_before_controller_validation() {
        crate::test_support::with_isolated_home(|_| {
            let run_id = "runner-owned-linux-v2-run";
            let plan: crate::agents::agent_tasks::scheduler::AgentTaskPlan = serde_json::from_str(
                include_str!("../../../tests/fixtures/agent_task_smoke_plan.json"),
            )
            .expect("deserialize durable test plan");
            crate::agents::agent_tasks::lifecycle::submit_plan(&plan, Some(run_id))
                .expect("persist durable run");
            crate::agents::agent_tasks::lifecycle::record_detached_lab_run(
                crate::agents::agent_tasks::lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id: "homeboy-lab",
                    runner_job_id: "linux-v2-pin-job",
                    remote_workspace: "/home/chubes/reaped-lab-workspace",
                    remote_command: &[
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "run".to_string(),
                    ],
                },
            )
            .expect("persist runner authority");
            crate::agents::agent_tasks::lifecycle::rewrite_record_for_test(run_id, |record| {
                record.metadata[homeboy::core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] =
                    serde_json::json!({
                        "schema": "homeboy/controller-runtime-pin/v2",
                        "originating": {
                            "build_identity": "homeboy linux-runner",
                            "pinned_executable": "/home/chubes/.local/share/homeboy/controller-runtimes/linux/homeboy",
                            "sha256": "linux-runner-sha256"
                        }
                    });
            })
            .expect("persist runner-owned v2 pin");

            let runtime = agent_task_lifecycle_pinned_runtime_for_mutation(run_id)
                .expect("runner authority is selected before controller-local validation")
                .expect("runner-owned v2 pin selects a runtime");
            assert!(matches!(
                runtime,
                AgentTaskLifecyclePinnedRuntime::Runner(ref pinned)
                    if pinned.runner_id == "homeboy-lab"
                        && pinned.executable
                            == std::path::Path::new("/home/chubes/.local/share/homeboy/controller-runtimes/linux/homeboy")
            ));
            assert_eq!(
                crate::agents::agent_tasks::lifecycle::status(run_id)
                    .expect("runtime resolution leaves the durable record intact")
                    .run_id,
                run_id
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn terminal_cook_with_a_controller_recipe_does_not_route_to_lab_or_origin_runtime() {
        crate::test_support::with_isolated_home(|home| {
            let target = home.path().join("active-task-worktree");
            std::fs::create_dir(&target).expect("create active task worktree");
            let mut plan: crate::agents::agent_tasks::scheduler::AgentTaskPlan =
                serde_json::from_str(include_str!(
                    "../../../tests/fixtures/agent_task_smoke_plan.json"
                ))
                .expect("deserialize durable test plan");
            plan.tasks[0].workspace.root = Some(target.display().to_string());

            let cook_id = "cook-runtime-a";
            let run_id = "cook-runtime-a-attempt-1";
            let options = crate::agents::agent_tasks::service::AgentTaskCookServiceOptions {
                cook_id: cook_id.to_string(),
                initial_run_id: run_id.to_string(),
                initial_plan: plan.clone(),
                to_worktree: target.display().to_string(),
                source_worktree_path: Some(target.clone()),
                provider_command: None,
                provider_invocation: None,
                gates: Default::default(),
                max_attempts: 1,
                no_finalize: true,
                draft_pr: false,
                base: "main".to_string(),
                task_base_sha: None,
                head: None,
                title: "runtime continuation fixture".to_string(),
                commit_message: "runtime continuation fixture".to_string(),
                source_refs: Vec::new(),
                protected_branches: Vec::new(),
                ai_tool: "test".to_string(),
                ai_model: None,
                ai_used_for: "test".to_string(),
                attempt_dispatcher: None,
                harvest_context: Default::default(),
            };
            crate::agents::agent_tasks::service::persist_initial_recipe(&options)
                .expect("persist runtime-A cook recipe");
            crate::agents::agent_tasks::lifecycle::submit_plan(&plan, Some(run_id))
                .expect("persist runtime-A lifecycle run");

            let invocation = home.path().join("runtime-a-invocation");
            let runtime_a = home.path().join("runtime-a");
            let identity = "homeboy test-runtime-a";
            std::fs::write(
                &runtime_a,
                format!(
                    "#!/bin/sh\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then\n  printf '%s\\n' '{{\"data\":{{\"display\":\"{identity}\"}}}}'\n  exit 0\nfi\nprintf '%s\\n' \"$@\" > \"$HOMEBOY_TEST_RUNTIME_A_INVOCATION\"\nexit 17\n"
                ),
            )
            .expect("write runtime-A fixture");
            std::fs::set_permissions(&runtime_a, std::fs::Permissions::from_mode(0o700))
                .expect("make runtime-A fixture executable");
            let digest = format!(
                "{:x}",
                Sha256::digest(std::fs::read(&runtime_a).expect("read runtime-A fixture"))
            );
            crate::agents::agent_tasks::lifecycle::rewrite_record_for_test(run_id, |record| {
                record.state = crate::agents::agent_tasks::lifecycle::AgentTaskRunState::Succeeded;
                record.metadata["controller_runtime"] = serde_json::json!({
                    "originating": {
                        "build_identity": identity,
                        "pinned_executable": runtime_a,
                        "sha256": digest,
                    }
                });
                record.metadata["cook_continuation_scheduler"] = serde_json::json!({
                    "status": "queued",
                    "cook_id": cook_id,
                    "run_id": run_id,
                });
            })
            .expect("record runtime-A pin");

            let _env = EnvGuard::remove("HOMEBOY_TEST_RUNTIME_A_INVOCATION");
            std::env::set_var("HOMEBOY_TEST_RUNTIME_A_INVOCATION", &invocation);
            let exit_code = delegate_cook_continue_to_pinned_runtime(
                cook_id,
                &[
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook-continue".to_string(),
                    cook_id.to_string(),
                    "--full".to_string(),
                ],
            )
            .expect("current runtime delegates to verified runtime A");

            assert_eq!(exit_code, None);
            assert!(
                !invocation.exists(),
                "controller continuation must not re-exec"
            );
            assert_eq!(
                crate::agents::agent_tasks::service::resolve_cook_continuation_run_id(cook_id)
                    .expect("resolve exact continuation attempt"),
                run_id
            );
            let pinned = crate::agents::agent_tasks::lifecycle::pinned_runtime_for_mutation(run_id)
                .expect("validate runtime-A pin")
                .expect("select runtime-A pin");
            assert!(pinned.is_file());
            assert_ne!(
                pinned, runtime_a,
                "runtime A must execute from its immutable pin"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn repaired_terminal_cook_continuation_stays_with_the_current_coordinator() {
        crate::test_support::with_isolated_home(|home| {
            let target = home.path().join("terminal-task-worktree");
            std::fs::create_dir(&target).expect("create terminal task worktree");
            let mut plan: crate::agents::agent_tasks::scheduler::AgentTaskPlan =
                serde_json::from_str(include_str!(
                    "../../../tests/fixtures/agent_task_smoke_plan.json"
                ))
                .expect("deserialize durable test plan");
            plan.tasks[0].workspace.root = Some(target.display().to_string());

            let cook_id = "cook-runtime-legacy-terminal";
            let run_id = "cook-runtime-legacy-terminal-attempt-1";
            let options = crate::agents::agent_tasks::service::AgentTaskCookServiceOptions {
                cook_id: cook_id.to_string(),
                initial_run_id: run_id.to_string(),
                initial_plan: plan.clone(),
                to_worktree: target.display().to_string(),
                source_worktree_path: Some(target),
                provider_command: None,
                provider_invocation: None,
                gates: Default::default(),
                max_attempts: 1,
                no_finalize: true,
                draft_pr: false,
                base: "main".to_string(),
                task_base_sha: None,
                head: None,
                title: "legacy terminal continuation fixture".to_string(),
                commit_message: "legacy terminal continuation fixture".to_string(),
                source_refs: Vec::new(),
                protected_branches: Vec::new(),
                ai_tool: "test".to_string(),
                ai_model: None,
                ai_used_for: "test".to_string(),
                attempt_dispatcher: None,
                harvest_context: Default::default(),
            };
            crate::agents::agent_tasks::service::persist_initial_recipe(&options)
                .expect("persist legacy Cook recipe");
            crate::agents::agent_tasks::lifecycle::submit_plan(&plan, Some(run_id))
                .expect("persist legacy lifecycle run");
            crate::agents::agent_tasks::lifecycle::rewrite_record_for_test(run_id, |record| {
                record.state = crate::agents::agent_tasks::lifecycle::AgentTaskRunState::Succeeded;
                record.metadata["controller_runtime"] = serde_json::json!({
                    "originating": { "build_identity": "homeboy legacy-runtime" }
                });
                record.metadata["cook_continuation_scheduler"] = serde_json::json!({
                    "status": "queued",
                    "cook_id": cook_id,
                    "run_id": run_id,
                    "coordinator_build_identity": homeboy::core::build_identity::current().display,
                });
            })
            .expect("mark recipe-bound legacy attempt terminal");

            assert_eq!(
                delegate_cook_continue_to_pinned_runtime(
                    cook_id,
                    &[
                        "homeboy".to_string(),
                        "agent-task".to_string(),
                        "cook-continue".to_string(),
                        cook_id.to_string(),
                    ],
                )
                .expect("current coordinator accepts repaired terminal attempt"),
                None
            );
        });
    }

    #[test]
    fn managed_runner_context_clears_before_production_run_command() {
        let _lock = env_lock().lock().unwrap_or_else(|err| err.into_inner());
        let previous = [
            crate::runner::RUNNER_HOSTED_EXEC_ENV,
            crate::runner::RUNNER_PLACEMENT_RESOLVED_ENV,
            crate::runner::RUNNER_ID_ENV,
            "HOMEBOY_LAB_RUNNER_ID",
            homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV,
        ]
        .map(|name| (name, std::env::var(name).ok()));
        std::env::set_var(crate::runner::RUNNER_HOSTED_EXEC_ENV, "1");
        std::env::set_var(crate::runner::RUNNER_PLACEMENT_RESOLVED_ENV, "1");
        std::env::set_var(crate::runner::RUNNER_ID_ENV, "homeboy-lab");
        std::env::set_var("HOMEBOY_LAB_RUNNER_ID", "homeboy-lab");
        std::env::set_var(
            homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV,
            "homeboy-lab",
        );

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
        assert!(std::env::var_os("HOMEBOY_LAB_RUNNER_ID").is_none());
        assert_eq!(
            std::env::var_os(homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV),
            Some("homeboy-lab".into()),
            "runner execution provenance survives without exposing controller transport intent"
        );

        for (name, value) in previous {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn recovery_action_diagnostic_names_the_source_reason_and_resolution() {
        let output =
            format_runner_exec_recovery_diagnostic(&crate::runner::RunnerExecRecoveryDiagnostic {
                source_run_id: "runner-exec-source-42".to_string(),
                reason: "runner has no persisted daemon session for recovery".to_string(),
                inspection_action: "homeboy runs show runner-exec-source-42".to_string(),
            });
        assert_eq!(
            output,
            "runner-exec recovery action required: source_run_id=runner-exec-source-42 reason=runner has no persisted daemon session for recovery inspect=`homeboy runs show runner-exec-source-42`"
        );
    }
}
