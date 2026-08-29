use clap::Parser;
use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::cli_surface::{Cli, Commands};
use homeboy::core::command_execution_plan::CommandSourceMaterialization;
use homeboy::core::component::{self, TargetSpec};
use homeboy::core::git;
use homeboy::core::lab_routing::{
    self, ExecutionPlacementOutcomeTarget, LabDispatchObserver, LabRouteOutcome, LabRoutingRequest,
    NoopLabDispatchObserver, PersistedRunRetrieval,
};
use homeboy::core::observation::{
    finish_run_best_effort, NewRunRecord, ObservationStore, RunStatus,
};
use homeboy::core::redaction::RedactionPolicy;
use homeboy::core::Error;
use homeboy::runner::runners::{self, RunnerExecOptions};
use homeboy_lab_contract::lab::transport_failure::{
    preacceptance_transport_error, LabJobAcceptanceDisposition, LabTransportOperation,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crate::agents::agent_task_service::DerivedCookBaselineCapability;
use crate::command_contract::{LabCommandPortability, LabCommandRoute};
use crate::commands::utils::resource_policy;
use crate::core::io::output_file::write_output_file;

/// Routes typed commands while retaining their parser-source contract through
/// controller-side plan materialization and Lab handoff.
pub(crate) fn route_after_parse_with_provenance(
    cli: &Cli,
    normalized_args: &[String],
    output_file: Option<&str>,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> homeboy::core::Result<Option<i32>> {
    // Contradictory argument combinations are rejected before any transport
    // context is consulted. The bail-outs below skip routing, not validation:
    // an invalid combination stays invalid wherever the process runs, and
    // letting one through means cook executes a request it already knows it
    // cannot honor (#10917).
    reject_contradictory_cook_arguments(cli)?;

    if let Some(exit_code) = consume_unmaterialized_replay_claim()? {
        return Ok(Some(exit_code));
    }

    // Preview is a controller-local read-only compilation. It must bypass every
    // placement route before runner selection or split Cook materialization can
    // create durable state or dispatch a provider.
    if resource_policy::is_cook_preview(&cli.command) {
        return Ok(None);
    }

    // A managed runner executes the controller-selected command once. Its argv
    // retains the controller's explicit placement for provenance, but must not
    // recursively route back through a runner-side controller daemon.
    let managed_runner_placement =
        crate::commands::utils::resource_policy::is_managed_runner_placement_context();
    let runner_side = lab_routing::is_lab_offload_subprocess()
        || managed_runner_placement
        || runner_resident_execution(cli);

    // A locally-placed Cook asking to detach is served by re-executing it in
    // its own session, so the durable run id means what Cook's help says it
    // means on every placement (#11476). This is evaluated before the
    // runner-side bail-out because the request needs a verdict — detach here,
    // or an explicit rejection there — in both contexts.
    if runner_side || cli.placement == homeboy::cli_surface::Placement::Local {
        if let Some(exit_code) =
            local_detach::intercept_local_cook_retry(cli, normalized_args, runner_side)?
        {
            return Ok(Some(exit_code));
        }
        if let Some(exit_code) = local_detach::intercept_local_detached_cook(
            cli,
            normalized_args,
            output_file,
            runner_side,
            Some("local"),
            None,
        )? {
            return Ok(Some(exit_code));
        }
    }

    // A controller-owned fanout wave asking to detach gets the same verdict,
    // regardless of the placement selected for its provider attempts. The flag
    // was advertised on `fanout cook-batch` and `fanout run-plan` but every gate
    // that acted on it tested for Cook, so a wave accepted a flag promising the
    // caller could disconnect and then blocked that caller for hours. This
    // either serves the request or refuses it explicitly; it never ignores it.
    if let Some(exit_code) =
        local_detach_fanout::intercept_local_detached_fanout(cli, normalized_args, runner_side)?
    {
        return Ok(Some(exit_code));
    }

    if runner_side {
        return Ok(None);
    }

    // Promotion owns target resolution because gate-feedback artifacts can
    // authorize an exact dirty candidate. Generic Lab routing has no artifact
    // provenance and would reject that target before local promotion starts.
    if cli.placement == homeboy::cli_surface::Placement::Local
        && matches!(
            cli.command,
            Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                command: crate::commands::agent_task::AgentTaskCommand::Promote(_),
            })
        )
    {
        return Ok(None);
    }

    if let (Some(runner_id), Commands::Runs(args)) = (cli.runner.as_deref(), &cli.command) {
        if !is_runs_list_runner_option(normalized_args) && !args.has_command_local_runner_option() {
            return Err(crate::commands::runs::global_runner_error(args, runner_id));
        }

        return Ok(None);
    }

    if is_command_local_runner_option(&cli.command) {
        return Ok(None);
    }

    // Provider discovery without an explicit runner or Lab placement describes
    // this controller's extensions, runtime defaults, and credential readiness.
    // Keep that scope before generic routing can consume a previously captured
    // default-runner pressure decision.
    if unscoped_provider_discovery_is_controller_local(cli) {
        return Ok(None);
    }

    // Lifecycle records are durable at their controller owner. Resolve that
    // ownership before default-runner selection: forwarding a controller-local
    // read to an otherwise healthy Lab runner changes a successful lookup into
    // "record not found". An explicitly runner-resident record is absent here
    // and continues through normal runner routing.
    if controller_owns_agent_task_lifecycle_command(cli)? {
        return Ok(None);
    }

    // This is a command safety boundary, not an offload fallback decision.
    // Reject before resolving any source workspace so explicit local execution
    // cannot be obscured by unrelated route-materialization failures.
    if cli.placement == homeboy::cli_surface::Placement::Local
        && destructive_fuzz_requires_lab(&cli.command)
    {
        return Err(destructive_fuzz_local_execution_error());
    }

    if let (Some(runner_id), Commands::Rig(args)) = (cli.runner.as_deref(), &cli.command) {
        if let Some(rig_id) = args.up_dry_run_rig_id() {
            let roots = homeboy::core::paths::PathRoots::from_environment()?;
            let (output, exit_code) =
                crate::commands::rig::up_runner_exec_plan(roots.config(), rig_id, runner_id)?;
            let stdout = serde_json::to_string_pretty(&output).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some("serialize rig up runner exec plan".to_string()),
                )
            })?;
            if let Some(path) = output_file {
                write_output_file(path, &stdout)?;
            }
            println!("{stdout}");
            return Ok(Some(exit_code));
        }
        if args.is_runner_source_management_command() {
            if let Some((source, id, all)) = args.runner_install_request() {
                // Lab routing resolves rig components and path inputs on the
                // controller before dispatch, so runner-targeted installs must
                // keep the controller registry pointed at the same source.
                let roots = homeboy::core::paths::PathRoots::from_environment()?;
                homeboy::rig::install(roots.config(), source, id, all)?;
            }
            let (stdout, stderr, exit_code) =
                run_rig_source_management_on_runner(runner_id, normalized_args, output_file)?;
            if !stderr.is_empty() {
                eprint!("{stderr}");
            }
            print!("{stdout}");
            return Ok(Some(exit_code));
        }
    }

    let lab_command = lab_offload_command(&cli.command)?;
    let normalized_args = inline_portable_settings_profiles(cli, normalized_args)?;

    // Admission owns runner inventory. Routing consumes its immutable result;
    // reopening readiness here can otherwise turn an admitted Lab attempt into
    // a local fallback.
    let preflight = authoritative_preflight()?;
    let lab_readiness = preflight.lab_readiness.as_ref();
    let inferred_runner_id = lab_command
        .is_some()
        .then(|| {
            preflight
                .placement
                .runner
                .as_ref()
                .map(|runner| runner.runner_id.clone())
        })
        .flatten();
    if detached_cook_can_queue(cli) && !is_unmaterialized_replay_worker() {
        // Persist before any bounded refresh. The scoped replay selector owns
        // ready and reverse-capacity admission after this durable boundary.
        return admit_unmaterialized_cook(
            cli,
            &normalized_args,
            &preflight,
            output_file,
            provenance,
        )
        .map(Some);
    }
    if preflight.deferred_workload
        == homeboy::core::parsed_command_preflight::DeferredWorkloadDecision::RunnerIncompatible
        && std::env::var_os("HOMEBOY_DEFERRED_WORKLOAD_REPLAY").is_some()
    {
        return Ok(Some(75));
    }

    if preflight.deferred_workload
        == homeboy::core::parsed_command_preflight::DeferredWorkloadDecision::Defer
        && std::env::var_os("HOMEBOY_DEFERRED_WORKLOAD_REPLAY").is_none()
    {
        let deferred = homeboy::deferred_workload::defer(deferred_workload_input(
            cli,
            &portable_deferred_args(&normalized_args),
            &preflight,
            review_test_deferred_requirements(cli)
                .expect("preflight only defers portable review tests"),
        )?)?;
        crate::commands::deferred_workload::ensure_worker(&homeboy::core::paths::homeboy()?)?;
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "schema": "homeboy/deferred-workload-result/v1",
                "status": "deferred",
                "deferred_workload_id": deferred.id,
                "command": deferred.command_label,
                "diagnostics": {
                    "worker_command": "homeboy deferred-workload worker",
                    "status_command": "homeboy deferred-workload status",
                    "ci_alternative": deferred.ci_alternative,
                    "resolved_portability": deferred.portability,
                    "reason": deferred.reason,
                },
            }))
            .unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(Some(0));
    }

    // A split-placement coordinator honors `--placement lab`; it must never fall
    // through to the generic local-only portability rejection, which contradicts
    // the documented guidance for Cook waves (#9373).
    if let Some(error) = split_placement_lab_runner_unavailable_error(
        &cli.command,
        cli.placement,
        inferred_runner_id.as_deref(),
        lab_readiness,
    ) {
        return Err(error);
    }
    if cli.placement == homeboy::cli_surface::Placement::Lab && inferred_runner_id.is_none() {
        return Err(required_lab_runner_unavailable_error(lab_readiness));
    }

    // Cooks must resolve provider ownership before the launcher can acknowledge
    // or observe them. Auto without a runner is a local provider placement and
    // needs the same daemon-owned supervision as explicit local execution.
    if !is_unmaterialized_replay_worker()
        && needs_provider_resolved_cook_interception(cli, inferred_runner_id.as_deref())
    {
        let provider_placement = if inferred_runner_id.is_some() {
            "lab"
        } else {
            "local"
        };
        if let Some(exit_code) = local_detach::intercept_local_detached_cook(
            cli,
            &normalized_args,
            output_file,
            false,
            Some(provider_placement),
            inferred_runner_id.as_deref(),
        )? {
            return Ok(Some(exit_code));
        }
    }

    if let Some(exit_code) = run_split_placement_cook(
        cli,
        &normalized_args,
        output_file,
        inferred_runner_id.as_deref(),
        &preflight.placement,
        provenance,
    )? {
        return Ok(Some(exit_code));
    }

    if let Some(exit_code) = run_split_placement_fanout(
        cli,
        output_file,
        inferred_runner_id.as_deref(),
        &preflight.placement,
    )? {
        return Ok(Some(exit_code));
    }

    // Split-placement coordinators (cook, fanout) above own their own runner
    // selection: they stay controller-local by contract and hand only their
    // child provider attempts to a runner. Everything below is the *generic*
    // Lab route, and it may only consume a policy-selected default runner when
    // the command's own contract authorizes an automatic offload.
    //
    // Without this gate a default Lab runner was attached to every command that
    // merely *had* a Lab contract, baked into the canonical placement decision
    // as `selected: lab`, and then dispatched — which is how read-only
    // lifecycle commands ended up querying a machine that does not own their
    // durable record (#11597, #11599) and how an explicitly local run acquired
    // a Lab handoff it could not later recover (#11600).
    let route_runner_id = preflight.generic_route_runner_id.as_deref();
    if lab_command.is_none()
        || (route_runner_id.is_none() && cli.placement != homeboy::cli_surface::Placement::Lab)
    {
        return Ok(None);
    }
    let run_handoff = if lab_command.is_some() && route_runner_id.is_some() {
        materialize_agent_task_run_handoff(cli, &normalized_args)?
    } else {
        None
    };
    let retry_handoff = if lab_command.is_some() && route_runner_id.is_some() {
        materialize_agent_task_retry_handoff(cli, &normalized_args)?
    } else {
        None
    };
    stage_retry_lab_handoff_before_preacceptance(retry_handoff.as_ref(), route_runner_id)?;
    let normalized_args = run_handoff
        .as_ref()
        .map(|handoff| handoff.args.as_slice())
        .or_else(|| {
            retry_handoff
                .as_ref()
                .map(|handoff| handoff.args.as_slice())
        })
        .unwrap_or(&normalized_args);
    let normalized_args = normalized_args.to_vec();
    let deferred_claim = inferred_runner_id
        .as_deref()
        .filter(|_| {
            preflight.deferred_workload
                == homeboy::core::parsed_command_preflight::DeferredWorkloadDecision::Dispatch
        })
        .map(|runner_id| {
            homeboy::deferred_workload::claim(
                &deferred_workload_input(
                    cli,
                    &portable_deferred_args(&normalized_args),
                    &preflight,
                    review_test_deferred_requirements(cli)
                        .expect("preflight only dispatches portable review tests"),
                )?,
                runner_id,
                &format!("{}:{}", std::process::id(), uuid::Uuid::new_v4()),
            )
        })
        .transpose()?
        .flatten();
    // A controller-owned `run` becomes a portable `run-plan`. Route according
    // to that materialized command so Lab stages its workspace and rewrites the
    // structured plan instead of taking the original runner-resident path.
    let lab_command = if run_handoff.is_some()
        || retry_handoff
            .as_ref()
            .is_some_and(|handoff| handoff.replays_generic_command)
    {
        lab_offload_command_for_materialized_args(&normalized_args)?
    } else {
        lab_command
    };
    let cook_plan = if lab_command.is_some() && route_runner_id.is_some() {
        materialize_agent_task_cook_plan(cli, provenance)?
    } else {
        None
    };
    let normalized_args =
        inject_agent_task_cook_attempt_plan(&normalized_args, cook_plan.as_ref())?;
    let needs_generic_detached_handoff = lab_command.is_some()
        && route_runner_id.is_some()
        && cli.detach_after_handoff
        && run_handoff.is_none()
        && retry_handoff.is_none()
        && cook_plan.is_none();
    let observer = lab_dispatch_observer(cli, &normalized_args, route_runner_id);

    let capture_mutation_patch = cli.command.lab_offload_captures_mutation_patch();
    let mutation_flag = cli.command.lab_offload_mutation_flag();

    // For component-targeted write/fix commands (`homeboy review lint --fix <component>`,
    // `homeboy refactor --from lint --write <component>`), the component is
    // resolved on the controller to its source checkout and the args are
    // rewritten to `--path <source>`. Without this, the offload syncs and
    // diff-captures the controller's working directory while the remote re-resolves
    // the positional component to the runner's registered checkout and writes
    // fixes there — so the source-tree mutation lands outside the captured
    // workspace and the runner returns no patch to apply (#4315).
    let scoped_args = inject_lab_changed_files(&cli.command, &normalized_args)?;
    let normalized_args = scoped_args.as_deref().unwrap_or(&normalized_args);

    let rewritten_args =
        lab_route_source_path_args(&cli.command, normalized_args, capture_mutation_patch);
    let routed_args = rewritten_args.as_deref().unwrap_or(normalized_args);
    let generic_detached_source_path = needs_generic_detached_handoff
        .then(|| source_path_for_generic_detached_lab_handoff(normalized_args))
        .transpose()?;
    // Resolve this once on the controller. The decision records the same source
    // worktree that will be snapshotted, never an ambient identity inferred by a
    // later provider process.
    let routing_source_path = run_handoff
        .as_ref()
        .map(|handoff| handoff.primary_workspace.clone())
        .or_else(|| {
            retry_handoff
                .as_ref()
                .map(|handoff| handoff.primary_workspace.clone())
        })
        .or_else(|| generic_detached_source_path.clone())
        .map(Ok)
        .unwrap_or_else(|| authoritative_lab_source_path(routed_args))?;
    let job_overrides = lab_job_overrides(cli)?;

    // Cook materializes its provider task before routing. Bind that stable task
    // identity into the initial decision so the first Lab attempt is not an
    // artificial replacement of a generic command-level decision.
    let placement_task = cook_plan
        .as_ref()
        .and_then(|plan| plan.tasks.first())
        .map(|task| task.task_id.as_str())
        .unwrap_or("command");
    let placement_decision = preflight
        .placement
        .finalize(materialized_placement_identity(
            placement_task,
            Some(&routing_source_path),
        ));
    let generic_detached_handoff = needs_generic_detached_handoff
        .then(|| {
            materialize_generic_detached_lab_handoff(
                routed_args,
                &routing_source_path,
                lab_command
                    .as_ref()
                    .expect("generic detached handoff has a Lab command"),
                placement_decision.clone(),
            )
        })
        .transpose()?;
    // Only durable agent-task handoffs own placement outcomes. Dispatch
    // observations (trace and detached fanout) persist through their observers.
    let placement_outcome_target = placement_outcome_target(
        retry_handoff
            .as_ref()
            .map(|handoff| handoff.run_id.as_str()),
        generic_detached_handoff
            .as_ref()
            .map(|handoff| handoff.run_id.as_str()),
    );
    // Lab routing carries the durable plan opaquely as JSON (core does not
    // depend on the agent-task subsystem); serialize the selected typed plan.
    let durable_agent_task_plan = run_handoff
        .as_ref()
        .map(|handoff| &handoff.plan)
        .or_else(|| {
            retry_handoff
                .as_ref()
                .map(|handoff| &handoff.plan)
                .or(cook_plan.as_ref())
        })
        .or_else(|| {
            generic_detached_handoff
                .as_ref()
                .map(|handoff| &handoff.plan)
        })
        .map(|plan| {
            serde_json::to_value(plan).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize durable agent-task plan".to_string()),
                )
            })
        })
        .transpose()?;
    let durable_run_id = generic_detached_handoff
        .as_ref()
        .map(|handoff| handoff.run_id.as_str())
        .or_else(|| {
            retry_handoff
                .as_ref()
                .filter(|handoff| handoff.replays_generic_command)
                .map(|handoff| handoff.run_id.as_str())
        });

    let outcome = lab_routing::dispatch_lab_offload(
        LabRoutingRequest {
            placement_decision,
            command: lab_command,
            normalized_args: routed_args,
            explicit_runner: cli.runner.as_deref(),
            placement: cli.placement,
            allow_local_fallback: cli.placement.allows_local_fallback(),
            allow_dirty_lab_workspace: cli.allow_dirty_lab_workspace,
            skip_deps_hydration: cli.skip_deps_hydration,
            preserve_workspace_on_failure: cli.preserve_workspace_on_failure,
            capture_patch: capture_mutation_patch,
            mutation_flag,
            timeout: lab_route_dispatch_timeout(&cli.command),
            placement_outcome_target,
            detach_after_handoff: cli.detach_after_handoff,
            output_file_requested: output_file.is_some(),
            read_only_polling: cli
                .command
                .lab_route_contract()?
                .is_some_and(|contract| contract.command.routing_policy.read_only_polling),
            local_output_file: output_file,
            durable_agent_task_plan: durable_agent_task_plan.as_ref(),
            durable_run_id,
            // A serialized run-plan has no workspace CLI argument. Carry its
            // canonical plan root through the portable source channel so Lab
            // snapshots it before remapping nested plan/config paths.
            source_path: Some(&routing_source_path),
            expected_source_snapshot_identity: retry_handoff
                .as_ref()
                .and_then(|handoff| handoff.expected_source_snapshot_identity.as_deref()),
            verified_cook_baseline: None,
            require_controller_git_bundle: false,
            reuse_compatible_snapshot: retry_handoff.is_some(),
            job_overrides,
        },
        route_runner_id,
        observer,
    )
    .map_err(|error| match retry_handoff.as_ref() {
        Some(handoff) => {
            persist_retry_handoff_preacceptance_failure(handoff, route_runner_id, error)
        }
        None => error,
    })?;

    match outcome {
        LabRouteOutcome::RunLocal => {
            if let Some(record) = deferred_claim.as_ref() {
                homeboy::deferred_workload::terminalize(&record.id, false)?;
            }
            if destructive_fuzz_requires_lab(&cli.command) {
                return Err(destructive_fuzz_local_execution_error());
            }
            if let Some(warning) = agent_task_local_fanout_warning(&cli.command, lab_readiness) {
                eprintln!("{warning}");
            }
            Ok(None)
        }
        LabRouteOutcome::InFlight(output) | LabRouteOutcome::Offloaded(output) => {
            if let Some(record) = deferred_claim.as_ref() {
                homeboy::deferred_workload::terminalize(&record.id, output.exit_code == 0)?;
            }
            if !output.stderr.is_empty() {
                eprint!("{}", output.stderr);
            }
            if let Some(path) = output_file {
                write_offloaded_stdout(
                    path,
                    output
                        .output_file_content
                        .as_deref()
                        .unwrap_or(&output.stdout),
                )?;
            }
            print!("{}", output.stdout);
            Ok(Some(output.exit_code))
        }
    }
}

