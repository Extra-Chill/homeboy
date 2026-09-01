use clap::{ArgMatches, Command, CommandFactory, Parser};
use std::collections::BTreeSet;
use std::io::{IsTerminal, Write};
use std::process::Command as ProcessCommand;
use std::sync::OnceLock;
use uuid::Uuid;

use crate::capability_registry::CommandCapabilityRegistry;
use crate::cli_surface::{
    command_safety_manifest_from_dynamic, command_surface_from, Cli, CommandSafetyManifest,
    CommandSurfaceCommandProvenance, CommandSurfaceDoctorReport, CommandSurfaceRegistry, Commands,
    DynamicCommandDescriptor, ExtensionCommandArgContract, ExtensionCommandArgsContract,
    ExtensionCommandHealth, ExtensionCommandManifest,
};
use crate::command_capability::{
    classify as classify_command_capability, homeboy_owned_args, requires_startup_reconciliation,
    CommandCapability,
};
use crate::command_contract::{LabCommandRoute, LabCommandRouteSupport};
use crate::commands;
use crate::commands::cli;
use crate::commands::output_runtime;
use crate::commands::utils::{args, entity_suggest, resource_policy, response as output};
use homeboy_agents::agent_task_service::cook_continue_command;
use homeboy_core::extension::catalog::{is_extension_linked, load_all_extensions};
use homeboy_core::extension::readiness::ExtensionReadinessMode;
#[cfg(test)]
use homeboy_core::extension::readiness::READY_CHECK_SKIPPED_REASON;
use homeboy_core::extension::resolve::is_extension_compatible;
use homeboy_extension_contract::{
    api::v1::{
        ExtensionApiCatalogDiagnosticCode, ExtensionApiCatalogEntry, ExtensionApiCatalogRequest,
        ExtensionApiReadinessState, ExtensionApiReadinessStatus,
        EXTENSION_API_CATALOG_REQUEST_SCHEMA, EXTENSION_API_V1,
    },
    CliConfig, ExtensionCapability, ExtensionManifest as InstalledExtensionManifest,
};
use homeboy_runner_contract::{
    RunnerApiCapabilitiesRequest, RunnerApiCapabilitiesResponse, RunnerCapabilities,
    RUNNER_API_CAPABILITIES_REQUEST_SCHEMA, RUNNER_API_V1,
};
use homeboy_upgrade::upgrade;

/// A typed command package installed by a product composition root.
///
/// The base CLI owns common globals and response envelopes, while capability
/// crates own their Clap grammar and execution. Keeping this boundary here lets
/// a kernel-only CLI build omit product capabilities entirely.
pub trait CliCapability: Sync {
    fn name(&self) -> &'static str;
    fn command(&self) -> Command;
    /// Typed descriptor capabilities participate in the same fail-closed
    /// preflight as built-ins. Dynamic shell extensions intentionally remain
    /// outside this boundary because they are not typed descriptors.
    fn preflight(
        &self,
        _matches: &ArgMatches,
        _normalized_args: &[String],
    ) -> crate::core::parsed_command_preflight::ParsedCommandPreflightInput {
        use crate::core::parsed_command_preflight::{
            ControllerExecution, DeferredWorkloadPolicy, LabRouteIntent, ParsedCommandIdentity,
            ParsedCommandPreflightInput, PlacementIntent, ProvenanceRequirement,
            ResourceAdmissionRequirement, RunnerIntent, RunnerNormalization,
        };
        ParsedCommandPreflightInput {
            identity: ParsedCommandIdentity {
                family: self.name().to_string(),
                operation: Vec::new(),
            },
            resource_admission: ResourceAdmissionRequirement::Exempt,
            controller_execution: ControllerExecution::ControllerOnly,
            deferred_workload: DeferredWorkloadPolicy::Forbidden,
            placement: PlacementIntent::Local,
            runner: RunnerIntent::Default,
            runner_normalization: RunnerNormalization::None,
            lab_route: LabRouteIntent::Unsupported,
            provenance: ProvenanceRequirement::CaptureExecution,
        }
    }
    fn preflight_policy(
        &self,
        _matches: &ArgMatches,
        _normalized_args: &[String],
    ) -> crate::core::parsed_command_preflight::ParsedCommandPolicySnapshot {
        crate::core::parsed_command_preflight::ParsedCommandPolicySnapshot {
            resource_admission_evidence:
                crate::core::parsed_command_preflight::ResourceAdmissionEvidence::Unavailable,
            resource_policy: None,
            lab_readiness: None,
            selected_runner_id: None,
            generic_route: crate::core::parsed_command_preflight::GenericRoutePolicySnapshot {
                command_supports_lab: false,
                automatic_authorized: false,
                selected_runner_id: None,
            },
            deferred_pressure_refusal: false,
            runner_admitted: false,
            runner_incompatible: false,
            auto_local_capacity_fallback: false,
        }
    }
    fn run(&self, matches: &ArgMatches) -> crate::core::Result<(serde_json::Value, i32)>;

    /// Resolve a descriptor-composed command through the same typed Lab route
    /// contract as a built-in command. Absent remains fail-closed.
    fn lab_command_route(
        &self,
        _matches: &ArgMatches,
    ) -> crate::core::Result<Option<LabCommandRoute>> {
        Ok(None)
    }

    /// Declares scoped help and runner-guidance metadata for the route.
    fn lab_command_route_support(&self) -> Option<LabCommandRouteSupport> {
        None
    }
}

const COOK_PINNED_RUNTIME_ENV: &str = "HOMEBOY_COOK_PINNED_CONTROLLER_RUNTIME";
const RUNNER_EXEC_RECOVERY_OWNER_ENV: &str = "HOMEBOY_RUNNER_EXEC_RECOVERY_OWNER";
const RUNNER_EXEC_RECOVERY_CHILD_ENV: &str = "HOMEBOY_RUNNER_EXEC_RECOVERY_CHILD";
const CONTROLLER_FALLBACK_RECONCILIATION_ENV: &str = "HOMEBOY_CONTROLLER_FALLBACK_RECONCILIATION";

fn generic_route_policy_snapshot(
    cli: &Cli,
    selected_runner_id: Option<String>,
) -> crate::core::parsed_command_preflight::GenericRoutePolicySnapshot {
    let command = crate::commands::route::lab_offload_command(&cli.command)
        .ok()
        .flatten();
    crate::core::parsed_command_preflight::GenericRoutePolicySnapshot {
        command_supports_lab: command.is_some(),
        automatic_authorized: command.as_ref().is_some_and(|command| {
            cli.runner.is_some()
                || crate::core::lab_routing::authorizes_policy_lab_runner(
                    &command.command,
                    cli.placement,
                    crate::core::lab_routing::captured_pressure_severity().as_deref(),
                )
        }),
        selected_runner_id,
    }
}

pub(crate) fn runner_satisfies_admission_capabilities(
    runner_id: &str,
    required: &BTreeSet<&str>,
) -> crate::core::Result<bool> {
    if required.is_empty() {
        return Ok(true);
    }
    let inventory = capabilities_from_response(
        crate::runner::runners::RunnerDiscoveryService::capabilities_api(
            &RunnerApiCapabilitiesRequest {
                schema: RUNNER_API_CAPABILITIES_REQUEST_SCHEMA.to_string(),
                api_version: RUNNER_API_V1,
                runner_id: runner_id.to_string(),
            },
        )?,
    )?;
    Ok(runner_inventory_satisfies_admission_capabilities(
        &inventory, required,
    ))
}

fn capabilities_from_response(
    response: RunnerApiCapabilitiesResponse,
) -> crate::core::Result<RunnerCapabilities> {
    if let Some(failure) = response.failure {
        let failure_code = serde_json::to_value(failure.code)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string));
        return Err(crate::core::Error::validation_invalid_argument(
            "runner_api.capabilities",
            failure.message,
            failure_code,
            None,
        ));
    }

    response.capabilities.ok_or_else(|| {
        crate::core::Error::validation_invalid_argument(
            "runner_api.capabilities",
            "Runner API capabilities response omitted both capabilities and failure",
            None,
            None,
        )
    })
}

pub(crate) fn runner_inventory_satisfies_admission_capabilities(
    inventory: &crate::runner::runners::RunnerCapabilities,
    required: &BTreeSet<&str>,
) -> bool {
    required.iter().all(|required| {
        inventory.capabilities.contains(*required) || inventory.runtime_ids.contains(*required)
    })
}

