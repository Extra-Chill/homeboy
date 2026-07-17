use clap::Parser;
use homeboy::cli_surface::{Cli, Commands};
use homeboy::core::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::core::command_execution_plan::CommandSourceMaterialization;
use homeboy::core::component::{self, TargetSpec};
use homeboy::core::git;
use homeboy::core::lab_routing::{
    self, LabDispatchObserver, LabRouteOutcome, LabRoutingRequest, NoopLabDispatchObserver,
    PersistedRunRetrieval,
};
use homeboy::core::observation::{
    finish_run_best_effort, NewRunRecord, ObservationStore, RunStatus,
};
use homeboy::core::redaction::RedactionPolicy;
use homeboy::core::runners::{self, RunnerExecOptions};
use homeboy::core::Error;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::agent_task_service::DerivedCookBaselineCapability;
use crate::core::io::output_file::write_output_file;

pub fn route_after_parse(
    cli: &Cli,
    normalized_args: &[String],
    output_file: Option<&str>,
) -> homeboy::core::Result<Option<i32>> {
    // A managed runner executes the controller-selected command once. Its argv
    // retains the controller's explicit placement for provenance, but must not
    // recursively route back through a runner-side controller daemon.
    let managed_runner_placement =
        crate::commands::utils::resource_policy::is_managed_runner_placement_context();
    if lab_routing::is_lab_offload_subprocess() || managed_runner_placement {
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

    if let (Some(runner_id), Commands::Rig(args)) = (cli.runner.as_deref(), &cli.command) {
        if let Some(rig_id) = args.up_dry_run_rig_id() {
            let (output, exit_code) = crate::commands::rig::up_runner_exec_plan(rig_id, runner_id)?;
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
                homeboy::core::rig::install(source, id, all)?;
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

    let mut lab_command = lab_offload_command(&cli.command)?;

    let inferred_runner_id = if lab_command.is_some() {
        cli.runner
            .clone()
            .or_else(|| runners::resolve_default_lab_runner().ok().flatten())
    } else {
        None
    };

    if let Some(exit_code) = run_split_placement_cook(
        cli,
        normalized_args,
        output_file,
        inferred_runner_id.as_deref(),
    )? {
        return Ok(Some(exit_code));
    }

    if let Some(exit_code) =
        run_split_placement_fanout(cli, output_file, inferred_runner_id.as_deref())?
    {
        return Ok(Some(exit_code));
    }

    let run_handoff = if lab_command.is_some() && inferred_runner_id.is_some() {
        materialize_agent_task_run_handoff(cli, normalized_args)?
    } else {
        None
    };
    let retry_handoff = if lab_command.is_some() && inferred_runner_id.is_some() {
        materialize_agent_task_retry_handoff(cli, normalized_args)?
    } else {
        None
    };
    if retry_handoff.is_some() {
        if let Some(command) = lab_command.as_mut() {
            // A retry's task primary must be a real checkout so the provider can
            // capture a bounded git diff instead of receiving a source snapshot.
            command.command.workspace_mode_policy =
                homeboy::command_contract::LabWorkspaceModePolicy::GitCheckoutRequired;
        }
    }
    let normalized_args = run_handoff
        .as_ref()
        .map(|handoff| handoff.args.as_slice())
        .or_else(|| {
            retry_handoff
                .as_ref()
                .map(|handoff| handoff.args.as_slice())
        })
        .unwrap_or(normalized_args);
    let cook_plan = if lab_command.is_some() && inferred_runner_id.is_some() {
        materialize_agent_task_cook_plan(cli)?
    } else {
        None
    };
    let normalized_args = inject_agent_task_cook_attempt_plan(normalized_args, cook_plan.as_ref())?;
    let durable_agent_task_plan = run_handoff
        .as_ref()
        .map(|handoff| &handoff.plan)
        .or_else(|| {
            retry_handoff
                .as_ref()
                .map(|handoff| &handoff.plan)
                .or(cook_plan.as_ref())
        });
    let observer = lab_dispatch_observer(cli, &normalized_args, inferred_runner_id.as_deref());
    let active_run_id = observer
        .run_id()
        .map(str::to_string)
        .or_else(|| retry_handoff.as_ref().map(|handoff| handoff.run_id.clone()));

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
    let job_overrides = lab_job_overrides(cli)?;

    let outcome = lab_routing::dispatch_lab_offload(
        LabRoutingRequest {
            command: lab_command,
            normalized_args: routed_args,
            explicit_runner: cli.runner.as_deref(),
            placement: cli.placement,
            allow_local_fallback: cli.placement.allows_local_fallback(),
            allow_dirty_lab_workspace: cli.allow_dirty_lab_workspace,
            skip_deps_hydration: cli.skip_deps_hydration,
            capture_patch: capture_mutation_patch,
            mutation_flag,
            timeout: lab_route_dispatch_timeout(&cli.command),
            active_run_id: active_run_id.as_deref(),
            detach_after_handoff: cli.detach_after_handoff,
            output_file_requested: output_file.is_some(),
            read_only_polling: cli
                .command
                .lab_route_contract()?
                .is_some_and(|contract| contract.command.routing_policy.read_only_polling),
            local_output_file: output_file,
            durable_agent_task_plan,
            // A serialized run-plan has no workspace CLI argument. Carry its
            // canonical plan root through the portable source channel so Lab
            // snapshots it before remapping nested plan/config paths.
            source_path: run_handoff
                .as_ref()
                .map(|handoff| handoff.primary_workspace.as_path())
                .or_else(|| {
                    retry_handoff
                        .as_ref()
                        .map(|handoff| handoff.primary_workspace.as_path())
                }),
            verified_cook_baseline: None,
            require_controller_git_bundle: retry_handoff.is_some(),
            job_overrides,
        },
        inferred_runner_id.as_deref(),
        observer,
    )
    .map_err(|error| match retry_handoff.as_ref() {
        Some(handoff) => persist_retry_handoff_preacceptance_failure(handoff, error),
        None => error,
    })?;

    match outcome {
        LabRouteOutcome::RunLocal => {
            if destructive_fuzz_requires_lab(&cli.command) {
                return Err(destructive_fuzz_local_execution_error());
            }
            if let Some(warning) = agent_task_local_fanout_warning(&cli.command) {
                eprintln!("{warning}");
            }
            Ok(None)
        }
        LabRouteOutcome::InFlight(output) | LabRouteOutcome::Offloaded(output) => {
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

/// Fanout keeps durable batch state, worktree ownership, artifact ingestion,
/// gates, and finalization on the controller. Each child provider attempt is
/// the only unit handed to the explicitly selected Lab runner.
fn run_split_placement_fanout(
    cli: &Cli,
    output_file: Option<&str>,
    runner_id: Option<&str>,
) -> homeboy::core::Result<Option<i32>> {
    if cli.placement == homeboy::cli_surface::Placement::Local {
        return Ok(None);
    }
    let Some(runner_id) = runner_id else {
        return Ok(None);
    };
    let dispatcher = LabCookAttemptDispatcher {
        runner_id: runner_id.to_string(),
        allow_local_fallback: false,
        allow_dirty_lab_workspace: cli.allow_dirty_lab_workspace,
        skip_deps_hydration: cli.skip_deps_hydration,
        detach_after_handoff: cli.detach_after_handoff,
        source_path: None,
        job_overrides: lab_job_overrides(cli)?,
    };
    let attempt_dispatcher =
        move |options: &crate::core::agent_task_service::AgentTaskCookServiceOptions| {
            let mut dispatcher = dispatcher.clone();
            dispatcher.source_path = options
                .initial_plan
                .tasks
                .first()
                .and_then(|task| task.workspace.root.as_ref())
                .map(PathBuf::from);
            Arc::new(dispatcher)
                as Arc<dyn crate::core::agent_task_service::AgentTaskCookAttemptDispatcher>
        };
    let (value, exit_code) = match &cli.command {
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command:
                crate::commands::agent_task::AgentTaskCommand::Fanout(
                    crate::commands::agent_task::AgentTaskFanoutArgs {
                        command: crate::commands::agent_task::AgentTaskFanoutCommand::RunPlan(args),
                    },
                ),
        }) => crate::commands::agent_task::fanout::run_batch_cook_fanout_with_attempt_dispatcher(
            args.clone(),
            &attempt_dispatcher,
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
            crate::commands::agent_task::fanout::cook_batch_with_attempt_dispatcher(
                args.clone(),
                &attempt_dispatcher,
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

/// Cook owns controller-local target resolution, promotion, gates, retries, and
/// finalization. Its provider attempt is the only portable unit: a materialized
/// typed run-plan that mirrors its aggregate and artifacts back into the same
/// durable attempt record before this controller resumes.
fn run_split_placement_cook(
    cli: &Cli,
    _normalized_args: &[String],
    output_file: Option<&str>,
    runner_id: Option<&str>,
) -> homeboy::core::Result<Option<i32>> {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
    }) = &cli.command
    else {
        return Ok(None);
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
    if cli.placement == homeboy::cli_surface::Placement::Local {
        if cli.runner.is_some() {
            return Err(Error::validation_invalid_argument(
                "runner",
                "--placement local cannot be combined with --runner for agent-task cook; omit --runner for a fully local cook or select Lab placement for its provider attempt",
                cli.runner.clone(),
                None,
            ));
        }
        return Ok(None);
    }
    let Some(runner_id) = runner_id else {
        return Ok(None);
    };

    let plan = materialize_agent_task_cook_plan(cli)?.expect("cook plan");
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
    let mut controller = cook.clone();
    controller.dispatch.run_id = Some(cook_id);
    controller.attempt_run_id = Some(attempt_run_id);
    controller.attempt_plan = Some(serialized_plan);
    let dispatcher = Arc::new(LabCookAttemptDispatcher {
        runner_id: runner_id.to_string(),
        allow_local_fallback: cli.placement.allows_local_fallback(),
        allow_dirty_lab_workspace: cli.allow_dirty_lab_workspace,
        skip_deps_hydration: cli.skip_deps_hydration,
        detach_after_handoff: cli.detach_after_handoff,
        source_path,
        job_overrides: lab_job_overrides(cli)?,
    });
    let (value, exit_code) =
        crate::commands::agent_task::run::run_cook_with_executor_and_dispatcher(
            controller,
            homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor::discover(),
            Some(dispatcher),
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
    allow_local_fallback: bool,
    allow_dirty_lab_workspace: bool,
    skip_deps_hydration: bool,
    detach_after_handoff: bool,
    source_path: Option<PathBuf>,
    job_overrides: runners::LabJobOverrides,
}

pub(crate) fn reconstruct_cook_attempt_dispatcher(
    recipe: &serde_json::Value,
) -> homeboy::core::Result<
    Option<Arc<dyn crate::core::agent_task_service::AgentTaskCookAttemptDispatcher>>,
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
                .unwrap_or_else(|| serde_json::json!(false)),
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

impl crate::core::agent_task_service::AgentTaskCookAttemptDispatcher for LabCookAttemptDispatcher {
    fn durable_recipe(&self) -> homeboy::core::Result<serde_json::Value> {
        Ok(serde_json::json!({
            "kind": "lab",
            "runner_id": self.runner_id,
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

    fn dispatch_attempt(
        &self,
        plan: homeboy::core::agent_tasks::scheduler::AgentTaskPlan,
        run_id: &str,
        derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> homeboy::core::Result<()> {
        // The capability has already bound the promoted artifact and exact
        // baseline to this retry; only its evidence crosses the Lab boundary.
        let verified_cook_baseline =
            derived_cook_baseline.map(DerivedCookBaselineCapability::verified_baseline_provenance);
        let serialized_plan = serde_json::to_string(&plan).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize Lab cook attempt plan".to_string()),
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
        let outcome = lab_routing::dispatch_lab_offload(
            LabRoutingRequest {
                command: lab_offload_command(&provider_cli.command)?,
                normalized_args: &provider_args,
                explicit_runner: Some(&self.runner_id),
                placement: homeboy::cli_surface::Placement::Lab,
                allow_local_fallback: self.allow_local_fallback,
                allow_dirty_lab_workspace: self.allow_dirty_lab_workspace,
                skip_deps_hydration: self.skip_deps_hydration,
                capture_patch: false,
                mutation_flag: None,
                timeout: None,
                active_run_id: Some(run_id),
                detach_after_handoff: self.detach_after_handoff,
                output_file_requested: false,
                read_only_polling: false,
                require_controller_git_bundle: false,
                local_output_file: None,
                durable_agent_task_plan: Some(&plan),
                // A retry's baseline is controller-owned capability, not plan
                // data. Stage that exact clean checkout; never substitute the
                // controller's original workspace during nested Lab dispatch.
                source_path: cook_attempt_source_path(
                    derived_cook_baseline,
                    self.source_path.as_deref(),
                ),
                verified_cook_baseline: verified_cook_baseline.as_ref(),
                job_overrides: self.job_overrides.clone(),
            },
            Some(&self.runner_id),
            Box::new(NoopLabDispatchObserver),
        )
        .map_err(|error| {
            let recovery =
                format!("Resolve the Lab handoff, then retry controller-owned attempt `{run_id}`.");
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
            }
        })?;
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
    plan: homeboy::core::agent_tasks::scheduler::AgentTaskPlan,
    run_id: &str,
    runner_id: &str,
) -> homeboy::core::Result<serde_json::Value> {
    let source_path = plan
        .tasks
        .first()
        .and_then(|task| task.workspace.root.as_ref())
        .map(PathBuf::from);
    let dispatcher = LabCookAttemptDispatcher {
        runner_id: runner_id.to_string(),
        allow_local_fallback: false,
        allow_dirty_lab_workspace: false,
        skip_deps_hydration: false,
        // Controller actions yield after the daemon accepts the child. The
        // persisted run is the reconnect and terminal-event replay boundary.
        detach_after_handoff: true,
        source_path,
        job_overrides: runners::LabJobOverrides::default(),
    };
    <LabCookAttemptDispatcher as crate::core::agent_task_service::AgentTaskCookAttemptDispatcher>::prepare_for_cook(&dispatcher)?;
    <LabCookAttemptDispatcher as crate::core::agent_task_service::AgentTaskCookAttemptDispatcher>::dispatch_attempt(
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
    plan: Option<&homeboy::core::agent_tasks::scheduler::AgentTaskPlan>,
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
) -> homeboy::core::Result<Option<homeboy::core::agent_tasks::scheduler::AgentTaskPlan>> {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
    }) = &cli.command
    else {
        return Ok(None);
    };
    let mut dispatch = cook.dispatch.clone();
    if dispatch.prompt.is_none() {
        dispatch.prompt = cook.goal.clone();
    }
    if dispatch.cwd.is_none() && dispatch.workspace.is_none() {
        dispatch.workspace = Some(cook.to_worktree.clone());
    }
    let mut request =
        homeboy::core::agent_tasks::dispatch_service::resolve_dispatch_request(dispatch.into())?;
    homeboy::core::agent_tasks::dispatch_service::build_controller_dispatch_plan(&mut request)
        .map(Some)
}

fn lab_route_dispatch_timeout(command: &Commands) -> Option<std::time::Duration> {
    if matches!(command, Commands::Trace(_)) {
        return Some(lab_routing::lab_trace_dispatch_timeout());
    }
    None
}

struct AgentTaskRetryHandoff {
    args: Vec<String>,
    run_id: String,
    plan: homeboy::core::agent_tasks::scheduler::AgentTaskPlan,
    primary_workspace: PathBuf,
}

#[derive(Debug)]
struct AgentTaskRunHandoff {
    args: Vec<String>,
    plan: homeboy::core::agent_tasks::scheduler::AgentTaskPlan,
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

fn plan_primary_workspace(
    plan: &homeboy::core::agent_tasks::scheduler::AgentTaskPlan,
) -> homeboy::core::Result<PathBuf> {
    let mut roots = BTreeSet::new();
    for task in &plan.tasks {
        let root = task
            .workspace
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
            });
        if let Some(root) = root.filter(|root| !root.trim().is_empty()) {
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
    if !agent_task_lifecycle::run_record_exists(&retry.run_id)? {
        return Ok(None);
    }

    let source_plan = agent_task_lifecycle::load_controller_plan(&retry.run_id)?;
    let primary_workspace = retry_plan_primary_workspace(&source_plan)?;
    let record = agent_task_lifecycle::retry(&retry.run_id, retry.new_run_id.as_deref())?;
    let plan = agent_task_lifecycle::load_plan(&record.run_id)?;
    let retry_workspace = retry_plan_primary_workspace(&plan).map_err(|error| {
        Error::validation_invalid_argument(
            "workspace",
            "agent-task retry lost its git-backed task workspace while serializing the replacement plan",
            Some(primary_workspace.display().to_string()),
            Some(vec![
                format!(
                    "The original persisted plan uses {}; inspect the replacement plan for workspace serialization loss.",
                    primary_workspace.display()
                ),
                error.message,
            ]),
        )
    })?;
    if retry_workspace != primary_workspace {
        return Err(Error::validation_invalid_argument(
            "workspace",
            "agent-task retry changed its git-backed task workspace while serializing the replacement plan",
            Some(format!(
                "original: {}; replacement: {}",
                primary_workspace.display(),
                retry_workspace.display()
            )),
            Some(vec![
                "Retry preserves the original task workspace; inspect the replacement plan serialization before retrying through Lab.".to_string(),
            ]),
        ));
    }
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
    }))
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

fn retry_plan_primary_workspace(
    plan: &homeboy::core::agent_tasks::scheduler::AgentTaskPlan,
) -> homeboy::core::Result<PathBuf> {
    let mut roots = BTreeSet::new();
    for task in &plan.tasks {
        let root = task
            .workspace
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
            });
        if let Some(root) = root.filter(|root| !root.trim().is_empty()) {
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
                        "Retry the task from a plan with a git-backed workspace root.".to_string(),
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
    error: Error,
) -> Error {
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

/// Insert one env pair into the overrides, recording the key as secret when
/// the redaction policy considers the key sensitive or the value redacted.
fn insert_lab_env_override(
    overrides: &mut runners::LabJobOverrides,
    policy: &RedactionPolicy,
    name: String,
    value: String,
) {
    if policy.is_sensitive_key(&name) || policy.redact_string(&value) != value {
        overrides.secret_env_names.push(name.clone());
    }
    overrides.env.insert(name, value);
}

fn lab_job_overrides(cli: &Cli) -> homeboy::core::Result<runners::LabJobOverrides> {
    let mut overrides = runners::LabJobOverrides::default();
    let policy = RedactionPolicy::default();

    for raw in &cli.runner_env {
        let (name, value) = parse_lab_env_pair("runner-env", raw)?;
        insert_lab_env_override(&mut overrides, &policy, name, value);
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
            insert_lab_env_override(&mut overrides, &policy, name, value);
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

fn agent_task_local_fanout_warning(command: &Commands) -> Option<String> {
    let (label, concurrency, task_count) = match command {
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command: crate::commands::agent_task::AgentTaskCommand::Cook(args),
        }) => (
            "agent-task cook local fanout",
            args.dispatch.concurrency,
            args.dispatch.tasks.len()
                + usize::from(args.dispatch.prompt.is_some())
                + usize::from(args.dispatch.core.tasks_json.is_some()),
        ),
        _ => return None,
    };

    (concurrency > 1 || task_count > 1).then(|| {
        format!(
            "HOMEBOY_LOCAL_FANOUT_WARNING: {label} will execute on this controller with concurrency={concurrency}, tasks={task_count}, execution_location=local. Use --runner <runner-id> or --placement lab to prevent local provider fanout."
        )
    })
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
    let fanout_id = agent_task_fanout_cook_batch_dispatch_id(args);
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

fn agent_task_fanout_cook_batch_dispatch_id(
    args: &crate::commands::agent_task::AgentTaskFanoutCookBatchArgs,
) -> String {
    args.fanout_id.clone().unwrap_or_else(|| {
        let first = args
            .issues
            .first()
            .and_then(|issue| issue.split_once("/issues/").map(|(_, number)| number))
            .and_then(|number| number.split(|c| matches!(c, '/' | '?' | '#')).next())
            .filter(|value| !value.is_empty())
            .unwrap_or("batch");
        format!(
            "cook-batch-{}-issue-{}-{}",
            args.repo,
            first,
            args.issues.len()
        )
    })
}

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

fn lab_offload_command(
    command: &Commands,
) -> homeboy::core::Result<Option<runners::LabOffloadCommand>> {
    let Some(route_contract) = command.lab_route_contract()? else {
        return Ok(None);
    };
    Ok(Some(lab_routing::lab_offload_command_from_route_contract(
        route_contract,
    )))
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