/// Global route inputs available to descriptor-composed commands. Keeping this
/// separate from `Cli` lets a capability use the shared Lab dispatcher without
/// pretending its parsed arguments are a static `Commands` variant.
pub(crate) struct ComposedLabRouteOptions<'a> {
    pub placement: homeboy::cli_surface::Placement,
    pub runner: Option<&'a str>,
    pub allow_dirty_lab_workspace: bool,
    pub skip_deps_hydration: bool,
    pub preserve_workspace_on_failure: bool,
    pub detach_after_handoff: bool,
    pub runner_env: &'a [String],
    pub runner_secret_env: &'a [String],
    pub lab_env_json: Option<&'a str>,
    pub runner_workspace_root: Option<&'a str>,
}

/// Route a descriptor-composed command through the same core Lab contract and
/// placement dispatcher used by built-ins. Static-command controller adapters
/// (Cook/Fanout materialization and trace observers) intentionally remain on
/// their typed path; composed commands use the generic no-op observer.
pub(crate) fn route_composed_lab_command(
    route: &LabCommandRoute,
    options: ComposedLabRouteOptions<'_>,
    normalized_args: &[String],
    output_file: Option<&str>,
    preflight: &homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult,
) -> homeboy::core::Result<Option<i32>> {
    let Some(route_contract) = route.lab_route_contract() else {
        if options.placement == homeboy::cli_surface::Placement::Lab || options.runner.is_some() {
            return Err(Error::validation_invalid_argument(
                "placement",
                "this composed command has no Lab route contract",
                None,
                None,
            ));
        }
        return Ok(None);
    };
    if matches!(
        route_contract.command.portability,
        LabCommandPortability::LocalOnly(_)
    ) {
        if options.placement == homeboy::cli_surface::Placement::Lab || options.runner.is_some() {
            let LabCommandPortability::LocalOnly(reason) = route_contract.command.portability
            else {
                unreachable!("local-only route was matched above");
            };
            return Err(Error::validation_invalid_argument(
                "placement",
                format!("Lab placement is unavailable for this composed command: {reason}"),
                None,
                None,
            ));
        }
        return Ok(None);
    }
    if options.placement == homeboy::cli_surface::Placement::Local {
        return Ok(None);
    }

    let read_only_polling = route_contract.command.routing_policy.read_only_polling;
    let command = lab_routing::lab_offload_command_from_route_contract(route_contract);
    let task = command.command.hot_label;
    let runner_id = preflight.generic_route_runner_id.as_deref();
    if preflight.placement.required
        == homeboy_lab_runner_contract::ExecutionPlacementRequirement::Lab
        && runner_id.is_none()
    {
        return Err(Error::validation_invalid_argument(
            "placement",
            "required Lab placement has no selected ready runner",
            Some("lab".to_string()),
            None,
        ));
    }
    if runner_id.is_none() && options.placement != homeboy::cli_surface::Placement::Lab {
        return Ok(None);
    }

    let source_path = std::env::current_dir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve composed Lab source path".to_string()),
        )
    })?;
    let decision =
        preflight
            .placement
            .finalize(homeboy_lab_runner_contract::ExecutionPlacementIdentity {
                repository: source_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "controller-cwd".to_string()),
                workspace: source_path.display().to_string(),
                task: task.to_string(),
                candidate: homeboy::core::git::head_sha(&source_path),
                base: homeboy::core::git::rev_parse(&source_path, "origin/HEAD"),
            });
    let outcome = lab_routing::dispatch_lab_offload(
        LabRoutingRequest {
            placement_decision: decision,
            command: Some(command),
            normalized_args,
            explicit_runner: options.runner,
            placement: options.placement,
            allow_local_fallback: options.placement.allows_local_fallback(),
            allow_dirty_lab_workspace: options.allow_dirty_lab_workspace,
            skip_deps_hydration: options.skip_deps_hydration,
            preserve_workspace_on_failure: options.preserve_workspace_on_failure,
            capture_patch: route.lab_offload_captures_mutation_patch(),
            mutation_flag: route.lab_offload_mutation_flag(),
            timeout: None,
            placement_outcome_target: None,
            detach_after_handoff: options.detach_after_handoff,
            output_file_requested: output_file.is_some(),
            read_only_polling,
            local_output_file: output_file,
            durable_agent_task_plan: None,
            durable_run_id: None,
            source_path: Some(&source_path),
            expected_source_snapshot_identity: None,
            verified_cook_baseline: None,
            require_controller_git_bundle: false,
            reuse_compatible_snapshot: false,
            job_overrides: composed_lab_job_overrides(&options)?,
        },
        runner_id,
        Box::new(NoopLabDispatchObserver),
    )?;
    match outcome {
        LabRouteOutcome::RunLocal => Ok(None),
        LabRouteOutcome::InFlight(output) | LabRouteOutcome::Offloaded(output) => {
            if !output.stderr.is_empty() {
                eprint!("{}", output.stderr);
            }
            if let Some(path) = output_file {
                write_offloaded_stdout(path, &output.stdout)?;
            }
            print!("{}", output.stdout);
            Ok(Some(output.exit_code))
        }
    }
}

fn composed_lab_job_overrides(
    options: &ComposedLabRouteOptions<'_>,
) -> homeboy::core::Result<runners::LabJobOverrides> {
    let mut overrides = runners::LabJobOverrides::default();
    let policy = RedactionPolicy::default();
    for raw in options.runner_env {
        let (name, value) = parse_lab_env_pair("runner-env", raw)?;
        validate_explicit_runner_env(&policy, &name, &value)?;
        insert_lab_env_override(&mut overrides, &policy, name, value)?;
    }
    for name in options.runner_secret_env {
        overrides
            .secret_env_names
            .push(validate_lab_env_name("runner-secret-env", name)?);
    }
    if let Some(raw_json) = options.lab_env_json {
        let value: serde_json::Value = serde_json::from_str(raw_json).map_err(|error| {
            Error::validation_invalid_argument("lab-env-json", error.to_string(), None, None)
        })?;
        let object = value.as_object().ok_or_else(|| {
            Error::validation_invalid_argument(
                "lab-env-json",
                "--lab-env-json must be a JSON object of string or null values",
                Some(raw_json.to_string()),
                None,
            )
        })?;
        for (name, value) in object {
            let value = match value {
                serde_json::Value::String(value) => value.clone(),
                serde_json::Value::Null => String::new(),
                _ => {
                    return Err(Error::validation_invalid_argument(
                        "lab-env-json",
                        "values must be strings or null",
                        None,
                        None,
                    ))
                }
            };
            insert_lab_env_override(
                &mut overrides,
                &policy,
                validate_lab_env_name("lab-env-json", name)?,
                value,
            )?;
        }
    }
    overrides.secret_env_names.sort();
    overrides.secret_env_names.dedup();
    overrides.workspace_root = options
        .runner_workspace_root
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Ok(overrides)
}

fn materialized_placement_identity(
    task: &str,
    source_path: Option<&Path>,
) -> homeboy_lab_runner_contract::ExecutionPlacementIdentity {
    homeboy_lab_runner_contract::ExecutionPlacementIdentity {
        repository: source_path
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "runner-resident-or-unmaterialized".to_string()),
        workspace: source_path
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "runner-resident-or-unmaterialized".to_string()),
        task: task.to_string(),
        candidate: source_path.and_then(homeboy::core::git::head_sha),
        base: source_path.and_then(|path| homeboy::core::git::rev_parse(path, "origin/HEAD")),
    }
}

fn needs_provider_resolved_cook_interception(cli: &Cli, inferred_runner_id: Option<&str>) -> bool {
    // Explicit local placement was already intercepted before provider
    // resolution. Running it through this pass again consumes the supervised
    // child's one-use launch token twice and recursively detaches the child.
    cli.placement != homeboy::cli_surface::Placement::Local
        && (inferred_runner_id.is_some()
            || cli.placement.allows_local_fallback()
            || cli.placement == homeboy::cli_surface::Placement::Auto)
}

fn authoritative_preflight(
) -> homeboy::core::Result<homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult> {
    homeboy::core::parsed_command_preflight::captured_result().ok_or_else(|| {
        Error::internal_unexpected("route requires a completed parsed-command preflight result")
    })
}

fn finalize_placement(
    directive: &homeboy::core::parsed_command_preflight::PlacementDirective,
    task: &str,
    source_path: Option<&Path>,
) -> homeboy_lab_runner_contract::ExecutionPlacementDecision {
    directive.finalize(materialized_placement_identity(task, source_path))
}

#[cfg(test)]
fn fixture_preflight_decision(
    cli: &Cli,
    runner_id: Option<&str>,
    task: &str,
    source_path: Option<&Path>,
) -> homeboy::core::Result<homeboy_lab_runner_contract::ExecutionPlacementDecision> {
    let normalized = vec!["homeboy".to_string()];
    let result = homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult::new(
        normalized.clone(),
        resource_policy::parsed_command_preflight_input(cli, &normalized),
        None,
        None,
        homeboy::core::parsed_command_preflight::DeferredWorkloadDecision::NotApplicable,
        homeboy::core::parsed_command_preflight::FallbackDirective::None,
        crate::cli_runtime::placement_directive(cli, runner_id, false),
        runner_id.map(str::to_string),
    );
    Ok(finalize_placement(&result.placement, task, source_path))
}

#[cfg(test)]
fn placement_decision(
    cli: &Cli,
    runner_id: Option<&str>,
    task: &str,
    source_path: Option<&Path>,
) -> homeboy::core::Result<homeboy_lab_runner_contract::ExecutionPlacementDecision> {
    fixture_preflight_decision(cli, runner_id, task, source_path)
}

fn authoritative_lab_source_path(args: &[String]) -> homeboy::core::Result<PathBuf> {
    let mut args = args.iter().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--" {
            break;
        }
        if matches!(
            argument.as_str(),
            "--path" | "--cwd" | "--workspace" | "--to-worktree"
        ) {
            let value = args.next().ok_or_else(|| {
                Error::validation_invalid_argument(
                    argument.trim_start_matches("--"),
                    format!("{argument} requires a source worktree"),
                    None,
                    None,
                )
            })?;
            return Ok(PathBuf::from(value));
        }
        if let Some(value) = argument
            .strip_prefix("--path=")
            .or_else(|| argument.strip_prefix("--cwd="))
        {
            return Ok(PathBuf::from(value));
        }
    }
    std::env::current_dir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve controller source worktree".to_string()),
        )
    })
}

/// A runner-resident plan, or a nested command targeting the runner that
/// selected this process, executes in place. The execution-provenance marker
/// survives the parent handoff, while controller transport markers are
/// intentionally consumed before provider code runs.
fn runner_resident_execution(cli: &Cli) -> bool {
    homeboy::core::resource_policy_context::lab_execution_runner_id().is_some_and(|runner_id| {
        matches!(
            &cli.command,
            Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                command: crate::commands::agent_task::AgentTaskCommand::RunPlan(_),
            })
        ) || cli.runner.as_deref() == Some(runner_id.as_str())
    })
}

/// The coordinators that implement split placement: the coordinator itself
/// stays controller-owned while each provider attempt is dispatched to the
/// selected Lab runner (`run_split_placement_cook` /
/// `run_split_placement_fanout`).
///
/// For these commands `--placement lab` is supported and documented guidance,
/// even though their portability contract is `LocalOnly` — the contract
/// describes the *coordinator*, not the attempt.
fn split_placement_coordinator_label(command: &Commands) -> Option<&'static str> {
    use crate::commands::agent_task::{
        AgentTaskArgs, AgentTaskCommand, AgentTaskFanoutArgs, AgentTaskFanoutCommand,
    };

    match command {
        Commands::AgentTask(AgentTaskArgs {
            command: AgentTaskCommand::Cook(cook),
        }) if !cook.dispatch.core.queue_only => Some("agent-task cook"),
        Commands::AgentTask(AgentTaskArgs {
            command:
                AgentTaskCommand::Fanout(AgentTaskFanoutArgs {
                    command: AgentTaskFanoutCommand::RunPlan(_),
                }),
        }) => Some("agent-task fanout run-plan"),
        Commands::AgentTask(AgentTaskArgs {
            command:
                AgentTaskCommand::Fanout(AgentTaskFanoutArgs {
                    command: AgentTaskFanoutCommand::SubmitBatch(_),
                }),
        }) => Some("agent-task fanout submit-batch"),
        Commands::AgentTask(AgentTaskArgs {
            command:
                AgentTaskCommand::Fanout(AgentTaskFanoutArgs {
                    command: AgentTaskFanoutCommand::CookBatch(args),
                }),
        }) if args.run_plan => Some("agent-task fanout cook-batch --run-plan"),
        _ => None,
    }
}

fn detached_cook_can_queue(cli: &Cli) -> bool {
    cli.detach_after_handoff
        && !matches!(cli.placement, homeboy::cli_surface::Placement::Local)
        && matches!(
            cli.command,
            Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                command: crate::commands::agent_task::AgentTaskCommand::Cook(_),
            })
        )
}

fn admission_digest(value: impl AsRef<[u8]>) -> String {
    format!(
        "sha256:{}",
        homeboy_engine_primitives::content_hash::sha256_hex(value.as_ref())
    )
}

/// Admit a detached Cook without compiling an executable plan or touching its
/// destination. Only references, identities, counts, and one-way request
/// bindings enter durable state; execution inputs remain at their owning source.
fn admit_unmaterialized_cook(
    cli: &Cli,
    normalized_args: &[String],
    preflight: &homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult,
    output_file: Option<&str>,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> homeboy::core::Result<i32> {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
    }) = &cli.command
    else {
        return Err(Error::internal_unexpected(
            "unmaterialized admission called for a non-Cook command",
        ));
    };
    let resolved = crate::commands::agent_task::run::resolve_cook_destination(*cook.clone())?;
    crate::commands::agent_task::run::validate_cook_request_with_provenance(&resolved, provenance)?;
    let mut replay_args = normalized_args.to_vec();
    crate::commands::agent_task::run::rewrite_cook_identity_replay_argv(
        &mut replay_args,
        &resolved,
    );
    let cook_id = resolved
        .dispatch
        .run_id
        .clone()
        .unwrap_or_else(|| format!("agent-task-{}", uuid::Uuid::new_v4()));
    let readiness_state = preflight
        .lab_readiness
        .as_ref()
        .map(|value| value.state.as_str())
        .unwrap_or("absent");
    let state = unmaterialized_admission_state(preflight.lab_readiness.as_ref());
    let reason = preflight
        .lab_readiness
        .as_ref()
        .and_then(|value| value.reasons.first())
        .map(String::as_str)
        .unwrap_or("no eligible Lab runner is currently available");
    let digest_json =
        |value: serde_json::Value| admission_digest(serde_json::to_vec(&value).unwrap_or_default());
    let current_notification = homeboy::core::notification_route::current();
    let request_ref = digest_json(serde_json::json!({
        "argv": &replay_args,
        "notification": current_notification,
    }));
    let existing =
        agent_task_lifecycle::precheck_unmaterialized_cook_admission(&cook_id, &request_ref)?;
    let notification = current_notification.as_ref().map(|route| {
        serde_json::json!({
            "transport": route.transport,
            "route_ref": admission_digest(&route.route),
        })
    });
    let mut staged_intent = if existing.is_none() {
        Some(stage_unmaterialized_cook_replay_intent(
            &replay_args,
            &cook_id,
            current_notification.as_ref(),
        )?)
    } else {
        None
    };
    let binding = staged_intent.as_ref().map(|staged| serde_json::json!({
        "schema": "homeboy/unmaterialized-cook-binding/v1",
        "request_ref": request_ref,
        "candidate_policy": resolved.candidate_completion,
        "placement": {
            "requested": preflight.placement.requested,
            "local_fallback": preflight.placement.fallback.local_allowed,
            "runner_ref": preflight.placement.runner.as_ref().map(|runner| &runner.runner_id),
            "resource_policy": preflight.resource_policy,
        },
        "source": {
            "repository": resolved.dispatch.repo,
            "repository_identity": resolved.repository_identity,
            "task_refs": resolved.dispatch.task_url.iter().collect::<Vec<_>>(),
        },
        "base": resolved.base,
        "base_resolution": resolved.base_resolution,
        "head": resolved.head,
        "worktree_ref": resolved.to_worktree,
        "task": {
            "goal_ref": resolved.goal.as_ref().map(|value| admission_digest(value)),
            "prompt_ref": resolved.dispatch.prompt.as_ref().map(|value| admission_digest(value)),
            "task_count": resolved.dispatch.tasks.len().max(1),
        },
        "gates": {
            "public_count": resolved.gates.verify.len(),
            "private_count": resolved.gates.private_verify.len(),
            "binding": digest_json(serde_json::json!({
                "public": resolved.gates.verify,
                "private": resolved.gates.private_verify,
            })),
        },
        "provider_runtime_refs": {
            "backend": resolved.dispatch.backend,
            "selector": resolved.dispatch.selector,
            "model": resolved.dispatch.model,
            "required_capabilities": resolved.dispatch.required_capabilities,
            "secret_env_names": resolved.dispatch.secret_env,
            "provider_config_ref": resolved.dispatch.core.provider_config.as_ref().map(|value| admission_digest(value)),
            "runtime_generation": homeboy::core::build_identity::current().display,
        },
        "retry": {
            "max_attempts": resolved.max_attempts,
            "provider_executions": resolved.dispatch.core.attempts,
            "same_provider_retries": resolved.dispatch.core.same_provider_retries,
            "provider_rotations": resolved.dispatch.core.provider_rotations,
        },
        "publication": {
            "finalize": !resolved.no_finalize,
            "draft": resolved.draft_pr,
            "acceptance_required": resolved.require_acceptance,
        },
        "notification": notification,
        "replay_intent": staged.intent.as_ref().expect("staged intent"),
        "input_publication": {
            "state": "staged",
            "staging_root": staged.staging_root.display().to_string(),
            "published_root": staged.published_root.display().to_string(),
        },
    }));
    let record = if let Some(existing) = existing {
        if existing.metadata["unmaterialized_cook_admission"]["state"] == "preparing_inputs" {
            agent_task_lifecycle::recover_unmaterialized_cook_input_publication(&cook_id)?
        } else {
            existing
        }
    } else {
        let record = agent_task_lifecycle::prepare_unmaterialized_cook_admission(
            &cook_id,
            binding.expect("new admission binding"),
            state,
            reason,
        )?;
        staged_intent
            .take()
            .expect("new admission snapshot")
            .retain_for_recovery();
        let _ = record;
        agent_task_lifecycle::recover_unmaterialized_cook_input_publication(&cook_id)?
    };
    let reconciliation =
        crate::agents::agent_task_service::reconcile_unmaterialized_cook_admission(&cook_id)
            .unwrap_or_else(|error| {
                serde_json::json!({
                    "replayed": 0,
                    "error": homeboy::core::redaction::redact_string(&error.message),
                })
            });
    let record = agent_task_lifecycle::exact_record(&cook_id).unwrap_or(record);
    let admission = record.metadata["unmaterialized_cook_admission"].clone();
    let output = serde_json::json!({
        "schema": "homeboy/unmaterialized-cook-admission-result/v1",
        "status": admission["state"],
        "cook_id": cook_id,
        "run_id": cook_id,
        "materialized": false,
        "runner_readiness": readiness_state,
        "commands": admission["commands"],
        "retry": admission["retry"],
        "reconciliation": reconciliation,
    });
    let stdout = serde_json::to_string_pretty(&output).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize unmaterialized Cook admission".to_string()),
        )
    })?;
    if let Some(path) = output_file {
        write_output_file(path, &stdout)?;
    }
    println!("{stdout}");
    Ok(0)
}