pub(crate) fn select_unmaterialized_cook_runner(
    request: &serde_json::Value,
) -> crate::core::Result<serde_json::Value> {
    let placement = &request["binding"]["placement"];
    let required = request["binding"]["provider_runtime_refs"]["required_capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<BTreeSet<_>>();
    let unavailable = |runner_id: &str, selection: &str| {
        serde_json::json!({
            "state": "blocked_runner_unavailable",
            "reason": format!("runner `{runner_id}` does not satisfy required provider capabilities"),
            "selection": selection,
        })
    };
    if let Some(runner_id) = placement["runner_ref"].as_str() {
        let mut snapshot = crate::runner::runners::runner_admission_snapshot(runner_id)?;
        if snapshot.summary.accepting_jobs
            || crate::runner::refresh_explicit_detached_queue_runner(runner_id)?
        {
            return if runner_satisfies_admission_capabilities(runner_id, &required)? {
                Ok(
                    serde_json::json!({ "state": "eligible", "runner_id": runner_id, "selection": "explicit" }),
                )
            } else {
                Ok(unavailable(runner_id, "explicit"))
            };
        }
        snapshot = crate::runner::runners::runner_admission_snapshot(runner_id)?;
        return Ok(serde_json::json!({
            "state": if snapshot.summary.daemon_fresh { "blocked_runner_unavailable" } else { "blocked_runner_stale" },
            "reason": snapshot.summary.next_action.unwrap_or_else(|| format!("runner `{runner_id}` is not accepting jobs")),
            "selection": "explicit",
        }));
    }
    let readiness = crate::runner::refresh_lab_runner_readiness_for_admission()?;
    let selection = if readiness.state
        == crate::runner::runners::LabRunnerReadinessState::CapacityBlocked
    {
        crate::runner::refresh_detached_queue_runner()?
    } else if readiness.state == crate::runner::runners::LabRunnerReadinessState::ConnectedReady {
        readiness.selected_runner_id.clone()
    } else {
        None
    };
    if let Some(runner_id) = selection {
        let selection_kind = if readiness.state
            == crate::runner::runners::LabRunnerReadinessState::CapacityBlocked
        {
            "reverse_capacity_queue"
        } else {
            "configured_policy"
        };
        return if runner_satisfies_admission_capabilities(&runner_id, &required)? {
            Ok(
                serde_json::json!({ "state": "eligible", "runner_id": runner_id, "selection": selection_kind }),
            )
        } else {
            Ok(unavailable(&runner_id, selection_kind))
        };
    }
    Ok(serde_json::json!({
        "state": if readiness.state == crate::runner::runners::LabRunnerReadinessState::Stale { "blocked_runner_stale" } else if readiness.state == crate::runner::runners::LabRunnerReadinessState::CapacityBlocked { "queued" } else { "blocked_runner_unavailable" },
        "reason": readiness.reasons.first().cloned().unwrap_or_else(|| "no configured Lab runner currently satisfies admission policy".to_string()),
        "selection": "configured_policy",
    }))
}

pub struct CliRuntime {
    extension_discovery: OnceLock<ExtensionCliDiscovery>,
    capabilities: CommandCapabilityRegistry,
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

struct ComposedCommandRegistry {
    command: Command,
    provenance: Vec<CommandSurfaceCommandProvenance>,
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

fn startup_fast_path_output(
    runtime: &CliRuntime,
    args: &[String],
) -> Option<StartupFastPathOutput> {
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
            let Err(error) = runtime.build_augmented_command().try_get_matches_from(args) else {
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
    CliRuntime::new().run_startup_fast_path(args)
}

fn emit_startup_fast_path_output(output: StartupFastPathOutput) -> std::process::ExitCode {
    match output {
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

    std::process::ExitCode::SUCCESS
}

pub(crate) fn current_augmented_command_safety_manifest() -> CommandSafetyManifest {
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
pub(crate) fn register_startup_providers_before_reconcile() {
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
    homeboy_core::extension::audit_manifest_provider::register();
    homeboy_core::extension::component_script::register_component_script_runner();
    homeboy_core::extension::build::register_component_build_runner();
    homeboy_core::extension::lifecycle::register_component_install_runner();
    // Register extension-backed audit providers so code_audit can load
    // grammars, run fallback fingerprint scripts, and collect compiler
    // warnings without depending on the extension registry or script runner.
    homeboy_core::extension::audit_fingerprint_script_provider::register();
    homeboy_core::extension::audit_grammar_source_provider::register();
    homeboy_core::extension::audit_compiler_warning_provider::register();
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
fn register_startup_providers_after_reconcile(
    agent_task: &crate::core::defaults::AgentTaskConfig,
    capabilities: &[&dyn CliCapability],
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
    // orchestration tick can reconcile orphaned `running` records without core
    // depending on the agent-task subsystem. Loop waits are owned by their
    // durable Work jobs below rather than this unrelated periodic sweep.
    crate::agents::agent_task_service::register_orchestration_driver();
    crate::commands::route::register_unmaterialized_cook_replay_driver();
    crate::agents::agent_task_service::register_controller_upgrade_admission_provider();
    // New orchestration submissions share one versioned lifecycle driver.
    crate::agents::agent_task_service::register_work_job_driver();
    // A locally-placed detached Cook is a daemon-owned durable job: the daemon
    // owns its record, checkpointing, cancellation and HTTP inspection, while
    // the launcher-spawned child keeps the operator's execution environment.
    crate::agents::agent_task_service::register_cook_work_handler();
    // A locally-placed detached fanout wave is daemon-owned on the same terms:
    // the daemon supervises a coordinator it did not spawn, so no branch of its
    // lifecycle can re-run a child that already completed.
    crate::agents::agent_task_service::register_cook_batch_work_handler();
    crate::agents::agent_task_service::register_loop_work_job_handler();
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
    // Register the orchestration service behind daemon HTTP control-plane
    // routes without making core depend on the agent-task subsystem.
    crate::agents::orchestration::register();
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
    let lab_support = capabilities
        .iter()
        .filter_map(|capability| capability.lab_command_route_support())
        .collect::<Vec<_>>();
    crate::runner::set_lab_runner_hint_provider(move || {
        let summary = crate::command_contract::lab_runner_support_summary(&lab_support);
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
    register_startup_providers_after_reconcile(agent_task, &[])
}

impl CliRuntime {
    pub fn new() -> Self {
        Self::with_capabilities(&[])
    }

    pub fn with_capabilities(capabilities: &'static [&'static dyn CliCapability]) -> Self {
        Self::try_with_required_capabilities(capabilities, &[])
            .expect("CLI capability composition must be valid")
    }

    pub fn try_with_required_capabilities(
        capabilities: &'static [&'static dyn CliCapability],
        required: &[&str],
    ) -> crate::core::Result<Self> {
        Ok(Self {
            extension_discovery: OnceLock::new(),
            capabilities: CommandCapabilityRegistry::compose(
                capabilities,
                required,
                &Cli::command(),
            )?,
        })
    }

    pub fn run_startup_fast_path(&self, args: &[String]) -> Option<std::process::ExitCode> {
        startup_fast_path_output(self, args).map(emit_startup_fast_path_output)
    }

    pub fn run_from_args(&self, args: Vec<String>) -> std::process::ExitCode {
        #[cfg(feature = "test-support")]
        if let Some(path) = std::env::var_os("HOMEBOY_TEST_RUNTIME_INITIALIZATION_SENTINEL") {
            std::fs::write(path, b"initialized").expect("write runtime initialization sentinel");
        }

        let normalized = args::normalize(args);
        if let Some(exit) = run_config_read_fast_path(&normalized) {
            return exit;
        }
        let command_capability = classify_command_capability(&normalized);
        if let Some(message) = args::runner_exec_option_boundary_error(&normalized) {
            eprintln!("error: {message}");
            return std::process::ExitCode::from(2);
        }

        register_startup_providers_before_reconcile();
        if std::env::var_os(CONTROLLER_FALLBACK_RECONCILIATION_ENV).is_some() {
            let config = crate::core::defaults::load_config();
            if register_startup_providers_after_reconcile(
                &config.agent_task,
                &self.capabilities.capabilities(),
            )
            .is_err()
            {
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
        if let Err(error) = register_startup_providers_after_reconcile(
            &config.agent_task,
            &self.capabilities.capabilities(),
        ) {
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
            if let Ok(config_root) = crate::core::paths::homeboy() {
                let _ = crate::commands::deferred_workload::restart_worker_if_pending(&config_root);
            }
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
                    if matches!(
                        err.kind(),
                        clap::error::ErrorKind::DisplayHelp
                            | clap::error::ErrorKind::DisplayVersion
                    ) {
                        err.exit();
                    }
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
        self.run_matches_with_capability_admission(matches, normalized, None)
    }

    fn run_matches_with_capability_admission(
        &self,
        matches: ArgMatches,
        normalized: Vec<String>,
        capability_admission: Option<
            crate::core::parsed_command_preflight::ResourceAdmissionEvidence,
        >,
    ) -> std::process::ExitCode {
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

        if let Some((capability, capability_matches)) = self.capability_matches(&matches) {
            let route = match capability.lab_command_route(capability_matches) {
                Ok(route) => route,
                Err(error) => {
                    output_runtime::emit_json_result_for_identity(
                        Err(error),
                        output_file.as_deref(),
                        2,
                        &command_identity,
                    );
                    return std::process::ExitCode::from(2);
                }
            };
            let placement = *matches
                .get_one::<crate::cli_surface::Placement>("placement")
                .unwrap_or(&crate::cli_surface::Placement::Auto);
            let runner = matches.get_one::<String>("runner").map(String::as_str);
            if route.is_none()
                && (placement == crate::cli_surface::Placement::Lab || runner.is_some())
            {
                let error = crate::core::Error::validation_invalid_argument(
                    "placement",
                    "this composed command has no Lab route contract",
                    None,
                    None,
                );
                output_runtime::emit_json_result_for_identity(
                    Err(error),
                    output_file.as_deref(),
                    2,
                    &command_identity,
                );
                return std::process::ExitCode::from(2);
            }
            let mut routed_preflight = None;
            if let Some(route) = route {
                let runner_env = matches
                    .get_many::<String>("runner_env")
                    .map(|values| values.cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                let runner_secret_env = matches
                    .get_many::<String>("runner_secret_env")
                    .map(|values| values.cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                let options = crate::commands::route::ComposedLabRouteOptions {
                    placement,
                    runner,
                    allow_dirty_lab_workspace: matches.get_flag("allow_dirty_lab_workspace"),
                    skip_deps_hydration: matches.get_flag("skip_deps_hydration"),
                    preserve_workspace_on_failure: matches
                        .get_flag("preserve_workspace_on_failure"),
                    detach_after_handoff: matches.get_flag("detach_after_handoff"),
                    runner_env: &runner_env,
                    runner_secret_env: &runner_secret_env,
                    lab_env_json: matches
                        .get_one::<String>("lab_env_json")
                        .map(String::as_str),
                    runner_workspace_root: matches
                        .get_one::<String>("runner_workspace_root")
                        .map(String::as_str),
                };
                if let Some(exit_code) = preflight_composed_lab_route(
                    &route,
                    &options,
                    output_file.as_deref(),
                    &command_identity,
                ) {
                    return std::process::ExitCode::from(exit_code_to_u8(exit_code));
                }
                let preflight = match resolve_composed_capability_preflight(
                    capability,
                    capability_matches,
                    &route,
                    &options,
                    &normalized,
                ) {
                    Ok(preflight) => preflight,
                    Err(error) => {
                        output_runtime::emit_json_result_for_identity(
                            Err(error),
                            output_file.as_deref(),
                            2,
                            &command_identity,
                        );
                        return std::process::ExitCode::from(2);
                    }
                };
                crate::core::parsed_command_preflight::capture_result(preflight.clone());
                crate::commands::utils::execution_provenance::capture(&preflight);
                match crate::commands::route::route_composed_lab_command(
                    &route,
                    options,
                    &normalized,
                    output_file.as_deref(),
                    &preflight,
                ) {
                    Ok(Some(exit_code)) => {
                        return std::process::ExitCode::from(exit_code_to_u8(exit_code));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        output_runtime::emit_json_result_for_identity(
                            Err(error),
                            output_file.as_deref(),
                            2,
                            &command_identity,
                        );
                        return std::process::ExitCode::from(2);
                    }
                }
                routed_preflight = Some(preflight);
            }
            if let Some(path) = output_file.as_deref() {
                if let Some(exit) = output_file_path_exit_code(path, &command_identity) {
                    return exit;
                }
            }
            if let Some(preflight) = routed_preflight {
                let (json_result, exit_code) = if matches!(
                    preflight.resource_admission,
                    crate::core::parsed_command_preflight::ResourceAdmissionDecision::Rejected { .. }
                ) {
                    (
                        Err(resource_admission_error(&preflight.resource_admission)),
                        2,
                    )
                } else {
                    match capability.run(capability_matches) {
                        Ok((value, exit_code)) => (Ok(value), exit_code),
                        Err(error) => (Err(error), 2),
                    }
                };
                output_runtime::emit_json_result_for_identity(
                    json_result,
                    output_file.as_deref(),
                    exit_code,
                    &command_identity,
                );
                return std::process::ExitCode::from(exit_code_to_u8(exit_code));
            }
            let input = capability.preflight(capability_matches, &normalized);
            // Capabilities declare their requirement and policy only. Runtime
            // owns the host probe, then core derives the admission verdict.
            let evidence = capability_admission.unwrap_or_else(|| match input.resource_admission {
                crate::core::parsed_command_preflight::ResourceAdmissionRequirement::Exempt => {
                    crate::core::parsed_command_preflight::ResourceAdmissionEvidence::Unavailable
                }
                crate::core::parsed_command_preflight::ResourceAdmissionRequirement::Required { .. } => {
                    crate::commands::resources::run_preflight()
                        .map(|(resources, _)| resource_policy::resource_admission_evidence(&resources))
                        .unwrap_or(crate::core::parsed_command_preflight::ResourceAdmissionEvidence::Unavailable)
                }
            });
            let (json_result, exit_code) = self.run_capability_with_admission_evidence(
                capability,
                capability_matches,
                &normalized,
                evidence,
            );
            output_runtime::emit_json_result_for_identity(
                json_result,
                output_file.as_deref(),
                exit_code,
                &command_identity,
            );
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }

        if let Some(extension_cmd) = self.try_parse_extension_cli_command(&matches) {
            if let Some(path) = output_file.as_deref() {
                if let Some(exit) = output_file_path_exit_code(path, &command_identity) {
                    return exit;
                }
            }

            // Shell extensions are not typed descriptor capabilities, so they
            // cannot declare Lab routing or resource admission. Capture their
            // validated local contract before the shell adapter executes.
            let input = crate::core::parsed_command_preflight::ParsedCommandPreflightInput {
                identity: crate::core::parsed_command_preflight::ParsedCommandIdentity {
                    family: extension_cmd.tool.clone(),
                    operation: Vec::new(),
                },
                resource_admission:
                    crate::core::parsed_command_preflight::ResourceAdmissionRequirement::Exempt,
                controller_execution:
                    crate::core::parsed_command_preflight::ControllerExecution::ControllerOnly,
                deferred_workload:
                    crate::core::parsed_command_preflight::DeferredWorkloadPolicy::Forbidden,
                placement: crate::core::parsed_command_preflight::PlacementIntent::Local,
                runner: crate::core::parsed_command_preflight::RunnerIntent::CommandLocal,
                runner_normalization:
                    crate::core::parsed_command_preflight::RunnerNormalization::None,
                lab_route: crate::core::parsed_command_preflight::LabRouteIntent::Unsupported,
                provenance:
                    crate::core::parsed_command_preflight::ProvenanceRequirement::CaptureExecution,
            };
            let policy = crate::core::parsed_command_preflight::ParsedCommandPolicySnapshot {
                resource_admission_evidence:
                    crate::core::parsed_command_preflight::ResourceAdmissionEvidence::Unavailable,
                resource_policy: None,
                lab_readiness: None,
                selected_runner_id: None,
                generic_route: crate::core::parsed_command_preflight::GenericRoutePolicySnapshot {
                    command_supports_lab: false,
                    automatic_authorized: false,
                    selected_runner_id: None,
                },
                deferred_pressure_refusal: false,
                runner_admitted: false,
                runner_incompatible: false,
                auto_local_capacity_fallback: false,
            };
            let preflight =
                match crate::core::parsed_command_preflight::resolve_parsed_command_preflight(
                    normalized.clone(),
                    input,
                    policy,
                ) {
                    Ok(preflight) => preflight,
                    Err(error) => {
                        output_runtime::emit_json_result_for_identity(
                            Err(error),
                            output_file.as_deref(),
                            2,
                            &command_identity,
                        );
                        return std::process::ExitCode::from(2);
                    }
                };
            crate::core::parsed_command_preflight::capture_result(preflight.clone());
            crate::commands::utils::execution_provenance::capture(&preflight);
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
        let mut notification_resolution =
            match crate::core::notification_route_resolver::resolve_from_cli_or_env_with_evidence(
                cli.notification_transport.as_deref(),
                cli.notification_route.as_deref(),
            ) {
                Ok(resolution) => resolution,
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
        let mut notification_route = notification_resolution.route.clone();
        if let Some(route) = &notification_route {
            // Placement routing happens before the thread-local command scope.
            // Mirror the selected route into its existing durable handoff input.
            cli.notification_transport = Some(route.transport.clone());
            cli.notification_route = Some(route.route.clone());
        }
        commands::set_skip_deps_hydration(cli.skip_deps_hydration);
        normalize_runs_runner_options(&mut cli, &normalized);
        normalize_cook_runner_option(&mut cli, &normalized);
        if let Commands::AgentTask(agent_task) = &mut cli.command {
            if let crate::commands::agent_task::AgentTaskCommand::Cook(cook) =
                &mut agent_task.command
            {
                if let Err(err) = crate::commands::agent_task::run::snapshot_cook_prompt(cook) {
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
                if !cook.preview {
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
        }

        if let Err(err) = guard_cook_runtime_compatibility(&cli, &normalized) {
            output_runtime::emit_json_result_for_identity(
                Err(err),
                output_file.as_deref(),
                2,
                &command_identity,
            );
            return std::process::ExitCode::from(2);
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
            notification_resolution =
                match crate::core::notification_route_resolver::resolve_installed_with_evidence() {
                    Ok(resolution) => resolution,
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
            notification_route = notification_resolution.route.clone();
            if let Some(route) = &notification_route {
                cli.notification_transport = Some(route.transport.clone());
                cli.notification_route = Some(route.route.clone());
            }
        }
        if cli.detach_after_handoff
            && notification_route.is_none()
            && matches!(
                &cli.command,
                Commands::AgentTask(agent_task)
                    if matches!(
                        &agent_task.command,
                        crate::commands::agent_task::AgentTaskCommand::Cook(_)
                    )
            )
        {
            if let Some(warning) =
                crate::commands::agent_task::run::detached_cook_route_less_warning(
                    &notification_resolution.evidence,
                )
            {
                eprintln!("{warning}");
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

        // Resolve the parsed command contract before any admission probe. The
        // completed result is captured exactly once after preflight.
        let preflight_input = resource_policy::parsed_command_preflight_input(&cli, &normalized);

        // Capture controller pressure once before placement routing. The route
        // and persisted evidence reuse this preflight decision rather than
        // probing the host a second time.
        let managed_runner_placement = resource_policy::is_managed_runner_placement_context();
        if let Some(exit_code) = preflight_hot_command(
            &cli,
            &normalized,
            preflight_input.clone(),
            output_file.as_deref(),
            &command_identity,
        ) {
            if managed_runner_placement {
                resource_policy::clear_managed_runner_placement_context();
            }
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }
        if crate::core::parsed_command_preflight::captured_result().is_none() {
            let lab_readiness = matches!(
                preflight_input.lab_route,
                crate::core::parsed_command_preflight::LabRouteIntent::Supported { .. }
            )
            .then(|| crate::runner::lab_runner_readiness().ok())
            .flatten();
            let selected_runner_id = cli.runner.clone().or_else(|| {
                lab_readiness
                    .as_ref()
                    .and_then(|readiness| readiness.selected_runner_id.clone())
            });
            let result = match crate::core::parsed_command_preflight::resolve_parsed_command_preflight(
                normalized.clone(),
                preflight_input.clone(),
                crate::core::parsed_command_preflight::ParsedCommandPolicySnapshot {
                    resource_admission_evidence:
                        crate::core::parsed_command_preflight::ResourceAdmissionEvidence::Unavailable,
                    resource_policy: None,
                    lab_readiness: lab_readiness
                        .as_ref()
                        .map(|readiness| parsed_lab_readiness_snapshot(&cli, readiness)),
                    selected_runner_id: selected_runner_id.clone(),
                    generic_route: generic_route_policy_snapshot(&cli, selected_runner_id.clone()),
                    deferred_pressure_refusal: false,
                    runner_admitted: selected_runner_id.is_some() && lab_readiness.as_ref().is_some_and(|readiness| readiness.state == crate::runner::runners::LabRunnerReadinessState::ConnectedReady),
                    runner_incompatible: false,
                    auto_local_capacity_fallback: false,
                },
            ) {
                Ok(result) => result,
                Err(err) => {
                    output_runtime::emit_json_result_for_identity(Err(err), output_file.as_deref(), 2, &command_identity);
                    return std::process::ExitCode::from(2);
                }
            };
            crate::core::parsed_command_preflight::capture_result(result);
        }

        // Persist the actual preflight decision with the command intent before
        // placement routing can consume controller transport markers.
        let preflight = crate::core::parsed_command_preflight::captured_result()
            .expect("completed parsed-command preflight was captured");
        crate::commands::utils::execution_provenance::capture(&preflight);

        let route_result = crate::core::notification_route::with_current_resolution(
            Some(notification_resolution.evidence.clone()),
            || {
                crate::core::notification_route::with_current(notification_route.clone(), || {
                    crate::commands::route::route_after_parse_with_provenance(
                        &cli,
                        &normalized,
                        output_file.as_deref(),
                        Some(&command_provenance),
                    )
                })
            },
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

        let command_surface_doctor_report = matches!(
            &cli.command,
            Commands::SelfCmd(crate::commands::self_cmd::SelfArgs {
                command: crate::commands::self_cmd::SelfCommand::Doctor(_),
            })
        )
        .then(|| self.command_surface_doctor_report());
        let exit_code = crate::cli_surface::with_command_surface_doctor_report(
            command_surface_doctor_report,
            || {
                crate::core::notification_route::with_current_resolution(
                    Some(notification_resolution.evidence),
                    || {
                        crate::core::notification_route::with_current(notification_route, || {
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
                        })
                    },
                )
            },
        );
        // The command's initial outcome is now durable and returned. Historical
        // runner evidence is a separately owned
        // best-effort recovery concern and cannot delay that boundary.
        if requires_startup_reconciliation(&normalized) {
            schedule_runner_exec_recovery();
        }
        std::process::ExitCode::from(exit_code_to_u8(exit_code))
    }

    fn build_augmented_command(&self) -> Command {
        self.composed_command_registry().command
    }

    fn command_surface_doctor_report(&self) -> CommandSurfaceDoctorReport {
        let registry = self.composed_command_registry();
        crate::cli_surface::command_surface_doctor_report_from_composed(
            registry.command,
            registry.provenance,
        )
    }

    fn composed_command_registry(&self) -> ComposedCommandRegistry {
        let discovery = self.extension_discovery();
        self.capabilities
            .validate_external_names(discovery.info.iter().map(|info| info.tool.as_str()))
            .expect("dynamic and typed capability command names must not conflict");
        let mut command = build_augmented_command(&discovery.info, &discovery.health);
        let mut provenance = Cli::command_with_scoped_lab_args()
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
            .map(|subcommand| CommandSurfaceCommandProvenance {
                command: subcommand.get_name().to_string(),
                registry: CommandSurfaceRegistry::Core,
            })
            .collect::<Vec<_>>();
        provenance.extend(
            discovery
                .info
                .iter()
                .map(|info| CommandSurfaceCommandProvenance {
                    command: info.descriptor.name.clone(),
                    registry: CommandSurfaceRegistry::Extension,
                }),
        );
        for entry in self.capabilities.entries() {
            if !entry.command.is_hide_set() {
                provenance.push(CommandSurfaceCommandProvenance {
                    command: entry.capability.name().to_string(),
                    registry: CommandSurfaceRegistry::Descriptor,
                });
            }
            command = command.subcommand(entry.command.clone());
        }
        let support = self
            .capabilities
            .entries()
            .iter()
            .filter_map(|entry| entry.capability.lab_command_route_support())
            .collect::<Vec<_>>();
        ComposedCommandRegistry {
            command: crate::command_contract::scope_composed_lab_cli_arguments(command, &support),
            provenance,
        }
    }

    fn capability_matches<'a>(
        &'a self,
        matches: &'a ArgMatches,
    ) -> Option<(&'a dyn CliCapability, &'a ArgMatches)> {
        let (name, sub_matches) = matches.subcommand()?;
        self.capabilities
            .find(name)
            .map(|capability| (capability, sub_matches))
    }

    fn run_capability_with_admission_evidence(
        &self,
        capability: &dyn CliCapability,
        matches: &ArgMatches,
        normalized: &[String],
        evidence: crate::core::parsed_command_preflight::ResourceAdmissionEvidence,
    ) -> (crate::core::Result<serde_json::Value>, i32) {
        let input = capability.preflight(matches, normalized);
        let mut policy = capability.preflight_policy(matches, normalized);
        // Runtime owns the host observation. A capability can declare its
        // requirement but cannot bless itself by supplying admission evidence.
        policy.resource_admission_evidence = evidence;
        let preflight =
            match crate::core::parsed_command_preflight::resolve_parsed_command_preflight(
                normalized.to_vec(),
                input,
                policy,
            ) {
                Ok(preflight) => preflight,
                Err(error) => return (Err(error), 2),
            };
        crate::core::parsed_command_preflight::capture_result(preflight.clone());
        crate::commands::utils::execution_provenance::capture(&preflight);
        if matches!(
            preflight.resource_admission,
            crate::core::parsed_command_preflight::ResourceAdmissionDecision::Rejected { .. }
        ) {
            return (
                Err(resource_admission_error(&preflight.resource_admission)),
                2,
            );
        }
        match capability.run(matches) {
            Ok((value, exit_code)) => (Ok(value), exit_code),
            Err(error) => (Err(error), 2),
        }
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

/// Config inspection must not initialize daemon reconciliation, provider
/// discovery, or deferred workers. Those are unrelated to a local JSON read.
fn run_config_read_fast_path(args: &[String]) -> Option<std::process::ExitCode> {
    let cli = Cli::try_parse_from(args).ok()?;
    let Commands::Config(config) = cli.command else {
        return None;
    };
    if !crate::commands::config::is_read(&config) {
        return None;
    }

    let (result, exit_code) = output::map_cmd_result_to_json(crate::commands::config::run(config));
    output_runtime::emit_json_result_for_identity(
        result,
        cli.output.as_deref().and_then(std::path::Path::to_str),
        exit_code,
        &output::CommandIdentity::with_operation("config", "show"),
    );
    Some(std::process::ExitCode::from(exit_code_to_u8(exit_code)))
}

/// Durable Cook must not create a recipe under a runtime that the existing
/// update policy has already proven incompatible with the allowed stable.
/// Preview stays read-only and deliberately does not acquire this admission.
fn guard_cook_runtime_compatibility(
    cli: &Cli,
    normalized_args: &[String],
) -> homeboy::core::Result<()> {
    let is_durable_cook = matches!(
        &cli.command,
        Commands::AgentTask(agent_task)
            if matches!(
                &agent_task.command,
                crate::commands::agent_task::AgentTaskCommand::Cook(cook) if !cook.preview
            )
    );
    if !is_durable_cook {
        return Ok(());
    }
    let controller = homeboy_product_identity::build_identity();
    let Some(stable) =
        homeboy_upgrade::upgrade::update_check::incompatible_allowed_stable(&controller.display)
    else {
        return Ok(());
    };
    let replay = homeboy_core::engine::shell::quote_args(normalized_args);
    let recovery = format!("homeboy upgrade && {replay}");
    let mut error = homeboy::core::Error::validation_invalid_argument(
        "runtime_set",
        format!(
            "Durable Cook refused before preview or materialization because installed controller `{}` cannot satisfy the declared runtime contracts for latest allowed stable `{}`",
            controller.display, stable.version
        ),
        Some(controller.display),
        Some(vec![recovery.clone()]),
    );
    error.details["runtime_set"] = serde_json::json!({
        "latest_allowed_stable": stable.version,
        "required_contracts": stable.compatibility.map(|compatibility| compatibility.required_contracts),
        "recovery_command": recovery,
        "preserved_invocation": replay,
    });
    Err(error)
}

fn schedule_runner_exec_recovery() {
    // Unit tests execute this code inside the libtest binary. Re-executing
    // current_exe there recursively launches the complete test harness.
    if cfg!(test) {
        return;
    }
    // Startup recovery is one unit of work, so the boundary resolves the roots
    // once and every step below claims, spawns against, and terminalizes the
    // same installation. Each callee resolving its own would let the survey,
    // the owner claim, and the failure record land in different homes (#7505).
    let Ok(roots) = homeboy::core::paths::PathRoots::from_environment() else {
        return;
    };
    let Ok(Some(schedule)) = crate::runner::schedule_terminal_runner_exec_recovery(&roots) else {
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
                &roots,
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
            &roots,
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
    let is_cook_preview = matches!(
        &cli.command,
        Commands::AgentTask(agent_task)
            if matches!(
                &agent_task.command,
                crate::commands::agent_task::AgentTaskCommand::Cook(cook) if cook.preview
            )
    );
    if is_cook_preview {
        // Preview compiles a read-only controller-local plan. It must reach the
        // placement bypass below without sealing or re-executing a runtime.
        return Ok(None);
    }

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
    // Boundary: sealing the controller for a cook is one unit of work (#7505).
    let roots = homeboy::core::paths::PathRoots::from_environment()?;
    let pinned = crate::agents::agent_tasks::lifecycle::pin_current_controller_runtime(
        roots.data(),
        &request_id,
        || Ok(false),
    )
    .map_err(|error| annotate_cook_seal_failure(error, &request_id, normalized_args))?;
    let prompt_snapshot = match &cli.command {
        Commands::AgentTask(agent_task) => match &agent_task.command {
            crate::commands::agent_task::AgentTaskCommand::Cook(cook) => cook
                .prompt_snapshot
                .as_ref()
                .map(|snapshot| snapshot.content.clone()),
            _ => None,
        },
        _ => None,
    };
    let mut command = ProcessCommand::new(&pinned);
    command
        .args(&normalized_args[1..])
        .env(COOK_PINNED_RUNTIME_ENV, &pinned);
    let status = if let Some(prompt) = prompt_snapshot {
        command.stdin(std::process::Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(format!(
                    "execute pinned controller runtime {}",
                    pinned.display()
                )),
            )
        })?;
        child
            .stdin
            .take()
            .ok_or_else(|| {
                homeboy::core::Error::internal_unexpected(
                    "pinned Cook runtime stdin was not piped".to_string(),
                )
            })?
            .write_all(prompt.as_bytes())
            .map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some("write captured Cook prompt to pinned runtime stdin".to_string()),
                )
            })?;
        child.wait().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("wait for pinned Cook runtime".to_string()),
            )
        })?
    } else {
        command.status().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(format!(
                    "execute pinned controller runtime {}",
                    pinned.display()
                )),
            )
        })?
    };
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
    let run_id: Option<String> = match &cli.command {
        Commands::AgentTask(agent_task) => match &agent_task.command {
            crate::commands::agent_task::AgentTaskCommand::Run(args) => Some(args.run_id.clone()),
            crate::commands::agent_task::AgentTaskCommand::Resume(args)
                if crate::agents::agent_tasks::service::terminal_transport_recovery_required(
                    &args.run_id,
                ) =>
            {
                None
            }
            crate::commands::agent_task::AgentTaskCommand::Resume(args) => {
                Some(args.run_id.clone())
            }
            crate::commands::agent_task::AgentTaskCommand::Accept(args) => {
                Some(args.run_id.clone())
            }
            // Promotion mutates the durable source run (checkpointing apply and
            // final reports) and must therefore execute under the controller
            // runtime that admitted that run. Without this branch a promoted
            // runtime could own the process that writes an older run's record,
            // and any live stderr progress would be stranded behind a later
            // routing boundary.
            crate::commands::agent_task::AgentTaskCommand::Promote(args) => {
                let record = crate::agents::agent_tasks::lifecycle::status(&args.source).ok();
                if let Some(record) = record.as_ref() {
                    // Repair immutable evidence before handing mutation back to
                    // the historical controller that admitted this run.
                    crate::agents::agent_tasks::service::recover_missing_promotion_aggregate(
                        &record.run_id,
                    )?;
                }
                record.map(|record| record.run_id)
            }
            crate::commands::agent_task::AgentTaskCommand::CookContinue(args) => {
                if matches!(cli.placement, crate::cli_surface::Placement::Local) {
                    return Ok(None);
                }
                return delegate_cook_continue_to_pinned_runtime(
                    &args.cook_or_attempt_id,
                    normalized_args,
                );
            }
            _ => None,
        },
        _ => None,
    };
    let Some(run_id) = run_id else {
        return Ok(None);
    };
    delegate_agent_task_lifecycle_to_resolved_runtime(&run_id, normalized_args)
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
    let record = crate::agents::agent_tasks::lifecycle::status(run_id)?;
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
/// `cli` block, and broken-link health comes from
/// `broken_extension_links_in_root()`.
/// Neither reads readiness, so the rendered `--help` surface and the augmented
/// parser are byte-identical either way (#10616).
fn collect_extension_cli_info_metadata_only() -> ExtensionCliDiscovery {
    collect_extension_cli_info_with(ExtensionReadinessMode::Cached)
}

fn collect_extension_cli_info_with(readiness: ExtensionReadinessMode) -> ExtensionCliDiscovery {
    let catalog = homeboy_core::extension::catalog::list_api(&ExtensionApiCatalogRequest {
        schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
    });
    let readiness_by_id =
        commands::extension::extension_inventory_readiness(&catalog.entries, readiness);
    let broken_link_ids: Vec<String> = catalog
        .entries
        .iter()
        .filter(|entry| {
            entry.diagnostic.as_ref().is_some_and(|diagnostic| {
                diagnostic.code == ExtensionApiCatalogDiagnosticCode::BrokenInstallation
            })
        })
        .map(|entry| entry.id.clone())
        .collect();

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
                let health = catalog
                    .entries
                    .iter()
                    .find(|entry| entry.id == m.id)
                    .map(|entry| {
                        extension_command_health_from_api(&m, entry, readiness_by_id.get(&m.id))
                    })
                    .unwrap_or_else(extension_command_health_missing);
                let extension_manifest = extension_command_manifest(
                    &m,
                    &cli,
                    project_id_help.clone(),
                    args_help.clone(),
                    examples.clone(),
                    health,
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
    health: ExtensionCommandHealth,
) -> ExtensionCommandManifest {
    let project_id_help = project_id_help.unwrap_or_else(|| "Project ID".to_string());
    let args_help = args_help.unwrap_or_else(|| "Command arguments".to_string());
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

fn extension_command_health_from_api(
    extension: &InstalledExtensionManifest,
    entry: &ExtensionApiCatalogEntry,
    readiness: Option<&ExtensionApiReadinessStatus>,
) -> ExtensionCommandHealth {
    // An extension whose `ready_check` was never run is `unknown`, not `ready`.
    // A command-health contract that treated an absent measurement as ready
    // would reproduce the fail-open defect class in #10685. (#10616)
    let readiness_state = readiness.map(|status| status.state);
    let readiness_unknown = readiness_state == Some(ExtensionApiReadinessState::Unknown);
    let error = entry
        .diagnostic
        .as_ref()
        .map(|diagnostic| diagnostic.category.clone());
    let compatible = is_extension_compatible(extension, None);

    let status = if error.is_some() {
        "error"
    } else if !compatible {
        "incompatible"
    } else if readiness_unknown {
        "unknown"
    } else if readiness.is_some_and(|status| status.ready == Some(true)) {
        "ready"
    } else if readiness_state == Some(ExtensionApiReadinessState::TimedOut) {
        "timed_out"
    } else {
        "not_ready"
    };

    ExtensionCommandHealth {
        status: status.to_string(),
        ready: readiness.is_some_and(|status| status.ready == Some(true)),
        compatible,
        linked: is_extension_linked(&extension.id),
        reason: error.or_else(|| readiness.and_then(|status| status.reason.clone())),
        detail: readiness.and_then(|status| status.detail.clone()),
    }
}

fn extension_command_health_missing() -> ExtensionCommandHealth {
    ExtensionCommandHealth {
        status: "unknown".to_string(),
        ready: false,
        compatible: false,
        linked: false,
        reason: Some("summary_missing".to_string()),
        detail: Some("Extension loaded, but no extension catalog entry was available".to_string()),
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
            "Extension health warning: {} broken extension link(s): {}. Run `homeboy extension list` for details.",
            extension_health.broken_link_ids.len(),
            extension_health.broken_link_ids.join(", ")
        ));
        lines.extend(extension_health.broken_link_ids.iter().map(|id| {
            format!(
                "Repair `{id}`: `homeboy extension relink {id} <path>` or `homeboy extension uninstall {id}`."
            )
        }));
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
    refresh_error_details: Option<serde_json::Value>,
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
                refresh_error_details: None,
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
                    refresh_error_details: None,
                },
            )
        }
        Err(error) => {
            let timed_out = error.code == crate::core::ErrorCode::RemoteCommandTimeout;
            let refresh_error_code = error.code.as_str().to_string();
            let mut resolved = observed;
            resolved.reasons.insert(
                0,
                if timed_out {
                    "bounded_admission_refresh_timeout"
                } else {
                    "bounded_admission_refresh_failed"
                }
                .to_string(),
            );
            (
                resolved,
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
                    refresh_error_details: Some(error.details),
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
    normalized_args: &[String],
    preflight_input: crate::core::parsed_command_preflight::ParsedCommandPreflightInput,
    output_file: Option<&str>,
    command_identity: &output::CommandIdentity,
) -> Option<i32> {
    preflight_hot_command_with_input(
        cli,
        normalized_args,
        preflight_input,
        output_file,
        command_identity,
        || crate::commands::resources::run_preflight(),
    )
}

fn preflight_composed_lab_route(
    route: &LabCommandRoute,
    options: &crate::commands::route::ComposedLabRouteOptions<'_>,
    output_file: Option<&str>,
    command_identity: &output::CommandIdentity,
) -> Option<i32> {
    let hot_command = resource_policy::hot_command_for_lab_route(route)?;
    let Ok((resources, _)) = crate::commands::resources::run_preflight() else {
        return None;
    };
    let mut readiness = hot_command
        .lab_offload_supported
        .then(|| crate::runner::lab_runner_readiness().ok())
        .flatten();
    let observed_at_ms = unix_timestamp_ms();
    if hot_command.lab_offload_supported
        && options.runner.is_none()
        && !matches!(options.placement, crate::cli_surface::Placement::Local)
        && resource_policy::evaluate_with_runner_hint(hot_command, &resources, readiness.as_ref())
            .is_some()
    {
        if let Some(observed) = readiness.take() {
            let (resolved, _) = resolve_terminal_lab_inventory(observed, observed_at_ms, || {
                let refreshed = crate::runner::refresh_lab_runner_readiness_for_admission()?;
                Ok((refreshed, unix_timestamp_ms()))
            });
            readiness = Some(resolved);
        }
    }
    let warning =
        resource_policy::evaluate_with_runner_hint(hot_command, &resources, readiness.as_ref());
    let runner_hosted = resource_policy::is_runner_hosted_exec();
    let runner_admits_offload = options.runner.is_some()
        || readiness.as_ref().is_some_and(|readiness| {
            readiness.state == crate::runner::runners::LabRunnerReadinessState::ConnectedReady
                && readiness.selected_runner_id.is_some()
        });
    let auto_local_capacity_fallback = resource_policy::admits_auto_local_capacity_fallback(
        hot_command,
        &resources,
        readiness.as_ref(),
        options.placement,
    );
    let mut context = resource_policy::resource_policy_context_from_evaluation(
        hot_command,
        &resources,
        if runner_hosted {
            None
        } else {
            warning.as_ref()
        },
        options.placement.is_explicit_local_override(),
        auto_local_capacity_fallback,
        readiness.as_ref(),
        runner_hosted,
    );
    if let Some(runner) = options.runner {
        context.runner_selection.reason = "explicit_lab_runner".to_string();
        context.runner_selection.runner_id = Some(runner.to_string());
    }
    resource_policy::capture_context(context);
    let warning = warning?;
    if let Some(error) = resource_policy::non_interactive_preflight_error(
        &warning,
        options.placement.is_explicit_local_override() || runner_hosted,
        is_interactive_shell(),
        resource_policy::admission_recovery(
            &std::env::args().collect::<Vec<_>>(),
            readiness.as_ref(),
        ),
        runner_admits_offload || auto_local_capacity_fallback,
    ) {
        output_runtime::emit_json_result_for_identity(Err(error), output_file, 2, command_identity);
        return Some(2);
    }
    None
}

fn resolve_composed_capability_preflight(
    capability: &dyn CliCapability,
    matches: &ArgMatches,
    route: &LabCommandRoute,
    options: &crate::commands::route::ComposedLabRouteOptions<'_>,
    normalized: &[String],
) -> crate::core::Result<crate::core::parsed_command_preflight::ParsedCommandPreflightResult> {
    use crate::core::parsed_command_preflight::{
        ControllerExecution, GenericRoutePolicySnapshot, LabReadinessSnapshot, LabRouteIntent,
        PlacementIntent, ResourceAdmissionEvidence, ResourceHeat, RunnerIntent,
    };

    let mut input = capability.preflight(matches, normalized);
    input.controller_execution = ControllerExecution::Ordinary;
    input.placement = match options.placement {
        crate::cli_surface::Placement::Auto => PlacementIntent::Auto,
        crate::cli_surface::Placement::Local => PlacementIntent::Local,
        crate::cli_surface::Placement::Lab => PlacementIntent::Lab,
        crate::cli_surface::Placement::LabOrLocal => PlacementIntent::LabOrLocal,
    };
    input.runner = options
        .runner
        .map(|runner| RunnerIntent::Explicit(runner.to_string()))
        .unwrap_or(RunnerIntent::Default);
    let route_contract = route.lab_route_contract().ok_or_else(|| {
        crate::core::Error::validation_invalid_argument(
            "placement",
            "this composed command has no Lab route contract",
            None,
            None,
        )
    })?;
    input.lab_route = LabRouteIntent::Supported {
        automatic: matches!(
            route_contract.command.portability,
            crate::command_contract::LabCommandPortability::Portable
        ),
    };

    let context = resource_policy::captured_context();
    let readiness = context
        .as_ref()
        .map(|context| LabReadinessSnapshot {
            state: context.runner_selection.readiness_state.clone(),
            selected_runner_id: options
                .runner
                .map(str::to_string)
                .or_else(|| context.runner_selection.runner_id.clone()),
            available_runner_ids: context.runner_selection.available_runner_ids.clone(),
            reasons: context.runner_selection.readiness_reasons.clone(),
            remediation_commands: context.runner_selection.remediation_commands.clone(),
            repair_admitted_runner_ids: Vec::new(),
        })
        .or_else(|| {
            (options.placement != crate::cli_surface::Placement::Local)
                .then(|| crate::runner::lab_runner_readiness().ok())
                .flatten()
                .map(|readiness| resource_policy::lab_readiness_snapshot(&readiness))
        });
    let selected_runner_id = options.runner.map(str::to_string).or_else(|| {
        readiness
            .as_ref()
            .and_then(|value| value.selected_runner_id.clone())
    });
    let runner_admitted = selected_runner_id.as_ref().is_some_and(|selected| {
        readiness.as_ref().is_some_and(|readiness| {
            readiness.state == "connected_ready"
                && readiness
                    .available_runner_ids
                    .iter()
                    .any(|runner| runner == selected)
        })
    });
    let resource_admission_evidence = context
        .as_ref()
        .map(|context| ResourceAdmissionEvidence::Observed {
            pressure: match context.severity.as_str() {
                "hot" => ResourceHeat::Hot,
                "warm" => ResourceHeat::Warm,
                _ => ResourceHeat::None,
            },
        })
        .unwrap_or_else(|| match input.resource_admission {
            crate::core::parsed_command_preflight::ResourceAdmissionRequirement::Exempt => {
                ResourceAdmissionEvidence::Unavailable
            }
            crate::core::parsed_command_preflight::ResourceAdmissionRequirement::Required {
                ..
            } => crate::commands::resources::run_preflight()
                .map(|(resources, _)| resource_policy::resource_admission_evidence(&resources))
                .unwrap_or(ResourceAdmissionEvidence::Unavailable),
        });
    let command = crate::core::lab_routing::lab_offload_command_from_route_contract(route_contract);
    let automatic_authorized = options.runner.is_some()
        || crate::core::lab_routing::authorizes_policy_lab_runner(
            &command.command,
            options.placement,
            context.as_ref().map(|context| context.severity.as_str()),
        );
    let auto_local_capacity_fallback = context
        .as_ref()
        .is_some_and(|context| context.runner_selection.reason == "local_capacity_fallback");
    let mut policy = capability.preflight_policy(matches, normalized);
    policy.resource_admission_evidence = resource_admission_evidence;
    policy.resource_policy = context;
    policy.lab_readiness = readiness;
    policy.selected_runner_id = selected_runner_id.clone();
    policy.generic_route = GenericRoutePolicySnapshot {
        command_supports_lab: true,
        automatic_authorized,
        selected_runner_id,
    };
    policy.runner_admitted = runner_admitted;
    policy.auto_local_capacity_fallback = auto_local_capacity_fallback;
    crate::core::parsed_command_preflight::resolve_parsed_command_preflight(
        normalized.to_vec(),
        input,
        policy,
    )
}

fn preflight_hot_command_with_input(
    cli: &Cli,
    normalized_args: &[String],
    preflight_input: crate::core::parsed_command_preflight::ParsedCommandPreflightInput,
    output_file: Option<&str>,
    command_identity: &output::CommandIdentity,
    preflight: impl FnOnce() -> crate::commands::CmdResult<crate::commands::resources::DoctorOutput>,
) -> Option<i32> {
    if controller_owned_unmaterialized_resume(cli) {
        return None;
    }
    if let Err(err) = preflight_review_test_capability(cli) {
        output_runtime::emit_json_result_for_identity(Err(err), output_file, 2, command_identity);
        return Some(2);
    }
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
                && !nonlocal_cook_requires_durable_admission(cli)
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
            let required_lab_placement = required_lab_placement(cli, hot_command);
            let runner_admits_offload = resource_policy::runner_admits_lab_dispatch(
                hot_command,
                &resources,
                selected_lab_runner
                    .filter(|_| !matches!(cli.placement, crate::cli_surface::Placement::Local)),
                lab_readiness.as_ref(),
                required_lab_placement,
            );
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
            let required_lab_runner = required_lab_placement
                .then_some(selected_lab_runner)
                .flatten();
            // Required Lab placement resolves workload ownership before resource
            // guidance, whether policy selected the runner or the operator pinned
            // it. Controller pressure still matters for preparation and transport,
            // but it must not be presented as local provider execution.
            let warning = (!required_lab_placement)
                .then(|| {
                    resource_policy::evaluate_with_runner_hint(
                        hot_command,
                        &resources,
                        lab_readiness.as_ref(),
                    )
                })
                .flatten();
            let runner_hosted = resource_policy::is_runner_hosted_exec();
            if let Some(runner_id) = required_lab_runner {
                if let Some(notice) = resource_policy::lab_routed_controller_notice_message(
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
            let selected_runner_id = resource_policy_context.runner_selection.runner_id.clone();
            resource_policy::capture_context(resource_policy_context.clone());
            let result =
                match crate::core::parsed_command_preflight::resolve_parsed_command_preflight(
                    normalized_args.to_vec(),
                    preflight_input.clone(),
                    crate::core::parsed_command_preflight::ParsedCommandPolicySnapshot {
                        resource_admission_evidence: resource_policy::resource_admission_evidence(
                            &resources,
                        ),
                        resource_policy: Some(resource_policy_context),
                        lab_readiness: lab_readiness
                            .as_ref()
                            .map(|readiness| parsed_lab_readiness_snapshot(cli, readiness)),
                        selected_runner_id: selected_runner_id.clone(),
                        generic_route: generic_route_policy_snapshot(
                            cli,
                            selected_runner_id.clone(),
                        ),
                        deferred_pressure_refusal: warning.as_ref().is_some_and(|warning| {
                            review_test_deferred_workload_eligible(
                                cli,
                                warning,
                                runner_admits_offload,
                            )
                        }),
                        runner_admitted: runner_admits_offload,
                        runner_incompatible: review_test_runner_requirements(cli).is_some()
                            && selected_runner_id.is_some()
                            && !runner_admits_offload,
                        auto_local_capacity_fallback,
                    },
                ) {
                    Ok(result) => result,
                    Err(err) => {
                        output_runtime::emit_json_result_for_identity(
                            Err(err),
                            output_file,
                            2,
                            command_identity,
                        );
                        return Some(2);
                    }
                };
            crate::core::parsed_command_preflight::capture_result(result);
            if let Some(warning) = warning.as_ref() {
                if let Some(mut err) = resource_policy::non_interactive_preflight_error(
                    warning,
                    cli.placement.is_explicit_local_override() || runner_hosted,
                    is_interactive_shell(),
                    (!nonlocal_cook_requires_durable_admission(cli))
                        .then(|| {
                            resource_policy::admission_recovery(
                                normalized_args,
                                lab_readiness.as_ref(),
                            )
                        })
                        .flatten(),
                    runner_admits_offload || auto_local_capacity_fallback,
                ) {
                    if let Some(diagnostic) = lab_inventory_diagnostic {
                        err.details["lab_inventory_admission"] = serde_json::to_value(diagnostic)
                            .expect("Lab inventory admission diagnostic serializes");
                    }
                    if review_test_deferred_workload_eligible(cli, warning, runner_admits_offload)
                        || nonlocal_cook_requires_durable_admission(cli)
                    {
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

fn parsed_lab_readiness_snapshot(
    cli: &Cli,
    readiness: &crate::runner::runners::LabRunnerReadiness,
) -> crate::core::parsed_command_preflight::LabReadinessSnapshot {
    let mut snapshot = resource_policy::lab_readiness_snapshot(readiness);
    if let (Some(runner_id), Commands::Extension(args)) = (&cli.runner, &cli.command) {
        if args.is_readiness_repair_command()
            && crate::runner::runners::runner_readiness_repair_admitted(runner_id).unwrap_or(false)
        {
            snapshot.repair_admitted_runner_ids.push(runner_id.clone());
        }
    }
    snapshot
}

#[cfg(test)]
pub(crate) fn placement_directive(
    cli: &Cli,
    selected_runner_id: Option<&str>,
    auto_local_capacity_fallback: bool,
) -> crate::core::parsed_command_preflight::PlacementDirective {
    let normalized_args = vec!["homeboy".to_string()];
    let input = resource_policy::parsed_command_preflight_input(cli, &normalized_args);
    crate::core::parsed_command_preflight::resolve_parsed_command_preflight(
        normalized_args,
        input.clone(),
        crate::core::parsed_command_preflight::ParsedCommandPolicySnapshot {
            resource_admission_evidence:
                crate::core::parsed_command_preflight::ResourceAdmissionEvidence::Unavailable,
            resource_policy: None,
            lab_readiness: selected_runner_id.map(|runner_id| {
                crate::core::parsed_command_preflight::LabReadinessSnapshot {
                    state: "connected_ready".to_string(),
                    selected_runner_id: Some(runner_id.to_string()),
                    available_runner_ids: vec![runner_id.to_string()],
                    reasons: Vec::new(),
                    remediation_commands: Vec::new(),
                    repair_admitted_runner_ids: Vec::new(),
                }
            }),
            selected_runner_id: selected_runner_id.map(str::to_string),
            generic_route: generic_route_policy_snapshot(
                cli,
                selected_runner_id.map(str::to_string),
            ),
            deferred_pressure_refusal: false,
            runner_admitted: selected_runner_id.is_some(),
            runner_incompatible: false,
            auto_local_capacity_fallback,
        },
    )
    .expect("test placement fixture supplies admitted runner evidence")
    .placement
}

#[cfg(test)]
fn preflight_hot_command_with(
    cli: &Cli,
    output_file: Option<&str>,
    command_identity: &output::CommandIdentity,
    preflight: impl FnOnce() -> crate::commands::CmdResult<crate::commands::resources::DoctorOutput>,
) -> Option<i32> {
    let normalized_args = vec!["homeboy".to_string()];
    preflight_hot_command_with_input(
        cli,
        &normalized_args,
        resource_policy::parsed_command_preflight_input(cli, &normalized_args),
        output_file,
        command_identity,
        preflight,
    )
}

fn controller_owned_unmaterialized_resume(cli: &Cli) -> bool {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Resume(args),
    }) = &cli.command
    else {
        return false;
    };
    crate::agents::agent_tasks::lifecycle::exact_record(&args.run_id).is_ok_and(|record| {
        !record.state.is_terminal() && record.metadata["unmaterialized_cook_admission"].is_object()
    })
}

/// A non-local Cook owns a durable admission boundary. Resource pressure may
/// influence Lab placement, but cannot prevent creating an inspectable Cook.
/// Explicit local placement remains the only authorization for controller
/// provider execution.
fn nonlocal_cook_requires_durable_admission(cli: &Cli) -> bool {
    !matches!(cli.placement, crate::cli_surface::Placement::Local)
        && matches!(
            cli.command,
            Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                command: crate::commands::agent_task::AgentTaskCommand::Cook(_),
            })
        )
}

/// Verify `review test` can execute without creating work, admitting controller
/// capacity, selecting a runner, or preparing a Lab workspace.
fn preflight_review_test_capability(cli: &Cli) -> homeboy::core::Result<()> {
    let Commands::Review(review) = &cli.command else {
        return Ok(());
    };
    let Some(crate::commands::review::ReviewCommand::Test(args)) = review.command.as_ref() else {
        return Ok(());
    };

    let source = crate::commands::source_command::resolve_source_context(
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        None,
    )?;
    let passthrough_args = crate::commands::utils::args::filter_passthrough_args(
        crate::commands::utils::args::PassthroughCommand::Test,
        &args.args,
    );
    if args.should_use_self_check_dispatch(&passthrough_args)
        && source.component.has_script(ExtensionCapability::Test)
    {
        return Ok(());
    }

    crate::commands::source_command::resolve_source_context(
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        Some(ExtensionCapability::Test),
    )
	.and_then(|context| {
		homeboy::core::extension::resolve::resolve_execution_context(
			&context.component,
            ExtensionCapability::Test,
		)
		.map(|_| ())
	})
    .map_err(|mut error| {
        error.details["review_capability_preflight"] = serde_json::json!({
            "capability": "test",
            "effective_component": {
                "id": source.component_id,
                "source_path": source.source_path,
                "config_provenance": review_test_config_provenance(&source),
            },
        });
        error.with_hint(
            "Review capability preflight failed before resource admission or Lab routing; repair the component capability, then retry the same command.",
        )
    })
}

fn review_test_config_provenance(
    source: &homeboy::core::engine::execution_context::ExecutionContext,
) -> String {
    let portable_config = source.source_path.join("homeboy.json");
    if portable_config.is_file() {
        return format!("portable component config: {}", portable_config.display());
    }
    format!(
        "resolved component '{}' without a portable homeboy.json at {} (registry or synthetic target)",
        source.component_id,
        source.source_path.display()
    )
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

fn required_lab_placement(cli: &Cli, hot_command: resource_policy::HotCommand) -> bool {
    hot_command.lab_offload_supported
        && (cli.runner.is_some() || cli.placement == crate::cli_surface::Placement::Lab)
}

fn run_startup_update_checks(command: &Commands) {
    // Startup update checks — skip for upgrade (it handles this itself).
    if !matches!(
        command,
        Commands::Upgrade(_) | Commands::Daemon(_) | Commands::SelfCmd(_)
    ) {
        homeboy_upgrade::upgrade::update_check::run_startup_check();
        homeboy_upgrade::upgrade::extension_update_check::run_startup_check();
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

fn resource_admission_error(
    decision: &crate::core::parsed_command_preflight::ResourceAdmissionDecision,
) -> crate::core::Error {
    use crate::core::parsed_command_preflight::{
        ResourceAdmissionDecision, ResourceAdmissionEvidence,
    };

    let ResourceAdmissionDecision::Rejected {
        label,
        engages_at,
        evidence,
    } = decision
    else {
        unreachable!("only rejected admission decisions produce an error")
    };
    let observed = match evidence {
        ResourceAdmissionEvidence::Observed { pressure } => format!("{pressure:?}").to_lowercase(),
        ResourceAdmissionEvidence::Unavailable => "unavailable".to_string(),
    };
    let mut error = crate::core::Error::validation_invalid_argument(
        "resource-policy",
        format!(
            "Refusing to start `{label}`: resource admission requires pressure below {engages_at:?}, observed {observed}."
        ),
        None,
        None,
    );
    error.details["run_created"] = serde_json::Value::Bool(false);
    error.details["resource_admission"] =
        serde_json::to_value(decision).expect("resource admission decision serializes");
    error
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
Example: `homeboy agent-task cook --backend opencode --selector opencode.agent-task-executor --to-worktree repo@branch --goal 'Describe the task' --verify 'homeboy review test homeboy' --no-finalize`\n\
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

    // Entity matching (component/project/server/extension IDs) only makes
    // sense for a bare top-level token: that's the one place a user might
    // plausibly type an entity ID where a command was expected (`homeboy
    // catlog` instead of `homeboy component catlog`). For an unrecognized
    // subcommand nested under an already-known command (e.g. `agent-task
    // loop list`), the valid alternatives are a small, closed set that clap
    // itself already reports — there is no entity domain to consult. Running
    // full component/project inventory (which resolves git remotes for every
    // attached component) on every such error would turn a fail-fast parse
    // error into a slow, disk- and subprocess-heavy scan (#13630).
    let mut hints = command_domain_hints(&unrecognized, &parent_command).unwrap_or_else(|| {
        if parent_command.is_empty() {
            entity_suggest::find_entity_match(&unrecognized)
                .map(|entity_match| {
                    entity_suggest::generate_entity_hints(
                        &entity_match,
                        &parent_command,
                        &unrecognized,
                    )
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        }
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
        hints.extend(extension_health.broken_link_ids.iter().map(|id| {
            format!(
                "broken extension link `{id}`; repair with `homeboy extension relink {id} <path>` or `homeboy extension uninstall {id}`"
            )
        }));
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

    /// Tests are the entry point for their own unit of work, so the store
    /// resolves once here (#7505).
    fn test_lifecycle_store() -> homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore {
        homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
            .expect("lifecycle store")
    }
    use super::*;
    use clap::Parser;
    use sha2::{Digest, Sha256};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    #[test]
    fn empty_admission_requirements_avoid_runner_lookup() {
        assert!(runner_satisfies_admission_capabilities(
            "runner-that-does-not-exist",
            &BTreeSet::new()
        )
        .expect("empty requirements"));
    }

    #[test]
    fn capability_admission_rejects_unknown_runners() {
        let error = runner_satisfies_admission_capabilities(
            "runner-that-does-not-exist",
            &BTreeSet::from(["homeboy"]),
        )
        .expect_err("unknown runner must fail admission");

        assert_eq!(error.details["id"], "runner_not_found");
    }

    #[test]
    fn capability_admission_rejects_empty_api_envelopes() {
        let error = capabilities_from_response(RunnerApiCapabilitiesResponse {
            schema: homeboy_runner_contract::RUNNER_API_CAPABILITIES_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "runner-a".to_string(),
            capabilities: None,
            failure: None,
        })
        .expect_err("empty envelope must fail admission");

        assert!(error
            .message
            .contains("omitted both capabilities and failure"));
    }

    struct AdmissionFixtureCapability;

    static ADMISSION_FIXTURE_CAPABILITY: AdmissionFixtureCapability = AdmissionFixtureCapability;
    static ADMISSION_FIXTURE_RUNS: AtomicUsize = AtomicUsize::new(0);
    static ADMISSION_FIXTURE_CAPABILITIES: [&'static dyn CliCapability; 1] =
        [&ADMISSION_FIXTURE_CAPABILITY];

    struct ComposedDoctorCapability;
    struct HiddenComposedDoctorCapability;

    static COMPOSED_DOCTOR_CAPABILITY: ComposedDoctorCapability = ComposedDoctorCapability;
    static HIDDEN_COMPOSED_DOCTOR_CAPABILITY: HiddenComposedDoctorCapability =
        HiddenComposedDoctorCapability;
    static COMPOSED_DOCTOR_CAPABILITIES: [&'static dyn CliCapability; 2] = [
        &COMPOSED_DOCTOR_CAPABILITY,
        &HIDDEN_COMPOSED_DOCTOR_CAPABILITY,
    ];

    impl CliCapability for ComposedDoctorCapability {
        fn name(&self) -> &'static str {
            "triage"
        }

        fn command(&self) -> Command {
            Command::new(self.name()).about("composed doctor fixture")
        }

        fn run(&self, _matches: &ArgMatches) -> crate::core::Result<(serde_json::Value, i32)> {
            unreachable!("the command-surface fixture is never dispatched")
        }
    }

    impl CliCapability for HiddenComposedDoctorCapability {
        fn name(&self) -> &'static str {
            "hidden-doctor-fixture"
        }

        fn command(&self) -> Command {
            Command::new(self.name()).hide(true)
        }

        fn run(&self, _matches: &ArgMatches) -> crate::core::Result<(serde_json::Value, i32)> {
            unreachable!("the command-surface fixture is never dispatched")
        }
    }

    impl CliCapability for AdmissionFixtureCapability {
        fn name(&self) -> &'static str {
            "admission-fixture"
        }

        fn command(&self) -> Command {
            Command::new(self.name()).arg(
                clap::Arg::new("exempt")
                    .long("exempt")
                    .action(clap::ArgAction::SetTrue),
            )
        }

        fn preflight(
            &self,
            matches: &ArgMatches,
            _normalized_args: &[String],
        ) -> crate::core::parsed_command_preflight::ParsedCommandPreflightInput {
            use crate::core::parsed_command_preflight::{
                ControllerExecution, DeferredWorkloadPolicy, LabRouteIntent, ParsedCommandIdentity,
                ParsedCommandPreflightInput, PlacementIntent, ProvenanceRequirement,
                ResourceAdmissionRequirement, ResourceHeat, RunnerIntent, RunnerNormalization,
            };
            ParsedCommandPreflightInput {
                identity: ParsedCommandIdentity {
                    family: self.name().to_string(),
                    operation: Vec::new(),
                },
                resource_admission: if matches.get_flag("exempt") {
                    ResourceAdmissionRequirement::Exempt
                } else {
                    ResourceAdmissionRequirement::Required {
                        label: "admission fixture".to_string(),
                        engages_at: ResourceHeat::Warm,
                    }
                },
                controller_execution: ControllerExecution::ControllerOnly,
                deferred_workload: DeferredWorkloadPolicy::Forbidden,
                placement: PlacementIntent::Local,
                runner: RunnerIntent::Default,
                runner_normalization: RunnerNormalization::None,
                lab_route: LabRouteIntent::Unsupported,
                provenance: ProvenanceRequirement::CaptureExecution,
            }
        }

        fn run(&self, _matches: &ArgMatches) -> crate::core::Result<(serde_json::Value, i32)> {
            ADMISSION_FIXTURE_RUNS.fetch_add(1, Ordering::SeqCst);
            Ok((serde_json::json!({ "status": "ran" }), 0))
        }
    }

    fn run_admission_fixture(
        evidence: crate::core::parsed_command_preflight::ResourceAdmissionEvidence,
        exempt: bool,
    ) -> std::process::ExitCode {
        let runtime = CliRuntime::with_capabilities(&ADMISSION_FIXTURE_CAPABILITIES);
        let mut normalized = vec!["homeboy".to_string(), "admission-fixture".to_string()];
        if exempt {
            normalized.push("--exempt".to_string());
        }
        let matches = runtime
            .build_augmented_command()
            .try_get_matches_from(normalized.clone())
            .expect("fixture capability parses");
        runtime.run_matches_with_capability_admission(matches, normalized, Some(evidence))
    }

    #[test]
    fn typed_capability_resource_admission_rejects_before_run_and_admits_once() {
        use crate::core::parsed_command_preflight::{
            ResourceAdmissionDecision, ResourceAdmissionEvidence, ResourceHeat,
        };

        ADMISSION_FIXTURE_RUNS.store(0, Ordering::SeqCst);
        crate::core::parsed_command_preflight::reset_captured_result_for_test();
        let rejected = run_admission_fixture(
            ResourceAdmissionEvidence::Observed {
                pressure: ResourceHeat::Warm,
            },
            false,
        );
        assert_eq!(rejected, std::process::ExitCode::from(2));
        assert_eq!(ADMISSION_FIXTURE_RUNS.load(Ordering::SeqCst), 0);
        assert!(matches!(
            crate::core::parsed_command_preflight::captured_result()
                .expect("rejected preflight is captured")
                .resource_admission,
            ResourceAdmissionDecision::Rejected { .. }
        ));

        crate::core::parsed_command_preflight::reset_captured_result_for_test();
        let admitted = run_admission_fixture(
            ResourceAdmissionEvidence::Observed {
                pressure: ResourceHeat::None,
            },
            false,
        );
        assert_eq!(admitted, std::process::ExitCode::SUCCESS);
        assert_eq!(ADMISSION_FIXTURE_RUNS.load(Ordering::SeqCst), 1);
        assert_eq!(
            crate::core::parsed_command_preflight::captured_result()
                .expect("admitted preflight is captured")
                .resource_admission,
            ResourceAdmissionDecision::Admitted
        );

        crate::core::parsed_command_preflight::reset_captured_result_for_test();
        let unavailable = run_admission_fixture(ResourceAdmissionEvidence::Unavailable, false);
        assert_eq!(unavailable, std::process::ExitCode::from(2));
        assert_eq!(ADMISSION_FIXTURE_RUNS.load(Ordering::SeqCst), 1);

        crate::core::parsed_command_preflight::reset_captured_result_for_test();
        let exempt = run_admission_fixture(
            ResourceAdmissionEvidence::Observed {
                pressure: ResourceHeat::Hot,
            },
            true,
        );
        assert_eq!(exempt, std::process::ExitCode::SUCCESS);
        assert_eq!(ADMISSION_FIXTURE_RUNS.load(Ordering::SeqCst), 2);
        assert_eq!(
            crate::core::parsed_command_preflight::captured_result()
                .expect("exempt preflight is captured")
                .resource_admission,
            ResourceAdmissionDecision::NotRequired
        );
    }

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

    #[test]
    fn explicit_lab_placement_resolves_only_a_ready_policy_runner() {
        use crate::runner::runners::LabRunnerReadinessState;

        let cli = Cli::parse_from([
            "homeboy",
            "--placement",
            "lab",
            "agent-task",
            "cook",
            "--prompt",
            "route the provider attempt",
            "--to-worktree",
            "fixture@lab-placement",
            "--verify",
            "true",
        ]);
        let command = resource_policy::hot_command(&cli.command).expect("verified Cook is hot");
        assert!(required_lab_placement(&cli, command));
        assert_eq!(cli.runner, None);

        for (state, expected) in [
            (LabRunnerReadinessState::ConnectedReady, Some("lab-a")),
            (LabRunnerReadinessState::Stale, None),
            (LabRunnerReadinessState::Disconnected, None),
            (LabRunnerReadinessState::CapacityBlocked, None),
        ] {
            let readiness = lab_readiness(state);
            let selected =
                resource_policy_runner_hint(&cli, readiness.selected_runner_id.as_deref());
            assert_eq!(selected, expected, "{state:?}");
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
    fn metadata_extension_show_bypasses_hot_admission_without_lab_inventory() {
        use crate::core::parsed_command_preflight::{
            resolve_parsed_command_preflight, LabReadinessSnapshot, ParsedCommandPolicySnapshot,
            ResourceAdmissionDecision, ResourceAdmissionEvidence, ResourceHeat,
        };

        let cli = Cli::parse_from(["homeboy", "extension", "show", "fixture"]);
        let normalized_args = vec!["homeboy".to_string()];
        let input = resource_policy::parsed_command_preflight_input(&cli, &normalized_args);

        assert_eq!(
            input.resource_admission,
            crate::core::parsed_command_preflight::ResourceAdmissionRequirement::Exempt
        );
        assert_eq!(
            input.lab_route,
            crate::core::parsed_command_preflight::LabRouteIntent::Unsupported
        );

        for state in ["stale", "absent"] {
            let result = resolve_parsed_command_preflight(
                normalized_args.clone(),
                input.clone(),
                ParsedCommandPolicySnapshot {
                    resource_admission_evidence: ResourceAdmissionEvidence::Observed {
                        pressure: ResourceHeat::Hot,
                    },
                    resource_policy: None,
                    lab_readiness: Some(LabReadinessSnapshot {
                        state: state.to_string(),
                        selected_runner_id: None,
                        available_runner_ids: Vec::new(),
                        reasons: vec!["inventory is unavailable".to_string()],
                        remediation_commands: Vec::new(),
                        repair_admitted_runner_ids: Vec::new(),
                    }),
                    selected_runner_id: None,
                    generic_route: generic_route_policy_snapshot(&cli, None),
                    deferred_pressure_refusal: false,
                    runner_admitted: false,
                    runner_incompatible: false,
                    auto_local_capacity_fallback: false,
                },
            )
            .expect("metadata inspection must remain admitted locally");

            assert_eq!(
                result.resource_admission,
                ResourceAdmissionDecision::NotRequired
            );
        }
    }

    #[test]
    fn nonlocal_cook_persists_before_hot_resource_admission() {
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "fixture@durable-admission",
            "--verify",
            "true",
        ]);

        assert!(nonlocal_cook_requires_durable_admission(&cli));
        assert_eq!(
            preflight_hot_command_with(
                &cli,
                None,
                &output::CommandIdentity::with_operation("agent-task", "cook"),
                || Ok((hot_resources(), 0)),
            ),
            None,
            "Cook must reach its durable admission lifecycle before resource placement"
        );
    }

    #[test]
    fn unmaterialized_resume_bypasses_hot_noninteractive_resource_refusal() {
        crate::test_support::with_isolated_home(|_| {
            let run_id = "hot-unmaterialized-resume";
            crate::agents::agent_tasks::lifecycle::record_unmaterialized_cook_admission_in_store(
                &test_lifecycle_store(),
                run_id,
                serde_json::json!({
                    "request_ref": "sha256:resume",
                    "placement": { "local_fallback": false },
                }),
                "blocked_runner_unavailable",
                "waiting",
            )
            .expect("admitted");
            let cli = Cli::parse_from(["homeboy", "agent-task", "resume", run_id]);

            assert_eq!(
                preflight_hot_command_with(
                    &cli,
                    None,
                    &output::CommandIdentity::with_operation("agent-task", "resume"),
                    || -> crate::commands::CmdResult<crate::commands::resources::DoctorOutput> {
                        panic!("controller lifecycle resume must bypass resource preflight")
                    },
                ),
                None
            );
            let record = crate::agents::agent_tasks::lifecycle::exact_record(run_id)
                .expect("controller-owned admission");
            assert_eq!(
                record.metadata["unmaterialized_cook_admission"]["binding"]["placement"]
                    ["local_fallback"],
                false
            );
        });
    }

    #[test]
    fn review_test_missing_capability_fails_before_resource_admission_for_all_placements() {
        crate::test_support::with_isolated_home(|_| {
            let component = tempfile::tempdir().expect("component directory");
            std::fs::write(
                component.path().join("homeboy.json"),
                r#"{"id":"unconfigured-review-test"}"#,
            )
            .expect("write component config");
            let path = component.path().to_str().expect("component path");

            for placement in [
                vec!["--placement", "local"],
                vec!["--placement", "auto"],
                vec!["--placement", "lab"],
                vec!["--runner", "homeboy-lab"],
            ] {
                let mut argv = vec!["homeboy"];
                argv.extend(placement.iter().copied());
                argv.extend(["review", "test", "--path", path]);
                let cli = Cli::parse_from(argv);

                assert_eq!(
                    preflight_hot_command_with(
                        &cli,
                        None,
                        &output::CommandIdentity::with_operation("review", "test"),
                        || -> crate::commands::CmdResult<crate::commands::resources::DoctorOutput> {
                            panic!("capability preflight must run before resource admission")
                        },
                    ),
                    Some(2),
                    "{placement:?}"
                );

                let error = preflight_review_test_capability(&cli)
                    .expect_err("unconfigured review test must fail capability preflight");
                assert!(error.message.contains("No extension provider configured"));
                assert_eq!(
                    error.details["review_capability_preflight"]["effective_component"]["id"],
                    "unconfigured-review-test"
                );
                assert!(
                    error.details["review_capability_preflight"]["effective_component"]
                        ["config_provenance"]
                        .as_str()
                        .expect("config provenance")
                        .contains("homeboy.json")
                );
                assert!(error.hints.iter().any(|hint| hint
                    .message
                    .contains("component set unconfigured-review-test --extension")));
            }
        });
    }

    #[test]
    fn review_test_capability_preflight_only_accepts_eligible_component_scripts() {
        crate::test_support::with_isolated_home(|home| {
            let script_component = tempfile::tempdir().expect("script component directory");
            std::fs::write(
                script_component.path().join("homeboy.json"),
                r#"{"id":"script-review-test","scripts":{"test":["./test.sh"]}}"#,
            )
            .expect("write script component config");
            let script_path = script_component
                .path()
                .to_str()
                .expect("script component path");
            let script_cli = Cli::parse_from(["homeboy", "review", "test", "--path", script_path]);
            preflight_review_test_capability(&script_cli)
                .expect("component-owned test script remains supported");

            for test_mode in [vec!["--skip-lint"], vec!["--", "--testsuite=imports"]] {
                for placement in [
                    vec!["--placement", "local"],
                    vec!["--placement", "auto"],
                    vec!["--placement", "lab"],
                    vec!["--runner", "homeboy-lab"],
                ] {
                    let mut argv = vec!["homeboy"];
                    argv.extend(placement.iter().copied());
                    argv.extend(["review", "test"]);
                    if test_mode.first().copied() != Some("--") {
                        argv.extend(test_mode.iter().copied());
                    }
                    argv.extend(["--path", script_path]);
                    if test_mode.first().copied() == Some("--") {
                        argv.extend(test_mode.iter().copied());
                    }
                    let cli = Cli::parse_from(argv);

                    let result = preflight_review_test_capability(&cli);
                    assert!(
						result.is_err(),
						"modified test mode requires an extension test provider: {test_mode:?}, {placement:?}"
					);
                    let error = result.expect_err("error checked above");
                    assert!(
                        error.message.contains("No extension provider configured"),
                        "{test_mode:?}, {placement:?}: {}",
                        error.message
                    );

                    assert_eq!(
                        preflight_hot_command_with(
                            &cli,
                            None,
                            &output::CommandIdentity::with_operation("review", "test"),
                            || -> crate::commands::CmdResult<crate::commands::resources::DoctorOutput> {
                                panic!("unsupported scripted test mode must fail before resource admission")
                            },
                        ),
                        Some(2),
                        "{test_mode:?}, {placement:?}"
                    );
                }
            }

            let extension_component = tempfile::tempdir().expect("extension component directory");
            std::fs::write(
                extension_component.path().join("homeboy.json"),
                r#"{"id":"extension-review-test","extensions":{"fixture-test":{}}}"#,
            )
            .expect("write extension component config");
            let extension_dir = home.path().join(".config/homeboy/extensions/fixture-test");
            std::fs::create_dir_all(&extension_dir).expect("extension directory");
            std::fs::write(
                extension_dir.join("fixture-test.json"),
                r#"{"name":"fixture-test","version":"1.0.0","test":{"extension_script":"test.sh"}}"#,
            )
            .expect("write extension manifest");
            let extension_path = extension_component
                .path()
                .to_str()
                .expect("extension component path");
            let extension_cli =
                Cli::parse_from(["homeboy", "review", "test", "--path", extension_path]);
            preflight_review_test_capability(&extension_cli)
                .expect("configured extension test support remains valid");
        });
    }

    #[test]
    fn review_test_capability_preflight_preserves_extension_override_diagnostics() {
        crate::test_support::with_isolated_home(|home| {
            let component = tempfile::tempdir().expect("component directory");
            std::fs::write(
                component.path().join("homeboy.json"),
                r#"{"id":"override-review-test","extensions":{"fixture-without-test":{}}}"#,
            )
            .expect("write component config");
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/fixture-without-test");
            std::fs::create_dir_all(&extension_dir).expect("extension directory");
            std::fs::write(
                extension_dir.join("fixture-without-test.json"),
                r#"{"name":"fixture-without-test","version":"1.0.0"}"#,
            )
            .expect("write extension manifest");
            let path = component.path().to_str().expect("component path");

            for placement in [
                vec!["--placement", "local"],
                vec!["--placement", "auto"],
                vec!["--placement", "lab"],
                vec!["--runner", "homeboy-lab"],
            ] {
                let mut argv = vec!["homeboy"];
                argv.extend(placement.iter().copied());
                argv.extend([
                    "review",
                    "test",
                    "--path",
                    path,
                    "--extension",
                    "fixture-without-test",
                ]);
                let cli = Cli::parse_from(argv);

                assert_eq!(
                    preflight_hot_command_with(
                        &cli,
                        None,
                        &output::CommandIdentity::with_operation("review", "test"),
                        || -> crate::commands::CmdResult<crate::commands::resources::DoctorOutput> {
                            panic!("invalid extension override must fail before resource admission")
                        },
                    ),
                    Some(2),
                    "{placement:?}"
                );
                let error = preflight_review_test_capability(&cli)
                    .expect_err("override without test support must fail capability preflight");
                assert!(error
                    .message
                    .contains("explicit extension override 'fixture-without-test'"));
                assert!(error.hints.iter().any(|hint| hint
                    .message
                    .contains("Upgrade or refresh the runner extension")));
            }
        });
    }

    #[test]
    fn cook_preview_bypasses_runtime_sealing_before_runner_routing() {
        crate::test_support::with_isolated_home(|home| {
            let cli = Cli::parse_from([
                "homeboy",
                "--runner",
                "homeboy-lab",
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
            let args = vec![
                "homeboy".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--preview".to_string(),
                "--prompt".to_string(),
                "inspect the task".to_string(),
                "--to-worktree".to_string(),
                "fixture@preview".to_string(),
                "--verify".to_string(),
                "true".to_string(),
            ];
            let before = std::fs::read_dir(home).expect("read isolated home").count();

            assert_eq!(
                delegate_agent_task_cook_to_pinned_runtime(&cli, &args)
                    .expect("preview bypasses runtime sealing"),
                None
            );
            assert_eq!(
                std::fs::read_dir(home).expect("read isolated home").count(),
                before,
                "preview must not create controller runtime state before runner routing"
            );
        });
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
            lab_readiness(crate::runner::runners::LabRunnerReadinessState::Absent),
            100,
            || -> crate::core::Result<_> { panic!("fresh inventory must not refresh") },
        );

        assert_eq!(
            resolved.state,
            crate::runner::runners::LabRunnerReadinessState::Absent
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
            lab_readiness(crate::runner::runners::LabRunnerReadinessState::Absent),
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
                    "--target-version",
                    homeboy_upgrade::upgrade::current_version(),
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
    fn cook_preview_reaches_backend_resolution_before_runtime_validation() {
        std::thread::Builder::new()
            .name("cook-preview-runtime-routing".to_string())
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                crate::test_support::with_isolated_home(|_| {
                    let preview = CliRuntime::new().run_from_args(argv(&[
                        "homeboy",
                        "agent-task",
                        "cook",
                        "--preview",
                    ]));

                    assert_eq!(preview, std::process::ExitCode::SUCCESS);
                });
            })
            .expect("spawn Cook preview runtime-routing test")
            .join()
            .expect("Cook preview runtime-routing test");
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
                    startup_fast_path_output(&CliRuntime::new(), &argv(values)),
                    Some(StartupFastPathOutput::Help(_))
                ),
                "{values:?} should render before runtime initialization"
            );
        }

        assert!(startup_fast_path_output(
            &CliRuntime::new(),
            &argv(&["homeboy", "help", "status"])
        )
        .is_none());
    }

    #[test]
    fn version_fast_path_reports_the_build_version_without_extension_discovery() {
        match startup_fast_path_output(&CliRuntime::new(), &argv(&["homeboy", "--version"])) {
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
        match startup_fast_path_output(&CliRuntime::new(), &argv(argv_values)) {
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

    #[test]
    fn doctor_uses_the_same_composed_registry_as_root_help() {
        crate::test_support::with_isolated_home(|home| {
            write_cli_extension(home.path(), "sample-runtime", "sample-cli");
            let runtime = CliRuntime::with_capabilities(&COMPOSED_DOCTOR_CAPABILITIES);
            let registry = runtime.composed_command_registry();

            let help = registry
                .command
                .clone()
                .try_get_matches_from(["homeboy", "--help"])
                .expect_err("root help should terminate Clap parsing")
                .to_string();
            let report = crate::cli_surface::command_surface_doctor_report_from_composed(
                registry.command,
                registry.provenance,
            );

            assert!(report.agrees, "{:?}", report.drift_notes);
            assert!(help.contains("triage"), "root help omitted triage: {help}");
            assert!(
                help.contains("sample-cli"),
                "root help omitted extension command: {help}"
            );
            assert!(!help.contains("hidden-doctor-fixture"));
            assert!(!report
                .source_registry_commands
                .contains(&"hidden-doctor-fixture".to_string()));
            assert!(report.help_commands.contains(&"triage".to_string()));
            assert!(report
                .source_registry_commands
                .contains(&"triage".to_string()));
            for (command, registry) in [
                ("status", CommandSurfaceRegistry::Core),
                ("triage", CommandSurfaceRegistry::Descriptor),
                ("sample-cli", CommandSurfaceRegistry::Extension),
            ] {
                assert!(
                    report
                        .command_provenance
                        .iter()
                        .any(|entry| { entry.command == command && entry.registry == registry }),
                    "missing {registry:?} provenance for {command}"
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
            assert!(help.contains("homeboy extension relink stale-runtime <path>"));
            assert!(help.contains("homeboy extension uninstall stale-runtime"));
            assert!(!help.contains(extensions_dir.to_string_lossy().as_ref()));
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
        assert!(output.contains("broken extension link `sample-runtime`"));
        assert!(output.contains("homeboy extension relink sample-runtime <path>"));
        assert!(output.contains("homeboy extension uninstall sample-runtime"));
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

    #[test]
    fn nested_unrecognized_subcommand_does_not_trigger_entity_matching() {
        crate::test_support::with_isolated_home(|home| {
            entity_suggest::reset_entity_suggestion_cache_for_test();
            // A component whose id exactly matches the unrecognized token.
            // If a nested unrecognized subcommand (e.g. `agent-task loop
            // list`) ran the expensive entity-suggestion scan the way a
            // bare top-level typo does, this registration would produce a
            // "did you mean component 'list'" hint and force full
            // component/project inventory resolution (including per-
            // component git remote detection) just to reject a malformed
            // command line (#13630).
            homeboy::core::component::write_standalone_registration(
                &homeboy::core::component::Component {
                    id: "list".to_string(),
                    local_path: home.path().display().to_string(),
                    ..Default::default()
                },
            )
            .expect("register component");

            let err = Cli::command_with_scoped_lab_args()
                .try_get_matches_from(["homeboy", "agent-task", "loop", "list"])
                .expect_err("`list` is not a valid `agent-task loop` subcommand");

            let output = try_augment_clap_error(
                &err,
                &argv(&["homeboy", "agent-task", "loop", "list"]),
                &ExtensionCliHealth::default(),
            );

            // No augmentation for a nested unrecognized subcommand: the
            // caller falls straight through to clap's own immediate usage
            // error (`err.exit()`) instead of first waiting on a
            // component/project inventory scan.
            assert!(
                output.is_none(),
                "nested unrecognized subcommand must not trigger entity matching, got: {output:?}"
            );
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
        let err = crate::commands::route::route_after_parse_with_provenance(
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
            None,
        )
        .expect_err("runs show still rejects global runner");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("without --runner"));
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
            assert!(
                required_lab_placement(&cli, hot_command),
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
            let options = crate::agents::agent_tasks::service::CookRequest {
                identity: crate::agents::agent_task_service::CookIdentity {
                    cook_id: cook_id.to_string(),
                    initial_run_id: run_id.to_string(),
                    initial_plan: plan.clone(),
                },
                workspace: crate::agents::agent_task_service::CookWorkspace {
                    to_worktree: target.display().to_string(),
                    source_worktree_path: Some(target.clone()),
                    task_base_sha: None,
                    source_refs: Vec::new(),
                },
                provider_transport: crate::agents::agent_task_service::CookProviderTransport {
                    provider_command: None,
                    provider_invocation: None,
                    attempt_dispatcher: None,
                },
                gates: Default::default(),
                retry_policy: crate::agents::agent_task_service::CookRetryPolicy {
                    max_attempts: 1,
                },
                finalization: crate::agents::agent_task_service::CookFinalization {
                    no_finalize: true,
                    draft_pr: false,
                    base: "main".to_string(),
                    head: None,
                    title: "runtime continuation fixture".to_string(),
                    commit_message: "runtime continuation fixture".to_string(),
                    protected_branches: Vec::new(),
                },
                ai_disclosure: crate::agents::agent_task_service::CookAiDisclosure {
                    ai_tool: "test".to_string(),
                    ai_model: None,
                    ai_used_for: "test".to_string(),
                },
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
            let options = crate::agents::agent_tasks::service::CookRequest {
                identity: crate::agents::agent_task_service::CookIdentity {
                    cook_id: cook_id.to_string(),
                    initial_run_id: run_id.to_string(),
                    initial_plan: plan.clone(),
                },
                workspace: crate::agents::agent_task_service::CookWorkspace {
                    to_worktree: target.display().to_string(),
                    source_worktree_path: Some(target),
                    task_base_sha: None,
                    source_refs: Vec::new(),
                },
                provider_transport: crate::agents::agent_task_service::CookProviderTransport {
                    provider_command: None,
                    provider_invocation: None,
                    attempt_dispatcher: None,
                },
                gates: Default::default(),
                retry_policy: crate::agents::agent_task_service::CookRetryPolicy {
                    max_attempts: 1,
                },
                finalization: crate::agents::agent_task_service::CookFinalization {
                    no_finalize: true,
                    draft_pr: false,
                    base: "main".to_string(),
                    head: None,
                    title: "legacy terminal continuation fixture".to_string(),
                    commit_message: "legacy terminal continuation fixture".to_string(),
                    protected_branches: Vec::new(),
                },
                ai_disclosure: crate::agents::agent_task_service::CookAiDisclosure {
                    ai_tool: "test".to_string(),
                    ai_model: None,
                    ai_used_for: "test".to_string(),
                },
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