const COOK_REPLAY_INTENT_SCHEMA: &str = "homeboy/unmaterialized-cook-replay-intent/v1";
const COOK_REPLAY_CLAIM_COOK_ENV: &str = "HOMEBOY_COOK_REPLAY_CLAIM_COOK_ID";
const COOK_REPLAY_CLAIM_FENCE_ENV: &str = "HOMEBOY_COOK_REPLAY_CLAIM_FENCE";
const COOK_REPLAY_CLAIM_TOKEN_ENV: &str = "HOMEBOY_COOK_REPLAY_CLAIM_TOKEN";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UnmaterializedCookReplayIntent {
    schema: String,
    cook_id: String,
    argv: Vec<String>,
    input_refs: Vec<UnmaterializedCookReplayInputRef>,
    input_manifest: UnmaterializedCookReplayInputRef,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UnmaterializedCookReplayInputRef {
    kind: String,
    path: String,
    sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    argv_token: Option<String>,
}

#[cfg(test)]
fn build_unmaterialized_cook_replay_intent(
    normalized_args: &[String],
    cook_id: &str,
    notification: Option<&homeboy::core::notification_route::NotificationRoute>,
) -> homeboy::core::Result<UnmaterializedCookReplayIntent> {
    stage_unmaterialized_cook_replay_intent(normalized_args, cook_id, notification)?.publish()
}

struct StagedCookReplayIntent {
    intent: Option<UnmaterializedCookReplayIntent>,
    staging_root: PathBuf,
    published_root: PathBuf,
    published: bool,
}

impl StagedCookReplayIntent {
    #[cfg(test)]
    fn publish(mut self) -> homeboy::core::Result<UnmaterializedCookReplayIntent> {
        let published_parent = self.published_root.parent().ok_or_else(|| {
            Error::internal_unexpected("Cook replay publication root has no parent directory")
        })?;
        std::fs::create_dir_all(published_parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(published_parent.display().to_string()),
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(published_parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some(published_parent.display().to_string()),
                    )
                })?;
        }
        if self.published_root.exists() {
            std::fs::remove_dir_all(&self.staging_root).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(self.staging_root.display().to_string()),
                )
            })?;
        } else {
            std::fs::rename(&self.staging_root, &self.published_root).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "publish {} -> {}",
                        self.staging_root.display(),
                        self.published_root.display()
                    )),
                )
            })?;
        }
        self.published = true;
        Ok(self.intent.take().expect("staged replay intent"))
    }

    fn retain_for_recovery(mut self) {
        self.published = true;
    }
}

impl Drop for StagedCookReplayIntent {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_dir_all(&self.staging_root);
        }
    }
}

fn stage_unmaterialized_cook_replay_intent(
    normalized_args: &[String],
    cook_id: &str,
    notification: Option<&homeboy::core::notification_route::NotificationRoute>,
) -> homeboy::core::Result<StagedCookReplayIntent> {
    let published_root = replay_intent_storage_root(cook_id)?;
    let admission_dir = published_root.parent().ok_or_else(|| {
        Error::internal_unexpected("Cook replay input root has no parent directory")
    })?;
    let admissions_root = admission_dir.parent().ok_or_else(|| {
        Error::internal_unexpected("Cook replay admission directory has no parent")
    })?;
    std::fs::create_dir_all(admissions_root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(admissions_root.display().to_string()),
        )
    })?;
    let cook_segment = admission_dir
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("cook");
    let root = admissions_root.join(format!(
        ".{cook_segment}-inputs-stage-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?;
    let mut staged = StagedCookReplayIntent {
        intent: None,
        staging_root: root.clone(),
        published_root: published_root.clone(),
        published: false,
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| Error::internal_io(error.to_string(), Some(root.display().to_string())),
        )?;
    }
    let mut argv = Vec::with_capacity(normalized_args.len() + 2);
    let mut input_refs = Vec::new();
    let mut index = 0usize;
    let mut has_run_id = false;
    let mut has_notification_route = false;
    // Only flags before the bare separator are Homeboy's own. Rewriting a
    // forwarded `--prompt` or `--verify` into a replay reference would corrupt
    // the argument the provider was asked to run (#11577).
    let owned = crate::command_capability::homeboy_owned_args(normalized_args).len();
    while index < owned {
        let arg = &normalized_args[index];
        if arg == "--notification-route" || arg.starts_with("--notification-route=") {
            has_notification_route = true;
        }
        if arg == "--runner" {
            require_replay_value(normalized_args, index, arg)?;
            index += 2;
            continue;
        }
        if arg.starts_with("--runner=") {
            index += 1;
            continue;
        }
        if unsafe_inline_replay_flag(arg) {
            return Err(unsafe_inline_replay_error(arg));
        }
        if let Some((flag, value)) = arg.split_once('=') {
            if unsafe_inline_replay_flag(flag) {
                return Err(unsafe_inline_replay_error(flag));
            }
            if matches!(flag, "--prompt" | "--verify" | "--private-verify") {
                let (replacement_flag, reference) =
                    snapshot_replay_input(&root, cook_id, flag, value, input_refs.len())?;
                let replay_value = if replacement_flag == "--prompt" {
                    reference
                        .argv_token
                        .clone()
                        .expect("prompt replay input has an argv token")
                } else {
                    reference.path.clone()
                };
                argv.push(format!("{replacement_flag}={replay_value}"));
                input_refs.push(reference);
                index += 1;
                continue;
            }
            if replay_file_reference_flag(flag) {
                let (rendered, reference) =
                    canonical_replay_file_reference(&root, flag, value, input_refs.len())?;
                argv.push(format!("{flag}={rendered}"));
                input_refs.push(reference);
                index += 1;
                continue;
            }
            if replay_text_value_flag(flag) {
                let (token, reference) =
                    snapshot_replay_argv_value(&root, flag, value, input_refs.len())?;
                argv.push(format!("{flag}={token}"));
                input_refs.push(reference);
                index += 1;
                continue;
            }
        }
        if replay_text_value_flag(arg) {
            let value = require_replay_value(normalized_args, index, arg)?;
            let (token, reference) =
                snapshot_replay_argv_value(&root, arg, value, input_refs.len())?;
            argv.push(arg.clone());
            argv.push(token);
            input_refs.push(reference);
            index += 2;
            continue;
        }
        if matches!(arg.as_str(), "--prompt" | "--verify" | "--private-verify") {
            let value = require_replay_value(normalized_args, index, arg)?;
            let (replacement_flag, reference) =
                snapshot_replay_input(&root, cook_id, arg, value, input_refs.len())?;
            argv.push(replacement_flag.to_string());
            argv.push(if replacement_flag == "--prompt" {
                reference
                    .argv_token
                    .clone()
                    .expect("prompt replay input has an argv token")
            } else {
                reference.path.clone()
            });
            input_refs.push(reference);
            index += 2;
            continue;
        }
        if replay_file_reference_flag(arg) {
            let value = require_replay_value(normalized_args, index, arg)?;
            let (rendered, reference) =
                canonical_replay_file_reference(&root, arg, value, input_refs.len())?;
            argv.push(arg.clone());
            argv.push(rendered);
            input_refs.push(reference);
            index += 2;
            continue;
        }
        if arg == "--run-id" || arg.starts_with("--run-id=") {
            has_run_id = true;
        }
        if homeboy::core::redaction::redact_string(arg) != *arg {
            return Err(unsafe_inline_replay_error(
                arg.split('=').next().unwrap_or(arg),
            ));
        }
        reject_credential_bearing_url(arg)?;
        argv.push(arg.clone());
        index += 1;
    }
    if !has_run_id {
        argv.push("--run-id".to_string());
        argv.push(cook_id.to_string());
    }
    if !has_notification_route {
        if let Some(notification) = notification {
            let (token, reference) = snapshot_replay_argv_value(
                &root,
                "--notification-route",
                &notification.route,
                input_refs.len(),
            )?;
            argv.extend([
                "--notification-transport".to_string(),
                notification.transport.clone(),
                "--notification-route".to_string(),
                token,
            ]);
            input_refs.push(reference);
        }
    }
    // Carry the forwarded tail through verbatim, separator included, so the
    // provider receives exactly what it was given. Secret and credential
    // rejection still applies: a secret is no less secret past the separator.
    for argument in &normalized_args[owned..] {
        if homeboy::core::redaction::redact_string(argument) != *argument {
            return Err(unsafe_inline_replay_error(
                argument.split('=').next().unwrap_or(argument),
            ));
        }
        reject_credential_bearing_url(argument)?;
        argv.push(argument.clone());
    }
    let staging_prefix = root.display().to_string();
    let published_prefix = published_root.display().to_string();
    for argument in &mut argv {
        *argument = argument.replace(&staging_prefix, &published_prefix);
    }
    for reference in &mut input_refs {
        reference.path = reference.path.replace(&staging_prefix, &published_prefix);
    }
    let manifest = serde_json::to_string(&input_refs).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize Cook replay input manifest".to_string()),
        )
    })?;
    let mut input_manifest =
        persist_replay_input(&root, "manifest", &manifest, input_refs.len(), None)?;
    input_manifest.path = input_manifest
        .path
        .replace(&staging_prefix, &published_prefix);
    staged.intent = Some(UnmaterializedCookReplayIntent {
        schema: COOK_REPLAY_INTENT_SCHEMA.to_string(),
        cook_id: cook_id.to_string(),
        argv,
        input_refs,
        input_manifest,
    });
    Ok(staged)
}

fn replay_intent_storage_root(cook_id: &str) -> homeboy::core::Result<PathBuf> {
    Ok(homeboy::core::paths::homeboy_data()?
        .join("agent-task-cook-admissions")
        .join(homeboy::core::paths::sanitize_path_segment(cook_id))
        .join("inputs"))
}

fn require_replay_value<'a>(
    args: &'a [String],
    index: usize,
    flag: &str,
) -> homeboy::core::Result<&'a str> {
    args.get(index + 1).map(String::as_str).ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_admission.replay_intent",
            format!("{flag} requires a replayable value"),
            None,
            None,
        )
    })
}

fn unsafe_inline_replay_flag(flag: &str) -> bool {
    matches!(
        flag.split('=').next().unwrap_or(flag),
        "--runner-env" | "--lab-env-json" | "--gate-env" | "--resolved-provider-policy" | "--tasks"
    )
}

fn unsafe_inline_replay_error(flag: &str) -> Error {
    Error::validation_invalid_argument(
        "cook_admission.replay_intent",
        format!(
            "detached unmaterialized Cook admission refuses unsafe inline value `{flag}`"
        ),
        None,
        Some(vec![
            "Use --runner-secret-env/--secret-env names, --gate-env-from references, @file provider/client configuration, and file-backed prompt/gate inputs.".to_string(),
        ]),
    )
}

fn replay_file_reference_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--provider-config" | "--client-context" | "--verify-file" | "--private-verify-file"
    )
}

fn replay_text_value_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--task"
            | "--goal"
            | "--title"
            | "--commit-message"
            | "--command-policy-reason"
            | "--notification-route"
            | "--deny-command"
            | "--allow-command"
            | "--ai-tool"
            | "--ai-used-for"
            | "--acceptance-authority"
            | "--acceptance-policy"
            | "--gate-toolchain-spec"
            | "--gate-toolchain"
            | "--gate-package-artifact"
            | "--gate-extension-input"
            | "--provider-evidence"
            | "--provider-command"
            | "--provider-argv"
    )
}

fn canonical_replay_file_reference(
    root: &Path,
    flag: &str,
    value: &str,
    index: usize,
) -> homeboy::core::Result<(String, UnmaterializedCookReplayInputRef)> {
    let requires_at = matches!(flag, "--provider-config" | "--client-context");
    let path_value = if requires_at {
        value
            .strip_prefix('@')
            .ok_or_else(|| unsafe_inline_replay_error(flag))?
    } else {
        value
    };
    if path_value == "-" || path_value.starts_with("prompt:") {
        return Err(unsafe_inline_replay_error(flag));
    }
    let source = std::fs::canonicalize(path_value).map_err(|error| {
        Error::validation_invalid_argument(
            "cook_admission.replay_intent",
            format!("cannot bind replay input `{path_value}`: {error}"),
            Some(path_value.to_string()),
            None,
        )
    })?;
    let content = std::fs::read_to_string(&source).map_err(|error| {
        Error::internal_io(error.to_string(), Some(source.display().to_string()))
    })?;
    if requires_at {
        reject_secret_bearing_replay_content(flag, &content, true)?;
    }
    let reference =
        persist_replay_input(root, flag.trim_start_matches('-'), &content, index, None)?;
    let rendered = if requires_at {
        format!("@{}", reference.path)
    } else {
        reference.path.clone()
    };
    Ok((rendered, reference))
}

fn snapshot_replay_input(
    root: &Path,
    _cook_id: &str,
    flag: &str,
    value: &str,
    index: usize,
) -> homeboy::core::Result<(&'static str, UnmaterializedCookReplayInputRef)> {
    let (replacement, kind, content) = match flag {
        "--prompt" => (
            "--prompt",
            "prompt",
            homeboy::agents::agent_task_prompts::read_prompt_input(value)?,
        ),
        "--verify" => ("--verify-file", "verify", value.to_string()),
        "--private-verify" => ("--private-verify-file", "private-verify", value.to_string()),
        _ => unreachable!("snapshot input flag is closed"),
    };
    if homeboy::core::redaction::redact_string(&content) != content {
        return Err(unsafe_inline_replay_error(flag));
    }
    let argv_token = (flag == "--prompt").then(|| {
        format!(
            "homeboy-replay-ref:{index}:{}",
            admission_digest(content.as_bytes()).trim_start_matches("sha256:")
        )
    });
    let reference = persist_replay_input(root, kind, &content, index, argv_token)?;
    Ok((replacement, reference))
}

fn snapshot_replay_argv_value(
    root: &Path,
    flag: &str,
    value: &str,
    index: usize,
) -> homeboy::core::Result<(String, UnmaterializedCookReplayInputRef)> {
    reject_secret_bearing_replay_content(flag, value, false)?;
    let sha256 = admission_digest(value.as_bytes());
    let token = format!(
        "homeboy-replay-ref:{index}:{}",
        sha256.trim_start_matches("sha256:")
    );
    let reference = persist_replay_input(
        root,
        flag.trim_start_matches('-'),
        value,
        index,
        Some(token.clone()),
    )?;
    Ok((token, reference))
}

fn reject_secret_bearing_replay_content(
    flag: &str,
    content: &str,
    require_json: bool,
) -> homeboy::core::Result<()> {
    let policy = RedactionPolicy::default();
    let parsed = serde_json::from_str::<serde_json::Value>(content);
    if require_json && parsed.is_err() {
        return Err(Error::validation_invalid_argument(
            "cook_admission.replay_intent",
            format!("{flag} replay input must contain valid JSON"),
            None,
            None,
        ));
    }
    if let Ok(value) = parsed {
        if replay_json_contains_inline_secret(&value, &policy) {
            return Err(unsafe_inline_replay_error(flag));
        }
    } else if policy.redact_string(content) != content {
        return Err(unsafe_inline_replay_error(flag));
    }
    Ok(())
}

fn replay_json_contains_inline_secret(value: &serde_json::Value, policy: &RedactionPolicy) -> bool {
    match value {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized = key.to_ascii_lowercase().replace('-', "_");
            let configured_reference = normalized == "secret_env"
                || normalized.ends_with("_env")
                || normalized.ends_with("_ref")
                || normalized.ends_with("_refs");
            ((policy.is_sensitive_key(key) || policy.is_sensitive_header(key))
                && !configured_reference
                && !value.is_null())
                || replay_json_contains_inline_secret(value, policy)
        }),
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| replay_json_contains_inline_secret(value, policy)),
        serde_json::Value::String(value) => policy.redact_env_value(value) != *value,
        _ => false,
    }
}

fn persist_replay_input(
    root: &Path,
    kind: &str,
    content: &str,
    index: usize,
    argv_token: Option<String>,
) -> homeboy::core::Result<UnmaterializedCookReplayInputRef> {
    let sha256 = admission_digest(content.as_bytes());
    let safe_kind = kind
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let path = root.join(format!(
        "{index}-{safe_kind}-{}.txt",
        sha256.trim_start_matches("sha256:")
    ));
    if path.exists() {
        let existing = std::fs::read_to_string(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        if existing != content {
            return Err(Error::validation_invalid_argument(
                "cook_admission.replay_intent",
                "immutable replay input digest collision",
                Some(path.display().to_string()),
                None,
            ));
        }
    } else {
        homeboy_engine_primitives::local_files::write_file_owner_only(
            &path,
            &content,
            "persist Cook replay input",
        )?;
    }
    Ok(UnmaterializedCookReplayInputRef {
        kind: kind.to_string(),
        path: path.display().to_string(),
        sha256,
        argv_token,
    })
}

fn reject_credential_bearing_url(value: &str) -> homeboy::core::Result<()> {
    if !value.contains("://") {
        return Ok(());
    }
    let Some((_, remainder)) = value.split_once("://") else {
        return Ok(());
    };
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    let credential_query = value
        .split_once('?')
        .map(|(_, query)| query.split('#').next().unwrap_or(query))
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter_map(|pair| pair.split_once('=').map(|(key, _)| key))
        .any(|key| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "token" | "secret" | "password" | "authorization" | "auth" | "api_key" | "apikey"
            )
        });
    if authority.contains('@') || credential_query {
        return Err(unsafe_inline_replay_error("credential-bearing URL"));
    }
    Ok(())
}

fn consume_unmaterialized_replay_claim() -> homeboy::core::Result<Option<i32>> {
    let cook_id = std::env::var(COOK_REPLAY_CLAIM_COOK_ENV).ok();
    let fence = std::env::var(COOK_REPLAY_CLAIM_FENCE_ENV).ok();
    let token = std::env::var(COOK_REPLAY_CLAIM_TOKEN_ENV).ok();
    if cook_id.is_none() && fence.is_none() && token.is_none() {
        return Ok(None);
    }
    let (Some(cook_id), Some(fence), Some(token)) = (cook_id, fence, token) else {
        return Err(Error::validation_invalid_argument(
            "cook_admission.replay_claim",
            "Cook replay claim environment is incomplete",
            None,
            None,
        ));
    };
    let fence = fence.parse::<u64>().map_err(|_| {
        Error::validation_invalid_argument(
            "cook_admission.replay_claim",
            "Cook replay fence is not an unsigned generation",
            Some(fence),
            None,
        )
    })?;
    let consumed =
        agent_task_lifecycle::consume_unmaterialized_cook_replay_claim(&cook_id, fence, &token)?;
    Ok((!consumed).then_some(0))
}

fn is_unmaterialized_replay_worker() -> bool {
    std::env::var_os(COOK_REPLAY_CLAIM_COOK_ENV).is_some()
}

fn renew_unmaterialized_replay_claim_before_materialization() -> homeboy::core::Result<()> {
    let Some(cook_id) = std::env::var(COOK_REPLAY_CLAIM_COOK_ENV).ok() else {
        return Ok(());
    };
    let fence = std::env::var(COOK_REPLAY_CLAIM_FENCE_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_admission.replay_claim",
                "Cook replay materialization fence is missing or invalid",
                None,
                None,
            )
        })?;
    let token = std::env::var(COOK_REPLAY_CLAIM_TOKEN_ENV).map_err(|_| {
        Error::validation_invalid_argument(
            "cook_admission.replay_claim",
            "Cook replay materialization token is missing",
            None,
            None,
        )
    })?;
    if !agent_task_lifecycle::renew_unmaterialized_cook_replay_claim(&cook_id, fence, &token)? {
        return Err(Error::validation_invalid_argument(
            "cook_admission.replay_claim",
            "Cook replay worker lost its fenced lease before destination materialization",
            Some(cook_id),
            None,
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct CliCookAdmissionReplayDriver;

impl homeboy::core::daemon::orchestration::CookAdmissionReplayDriver
    for CliCookAdmissionReplayDriver
{
    fn select_runner(
        &self,
        request: &serde_json::Value,
    ) -> homeboy::core::Result<serde_json::Value> {
        crate::cli_runtime::select_unmaterialized_cook_runner(request)
    }

    fn replay(&self, request: &serde_json::Value) -> homeboy::core::Result<serde_json::Value> {
        if request["schema"] != "homeboy/unmaterialized-cook-replay-request/v1" {
            return Err(Error::validation_invalid_argument(
                "cook_admission.replay",
                "unsupported Cook replay request schema",
                None,
                None,
            ));
        }
        let intent: UnmaterializedCookReplayIntent =
            serde_json::from_value(request["intent"].clone()).map_err(|error| {
                Error::validation_invalid_argument(
                    "cook_admission.replay_intent",
                    format!("invalid Cook replay intent: {error}"),
                    None,
                    None,
                )
            })?;
        validate_replay_intent(&intent, request)?;
        let runner_id = request["runner_id"].as_str().expect("validated runner id");
        let fence = request["fence"].as_u64().expect("validated fence");
        let token = request["token"].as_str().expect("validated token");
        let mut replay_argv = intent.argv.clone();
        for reference in &intent.input_refs {
            let Some(token) = reference.argv_token.as_deref() else {
                continue;
            };
            let content = std::fs::read_to_string(&reference.path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(reference.path.clone()))
            })?;
            for argument in &mut replay_argv {
                if argument == token {
                    *argument = content.clone();
                } else if argument.ends_with(token)
                    && argument.as_bytes().get(argument.len() - token.len() - 1) == Some(&b'=')
                {
                    let flag = argument[..argument.len() - token.len()].to_string();
                    *argument = format!("{flag}{content}");
                }
            }
        }
        let placement = Cli::try_parse_from(&replay_argv)
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "cook_admission.replay_intent",
                    format!("replay Cook arguments are invalid: {error}"),
                    None,
                    None,
                )
            })?
            .placement;
        let mut args = replay_argv.into_iter().skip(1).collect::<Vec<_>>();
        // `--runner` is a pin and conflicts with explicit placement. Required
        // Lab placement already selects a ready runner during replay, retaining
        // the operator's durable request instead of rewriting it as Auto.
        if placement != homeboy::cli_surface::Placement::Lab {
            args.splice(0..0, ["--runner".to_string(), runner_id.to_string()]);
        }
        let worker_log = Path::new(&intent.input_manifest.path)
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                Error::internal_unexpected("Cook replay manifest has no admission root")
            })?
            .join(format!("replay-worker-{fence}.log"));
        homeboy_engine_primitives::local_files::write_file_owner_only(
            &worker_log,
            "",
            "create Cook replay worker log",
        )?;
        let worker_stderr = std::fs::OpenOptions::new()
            .append(true)
            .open(&worker_log)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(worker_log.display().to_string()))
            })?;
        let worker_stdout = worker_stderr.try_clone().map_err(|error| {
            Error::internal_io(error.to_string(), Some(worker_log.display().to_string()))
        })?;
        let child = std::process::Command::new(std::env::current_exe().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("resolve replay executable".to_string()),
            )
        })?)
        .args(&args)
        .env(COOK_REPLAY_CLAIM_COOK_ENV, &intent.cook_id)
        .env(COOK_REPLAY_CLAIM_FENCE_ENV, fence.to_string())
        .env(COOK_REPLAY_CLAIM_TOKEN_ENV, token)
        .stdin(Stdio::null())
        .stdout(Stdio::from(worker_stdout))
        .stderr(Stdio::from(worker_stderr))
        .spawn()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("spawn Cook replay worker".to_string()),
            )
        })?;
        let worker_pid = child.id();
        supervise_replay_worker(intent.cook_id.clone(), fence, token.to_string(), child);
        Ok(serde_json::json!({
            "schema": "homeboy/unmaterialized-cook-replay-receipt/v1",
            "worker_pid": worker_pid,
            "worker_log": worker_log,
            "fence": fence,
        }))
    }
}

fn supervise_replay_worker(
    cook_id: String,
    fence: u64,
    token: String,
    mut child: std::process::Child,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = child.wait();
        let _ = agent_task_lifecycle::release_unmaterialized_cook_replay_claim_after_worker_exit(
            &cook_id, fence, &token,
        );
    })
}

fn validate_replay_intent(
    intent: &UnmaterializedCookReplayIntent,
    request: &serde_json::Value,
) -> homeboy::core::Result<()> {
    if intent.schema != COOK_REPLAY_INTENT_SCHEMA
        || request["cook_id"].as_str() != Some(intent.cook_id.as_str())
        || request["runner_id"].as_str().is_none()
        || request["token"].as_str().is_none()
        || request["fence"].as_u64().is_none()
    {
        return Err(Error::validation_invalid_argument(
            "cook_admission.replay_intent",
            "Cook replay intent does not match its fenced request",
            None,
            None,
        ));
    }
    for reference in intent
        .input_refs
        .iter()
        .chain(std::iter::once(&intent.input_manifest))
    {
        let bytes = std::fs::read(&reference.path).map_err(|error| {
            Error::validation_invalid_argument(
                "cook_admission.replay_intent",
                format!("replay input is unavailable: {error}"),
                Some(reference.path.clone()),
                None,
            )
        })?;
        if admission_digest(bytes) != reference.sha256 {
            return Err(Error::validation_invalid_argument(
                "cook_admission.replay_intent",
                "replay input changed after admission",
                Some(reference.path.clone()),
                None,
            ));
        }
    }
    let manifest = std::fs::read_to_string(&intent.input_manifest.path).map_err(|error| {
        Error::validation_invalid_argument(
            "cook_admission.replay_intent",
            format!("replay input manifest is unavailable: {error}"),
            Some(intent.input_manifest.path.clone()),
            None,
        )
    })?;
    let manifest_refs: Vec<UnmaterializedCookReplayInputRef> = serde_json::from_str(&manifest)
        .map_err(|error| {
            Error::validation_invalid_argument(
                "cook_admission.replay_intent",
                format!("replay input manifest is invalid: {error}"),
                Some(intent.input_manifest.path.clone()),
                None,
            )
        })?;
    if manifest_refs != intent.input_refs {
        return Err(Error::validation_invalid_argument(
            "cook_admission.replay_intent",
            "replay input manifest does not match the typed intent",
            Some(intent.input_manifest.path.clone()),
            None,
        ));
    }
    Ok(())
}

pub(crate) fn register_unmaterialized_cook_replay_driver() {
    homeboy::core::daemon::orchestration::register_cook_admission_replay_driver(Arc::new(
        CliCookAdmissionReplayDriver,
    ));
}

fn unmaterialized_admission_state(
    readiness: Option<&homeboy::core::parsed_command_preflight::LabReadinessSnapshot>,
) -> &'static str {
    match readiness.map(|value| value.state.as_str()) {
        Some("stale") => "blocked_runner_stale",
        // A healthy reverse runner at capacity owns a durable broker queue. It
        // is waiting for a slot, not unavailable.
        Some("capacity_blocked") => "queued",
        _ => "blocked_runner_unavailable",
    }
}

/// Explain a Lab placement that cannot be served, without contradicting the
/// documented guidance.
///
/// Docs recommend global `--placement lab` for Cook waves, and the runtime
/// honors it: the coordinator stays controller-owned while provider attempts go
/// to the selected runner. When no runner can be selected, the generic
/// portability rejection ("`--placement lab` is unavailable for this local-only
/// command") is a contradiction the operator has to reverse-engineer (#9373).
/// Report the real cause — no ready Lab runner — with the readiness verdict and
/// its remediation commands instead.
///
/// `--placement lab-or-local` intentionally does not reach here: it authorizes
/// controller execution when Lab cannot be served.
fn split_placement_lab_runner_unavailable_error(
    command: &Commands,
    placement: homeboy::cli_surface::Placement,
    inferred_runner_id: Option<&str>,
    readiness: Option<&homeboy::core::parsed_command_preflight::LabReadinessSnapshot>,
) -> Option<Error> {
    if placement != homeboy::cli_surface::Placement::Lab || inferred_runner_id.is_some() {
        return None;
    }
    let label = split_placement_coordinator_label(command)?;
    let state = readiness
        .map(|readiness| readiness.state.as_str())
        .unwrap_or("unknown");
    let mut hints = Vec::new();
    if let Some(readiness) = readiness {
        hints.extend(
            readiness
                .reasons
                .iter()
                .map(|reason| format!("Lab runner readiness: {reason}")),
        );
        hints.extend(readiness.remediation_commands.iter().cloned());
    }
    hints.push(format!(
        "`--placement lab` is the supported spelling for {label} waves: it selects the Lab runner for each provider attempt while the coordinator stays on this controller."
    ));
    hints.push(
        "Pin one runner explicitly with `--runner <runner-id>` when several are configured."
            .to_string(),
    );
    hints.push(
        "Use `--placement lab-or-local` to authorize controller execution when no Lab runner is ready."
            .to_string(),
    );
    Some(Error::validation_invalid_argument(
        "placement",
        format!(
            "{label} accepts `--placement lab` but requires an eligible Lab runner; none could be selected (readiness: {state}), so controller-owned target preparation did not start and no provider attempt was dispatched"
        ),
        Some("lab".to_string()),
        Some(hints),
    ))
}

fn required_lab_runner_unavailable_error(
    readiness: Option<&homeboy::core::parsed_command_preflight::LabReadinessSnapshot>,
) -> Error {
    let state = readiness
        .map(|readiness| readiness.state.as_str())
        .unwrap_or("unknown");
    let mut hints = readiness
        .into_iter()
        .flat_map(|readiness| readiness.remediation_commands.iter().cloned())
        .collect::<Vec<_>>();
    hints.push(
        "Wait for an eligible runner, or use `--runner <runner-id>` to pin one that is ready."
            .to_string(),
    );
    Error::validation_invalid_argument(
        "placement",
        format!(
            "required Lab placement has no selected ready runner (readiness: {state}); controller-side source preparation did not start and no workload executed locally"
        ),
        Some("lab".to_string()),
        Some(hints),
    )
}

/// Fanout keeps durable batch state, worktree ownership, artifact ingestion,
/// gates, and finalization on the controller. Each child provider attempt is
/// the only unit handed to the explicitly selected Lab runner.
fn run_split_placement_fanout(
    cli: &Cli,
    output_file: Option<&str>,
    runner_id: Option<&str>,
    directive: &homeboy::core::parsed_command_preflight::PlacementDirective,
) -> homeboy::core::Result<Option<i32>> {
    if cli.placement == homeboy::cli_surface::Placement::Local {
        return Ok(None);
    }
    let Some(runner_id) = runner_id else {
        return Ok(None);
    };
    let runner_id = runner_id.to_string();
    let job_overrides = lab_job_overrides(cli)?;
    let placement = cli.placement;
    let directive = directive.clone();
    let allow_dirty_lab_workspace = cli.allow_dirty_lab_workspace;
    let skip_deps_hydration = cli.skip_deps_hydration;
    let detach_after_handoff = cli.detach_after_handoff;
    let attempt_dispatcher = move |options: &crate::agents::agent_task_service::CookRequest| {
        // Fanout compiles each child worktree before this factory runs. Bind
        // its decision here, not at coordinator startup, so the decision
        // names the child candidate/base that will actually be dispatched.
        let source_path = options
            .initial_plan
            .tasks
            .first()
            .and_then(|task| task.workspace.root.as_ref())
            .map(PathBuf::from);
        let task = options
            .initial_plan
            .tasks
            .first()
            .map(|task| task.task_id.as_str())
            .unwrap_or("fanout-provider-attempt");
        let placement_decision = options
            .initial_plan
            .metadata
            .get("execution_placement_decision")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_else(|| finalize_placement(&directive, task, source_path.as_deref()));
        let selected_runner_id = placement_decision
            .runner
            .as_ref()
            .map(|runner| runner.runner_id.clone())
            .unwrap_or_else(|| runner_id.clone());
        Arc::new(LabCookAttemptDispatcher {
            runner_id: selected_runner_id,
            placement_decision,
            allow_local_fallback: false,
            allow_dirty_lab_workspace,
            skip_deps_hydration,
            detach_after_handoff,
            source_path,
            job_overrides: job_overrides.clone(),
            progress_reporter: crate::commands::agent_task::CookProgressReporter::new(false),
        }) as Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>
    };
    let (value, exit_code) = match &cli.command {
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command:
                crate::commands::agent_task::AgentTaskCommand::Fanout(
                    crate::commands::agent_task::AgentTaskFanoutArgs {
                        command: crate::commands::agent_task::AgentTaskFanoutCommand::RunPlan(args),
                    },
                ),
        }) => crate::commands::agent_task::fanout::run_batch_cook_fanout_with_attempt_dispatcher_and_placement(
            args.clone(),
            &attempt_dispatcher,
            placement,
        )?,
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command:
                crate::commands::agent_task::AgentTaskCommand::Fanout(
                    crate::commands::agent_task::AgentTaskFanoutArgs {
                        command:
                            crate::commands::agent_task::AgentTaskFanoutCommand::CookBatch(args),
                    },
                ),
        }) if args.run_plan => {
            crate::commands::agent_task::fanout::cook_batch_with_attempt_dispatcher_and_placement(
                *args.clone(),
                &attempt_dispatcher,
                placement,
            )?
        }
        _ => return Ok(None),
    };
    let stdout = serde_json::to_string_pretty(&value).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize controller-owned fanout result".to_string()),
        )
    })?;
    if let Some(path) = output_file {
        write_output_file(path, &stdout)?;
    }
    print!("{stdout}");
    Ok(Some(exit_code))
}

/// Reject `agent-task cook` requests whose flags contradict each other.
///
/// These are properties of the argument combination alone, not of the execution
/// context, so they are evaluated before `route_after_parse` consults any
/// transport state. The bail-outs at the top of routing exist to stop a
/// runner-side process recursing back through controller routing; they were
/// never meant to waive validation. While they did, a single inherited
/// controller-transport variable let a contradictory cook through to real
/// worktree and provider work instead of a fast rejection (#10917).
///
/// This is the only place this rejection lives. `run_split_placement_cook`
/// deliberately does not repeat it: duplicated validation drifts, and which
/// message an operator sees should never depend on the path taken to reach it.
///
/// `--placement local --detach-after-handoff` used to be rejected here too. It
/// is not a contradiction — it is a request this controller can now serve by
/// re-executing the Cook in its own session, so its verdict belongs to
/// [`local_detach::intercept_local_detached_cook`] (#11476).
fn reject_contradictory_cook_arguments(cli: &Cli) -> homeboy::core::Result<()> {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
    }) = &cli.command
    else {
        return Ok(());
    };

    if cook.dispatch.core.queue_only {
        return Err(Error::validation_invalid_argument(
            "queue-only",
            "agent-task cook cannot queue its controller-owned lifecycle; it must retain provider completion to ingest artifacts, promote candidates, run gates, and finalize",
            None,
            Some(vec![
                "Use `homeboy agent-task run-plan --plan <materialized-plan> --record-run-id <run-id> --queue-only` only when a controller owns the corresponding continuation.".to_string(),
            ]),
        ));
    }

    Ok(())
}

/// Cook owns controller-local target resolution, promotion, gates, retries, and
/// finalization. Its provider attempt is the only portable unit: a materialized
/// typed run-plan that mirrors its aggregate and artifacts back into the same
/// durable attempt record before this controller resumes.
fn run_split_placement_cook(
    cli: &Cli,
    _normalized_args: &[String],
    output_file: Option<&str>,
    runner_id: Option<&str>,
    directive: &homeboy::core::parsed_command_preflight::PlacementDirective,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> homeboy::core::Result<Option<i32>> {
    run_split_placement_cook_with_runtime(
        cli,
        output_file,
        runner_id,
        directive,
        provenance,
        None,
        None,
    )
}

fn run_split_placement_cook_with_runtime(
    cli: &Cli,
    output_file: Option<&str>,
    runner_id: Option<&str>,
    directive: &homeboy::core::parsed_command_preflight::PlacementDirective,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
    dispatcher_override: Option<
        Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
    executor_override: Option<homeboy::agents::agent_task_scheduler::SharedAgentTaskExecutor>,
) -> homeboy::core::Result<Option<i32>> {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
    }) = &cli.command
    else {
        return Ok(None);
    };
    // Contradictory combinations are already rejected by
    // `reject_contradictory_cook_arguments` before any routing decision, so
    // reaching here means the request is internally consistent.
    //
    // `--runner` implies Lab placement and is mutually exclusive with an
    // explicit `--placement` at argument parsing, so `--placement local` here
    // always means a fully local cook with no pinned runner.
    if cli.placement == homeboy::cli_surface::Placement::Local {
        return Ok(None);
    }
    let Some(runner_id) = runner_id else {
        if cli.detach_after_handoff {
            return Err(Error::validation_invalid_argument(
                "runner",
                "a detached Cook requires an eligible Lab runner or reverse-runner durable queue; controller-local execution was not authorized",
                None,
                Some(vec![
                    "Wait for a reverse runner to reconnect or free capacity, then retry.".to_string(),
                    "Use `--placement local` to explicitly authorize controller execution.".to_string(),
                ]),
            ));
        }
        return Ok(None);
    };

    renew_unmaterialized_replay_claim_before_materialization()?;
    let plan = materialize_agent_task_cook_plan(cli, provenance)
        .map_err(|error| annotate_cook_controller_preparation_error(error, runner_id))?
        .expect("cook plan");
    let serialized_plan = serde_json::to_string(&plan).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize Lab cook attempt plan".to_string()),
        )
    })?;
    let cook_id = cook
        .dispatch
        .run_id
        .clone()
        .unwrap_or_else(|| format!("agent-task-{}", uuid::Uuid::new_v4()));
    let attempt_run_id = agent_task_lifecycle::cook_attempt_run_id(&cook_id, 1);
    let source_path = plan
        .tasks
        .first()
        .and_then(|task| task.workspace.root.as_ref())
        .map(PathBuf::from);
    let placement_task = plan
        .tasks
        .first()
        .map(|task| task.task_id.as_str())
        .unwrap_or("cook-provider-attempt");
    let progress_reporter =
        crate::commands::agent_task::CookProgressReporter::new(cook.no_progress);
    let mut controller = cook.clone();
    controller.dispatch.run_id = Some(cook_id);
    controller.attempt_run_id = Some(attempt_run_id);
    controller.attempt_plan = Some(serialized_plan);
    let dispatcher = if let Some(dispatcher) = dispatcher_override {
        dispatcher
    } else {
        Arc::new(LabCookAttemptDispatcher {
            runner_id: runner_id.to_string(),
            placement_decision: finalize_placement(
                directive,
                placement_task,
                source_path.as_deref(),
            ),
            allow_local_fallback: cli.runner.is_none() && cli.placement.allows_local_fallback(),
            allow_dirty_lab_workspace: cli.allow_dirty_lab_workspace,
            skip_deps_hydration: cli.skip_deps_hydration,
            detach_after_handoff: cli.detach_after_handoff,
            source_path,
            job_overrides: lab_job_overrides(cli)?,
            progress_reporter: progress_reporter.clone(),
        })
    };
    let progress = |phase: &str,
                    cook_id: Option<&str>,
                    run_id: Option<&str>,
                    activity: Option<&str>,
                    terminal_retry_command: Option<&str>| {
        if cook.no_progress && phase == "durable_identity" {
            if let Some(run_id) = run_id {
                crate::commands::agent_task::run::announce_durable_cook_identity(cook_id, run_id);
            }
        } else {
            progress_reporter.report(phase, cook_id, run_id, activity, terminal_retry_command);
        }
        Ok(())
    };
    let (value, exit_code) =
        crate::commands::agent_task::run::run_cook_with_executor_and_dispatcher_with_progress(
            *controller,
            executor_override.unwrap_or_else(|| std::sync::Arc::new(
                homeboy::agents::agent_tasks::provider::ExtensionProviderAgentTaskExecutor::discover(),
            )),
            Some(dispatcher),
            Some(&progress),
            provenance,
        )?;
    let stdout = serde_json::to_string_pretty(&value).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize controller-owned cook result".to_string()),
        )
    })?;
    if let Some(path) = output_file {
        write_output_file(path, &stdout)?;
    }
    print!("{stdout}");
    Ok(Some(exit_code))
}

/// The controller supplies this transport to the cook service. Every attempt
/// uses the same durable run id, while Lab only executes the provider plan.
#[derive(Debug, Clone)]
struct LabCookAttemptDispatcher {
    runner_id: String,
    placement_decision: homeboy_lab_runner_contract::ExecutionPlacementDecision,
    allow_local_fallback: bool,
    allow_dirty_lab_workspace: bool,
    skip_deps_hydration: bool,
    detach_after_handoff: bool,
    source_path: Option<PathBuf>,
    job_overrides: runners::LabJobOverrides,
    progress_reporter: crate::commands::agent_task::CookProgressReporter,
}

pub(crate) fn reconstruct_cook_attempt_dispatcher(
    recipe: &serde_json::Value,
) -> homeboy::core::Result<
    Option<Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>>,
> {
    let kind = recipe
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.promotion_transport.attempt_dispatch",
                "durable attempt dispatcher recipe is missing its kind",
                None,
                None,
            )
        })?;
    if kind == "local" {
        return Ok(None);
    }
    if kind != "lab" {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.promotion_transport.attempt_dispatch.kind",
            format!("unsupported durable attempt dispatcher kind `{kind}`"),
            None,
            None,
        ));
    }
    let value = |name: &str| {
        recipe.get(name).cloned().ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.promotion_transport.attempt_dispatch",
                format!("Lab attempt dispatcher recipe is missing `{name}`"),
                None,
                None,
            )
        })
    };
    let overrides = value("job_overrides")?;
    let dispatcher = LabCookAttemptDispatcher {
        runner_id: decode_cook_dispatch_field("runner_id", value("runner_id")?)?,
        placement_decision: decode_cook_dispatch_field(
            "execution_placement_decision",
            value("execution_placement_decision")?,
        )?,
        allow_local_fallback: decode_cook_dispatch_field(
            "allow_local_fallback",
            value("allow_local_fallback")?,
        )?,
        allow_dirty_lab_workspace: decode_cook_dispatch_field(
            "allow_dirty_lab_workspace",
            value("allow_dirty_lab_workspace")?,
        )?,
        skip_deps_hydration: decode_cook_dispatch_field(
            "skip_deps_hydration",
            value("skip_deps_hydration")?,
        )?,
        detach_after_handoff: decode_cook_dispatch_field(
            "detach_after_handoff",
            recipe
                .get("detach_after_handoff")
                .cloned()
                .unwrap_or(serde_json::json!(false)),
        )?,
        source_path: decode_cook_dispatch_field("source_path", value("source_path")?)?,
        job_overrides: runners::LabJobOverrides {
            env: decode_cook_dispatch_field("job_overrides.env", overrides["env"].clone())?,
            secret_env_names: decode_cook_dispatch_field(
                "job_overrides.secret_env_names",
                overrides["secret_env_names"].clone(),
            )?,
            workspace_root: decode_cook_dispatch_field(
                "job_overrides.workspace_root",
                overrides["workspace_root"].clone(),
            )?,
        },
        progress_reporter: crate::commands::agent_task::CookProgressReporter::new(false),
    };
    Ok(Some(Arc::new(dispatcher)))
}

fn decode_cook_dispatch_field<T: serde::de::DeserializeOwned>(
    name: &str,
    value: serde_json::Value,
) -> homeboy::core::Result<T> {
    serde_json::from_value(value).map_err(|error| {
        Error::validation_invalid_argument(
            "cook_recipe.promotion_transport.attempt_dispatch",
            format!("malformed Lab attempt dispatcher field `{name}`: {error}"),
            None,
            None,
        )
    })
}

fn cook_attempt_source_path<'a>(
    derived_cook_baseline: Option<&'a DerivedCookBaselineCapability>,
    controller_source_path: Option<&'a Path>,
) -> Option<&'a Path> {
    derived_cook_baseline
        .map(|capability| capability.canonical_path())
        .or(controller_source_path)
}

impl crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher
    for LabCookAttemptDispatcher
{
    fn durable_recipe(&self) -> homeboy::core::Result<serde_json::Value> {
        Ok(serde_json::json!({
            "kind": "lab",
            "runner_id": self.runner_id,
            "execution_placement_decision": self.placement_decision,
            "allow_local_fallback": self.allow_local_fallback,
            "allow_dirty_lab_workspace": self.allow_dirty_lab_workspace,
            "skip_deps_hydration": self.skip_deps_hydration,
            "detach_after_handoff": self.detach_after_handoff,
            "source_path": self.source_path,
            "job_overrides": {
                "env": self.job_overrides.env,
                "secret_env_names": self.job_overrides.secret_env_names,
                "workspace_root": self.job_overrides.workspace_root,
            },
        }))
    }

    fn prepare_for_cook(&self) -> homeboy::core::Result<()> {
        runners::prepare_explicit_lab_runner_for_offload(&self.runner_id)
    }

    fn pre_execution_failure_phase(&self) -> &'static str {
        "transport_dispatcher_prepare"
    }

    fn dispatch_attempt(
        &self,
        mut plan: homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
        run_id: &str,
        derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> homeboy::core::Result<()> {
        // Preserve the controller's canonical decision across ordinary retry,
        // continuation, and fanout replay. A derived baseline is the declared
        // pre-staging transition where a changed candidate may replace it.
        let source_path =
            cook_attempt_source_path(derived_cook_baseline, self.source_path.as_deref());
        let task = plan
            .tasks
            .first()
            .map(|task| task.task_id.clone())
            .unwrap_or_else(|| self.placement_decision.identity.task.clone());
        let placement_decision = resolve_cook_attempt_placement_decision(
            &mut plan,
            run_id,
            &self.placement_decision,
            &self.runner_id,
            &task,
            source_path,
        )?;
        // The capability has already bound the promoted artifact and exact
        // baseline to this retry; only its evidence crosses the Lab boundary.
        let verified_cook_baseline =
            derived_cook_baseline.map(DerivedCookBaselineCapability::verified_baseline_provenance);
        if let Some(verified_cook_baseline) = verified_cook_baseline.as_ref() {
            attach_verified_cook_baseline(&mut plan, verified_cook_baseline);
        }
        let serialized_plan = serde_json::to_string(&plan).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize Lab cook attempt plan".to_string()),
            )
        })?;
        // Lab routing carries the durable plan opaquely as JSON (only its
        // presence is consulted); serialize the typed plan for the request.
        let durable_agent_task_plan = serde_json::to_value(&plan).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize durable agent-task plan".to_string()),
            )
        })?;
        let provider_args = lab_cook_attempt_args(serialized_plan, run_id);
        let provider_cli = Cli::try_parse_from(&provider_args).map_err(|error| {
            Error::validation_invalid_argument(
                "agent-task cook",
                format!("build Lab provider attempt: {error}"),
                Some(run_id.to_string()),
                None,
            )
        })?;
        // Stage the controller-owned identity before Lab preflight. A rejected
        // handoff can then terminalize this record with a retryable diagnosis.
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        stage_controller_lab_handoff_before_preacceptance(
            run_id,
            &self.runner_id,
            &provider_args,
            &plan,
        )?;
        let (heartbeat_stop, heartbeat_wait) = mpsc::channel();
        let heartbeat_run_id = run_id.to_string();
        let heartbeat_progress_reporter = self.progress_reporter.clone();
        let outcome = std::thread::scope(|scope| {
            scope.spawn(move || {
                while let Err(mpsc::RecvTimeoutError::Timeout) =
                    heartbeat_wait.recv_timeout(Duration::from_secs(15))
                {
                    // A Lab-offloaded attempt runs the provider on the runner,
                    // so neither the local process tree nor a local worktree
                    // describes it. Report liveness without activity rather
                    // than sampling this host and reporting someone else's
                    // work as the provider's (#11482).
                    heartbeat_progress_reporter.report(
                        "heartbeat",
                        None,
                        Some(&heartbeat_run_id),
                        None,
                        None,
                    );
                }
            });
            let outcome = lab_routing::dispatch_lab_offload(
                LabRoutingRequest {
                    placement_decision: placement_decision.clone(),
                    command: lab_offload_command(&provider_cli.command)?,
                    normalized_args: &provider_args,
                    explicit_runner: Some(&self.runner_id),
                    placement: homeboy::cli_surface::Placement::Lab,
                    allow_local_fallback: self.allow_local_fallback,
                    allow_dirty_lab_workspace: self.allow_dirty_lab_workspace,
                    skip_deps_hydration: self.skip_deps_hydration,
                    preserve_workspace_on_failure: false,
                    capture_patch: false,
                    mutation_flag: None,
                    timeout: None,
                    placement_outcome_target: Some(
                        ExecutionPlacementOutcomeTarget::AgentTaskLifecycle { run_id },
                    ),
                    detach_after_handoff: self.detach_after_handoff,
                    output_file_requested: false,
                    read_only_polling: false,
                    require_controller_git_bundle: false,
                    reuse_compatible_snapshot: false,
                    local_output_file: None,
                    durable_agent_task_plan: Some(&durable_agent_task_plan),
                    durable_run_id: None,
                    // A retry's baseline is controller-owned capability, not plan
                    // data. Stage that exact clean checkout; never substitute the
                    // controller's original workspace during nested Lab dispatch.
                    source_path,
                    expected_source_snapshot_identity: None,
                    verified_cook_baseline: verified_cook_baseline.as_ref(),
                    job_overrides: self.job_overrides.clone(),
                },
                Some(&self.runner_id),
                Box::new(NoopLabDispatchObserver),
            );
            let _ = heartbeat_stop.send(());
            outcome
        });
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(_error) if agent_task_lifecycle::has_accepted_runner_handoff(run_id)? => {
                // The daemon owns an accepted job. A controller session can
                // disappear during a refresh, but its durable runner/job IDs
                // remain sufficient for later authoritative reconciliation.
                return Ok(());
            }
            Err(error) => {
                // This boundary is reached only after runner preflight has
                // accepted the Cook but before a provider command is handed
                // off. Transport, daemon publication, and reconciliation
                // failures here are safe to retry and must not spend provider
                // budget by being classified as invalid input.
                let error =
                    durable_lab_preacceptance_transport_error(run_id, &self.runner_id, error);
                let recovery = format!(
                    "Resolve the Lab handoff, then retry controller-owned attempt `{run_id}`."
                );
                return Err(
                    match agent_task_lifecycle::record_pre_execution_failure(
                        run_id,
                        &plan,
                        "lab_handoff_preacceptance",
                        &error,
                    ) {
                        Ok(_) => error.with_hint(recovery),
                        Err(record_error) => error.with_hint(format!(
                            "{recovery} Homeboy also could not persist the handoff failure: {}",
                            record_error.message
                        )),
                    },
                );
            }
        };
        match outcome {
            LabRouteOutcome::Offloaded(remote) if remote.exit_code == 0 => Ok(()),
            LabRouteOutcome::Offloaded(remote) => Err(Error::validation_invalid_argument(
                "agent-task cook attempt",
                format!("Lab provider attempt {run_id} failed with exit code {}", remote.exit_code),
                Some(run_id.to_string()),
                Some(vec![format!(
                    "Inspect the controller-owned attempt with `homeboy agent-task status {run_id}`."
                )]),
            )),
            LabRouteOutcome::RunLocal => Err(Error::validation_invalid_argument(
                "agent-task cook attempt",
                format!("Lab did not accept controller-owned provider attempt {run_id}"),
                Some(run_id.to_string()),
                Some(vec![format!(
                    "Resolve the Lab handoff, then retry the controller-owned attempt with `homeboy agent-task retry {run_id} --run --runner {}`.",
                    self.runner_id
                )]),
            )),
            LabRouteOutcome::InFlight(_) => Ok(()),
        }
    }
}

fn finalize_replacement_attempt(
    prior: &homeboy_lab_runner_contract::ExecutionPlacementDecision,
    runner_id: &str,
    task: &str,
    source_path: Option<&Path>,
) -> homeboy_lab_runner_contract::ExecutionPlacementDecision {
    let directive = homeboy::core::parsed_command_preflight::PlacementDirective {
        requested: prior.requested,
        required: prior.required,
        selected: prior.selected,
        runner: Some(
            homeboy_lab_runner_contract::ExecutionPlacementRunnerSelection {
                runner_id: runner_id.to_string(),
                source: prior
                    .runner
                    .as_ref()
                    .map(|runner| runner.source)
                    .unwrap_or(homeboy_lab_runner_contract::RunnerSelectionSource::Explicit),
            },
        ),
        fallback: prior.fallback.clone(),
        override_authorization: prior.override_authorization.clone(),
    };
    directive.finalize(homeboy_lab_runner_contract::ExecutionPlacementIdentity {
        repository: source_path
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| prior.identity.repository.clone()),
        workspace: source_path
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| prior.identity.workspace.clone()),
        task: task.to_string(),
        candidate: source_path
            .and_then(homeboy::core::git::head_sha)
            .or_else(|| prior.identity.candidate.clone()),
        base: source_path
            .and_then(|path| homeboy::core::git::rev_parse(path, "origin/HEAD"))
            .or_else(|| prior.identity.base.clone()),
    })
}

fn resolve_cook_attempt_placement_decision(
    plan: &mut homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
    run_id: &str,
    initial: &homeboy_lab_runner_contract::ExecutionPlacementDecision,
    runner_id: &str,
    task: &str,
    source_path: Option<&Path>,
) -> homeboy::core::Result<homeboy_lab_runner_contract::ExecutionPlacementDecision> {
    let replacement = finalize_replacement_attempt(initial, runner_id, task, source_path);
    let persisted = plan
        .metadata
        .get("execution_placement_decision")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            Error::validation_invalid_argument(
                "execution_placement_decision",
                format!("durable plan has malformed canonical placement decision: {error}"),
                Some(run_id.to_string()),
                None,
            )
        })?;
    let placement_decision = persisted.unwrap_or_else(|| initial.clone());
    let stale_reasons = placement_decision.stale_reasons(&replacement);
    if !plan.metadata.is_object() {
        plan.metadata = serde_json::json!({ "legacy_metadata": plan.metadata });
    }
    if !stale_reasons.is_empty() {
        plan.metadata["execution_placement_invalidated"] = serde_json::json!({
            "lifecycle_transition": "pre_staging_replacement",
            "prior_decision_id": placement_decision.decision_id,
            "replacement_decision_id": replacement.decision_id,
            "reasons": stale_reasons,
            "evidence": {
                "prior_identity": placement_decision.identity,
                "replacement_identity": replacement.identity,
                "prior_policy": {
                    "id": placement_decision.policy_id,
                    "revision": placement_decision.policy_revision,
                },
                "replacement_policy": {
                    "id": replacement.policy_id,
                    "revision": replacement.policy_revision,
                },
            },
        });
    }
    let placement_decision = if stale_reasons.is_empty() {
        placement_decision
    } else {
        replacement
    };
    plan.metadata["execution_placement_decision"] = serde_json::to_value(&placement_decision)
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize execution placement decision".to_string()),
            )
        })?;
    Ok(placement_decision)
}

/// Establish controller authority before Lab routing can reconcile a runner
/// snapshot. Lab source staging and preflight may observe an accepted daemon
/// job before the offload executor reaches its later proxy-recording step.
fn stage_controller_lab_handoff_before_preacceptance(
    run_id: &str,
    runner_id: &str,
    remote_command: &[String],
    plan: &homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
) -> homeboy::core::Result<()> {
    agent_task_lifecycle::record_lab_offload_planned(agent_task_lifecycle::LabOffloadProxyPlan {
        run_id,
        runner_id,
        // Lab replaces this controller-side placeholder with its materialized
        // workspace once it reaches the executor.
        remote_workspace: "pending",
        remote_command,
        durable_plan: Some(plan),
    })?;
    Ok(())
}

fn stage_retry_lab_handoff_before_preacceptance(
    handoff: Option<&AgentTaskRetryHandoff>,
    runner_id: Option<&str>,
) -> homeboy::core::Result<()> {
    if let (Some(handoff), Some(runner_id)) = (handoff, runner_id) {
        stage_controller_lab_handoff_before_preacceptance(
            &handoff.run_id,
            runner_id,
            &handoff.args,
            &handoff.plan,
        )?;
    }
    Ok(())
}

fn attach_verified_cook_baseline(
    plan: &mut homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
    baseline: &serde_json::Value,
) {
    let Some(source_task_id) = baseline
        .get("source_task_id")
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    for task in &mut plan.tasks {
        if task.task_id == source_task_id {
            if !task.metadata.is_object() {
                task.metadata = serde_json::json!({});
            }
            task.metadata["verified_cook_baseline"] = baseline.clone();
        }
    }
}

/// Build the runner-side child invocation after the controller has consumed
/// Lab selection. The accepted runner workspace is the child's local context.
fn lab_cook_attempt_args(serialized_plan: String, run_id: &str) -> Vec<String> {
    vec![
        "homeboy".to_string(),
        "--placement".to_string(),
        "local".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan".to_string(),
        serialized_plan,
        "--record-run-id".to_string(),
        run_id.to_string(),
    ]
}

/// Dispatch one controller-owned plan through the canonical Lab attempt
/// transport. The durable run record is created before handoff and receives the
/// typed runner/job identity as soon as the daemon accepts it.
pub(crate) fn dispatch_controller_plan_to_lab(
    mut plan: homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
    run_id: &str,
    runner_id: &str,
) -> homeboy::core::Result<serde_json::Value> {
    let source_path = plan
        .tasks
        .first()
        .and_then(|task| task.workspace.root.as_ref())
        .map(PathBuf::from);
    let placement_decision = plan
        .metadata
        .get("execution_placement_decision")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            Error::validation_invalid_argument(
                "execution_placement_decision",
                format!("stored controller plan has malformed placement decision: {error}"),
                Some(run_id.to_string()),
                None,
            )
        })?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "execution_placement_decision",
                "controller plan requires its authoritative preflight placement decision",
                Some(run_id.to_string()),
                None,
            )
        })?;
    if !plan.metadata.is_object() {
        plan.metadata = serde_json::json!({ "legacy_metadata": plan.metadata });
    }
    plan.metadata["execution_placement_decision"] = serde_json::to_value(&placement_decision)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    let dispatcher = LabCookAttemptDispatcher {
        runner_id: runner_id.to_string(),
        placement_decision,
        allow_local_fallback: false,
        allow_dirty_lab_workspace: false,
        skip_deps_hydration: false,
        // Controller actions yield after the daemon accepts the child. The
        // persisted run is the reconnect and terminal-event replay boundary.
        detach_after_handoff: true,
        source_path,
        job_overrides: runners::LabJobOverrides::default(),
        progress_reporter: crate::commands::agent_task::CookProgressReporter::new(false),
    };
    <LabCookAttemptDispatcher as crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>::prepare_for_cook(&dispatcher)?;
    <LabCookAttemptDispatcher as crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>::dispatch_attempt(
        &dispatcher,
        plan,
        run_id,
        None,
    )?;
    let record = agent_task_lifecycle::status(run_id)?;
    Ok(serde_json::json!({
        "schema": "homeboy/agent-task-controller-lab-handoff/v1",
        "run_id": run_id,
        "runner_id": runner_id,
        "identity": record.metadata.get("runner_handoff").and_then(|handoff| handoff.get("identity")).cloned(),
        "run": record,
    }))
}

/// Transfer the exact controller-compiled cook plan rather than asking the
/// runner to rebuild it from command-line inputs after the durable handoff.
fn inject_agent_task_cook_attempt_plan(
    args: &[String],
    plan: Option<&homeboy::agents::agent_tasks::scheduler::AgentTaskPlan>,
) -> homeboy::core::Result<Vec<String>> {
    let Some(plan) = plan else {
        return Ok(args.to_vec());
    };
    let agent_task_index = args.iter().position(|arg| arg == "agent-task");
    let cook_index = agent_task_index.and_then(|index| {
        args[index + 1..]
            .iter()
            .position(|arg| arg == "cook")
            .map(|offset| index + offset + 1)
    });
    let Some(cook_index) = cook_index else {
        return Ok(args.to_vec());
    };
    let serialized = serde_json::to_string(plan).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize agent-task cook attempt plan for Lab handoff".to_string()),
        )
    })?;
    let mut rewritten = args.to_vec();
    rewritten.splice(
        cook_index + 1..cook_index + 1,
        ["--attempt-plan".to_string(), serialized],
    );
    Ok(rewritten)
}

/// Materialize a cook's scheduler plan on the controller before the Lab
/// handoff. The handoff record is transport state; this plan is the durable
/// user task a later retry must execute.
fn materialize_agent_task_cook_plan(
    cli: &Cli,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> homeboy::core::Result<Option<homeboy::agents::agent_tasks::scheduler::AgentTaskPlan>> {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
    }) = &cli.command
    else {
        return Ok(None);
    };
    let cook = crate::commands::agent_task::run::resolve_cook_destination(*cook.clone())?;
    crate::commands::agent_task::run::validate_cook_request_with_provenance(&cook, provenance)?;
    let provision = crate::commands::agent_task::run::provision_cook_destination(&cook)?;
    let mut plan = crate::commands::agent_task::run::compile_cook_plan(&cook, provision)?;
    if let Some(provenance) = provenance {
        crate::commands::agent_task::run::record_cook_argument_provenance(&mut plan, provenance);
    }
    Ok(Some(plan))
}

fn annotate_cook_controller_preparation_error(mut error: Error, runner_id: &str) -> Error {
    let cause = error.message.clone();
    error.message = format!(
        "agent-task cook failed during controller-owned target preparation before its Lab provider attempt was dispatched: {cause}"
    );
    if !error.details.is_object() {
        error.details = serde_json::json!({ "cause": error.details });
    }
    error.details["cook_phase"] = serde_json::json!("controller_target_preparation");
    error.details["provider_execution_placement"] = serde_json::json!("lab");
    error.details["selected_runner_id"] = serde_json::json!(runner_id);
    error.with_hint(
        "The controller must resolve and provision the managed target before portable provider execution can start; repair this controller preparation failure and retry the same Lab placement.",
    )
}

fn inline_portable_settings_profiles(
    cli: &Cli,
    args: &[String],
) -> homeboy::core::Result<Vec<String>> {
    let (settings, command_token) = match &cli.command {
        Commands::Review(review) => match review.command.as_ref() {
            Some(crate::commands::review::ReviewCommand::Test(test)) => {
                (&test.setting_args, "test")
            }
            _ => return Ok(args.to_vec()),
        },
        Commands::Bench(bench) => match bench.portable_settings() {
            Some(settings) => (settings, "bench"),
            None => return Ok(args.to_vec()),
        },
        _ => return Ok(args.to_vec()),
    };
    if settings.settings_json_file.is_empty() {
        return Ok(args.to_vec());
    }

    let profile_values = settings.settings_profile_json_overrides()?;
    if let Some(key) = profile_values
        .iter()
        .find_map(|(key, value)| credential_shaped_setting_key(key, value))
    {
        return Err(Error::validation_invalid_argument(
            "settings-json-file",
            format!(
                "settings profile contains credential-shaped setting `{key}` and cannot be inlined into a portable or deferred command; use --runner-secret-env NAME for a runner-owned secret reference"
            ),
            Some(key.clone()),
            Some(vec![
                "Move the credential to the runner-owned secret store and pass its explicit identity with `--runner-secret-env NAME`. Keep only non-secret connection settings in the profile.".to_string(),
            ]),
        ));
    }
    let mut rewritten = Vec::with_capacity(args.len() + profile_values.len() * 2);
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            rewritten.push(arg.clone());
            rewritten.extend(iter.cloned());
            break;
        }
        if arg == "--settings-json-file" || arg == "--settings-profile" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--settings-json-file=") || arg.starts_with("--settings-profile=") {
            continue;
        }
        rewritten.push(arg.clone());
    }

    let insertion = rewritten
        .iter()
        .rposition(|arg| arg == command_token)
        .map(|index| index + 1)
        .ok_or_else(|| {
            Error::internal_unexpected("settings profile normalization lost the portable command")
        })?;
    let mut portable_profile_args = Vec::with_capacity(profile_values.len() * 2);
    for (key, value) in profile_values {
        let value = serde_json::to_string(&value).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize test settings profile".to_string()),
            )
        })?;
        portable_profile_args.push("--setting-json".to_string());
        portable_profile_args.push(format!("{key}={value}"));
    }
    rewritten.splice(insertion..insertion, portable_profile_args);
    Ok(rewritten)
}

fn credential_shaped_setting_key(key: &str, value: &serde_json::Value) -> Option<String> {
    if credential_shaped_key(key) {
        return Some(key.to_string());
    }
    match value {
        serde_json::Value::Object(values) => values.iter().find_map(|(nested, value)| {
            credential_shaped_setting_key(&format!("{key}.{nested}"), value)
        }),
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| credential_shaped_setting_key(key, value)),
        _ => None,
    }
}

fn credential_shaped_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "password",
        "secret",
        "token",
        "credential",
        "api_key",
        "apikey",
        "private_key",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn deferred_workload_input(
    cli: &Cli,
    args: &[String],
    preflight: &homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult,
    test_requirements: homeboy::deferred_workload::DeferredWorkloadRequirements,
) -> homeboy::core::Result<homeboy::deferred_workload::DeferredWorkloadInput> {
    Ok(homeboy::deferred_workload::DeferredWorkloadInput {
        command_label: "review test".to_string(),
        args: args.to_vec(),
        placement: match cli.placement {
            homeboy::cli_surface::Placement::Auto => "auto",
            homeboy::cli_surface::Placement::LabOrLocal => "lab-or-local",
            homeboy::cli_surface::Placement::Lab => "lab",
            homeboy::cli_surface::Placement::Local => "local",
        }
        .to_string(),
        resource_requirement: "eligible_lab_runner".to_string(),
        portability: "portable_lab_route".to_string(),
        reason: "no eligible Lab runner is currently ready".to_string(),
        ci_alternative: "Run the same command in CI or configure a ready Homeboy runner."
            .to_string(),
        resolved_contract: serde_json::json!({
            "label": "review test",
            "portability": "portable_lab_route",
            "required_runtimes": test_requirements.required_runtimes,
            "required_capabilities": test_requirements.required_capabilities,
        }),
        resolved_resources: preflight
            .resource_policy
            .as_ref()
            .and_then(|context| serde_json::to_value(context).ok())
            .unwrap_or_else(|| serde_json::json!({ "severity": "unknown" })),
        test_requirements,
        // The singleton worker replays this record from a stable root, long
        // after this process is gone. The worktree the workload belongs to has
        // to travel with the record rather than being inherited from whatever
        // directory the replay happens to start in (#12081).
        source_directory: Some(authoritative_lab_source_path(args)?.display().to_string()),
        job_overrides: lab_job_overrides(cli)?,
    })
}

fn review_test_deferred_requirements(
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

/// Preserve a command which can be replayed with an explicit runner. Placement
/// is controller selection state, not portable workload intent.
fn portable_deferred_args(args: &[String]) -> Vec<String> {
    let mut result = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--placement" || arg == "--runner" {
            skip_next = true;
            continue;
        }
        if arg.starts_with("--placement=") || arg.starts_with("--runner=") {
            continue;
        }
        result.push(arg.clone());
    }
    result
}

struct GenericDetachedLabHandoff {
    run_id: String,
    plan: homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
}

const GENERIC_LAB_COMMAND_REPLAY_SCHEMA: &str = "homeboy/generic-lab-command-replay/v1";

#[derive(Debug, serde::Deserialize)]
struct GenericLabCommandReplay {
    schema: String,
    normalized_args: Vec<String>,
    materialization: GenericLabMaterialization,
}

#[derive(Debug, serde::Deserialize)]
struct GenericLabMaterialization {
    canonical_root: String,
    content_identity: String,
}

fn structured_command_inputs(args: &[String]) -> serde_json::Value {
    let mut options = BTreeMap::<String, Vec<serde_json::Value>>::new();
    let mut positional = Vec::new();
    let mut passthrough = Vec::new();
    let mut iter = args.iter().skip(1).peekable();
    let mut after_separator = false;
    while let Some(arg) = iter.next() {
        if after_separator {
            passthrough.push(arg.clone());
            continue;
        }
        if arg == "--" {
            after_separator = true;
            continue;
        }
        if let Some(option) = arg.strip_prefix("--") {
            if let Some((name, value)) = option.split_once('=') {
                options
                    .entry(name.to_string())
                    .or_default()
                    .push(serde_json::Value::String(value.to_string()));
            } else if iter.peek().is_some_and(|value| !value.starts_with('-')) {
                options
                    .entry(option.to_string())
                    .or_default()
                    .push(serde_json::Value::String(
                        iter.next().expect("peeked value").clone(),
                    ));
            } else {
                options
                    .entry(option.to_string())
                    .or_default()
                    .push(serde_json::Value::Bool(true));
            }
        } else {
            positional.push(arg.clone());
        }
    }
    serde_json::json!({
        "positional": positional,
        "options": options,
        "passthrough": passthrough,
    })
}

/// Give detached portable commands a controller-owned identity before Lab
/// admission. Agent-task commands retain their richer command-specific plans.
fn materialize_generic_detached_lab_handoff(
    args: &[String],
    source_path: &Path,
    command: &runners::LabOffloadCommand,
    placement_decision: homeboy_lab_runner_contract::ExecutionPlacementDecision,
) -> homeboy::core::Result<GenericDetachedLabHandoff> {
    let run_id =
        explicit_run_id(args).unwrap_or_else(|| format!("lab-offload-{}", uuid::Uuid::new_v4()));
    let canonical_root = source_path.canonicalize().map_err(|error| {
        Error::validation_invalid_argument(
            "workspace",
            "detached Lab handoff could not canonicalize its source workspace",
            Some(source_path.display().to_string()),
            Some(vec![error.to_string()]),
        )
    })?;
    let runner_id = placement_decision.runner.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner",
            "detached Lab replay requires a selected runner to bind its transfer policy",
            None,
            None,
        )
    })?;
    let content_identity = homeboy::runner::generic_lab_replay_artifact_identity_for_runner(
        &runner_id.runner_id,
        &canonical_root,
    )
    // Plan-only controller tests may construct a placement decision without a
    // persisted runner. A real replay still rejects that legacy/default policy
    // if the selected runner later resolves any additional exclusion.
    .or_else(|error| {
        (error.message == "Runner not found")
            .then(|| homeboy::runner::generic_lab_replay_artifact_identity(&canonical_root))
            .transpose()?
            .ok_or(error)
    })?;
    let repository_remote =
        homeboy::core::git::release_download::detect_remote_url(&canonical_root);
    let revision = homeboy::core::git::head_sha(&canonical_root);
    let portable_args = portable_deferred_args(args);
    let replay = serde_json::json!({
        "schema": GENERIC_LAB_COMMAND_REPLAY_SCHEMA,
        "normalized_args": portable_args,
        "inputs": structured_command_inputs(args),
        "lab_command": command,
        "materialization": {
            "canonical_root": canonical_root,
            "content_identity": content_identity,
            "repository_remote": repository_remote,
            "revision": revision,
        },
    });
    let mut plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
        format!("lab-offload-{run_id}"),
        Vec::new(),
    );
    plan.metadata = serde_json::json!({
        "execution_placement_decision": placement_decision,
        "generic_lab_command_replay": replay,
    });
    if agent_task_lifecycle::run_record_exists(&run_id)? {
        let persisted = agent_task_lifecycle::load_controller_plan(&run_id)?;
        let persisted_decision = persisted
            .metadata
            .get("execution_placement_decision")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "execution_placement_decision",
                    format!("detached Lab run has malformed canonical placement decision: {error}"),
                    Some(run_id.clone()),
                    None,
                )
            })?;
        if persisted.plan_id != plan.plan_id
            || persisted_decision.as_ref() != Some(&placement_decision)
            || persisted.metadata.get("generic_lab_command_replay")
                != plan.metadata.get("generic_lab_command_replay")
        {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "detached Lab run id already belongs to a different immutable handoff",
                Some(run_id),
                None,
            ));
        }
    } else {
        agent_task_lifecycle::submit_plan(&plan, Some(&run_id))?;
    }
    Ok(GenericDetachedLabHandoff { run_id, plan })
}

fn explicit_run_id(args: &[String]) -> Option<String> {
    args.iter().enumerate().find_map(|(index, arg)| {
        arg.strip_prefix("--run-id=")
            .map(str::to_string)
            .or_else(|| {
                (arg == "--run-id")
                    .then(|| args.get(index + 1).cloned())
                    .flatten()
            })
    })
}

fn source_path_for_generic_detached_lab_handoff(args: &[String]) -> homeboy::core::Result<PathBuf> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == "--path" || arg == "--cwd" {
            let path = args.next().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "path",
                    "Lab handoff source flag requires a value",
                    None,
                    None,
                )
            })?;
            return Ok(PathBuf::from(shellexpand::tilde(path).to_string()));
        }
        if let Some(path) = arg
            .strip_prefix("--path=")
            .or_else(|| arg.strip_prefix("--cwd="))
        {
            return Ok(PathBuf::from(shellexpand::tilde(path).to_string()));
        }
    }
    std::env::current_dir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read cwd for Lab handoff".to_string()),
        )
    })
}

fn lab_route_dispatch_timeout(command: &Commands) -> Option<std::time::Duration> {
    if matches!(command, Commands::Trace(_)) {
        return Some(lab_routing::lab_trace_dispatch_timeout());
    }
    // An explicitly runner-scoped provider catalog read is still a diagnostic
    // read. Bound its dispatch so `--runner <id>` degrades to a labelled
    // dispatch-timeout error, with durable runner/job identity to follow, well
    // inside a caller's patience (#9763).
    if matches!(
        command,
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command: crate::commands::agent_task::AgentTaskCommand::Providers(_),
        })
    ) {
        return Some(lab_routing::lab_provider_discovery_dispatch_timeout());
    }
    None
}

struct AgentTaskRetryHandoff {
    args: Vec<String>,
    run_id: String,
    plan: homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
    primary_workspace: PathBuf,
    replays_generic_command: bool,
    expected_source_snapshot_identity: Option<String>,
}

fn generic_lab_command_replay(
    plan: &homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
) -> homeboy::core::Result<Option<GenericLabCommandReplay>> {
    let Some(value) = plan.metadata.get("generic_lab_command_replay") else {
        return Ok(None);
    };
    let replay: GenericLabCommandReplay =
        serde_json::from_value(value.clone()).map_err(|error| {
            Error::validation_invalid_argument(
                "generic_lab_command_replay",
                format!("persisted Lab replay plan is malformed: {error}"),
                Some(plan.plan_id.clone()),
                None,
            )
        })?;
    if replay.schema != GENERIC_LAB_COMMAND_REPLAY_SCHEMA
        || replay.normalized_args.is_empty()
        || replay.materialization.canonical_root.trim().is_empty()
        || replay.materialization.content_identity.trim().is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "generic_lab_command_replay",
            "persisted Lab replay plan lacks required rematerialization identity",
            Some(plan.plan_id.clone()),
            None,
        ));
    }
    Ok(Some(replay))
}

#[derive(Debug)]
struct AgentTaskRunHandoff {
    args: Vec<String>,
    plan: homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
    primary_workspace: PathBuf,
}

/// A submitted run is portable only after its controller-owned plan has been
/// serialized into the runner command. A missing local record is runner-owned,
/// so preserve the original command for the runner to resolve from its store.
fn materialize_agent_task_run_handoff(
    cli: &Cli,
    normalized_args: &[String],
) -> homeboy::core::Result<Option<AgentTaskRunHandoff>> {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Run(run),
    }) = &cli.command
    else {
        return Ok(None);
    };
    if !agent_task_lifecycle::run_record_exists(&run.run_id)? {
        return Ok(None);
    }
    if agent_task_lifecycle::exact_record(&run.run_id)
        .ok()
        .is_some_and(|record| agent_task_lifecycle::is_unmaterialized_cook_admission(&record))
    {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "unmaterialized Cook admission must continue through its fenced resume path",
            Some(run.run_id.clone()),
            Some(vec![format!("homeboy agent-task resume {}", run.run_id)]),
        )
        .with_hint(format!("Run `homeboy agent-task resume {}`.", run.run_id)));
    }

    let plan = agent_task_lifecycle::load_plan(&run.run_id)?;
    let serialized_plan = serde_json::to_string(&plan).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize agent-task run plan for Lab handoff".to_string()),
        )
    })?;
    let agent_task_index = normalized_args
        .iter()
        .position(|arg| arg == "agent-task")
        .ok_or_else(|| Error::internal_unexpected("agent-task run argv was missing agent-task"))?;
    let primary_workspace = plan_primary_workspace(&plan)?;
    let mut args = retry_handoff_prefix(&normalized_args[..agent_task_index]);
    args.extend([
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan".to_string(),
        serialized_plan,
        "--record-run-id".to_string(),
        run.run_id.clone(),
    ]);
    if let Some(timeout_ms) = run.timeout_ms {
        args.extend(["--timeout-ms".to_string(), timeout_ms.to_string()]);
    }

    Ok(Some(AgentTaskRunHandoff {
        args,
        primary_workspace,
        plan,
    }))
}

/// The single place that reads a task's declared source root, in priority
/// order: `workspace.root`, then `executor.config.workspace_root`, then
/// `metadata.workspace.root`. Both the run and retry handoff paths resolve the
/// primary checkout from this one chain so they never drift apart.
fn task_declared_source_root(
    task: &homeboy::agents::agent_tasks::AgentTaskRequest,
) -> Option<&str> {
    task.workspace
        .root
        .as_deref()
        .or_else(|| {
            task.executor
                .config
                .get("workspace_root")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            task.metadata
                .get("workspace")
                .and_then(|workspace| workspace.get("root"))
                .and_then(serde_json::Value::as_str)
        })
        .filter(|root| !root.trim().is_empty())
}

fn plan_primary_workspace(
    plan: &homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
) -> homeboy::core::Result<PathBuf> {
    let mut roots = BTreeSet::new();
    for task in &plan.tasks {
        if let Some(root) = task_declared_source_root(task) {
            roots.insert(PathBuf::from(root));
        }
    }
    match roots.len() {
        0 => Err(Error::validation_invalid_argument(
            "workspace",
            "agent-task run through Lab requires exactly one task workspace before handoff",
            Some(format!("plan_id={}", plan.plan_id)),
            Some(vec![
                "Declare one task workspace.root, executor.config.workspace_root, or metadata.workspace.root in the submitted plan.".to_string(),
            ]),
        )),
        1 => {
            let root = roots.into_iter().next().expect("one workspace root");
            root.canonicalize().map_err(|error| {
                Error::validation_invalid_argument(
                    "workspace",
                    "agent-task run through Lab could not resolve the declared task workspace before handoff",
                    Some(format!("workspace={}", root.display())),
                    Some(vec![
                        error.to_string(),
                        "Restore the managed worktree or submit a plan with an existing workspace root before retrying.".to_string(),
                    ]),
                )
            })
        }
        _ => Err(Error::validation_invalid_argument(
            "workspace",
            "agent-task run through Lab found multiple task workspaces and cannot choose a primary checkout",
            Some(
                roots
                    .iter()
                    .map(|root| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Some(vec![
                "Run one workspace-scoped plan at a time, or split the tasks into separate runs."
                    .to_string(),
            ]),
        )),
    }
}

/// Retries are controller-owned because the source plan lives in the local
/// durable lifecycle store. Materialize it before Lab dispatch, then run the
/// replacement plan remotely under the new durable run id.
fn materialize_agent_task_retry_handoff(
    cli: &Cli,
    normalized_args: &[String],
) -> homeboy::core::Result<Option<AgentTaskRetryHandoff>> {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Retry(retry),
    }) = &cli.command
    else {
        return Ok(None);
    };
    if !retry.run {
        return Ok(None);
    }
    // Use the same resolution `retry` applies (a cook id resolves to its latest
    // run). A plain exact-match here let a resolvable id fall through and ship an
    // unrunnable `agent-task retry <id>` to a runner with no such record (#8390).
    //
    // When the id does NOT resolve to a controller record, the retry is not the
    // controller's to materialize: it stays portable so the runner reads the run
    // from its own store (a runner-owned retry). Returning `None` here preserves
    // that behavior; only a resolvable controller record is materialized into a
    // self-contained run-plan handoff.
    // The existence check and the cook-id read are one decision about one
    // record, so they resolve one store rather than the environment twice
    // (#7505).
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    if !agent_task_lifecycle::run_record_exists_resolved_in_store(&lifecycle_store, &retry.run_id)?
    {
        return Ok(None);
    }
    if agent_task_lifecycle::status_in_store(
        &lifecycle_store,
        &retry.run_id,
        agent_task_lifecycle::AgentTaskStatusOptions::default(),
        false,
    )?
    .record
    .metadata["cook_id"]
        .is_string()
    {
        // Cook retries must return through the controller so its promotion,
        // gates, and finalization lifecycle consumes the successful patch.
        return Ok(None);
    }

    let retry_result = crate::agents::agent_task_service::retry_with_preflight(
        &retry.run_id,
        retry.new_run_id.as_deref(),
        true,
        retry.force,
        validate_generic_lab_command_replay_workspace,
    )?;
    if !retry_result.run {
        return Ok(None);
    }
    let record = retry_result.record;
    let plan = agent_task_lifecycle::load_plan(&record.run_id)?;
    if let Some(replay) = generic_lab_command_replay(&plan)? {
        let primary_workspace = PathBuf::from(&replay.materialization.canonical_root);
        return Ok(Some(AgentTaskRetryHandoff {
            args: replay.normalized_args,
            run_id: record.run_id,
            plan,
            primary_workspace,
            replays_generic_command: true,
            expected_source_snapshot_identity: Some(replay.materialization.content_identity),
        }));
    }
    let primary_workspace = match retry_plan_primary_workspace(&plan) {
        Ok(workspace) => workspace,
        Err(error) => {
            agent_task_lifecycle::record_pre_execution_failure(
                &record.run_id,
                &plan,
                "validate_retry_workspace",
                &error,
            )?;
            return Err(error);
        }
    };
    let serialized_plan = serde_json::to_string(&plan).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize agent-task retry plan for Lab handoff".to_string()),
        )
    })?;
    let agent_task_index = normalized_args
        .iter()
        .position(|arg| arg == "agent-task")
        .ok_or_else(|| {
            Error::internal_unexpected("agent-task retry argv was missing agent-task")
        })?;
    let mut args = retry_handoff_prefix(&normalized_args[..agent_task_index]);
    // A retry executes the original task, not the controller invocation. Carry
    // its checkout through the route request rather than emitting an unsupported
    // global --cwd argument; staging makes it the git-backed Lab primary.
    args.extend([
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan".to_string(),
        serialized_plan,
        "--record-run-id".to_string(),
        record.run_id.clone(),
    ]);

    Ok(Some(AgentTaskRetryHandoff {
        args,
        run_id: record.run_id,
        plan,
        primary_workspace,
        replays_generic_command: false,
        expected_source_snapshot_identity: None,
    }))
}

pub(crate) fn validate_generic_lab_command_replay_workspace(
    plan: &homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
) -> homeboy::core::Result<()> {
    let Some(replay) = generic_lab_command_replay(plan)? else {
        return Ok(());
    };
    let primary_workspace = PathBuf::from(&replay.materialization.canonical_root);
    if homeboy::runner::generic_lab_replay_identity_excludes(
        &replay.materialization.content_identity,
    )
    .is_err()
    {
        return Err(Error::validation_invalid_argument(
            "generic_lab_command_replay",
            "agent-task retry uses a legacy Lab replay identity that cannot attest an immutable transfer artifact",
            Some(primary_workspace.display().to_string()),
            Some(vec!["Reissue the command as a new Lab run to create an immutable replay artifact.".to_string()]),
        ));
    }
    Ok(())
}

fn retry_handoff_prefix(args: &[String]) -> Vec<String> {
    let mut rewritten = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--cwd" || arg == "--path" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--cwd=") || arg.starts_with("--path=") {
            continue;
        }
        rewritten.push(arg.clone());
    }
    rewritten
}

/// Resolve the durable managed worktree a retry task should continue against.
///
/// Returns `Ok(Some(git_root))` when the task carries a Homeboy worktree handle
/// (its recorded `workspace.slug`) that still resolves to an active checkout,
/// `Ok(None)` when the task was never anchored to a managed worktree (so the
/// caller falls back to the recorded `workspace.root`), and a precise
/// recoverable error when the handle is recorded but no longer usable. This is
/// what keeps a cleaned-up ephemeral initial-baseline directory from being
/// mistaken for the canonical continuation workspace (#9195).
fn retry_task_managed_worktree(
    task: &homeboy::agents::agent_tasks::AgentTaskRequest,
) -> homeboy::core::Result<Option<PathBuf>> {
    let Some(handle) = task
        .workspace
        .slug
        .as_deref()
        .filter(|handle| !handle.trim().is_empty())
    else {
        return Ok(None);
    };

    let Some(record) = homeboy::core::worktree::resolve_workspace_ref_if_present(handle)? else {
        // No Homeboy record for this handle: the task was not anchored to a
        // managed worktree, so leave resolution to the recorded root fallback.
        return Ok(None);
    };

    if record.state() != &homeboy::core::worktree::TaskWorktreeState::Active {
        return Err(Error::validation_invalid_argument(
            "workspace",
            format!(
                "agent-task retry task '{}' managed worktree '{}' is no longer active; its work is unavailable for continuation",
                task.task_id,
                record.handle()
            ),
            Some(handle.to_string()),
            Some(vec![
                "Recreate the worktree with `homeboy worktree add`, or retry against an explicit replacement workspace.".to_string(),
            ]),
        ));
    }

    let path = PathBuf::from(record.path());
    let git_root = git::repo_root(&path).ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace",
            format!(
                "agent-task retry task '{}' managed worktree '{}' points at a missing checkout {}",
                task.task_id,
                record.handle(),
                path.display()
            ),
            Some(path.display().to_string()),
            Some(vec![
                "Recreate the worktree with `homeboy worktree add`, or remove the stale record and retry against an explicit replacement workspace.".to_string(),
            ]),
        )
    })?;
    Ok(Some(git_root))
}

fn retry_plan_primary_workspace(
    plan: &homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
) -> homeboy::core::Result<PathBuf> {
    let mut roots = BTreeSet::new();
    for task in &plan.tasks {
        // The authoritative continuation workspace is the managed worktree the
        // task was recorded against, not whichever `workspace.root` the original
        // plan captured. A pre-provider or gate-failed cook may have run against
        // an ephemeral initial-baseline directory that has since been cleaned
        // up; trusting that path fails retry with "not inside a git checkout"
        // even though the durable worktree record still resolves. Prefer the
        // recorded worktree handle so baseline artifacts stay evidence, never
        // the canonical continuation workspace (#9195).
        if let Some(git_root) = retry_task_managed_worktree(task)? {
            roots.insert(git_root);
            continue;
        }

        if let Some(root) = task_declared_source_root(task) {
            let path = PathBuf::from(root);
            let git_root = git::repo_root(&path).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "workspace",
                    format!(
                        "agent-task retry task '{}' workspace is not inside a git checkout",
                        task.task_id
                    ),
                    Some(path.display().to_string()),
                    Some(vec![
                        "Retry the task from a plan with a git-backed workspace root, or record its managed worktree handle so retry can resolve the durable checkout.".to_string(),
                    ]),
                )
            })?;
            roots.insert(git_root);
        }
    }

    match roots.len() {
        1 => Ok(roots.into_iter().next().expect("one retry workspace")),
        0 => Err(Error::validation_invalid_argument(
            "workspace",
            "agent-task retry --run through Lab cannot rematerialize a task workspace because the original persisted plan has none; the controller cwd cannot become the task primary",
            None,
            Some(vec![
                "Record workspace.root or executor.config.workspace_root in the task plan before retrying.".to_string(),
            ]),
        )),
        _ => Err(Error::validation_invalid_argument(
            "workspace",
            "agent-task retry --run through Lab found multiple task workspaces and cannot choose a primary checkout",
            Some(
                roots
                    .iter()
                    .map(|root| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Some(vec![
                "Retry a plan whose tasks share one git-backed workspace, or split the tasks into separate retries.".to_string(),
            ]),
        )),
    }
}

fn persist_retry_handoff_preacceptance_failure(
    handoff: &AgentTaskRetryHandoff,
    route_runner_id: Option<&str>,
    error: Error,
) -> Error {
    let selected_runner = route_runner_id
        .map(str::to_string)
        .or_else(|| {
            agent_task_lifecycle::status(&handoff.run_id)
                .ok()
                .and_then(|record| record.runner_id().map(str::to_string))
        })
        .unwrap_or_else(|| "selected-lab-runner".to_string());
    let error = durable_lab_preacceptance_transport_error(&handoff.run_id, &selected_runner, error);
    let recovery = format!(
        "Fix the Lab preflight failure, then retry with `homeboy agent-task retry {} --run --runner <runner-id> --detach-after-handoff`.",
        handoff.run_id
    );
    if let Err(record_error) = agent_task_lifecycle::record_pre_execution_failure(
        &handoff.run_id,
        &handoff.plan,
        "detached_lab_handoff_preacceptance",
        &error,
    ) {
        return error.with_hint(format!(
            "{recovery} Homeboy also could not persist the replacement-run failure: {}",
            record_error.message
        ));
    }
    error.with_hint(recovery)
}

fn durable_lab_preacceptance_transport_error(
    run_id: &str,
    selected_runner: &str,
    mut error: Error,
) -> Error {
    error.retryable = Some(true);
    let is_transport = matches!(
        error.code.as_str(),
        "internal.io_error" | "runner.lab_transport_failure"
    ) || error.details.get("daemon_transport_error").is_some();
    if !is_transport {
        return error;
    }
    preacceptance_transport_error(
        run_id,
        selected_runner,
        LabTransportOperation::DispatchCookAttempt,
        LabJobAcceptanceDisposition::NoJobAccepted,
        error,
    )
}

/// Insert one env pair into the overrides, recording the key as secret when
/// the redaction policy considers the key sensitive or the value redacted.
fn insert_lab_env_override(
    overrides: &mut runners::LabJobOverrides,
    policy: &RedactionPolicy,
    name: String,
    value: String,
) -> homeboy::core::Result<()> {
    if policy.is_sensitive_key(&name) || policy.redact_string(&value) != value {
        return Err(Error::validation_invalid_argument(
            "runner-env",
            format!(
                "inline environment value for `{name}` is sensitive; use `--runner-secret-env {name}`"
            ),
            Some(name),
            Some(vec![
                "Configure the runner-owned secret reference and use `--runner-secret-env NAME` instead."
                    .to_string(),
            ]),
        ));
    }
    overrides.env.insert(name, value);
    Ok(())
}

fn validate_explicit_runner_env(
    policy: &RedactionPolicy,
    name: &str,
    value: &str,
) -> homeboy::core::Result<()> {
    if policy.is_sensitive_key(name) || policy.redact_string(value) != value {
        return Err(Error::validation_invalid_argument(
            "runner-env",
            format!(
                "--runner-env {name}=… carries a sensitive value and cannot be persisted or dispatched inline; use `--runner-secret-env {name}`"
            ),
            Some(name.to_string()),
            Some(vec![format!(
                "Configure the runner-owned secret reference and use `--runner-secret-env {name}` instead."
            )]),
        ));
    }
    Ok(())
}

fn lab_job_overrides(cli: &Cli) -> homeboy::core::Result<runners::LabJobOverrides> {
    let mut overrides = runners::LabJobOverrides::default();
    let policy = RedactionPolicy::default();
    let portable_env = portable_test_env(cli)?;

    for (name, value) in portable_env.public_env {
        insert_lab_env_override(&mut overrides, &policy, name, value)?;
    }
    // References name the runner-owned secret_env entries. They never read the
    // controller environment and are the only secret data retained by deferral.
    overrides
        .secret_env_names
        .extend(portable_env.secret_env.into_keys());

    for raw in &cli.runner_env {
        let (name, value) = parse_lab_env_pair("runner-env", raw)?;
        validate_explicit_runner_env(&policy, &name, &value)?;
        insert_lab_env_override(&mut overrides, &policy, name, value)?;
    }

    for name in &cli.runner_secret_env {
        let name = validate_lab_env_name("runner-secret-env", name)?;
        overrides.secret_env_names.push(name);
    }

    if let Some(raw_json) = cli.lab_env_json.as_deref() {
        let value: serde_json::Value = serde_json::from_str(raw_json).map_err(|err| {
            Error::validation_invalid_argument(
                "lab-env-json",
                format!("--lab-env-json must be a JSON object: {err}"),
                Some(raw_json.to_string()),
                None,
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            Error::validation_invalid_argument(
                "lab-env-json",
                "--lab-env-json must be a JSON object of string or null values",
                Some(raw_json.to_string()),
                None,
            )
        })?;
        for (name, value) in object {
            let name = validate_lab_env_name("lab-env-json", name)?;
            let value = match value {
                serde_json::Value::String(value) => value.clone(),
                serde_json::Value::Null => String::new(),
                _ => {
                    return Err(Error::validation_invalid_argument(
                        "lab-env-json",
                        format!("--lab-env-json value for `{name}` must be a string or null"),
                        Some(value.to_string()),
                        None,
                    ));
                }
            };
            insert_lab_env_override(&mut overrides, &policy, name, value)?;
        }
    }

    overrides.secret_env_names.sort();
    overrides.secret_env_names.dedup();
    overrides.workspace_root = cli
        .runner_workspace_root
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(overrides)
}

/// The test manifest, not the ambient controller, owns portable environment.
/// This runs before direct Lab dispatch and deferred-plan persistence.
fn portable_test_env(cli: &Cli) -> homeboy::core::Result<homeboy_extension::test::PortableTestEnv> {
    let Commands::Review(review) = &cli.command else {
        return Ok(homeboy_extension::test::PortableTestEnv {
            public_env: Vec::new(),
            secret_env: Default::default(),
        });
    };
    let Some(crate::commands::review::ReviewCommand::Test(args)) = review.command.as_ref() else {
        return Ok(homeboy_extension::test::PortableTestEnv {
            public_env: Vec::new(),
            secret_env: Default::default(),
        });
    };
    let context = crate::commands::source_command::resolve_source_context(
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        Some(homeboy_extension::ExtensionCapability::Test),
    )?;
    homeboy_extension::test::portable_env(&context.component)
}

fn parse_lab_env_pair(source: &str, raw: &str) -> homeboy::core::Result<(String, String)> {
    let (name, value) = raw.split_once('=').ok_or_else(|| {
        Error::validation_invalid_argument(
            source,
            format!("--{source} expects KEY=VALUE"),
            Some(raw.to_string()),
            None,
        )
    })?;
    Ok((validate_lab_env_name(source, name)?, value.to_string()))
}

fn validate_lab_env_name(source: &str, name: &str) -> homeboy::core::Result<String> {
    let name = name.trim();
    if name.is_empty()
        || name.contains('=')
        || !name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return Err(Error::validation_invalid_argument(
            source,
            format!("--{source} environment names must be non-empty ASCII identifiers"),
            Some(name.to_string()),
            None,
        ));
    }
    Ok(name.to_string())
}

/// Warn before N provider attempts run on the controller instead of the Lab.
///
/// Batch fanout is the command most likely to overwhelm a controller, so it is
/// covered here alongside `cook`. When a Lab runner was looked for and none was
/// eligible, the readiness verdict carries the only explanation of why, and its
/// remediation commands are the operator's shortest path back to the Lab.
fn agent_task_local_fanout_warning(
    command: &Commands,
    readiness: Option<&homeboy::core::parsed_command_preflight::LabReadinessSnapshot>,
) -> Option<String> {
    use crate::commands::agent_task::{
        AgentTaskArgs, AgentTaskCommand, AgentTaskFanoutArgs, AgentTaskFanoutCommand,
    };

    let (label, concurrency, task_count) = match command {
        Commands::AgentTask(AgentTaskArgs {
            command: AgentTaskCommand::Cook(args),
        }) => {
            let tasks = args.dispatch.tasks.len()
                + usize::from(args.dispatch.prompt.is_some())
                + usize::from(args.dispatch.core.tasks_json.is_some());
            if args.dispatch.concurrency <= 1 && tasks <= 1 {
                return None;
            }
            (
                "agent-task cook local fanout",
                args.dispatch.concurrency.to_string(),
                tasks.to_string(),
            )
        }
        Commands::AgentTask(AgentTaskArgs {
            command:
                AgentTaskCommand::Fanout(AgentTaskFanoutArgs {
                    command: AgentTaskFanoutCommand::CookBatch(args),
                }),
        }) if args.run_plan && !args.preview => {
            if args.issues.len() <= 1 {
                return None;
            }
            // cook-batch has no --concurrency flag: local workers are capped by
            // available parallelism, so report what will actually run.
            let workers = std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
                .min(args.issues.len());
            (
                "agent-task fanout cook-batch local fanout",
                workers.to_string(),
                args.issues.len().to_string(),
            )
        }
        Commands::AgentTask(AgentTaskArgs {
            command:
                AgentTaskCommand::Fanout(AgentTaskFanoutArgs {
                    command: AgentTaskFanoutCommand::RunPlan(_),
                }),
        }) => (
            // The plan is read downstream, so the child count is not known here.
            "agent-task fanout run-plan local fanout",
            "available-parallelism".to_string(),
            "plan-defined".to_string(),
        ),
        _ => return None,
    };

    let mut warning = format!(
        "HOMEBOY_LOCAL_FANOUT_WARNING: {label} will execute on this controller with concurrency={concurrency}, tasks={task_count}, execution_location=local. Use --runner <runner-id> or --placement lab to prevent local provider fanout."
    );
    if let Some(readiness) = readiness {
        for reason in &readiness.reasons {
            warning.push_str(&format!(" lab_unavailable_reason={reason}"));
        }
        for remediation in &readiness.remediation_commands {
            warning.push_str(&format!(" remediation=`{remediation}`"));
        }
    }
    Some(warning)
}

fn inject_lab_changed_files(
    command: &Commands,
    normalized_args: &[String],
) -> homeboy::core::Result<Option<Vec<String>>> {
    let Commands::Review(args) = command else {
        return Ok(None);
    };
    let Some(component_args) = args.lab_changed_scope_component_args() else {
        return Ok(None);
    };
    if has_lab_changed_files_json(normalized_args) {
        return Ok(None);
    }

    let target = component::resolve_target(TargetSpec::new(
        component_args.component.as_deref(),
        component_args.path.as_deref(),
    ))?;
    let source_path = target.source_path.to_string_lossy();
    let changed_files = git::get_dirty_files(&source_path)?;
    let payload = serde_json::to_string(&changed_files).map_err(|error| {
        homeboy::core::Error::internal_unexpected(format!(
            "failed to encode Lab changed-file payload: {error}"
        ))
    })?;

    let mut rewritten = Vec::with_capacity(normalized_args.len() + 2);
    let insert_at = normalized_args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(normalized_args.len());
    rewritten.extend_from_slice(&normalized_args[..insert_at]);
    rewritten.push("--lab-changed-files-json".to_string());
    rewritten.push(payload);
    rewritten.extend_from_slice(&normalized_args[insert_at..]);
    Ok(Some(rewritten))
}

fn has_lab_changed_files_json(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--lab-changed-files-json" || arg.starts_with("--lab-changed-files-json=")
    })
}

/// Build the Lab dispatch observer for the parsed command. Only `trace`
/// participates in dispatch observation; every other command uses the no-op
/// observer. The core routing service owns the observation lifecycle; this
/// adapter only supplies the implementation.
fn lab_dispatch_observer(
    cli: &Cli,
    normalized_args: &[String],
    runner_id: Option<&str>,
) -> Box<dyn LabDispatchObserver> {
    match &cli.command {
        Commands::Trace(args) => {
            crate::commands::trace::start_lab_dispatch_observation(args, normalized_args, runner_id)
                .map(|observation| Box::new(observation) as Box<dyn LabDispatchObserver>)
                .unwrap_or_else(|| Box::new(NoopLabDispatchObserver))
        }
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command:
                crate::commands::agent_task::AgentTaskCommand::Fanout(
                    crate::commands::agent_task::AgentTaskFanoutArgs {
                        command:
                            crate::commands::agent_task::AgentTaskFanoutCommand::CookBatch(args),
                    },
                ),
        }) if cli.detach_after_handoff && args.run_plan => {
            start_agent_task_fanout_lab_dispatch_observation(args, normalized_args, runner_id)
                .map(|observation| Box::new(observation) as Box<dyn LabDispatchObserver>)
                .unwrap_or_else(|| Box::new(NoopLabDispatchObserver))
        }
        _ => Box::new(NoopLabDispatchObserver),
    }
}

fn placement_outcome_target<'a>(
    retry_run_id: Option<&'a str>,
    detached_run_id: Option<&'a str>,
) -> Option<ExecutionPlacementOutcomeTarget<'a>> {
    retry_run_id
        .or(detached_run_id)
        .map(|run_id| ExecutionPlacementOutcomeTarget::AgentTaskLifecycle { run_id })
}

struct AgentTaskFanoutLabDispatchObservation {
    store: ObservationStore,
    run_id: String,
    fanout_id: String,
}

impl LabDispatchObserver for AgentTaskFanoutLabDispatchObservation {
    fn run_id(&self) -> Option<&str> {
        Some(self.run_id.as_str())
    }

    fn finish(
        self: Box<Self>,
        status: RunStatus,
        metadata: serde_json::Value,
    ) -> Option<PersistedRunRetrieval> {
        let metadata =
            agent_task_fanout_finish_metadata(metadata, &self.run_id, &self.fanout_id, status);
        finish_run_best_effort(&self.store, &self.run_id, status, Some(metadata));
        Some(PersistedRunRetrieval::for_run(&self.run_id))
    }
}

fn agent_task_fanout_finish_metadata(
    mut metadata: serde_json::Value,
    run_id: &str,
    fanout_id: &str,
    status: RunStatus,
) -> serde_json::Value {
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "agent_task_lab_dispatch".to_string(),
            serde_json::json!({
                "schema": "homeboy/agent-task-fanout-lab-dispatch/v1",
                "fanout_id": fanout_id,
                "phase": "route_lab_dispatch",
                "status": status.as_str(),
            }),
        );
        object.insert(
            "fanout_id".to_string(),
            serde_json::Value::String(fanout_id.to_string()),
        );
        object.insert(
            "follow_commands".to_string(),
            serde_json::json!({
                "dispatch_status": format!("homeboy runs show {run_id}"),
                "dispatch_evidence": format!("homeboy runs evidence --run {run_id}"),
                "fanout_status": format!("homeboy agent-task fanout status {fanout_id}"),
            }),
        );
    }
    metadata
}

fn start_agent_task_fanout_lab_dispatch_observation(
    args: &crate::commands::agent_task::AgentTaskFanoutCookBatchArgs,
    normalized_args: &[String],
    runner_id: Option<&str>,
) -> Option<AgentTaskFanoutLabDispatchObservation> {
    let store = ObservationStore::open_initialized().ok()?;
    let cwd = std::env::current_dir().ok();
    // Planning owns generated batch identities. Reusing its exact result keeps
    // the observer's status command pointed at the record admission creates.
    let fanout_id = crate::commands::agent_task::fanout::cook_batch_fanout_id(args).ok()?;
    let run = store
        .start_run(
            NewRunRecord::builder("agent-task")
                .component_id(args.repo.clone())
                .command(normalized_args.join(" "))
                .optional_cwd_path(cwd.as_deref())
                .current_homeboy_version()
                .metadata(serde_json::json!({
                    "agent_task_lab_dispatch": {
                        "schema": "homeboy/agent-task-fanout-lab-dispatch/v1",
                        "fanout_id": fanout_id,
                        "phase": "route_before_lab_dispatch",
                        "status": "running",
                        "runner_id": runner_id,
                        "detach_after_handoff": true,
                        "run_plan": true,
                        "issue_count": args.issues.len(),
                    },
                    "runner_id": runner_id,
                    "fanout_id": fanout_id,
                    "follow_commands": {
                        "fanout_status": format!("homeboy agent-task fanout status {}", fanout_id),
                    },
                }))
                .build(),
        )
        .ok()?;
    eprintln!(
        "Lab offload handoff: local dispatch run `{}` is durable before remote preflight; inspect dispatch with `homeboy runs show {}`. Once the fanout batch is submitted, inspect it with `homeboy agent-task fanout status {}`.",
        run.id, run.id, fanout_id
    );
    Some(AgentTaskFanoutLabDispatchObservation {
        store,
        run_id: run.id,
        fanout_id,
    })
}

mod local_detach;
mod local_detach_fanout;
mod rig_source;
use rig_source::*;
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

fn is_command_local_runner_option(command: &Commands) -> bool {
    match command {
        Commands::Runs(args) if args.has_command_local_runner_option() => true,
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command: crate::commands::agent_task::AgentTaskCommand::Doctor(_),
        }) => true,
        // This command coordinates its own sync and runner executions. Routing
        // the coordinator itself would make its child executions re-enter Lab.
        Commands::Extension(args) => args.owns_runner_execution(),
        Commands::Fuzz(args) => args.consumes_runner_as_plan_input(),
        _ => false,
    }
}

fn write_offloaded_stdout(path: &str, stdout: &str) -> homeboy::core::Result<()> {
    write_output_file(path, stdout)
}

pub(crate) fn lab_offload_command(
    command: &Commands,
) -> homeboy::core::Result<Option<runners::LabOffloadCommand>> {
    let Some(route_contract) = command.lab_route_contract()? else {
        return Ok(None);
    };
    Ok(Some(lab_routing::lab_offload_command_from_route_contract(
        route_contract,
    )))
}

fn unscoped_provider_discovery_is_controller_local(cli: &Cli) -> bool {
    matches!(
        (&cli.command, cli.runner.as_deref(), cli.placement),
        (
            Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                command: crate::commands::agent_task::AgentTaskCommand::Providers(_),
            }),
            None,
            homeboy::cli_surface::Placement::Auto,
        )
    )
}

fn controller_owns_agent_task_lifecycle_command(cli: &Cli) -> homeboy::core::Result<bool> {
    use crate::commands::agent_task::AgentTaskCommand;

    let Commands::AgentTask(agent_task) = &cli.command else {
        return Ok(false);
    };
    let run_id = match &agent_task.command {
        AgentTaskCommand::Run(args)
            if agent_task_lifecycle::exact_record(&args.run_id)
                .ok()
                .is_some_and(|record| {
                    agent_task_lifecycle::is_unmaterialized_cook_admission(&record)
                }) =>
        {
            Some(&args.run_id)
        }
        AgentTaskCommand::Status(args) => Some(&args.run_id),
        AgentTaskCommand::Logs(args) => Some(&args.run_id),
        AgentTaskCommand::Evidence(args) => Some(&args.run_id),
        AgentTaskCommand::Diagnose(args) => Some(&args.run_id),
        AgentTaskCommand::Review(args) => Some(&args.run_id),
        AgentTaskCommand::Retry(args) => Some(&args.run_id),
        AgentTaskCommand::Reconcile(args) => Some(&args.run_id),
        _ => None,
    };
    let Some(run_id) = run_id else {
        return Ok(false);
    };
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    Some(agent_task_lifecycle::run_record_exists_resolved_in_store(
        &lifecycle_store,
        run_id,
    )?)
    .map(Ok)
    .transpose()
    .map(|present| present.unwrap_or(false))
}

fn lab_offload_command_for_materialized_args(
    args: &[String],
) -> homeboy::core::Result<Option<runners::LabOffloadCommand>> {
    let cli = Cli::try_parse_from(args).map_err(|error| {
        Error::validation_invalid_argument(
            "agent-task run",
            format!("build materialized Lab run-plan: {error}"),
            None,
            None,
        )
    })?;
    lab_offload_command(&cli.command)
}

fn destructive_fuzz_requires_lab(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Fuzz(args) if args.destructive_local_execution_requires_override()
    )
}

fn destructive_fuzz_local_execution_error() -> homeboy::core::Error {
    homeboy::core::Error::validation_invalid_argument(
        "allow_local_destructive_fuzz",
        "destructive fuzz refused local controller execution".to_string(),
        Some("--allow-destructive".to_string()),
        Some(vec![
            "Use --placement lab or pass --runner <runner-id> to run destructive fuzz on Lab.".to_string(),
            "Configure a default Lab runner so destructive fuzz offloads automatically.".to_string(),
            "If local execution is absolutely intentional, pass --allow-local-destructive-fuzz together with --allow-destructive.".to_string(),
        ]),
    )
}

fn lab_route_source_path_args(
    command: &Commands,
    normalized_args: &[String],
    capture_mutation_patch: bool,
) -> Option<Vec<String>> {
    if capture_mutation_patch || command_prefers_controller_source_path(command) {
        if let Some(rewritten) = rewrite_component_target_to_path(command, normalized_args) {
            return Some(rewritten);
        }
    }

    rewrite_ad_hoc_lab_workspace_to_path(command, normalized_args)
}

fn command_prefers_controller_source_path(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Review(crate::commands::review::ReviewArgs {
            command: Some(crate::commands::review::ReviewCommand::Lint(_)),
            ..
        })
    )
}

/// When a `lint --fix` / `refactor --write` command targets a component by id
/// (positionally or via `--component`/`--components`), resolve that component to
/// its on-disk source path and rewrite the offload args to `--path <source>`.
///
/// The Lab offload patch-capture pipeline (`lab_offload_source_path` →
/// workspace sync → before/after diff) keys entirely off the resolved source
/// path. A bare positional component id resolves to the controller working
/// directory for the sync/diff, but on the remote runner it re-resolves to the
/// runner's registered component checkout — so write fixes land outside the
/// captured workspace and no patch is produced (#4315). Rewriting to `--path`
/// makes the synced workspace, the remote command's working tree, and the
/// captured diff all reference the same directory.
///
/// Returns `None` (leaving args untouched) when there is nothing to rewrite:
/// no component target, an explicit `--path` is already present, the component
/// cannot be resolved, or the command is not a component-targeted lint/refactor.
fn rewrite_component_target_to_path(
    command: &Commands,
    normalized_args: &[String],
) -> Option<Vec<String>> {
    let (component_id, has_path_override) = match command {
        Commands::Refactor(args) if args.is_hot_resource_command() => (
            args.lab_offload_positional_component(),
            args.lab_offload_has_path_override(),
        ),
        Commands::Review(args) => match &args.command {
            Some(crate::commands::review::ReviewCommand::Lint(lint_args)) => (
                lint_args.lab_offload_positional_component(),
                lint_args.lab_offload_has_path_override(),
            ),
            _ => return None,
        },
        _ => return None,
    };

    if has_path_override {
        return None;
    }
    let component_id = component_id?;

    let source_path = resolve_component_source_path(&component_id)?;
    Some(strip_component_target_args(
        normalized_args,
        &component_id,
        &source_path,
    ))
}

/// Resolve a component id to its canonical on-disk source path. Returns `None`
/// when resolution fails so the caller can fall back to the original args and
/// let the normal offload path surface any downstream error.
fn resolve_component_source_path(component_id: &str) -> Option<String> {
    let target = component::resolve_target(TargetSpec::new(Some(component_id), None)).ok()?;
    Some(target.source_path.to_string_lossy().to_string())
}

/// Lab sync already materializes the controller CWD when no explicit source path
/// is supplied. For component commands, make that implicit source explicit so
/// the runner re-enters the command through `--path <runner-workspace>` and can
/// synthesize an ad-hoc component instead of requiring registry state there.
fn rewrite_ad_hoc_lab_workspace_to_path(
    command: &Commands,
    normalized_args: &[String],
) -> Option<Vec<String>> {
    let contract = command.lab_contract()?;
    let plan = lab_routing::lab_route_plan_from_contract(contract);
    if plan.source_materialization != CommandSourceMaterialization::ControllerCwdAsPathArg {
        return None;
    }

    let needs_path = matches!(
        command,
        Commands::Review(args)
            if args.nested_component_args().is_some_and(|component| {
                component.component.is_none() && component.path.is_none()
            })
    );
    if !needs_path {
        return None;
    }

    let source_path = std::env::current_dir().ok()?;
    Some(insert_path_arg_before_passthrough(
        normalized_args,
        &source_path.to_string_lossy(),
    ))
}

fn insert_path_arg_before_passthrough(
    normalized_args: &[String],
    source_path: &str,
) -> Vec<String> {
    let mut rewritten = Vec::with_capacity(normalized_args.len() + 2);
    let mut inserted = false;
    for arg in normalized_args {
        if !inserted && arg == "--" {
            rewritten.push("--path".to_string());
            rewritten.push(source_path.to_string());
            inserted = true;
        }
        rewritten.push(arg.clone());
    }
    if !inserted {
        rewritten.push("--path".to_string());
        rewritten.push(source_path.to_string());
    }
    rewritten
}

/// Drop component-targeting args (the bare positional id and any
/// `-c`/`--component`/`--components` flags) and append `--path <source_path>`.
fn strip_component_target_args(
    normalized_args: &[String],
    component_id: &str,
    source_path: &str,
) -> Vec<String> {
    let mut rewritten = Vec::with_capacity(normalized_args.len() + 1);
    let mut iter = normalized_args.iter().peekable();
    let mut passthrough = false;
    let mut positional_stripped = false;
    while let Some(arg) = iter.next() {
        if rewritten.is_empty() {
            rewritten.push(arg.clone());
            continue;
        }
        if passthrough {
            rewritten.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            rewritten.push(arg.clone());
            continue;
        }
        // Flagged component selectors that consume a following value.
        if arg == "-c" || arg == "--component" || arg == "--components" {
            let _ = iter.next();
            continue;
        }
        // Inline `--component=<id>` / `--components=<list>` / `-c<id>` forms.
        if arg.starts_with("--component=")
            || arg.starts_with("--components=")
            || (arg.starts_with("-c") && arg.len() > 2 && !arg.starts_with("--"))
        {
            continue;
        }
        // The bare positional component token (strip only the first match so an
        // unrelated later argument that happens to equal the id is preserved).
        if !positional_stripped && !arg.starts_with('-') && arg == component_id {
            positional_stripped = true;
            continue;
        }
        rewritten.push(arg.clone());
    }
    rewritten.push("--path".to_string());
    rewritten.push(source_path.to_string());
    rewritten
}

#[cfg(test)]
mod tests;
