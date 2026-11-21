//! Read-only Cook preview and replay projections.

use super::run::*;

pub(crate) fn preview_cook(
    mut args: AgentTaskCookArgs,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> CmdResult<Value> {
    let mut progress = Vec::new();
    record_preview_phase(&mut progress, "prompt_input");
    default_cook_prompt_from_task_url(&mut args);
    if args
        .dispatch
        .prompt
        .as_deref()
        .is_some_and(|prompt| prompt == "-")
    {
        snapshot_cook_prompt_bounded(&mut args, PREVIEW_STDIN_TIMEOUT)?;
    } else if args.dispatch.prompt.as_deref().is_some_and(|prompt| {
        prompt.starts_with('@')
            && homeboy::agents::agent_task_prompts::stored_prompt_ref_id(prompt).is_none()
    }) {
        args.dispatch.prompt = Some(
            homeboy::agents::agent_task_prompts::read_prompt_input_bounded(
                args.dispatch.prompt.as_deref().expect("prompt source"),
                PREVIEW_STDIN_TIMEOUT,
            )?,
        );
    }
    record_preview_phase(&mut progress, "input_validation");
    let retained_gate_contract = args.gates.has_retained_contract();
    args.gates.snapshot_file_inputs()?;
    args.repro.snapshot_file_inputs()?;
    // Authorization is a security boundary, not an unrelated task-input
    // validation, so preserve its precedence over backend guidance.
    validate_cook_source_authority(&args, provenance)?;
    bind_cook_preview_lifecycle(&mut args);
    let repository = prepare_cook_request(&mut args, provenance)?;
    let preview_request =
        dispatch_service::resolve_dispatch_request(resolved_dispatch_args_for_cook(&args)?.into())?;
    dispatch_service::require_model_override_acknowledgement(&preview_request)?;
    record_preview_phase(&mut progress, "destination_resolution");
    let (mut args, mut provision) =
        with_preview_heartbeat(&mut progress, "destination_resolution", || {
            let args = resolve_cook_destination_with_component_lint_gate(
                args,
                repository.as_ref(),
                retained_gate_contract,
            )?;
            let provision = provision_cook_destination(&args, repository.as_ref(), true)?;
            Ok::<_, homeboy::core::Error>((args, provision))
        })?;
    project_preview_dirty_admission(&mut provision);
    let capacity_path = provision.get("path").and_then(Value::as_str).or_else(|| {
        args.repository_identity
            .as_ref()
            .and_then(|identity| identity.get("repository_path"))
            .and_then(Value::as_str)
    });
    let capacity = capacity_path.map(|path| {
        homeboy::core::cleanup::reconstructable_artifact_capacity_preview(Path::new(path))
    });
    let staging_component = preview_staging_component(&args, &provision)?;
    let base_preparation = if let (Some(path), Some(base)) = (
        provision
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| Path::new(path).is_dir()),
        args.base.as_deref(),
    ) {
        record_preview_phase(&mut progress, "base_preparation");
        let preparation = with_preview_heartbeat(&mut progress, "base_preparation", || {
            homeboy::agents::agent_task_service::prepare_cook_workspace_base(
                Path::new(path),
                base,
                args.dispatch.task_url.as_deref(),
            )
        })?;
        args.prepared_base_sha = preparation.base_sha.clone();
        serde_json::to_value(preparation)
            .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?
    } else {
        serde_json::json!({
            "schema": "homeboy/cook-base-preparation/v1",
            "declared_base": args.base,
            "base_sha": null,
            "provenance": "workspace_unmaterialized",
            "remote_freshness": "deferred",
            "topology": null,
        })
    };
    provision["base_preparation"] = base_preparation;
    let replay = cook_preview_replay_argv(&args);
    if provision["details"]["worktree_provider_lookup"] == "timed_out" {
        provision["recovery_argv"] = serde_json::json!(replay.argv.clone());
    }
    record_preview_phase(&mut progress, "placement_projection");
    let placement = preview_placement_policy_with_admission(&replay.argv);
    let notification_resolution = homeboy::core::notification_route::current_resolution();
    reject_gates_unexecutable_under_resolved_placement(
        args.gates
            .verify
            .iter()
            .chain(&args.gates.private_verify)
            .cloned(),
    )?;
    if matches!(
        provision["action"].as_str(),
        Some("materialization_required" | "unresolved_provider")
    ) {
        let failure = provision.get("failure").cloned();
        let exit_code = if provision["action"] == "unresolved_provider" {
            1
        } else {
            0
        };
        let mut resolved = cook_preview_resolved_request(&args, placement);
        resolved["base_preparation"] = provision["base_preparation"].clone();
        resolved["workspace"] = provision;
        apply_preview_worktree_capacity(
            &mut resolved,
            capacity.as_ref(),
            args.repository_identity.as_ref(),
        );
        attach_preview_staging_component(&mut resolved, staging_component.as_ref());
        return Ok((
            cook_preview_result(resolved, progress, replay, None, failure),
            exit_code,
        ));
    }
    let gate_workspace = args.dispatch.cwd.as_deref().map(Path::new).or_else(|| {
        args.to_worktree
            .as_deref()
            .map(Path::new)
            .filter(|path| path.is_dir())
    });
    record_preview_phase(&mut progress, "gate_contract_validation");
    let mut gate_contract_validation = validate_gate_contracts(
        args.gates
            .verify
            .iter()
            .chain(&args.gates.private_verify)
            .cloned(),
        gate_workspace,
        &crate::cli_runtime::current_augmented_command_contract(),
    )?;
    gate_contract_validation
        .gates
        .extend(validate_cargo_gate_contracts(
            args.gates
                .verify
                .iter()
                .chain(&args.gates.private_verify)
                .cloned(),
            gate_workspace,
        )?);
    record_preview_phase(&mut progress, "provider_preflight");

    // Preview binds evidence to the same resolved workspace, but only projects
    // its read-only paths. Cook alone performs the later secure copy.
    let mut compile_args = args.clone();
    let (evidence, evidence_provenance) = if !args.provider_evidence_inputs.is_empty() {
        record_preview_phase(&mut progress, "provider_evidence_projection");
        let workspace = provision["path"].as_str().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence cannot be projected until Cook has a resolved workspace path",
                None,
                None,
            )
        })?;
        let dispatch = resolved_dispatch_args_for_cook(&args)?;
        let admitted_evidence = admit_provider_evidence_inputs(&args.provider_evidence_inputs)?;
        compile_args.dispatch.prompt = dispatch.prompt;
        let evidence =
            projected_provider_evidence(&args.provider_evidence_inputs, Some(workspace))?;
        let projected_paths = projected_provider_evidence_paths(&evidence);
        rewrite_provider_evidence_prompt(
            &mut compile_args.dispatch.prompt,
            &args.provider_evidence_inputs,
            &admitted_evidence,
            &evidence,
            &projected_paths,
        )?;
        (
            Some(evidence),
            Some(provider_evidence_controller_provenance_from_admitted(
                &args.provider_evidence_inputs,
                &admitted_evidence,
            )),
        )
    } else {
        (None, None)
    };
    record_preview_phase(&mut progress, "plan_compilation");
    let mut plan = compile_cook_plan(&compile_args, provision.clone(), evidence.as_deref())?;
    if let Some(evidence) = evidence {
        for task in &mut plan.tasks {
            if !task.executor.config.is_object() {
                task.executor.config = serde_json::json!({});
            }
            task.executor.config["evidence_inputs"] = serde_json::to_value(&evidence)
                .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
        }
        plan.metadata["controller_provider_evidence"] =
            serde_json::to_value(evidence_provenance)
                .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
    }
    // Preview reports the route provider readiness binds, never rotation entry
    // 0 claimed as fact (#15854). Lab placement returns the plan unchanged:
    // the runner binds with its own credentials, and the projection says so.
    let catalog = homeboy::agents::agent_task_provider::AgentTaskProviderCatalog::discover();
    let configured_plan = plan;
    let mut plan = validate_cook_provider_execution_plan(&configured_plan, &catalog)?;
    resolve_cook_execution_budget(&args, &mut plan)?;
    let binding_runner_owned = placement["selected"].as_str() == Some("lab");
    if let Some(disclosure) = cook_provider_readiness_disclosure(&plan, binding_runner_owned) {
        eprintln!("{}", disclosure);
    }
    eprintln!("{}", cook_effective_plan_disclosure(&plan));
    plan.metadata["gate_contract_validation"] = serde_json::to_value(&gate_contract_validation)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
    preflight_preview_lab_provider_secret_env(&placement, &plan)?;

    let executor = plan
        .tasks
        .first()
        .map(|task| serde_json::to_value(&task.executor).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    let mut resolved = cook_preview_resolved_request(&args, placement);
    resolved["base_preparation"] = provision["base_preparation"].clone();
    resolved["workspace"] = provision;
    apply_preview_worktree_capacity(
        &mut resolved,
        capacity.as_ref(),
        args.repository_identity.as_ref(),
    );
    resolved["gate_inputs"] = serde_json::to_value(&args.gates.gate_inputs)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
    resolved["gate_contract_validation"] =
        preview_gate_contract_disclosure(&gate_contract_validation, &args.gates.verify);
    resolved["provider"] = executor;
    resolved["provider_readiness"] =
        cook_provider_readiness_projection(&configured_plan, &plan, binding_runner_owned);
    resolved["retry_budget"] = plan.metadata["cook_retry_policy"].clone();
    resolved["notification_resolution"] = serde_json::to_value(notification_resolution)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
    attach_preview_staging_component(&mut resolved, staging_component.as_ref());
    Ok((
        cook_preview_result(resolved, progress, replay, None, None),
        0,
    ))
}

pub(super) fn preflight_cook_gates(args: &AgentTaskCookArgs) -> homeboy::core::Result<()> {
    let mut gates: homeboy::agents::agent_tasks::gate::VerifyGateOptions =
        args.gates.clone().into();
    let workspace = cook_gate_preflight_workspace(args)?;
    preflight_resolved_cook_gates(&mut gates, &workspace)
}

pub(super) fn cook_gate_preflight_workspace(
    args: &AgentTaskCookArgs,
) -> homeboy::core::Result<PathBuf> {
    // Configured components deliberately omit repository_path from their
    // durable identity. Resolve their checkout instead of snapshotting the
    // controller's unrelated invocation directory during Lab preflight.
    let workspace = args
        .dispatch
        .cwd
        .as_deref()
        .map(Path::new)
        .or_else(|| {
            args.repository_identity
                .as_ref()
                .and_then(|identity| identity["repository_path"].as_str())
                .map(Path::new)
        })
        .map(Path::to_path_buf);
    let workspace = match workspace {
        Some(workspace) => Some(workspace),
        None => cook_component_id(args)
            .map(cook_component_path_for_repository_name)
            .transpose()?
            .flatten(),
    };
    match workspace {
        Some(workspace) => Ok(workspace),
        None => std::env::current_dir()
            .map_err(|error| homeboy::core::Error::internal_io(error.to_string(), None)),
    }
}

pub(super) fn preflight_resolved_cook_gates(
    gates: &mut homeboy::agents::agent_tasks::gate::VerifyGateOptions,
    workspace: &Path,
) -> homeboy::core::Result<()> {
    if let Some(result) = homeboy::core::parsed_command_preflight::captured_result() {
        if result.placement.selected == homeboy_lab_contract::EffectiveExecutionPlacement::Lab {
            gates.gate_environment.lab_runner = result
                .placement
                .runner
                .map(|runner| runner.runner_id)
                .or(gates.gate_environment.lab_runner.clone());
        }
    }
    gates.preflight(workspace)
}

pub(super) fn preflight_preview_lab_provider_secret_env(
    placement: &Value,
    plan: &homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
) -> homeboy::core::Result<()> {
    if placement.get("selected").and_then(Value::as_str) != Some("lab") {
        return Ok(());
    }
    let Some(runner_id) = placement.get("selected_runner").and_then(Value::as_str) else {
        return Ok(());
    };
    let required = homeboy::runner::agent_task_runner_provider_secret_env_names(plan);
    if required.is_empty() {
        return Ok(());
    }
    let inventory = homeboy::runner::runner_secret_identity_inventory(runner_id)?;
    let missing = required
        .into_iter()
        .filter(|name| !inventory.identities.contains(name))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(homeboy::core::Error::validation_invalid_argument(
        "secret-env",
        format!(
            "selected Lab runner `{runner_id}` is missing required secret identity reference(s): {}",
            missing.join(", ")
        ),
        Some(runner_id.to_string()),
        Some(vec![
            "Configure these names in the selected runner's secret_env map before Cook dispatch.".to_string(),
            format!("Inspect redacted references with `homeboy runner env {runner_id}`."),
        ]),
    ))
}

pub(super) fn preview_staging_component(
    args: &AgentTaskCookArgs,
    provision: &Value,
) -> homeboy::core::Result<Option<Value>> {
    let component_id = homeboy::agents::agent_task_service::admitted_component_id(
        args.repository_identity.as_ref(),
    );
    let Some(path) = provision
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| Path::new(path).is_dir())
    else {
        return Ok(component_id.map(|component_id| {
            serde_json::json!({
                "component_id": component_id,
                "resolved": false,
            })
        }));
    };
    let component =
        homeboy::core::deps::resolve_checkout_component(component_id.as_deref(), Path::new(path))?;
    Ok(Some(serde_json::json!({
        "component_id": component.id,
        "resolved": true,
    })))
}

pub(super) fn attach_preview_staging_component(
    resolved: &mut Value,
    staging_component: Option<&Value>,
) {
    if let Some(staging_component) = staging_component {
        resolved["staging_component"] = staging_component.clone();
    }
}

pub(super) fn cook_preview_result(
    resolved: Value,
    progress: Vec<Value>,
    replay: PreviewReplayArgv,
    provider: Option<Value>,
    failure: Option<Value>,
) -> Value {
    let mut result = serde_json::json!({
        "schema": "homeboy/agent-task-cook-preview/v1",
        "mutates": false,
        "resolved": resolved,
        "progress": progress,
        "replay_argv": replay.argv,
        "replay_requires": replay.requires,
    });
    if let Some(provider) = provider {
        result["resolved"]["provider"] = provider;
    }
    if let Some(failure) = failure {
        result["failure"] = failure;
    } else if let Some(failure) = cook_preview_blocked_admission_failure(&result["resolved"]) {
        result["failure"] = failure;
    }
    result
}

pub(super) fn cook_preview_blocked_admission_failure(resolved: &Value) -> Option<Value> {
    let admission = resolved.get("placement")?.get("admission")?;
    if admission.get("state").and_then(Value::as_str) != Some("blocked") {
        return None;
    }
    let blocker = admission.get("remaining_blocker").and_then(Value::as_str)?;
    let mut failure = serde_json::json!({ "message": blocker });
    if let Some(next_action) = admission.get("next_action").cloned() {
        failure["next_action"] = next_action;
    }
    Some(failure)
}

pub(super) fn apply_preview_worktree_capacity(
    resolved: &mut Value,
    capacity: Option<&Value>,
    repository_identity: Option<&Value>,
) {
    let Some(capacity) = capacity else { return };
    resolved["workspace"]["capacity"] = capacity.clone();
    let allocates_controller_worktree = resolved["workspace"]["kind"] == "native"
        && resolved["workspace"]["action"] == "planned_create";
    let provider_on_lab = resolved["placement"]["selected"].as_str() == Some("lab")
        || resolved["placement"]["requested"].as_str() == Some("lab");
    if capacity["state"] != "shortfall" || (provider_on_lab && !allocates_controller_worktree) {
        return;
    }
    if provider_on_lab {
        // A Lab-placed Cook's planned controller worktree is a plain source
        // checkout: the provider attempt and deterministic gates run on the
        // admitted runner, so no dependency hydration or build output lands
        // here. Size admission to that checkout instead of the flat
        // reconstructable-artifact reserve (#16149).
        let source = cook_checkout_estimate_source(repository_identity, &resolved["workspace"]);
        let estimate = source.as_deref().and_then(|path| {
            homeboy::core::cleanup::estimated_checkout_allocation_bytes(Path::new(path))
        });
        let inspection_path = repository_identity
            .and_then(|identity| identity.get("repository_path"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(required_bytes) = estimate {
            let available_bytes = capacity["available_bytes"].as_u64();
            resolved["workspace"]["capacity"]["lab_checkout_allocation"] = serde_json::json!({
                "estimated_bytes": required_bytes,
                "source": source,
            });
            if available_bytes.is_some_and(|available| available >= required_bytes) {
                return;
            }
            let available = available_bytes
                .map(|available| available.to_string())
                .unwrap_or_else(|| "an unmeasured number of".to_string());
            let blocker = format!(
                "local worktree filesystem cannot hold this Lab-placed Cook's plain source checkout: ~{required_bytes} bytes needed, {available} bytes available"
            );
            let next_action = worktree_artifact_inspection_action(
                inspection_path.as_deref().or(source.as_deref()),
            );
            let capacity = resolved["workspace"]["capacity"].clone();
            resolved["placement"]["admission"] = render_cook_admission(
                crate::commands::infra::route::CookAdmissionDecision::Blocked {
                    reason: blocker,
                    next_action: Some(next_action),
                },
            );
            resolved["placement"]["admission"]["capacity"] = capacity;
            return;
        }
        // Without a readable source tree the checkout cannot be sized;
        // fall through to the reserve-based blocker below (fail closed).
    }
    let shortfall = capacity["shortfall_bytes"].as_u64().unwrap_or_default();
    let blocker =
        format!("local worktree filesystem is {shortfall} bytes below its configured reserve");
    // The reserve shortfall only words a hint; the operator's cleanup command
    // measures the merged worktrees that actually hold reclaimable space on
    // demand (#16396).
    resolved["placement"]["admission"] = render_cook_admission(
        crate::commands::infra::route::CookAdmissionDecision::Blocked {
            reason: blocker,
            next_action: Some("homeboy worktree cleanup --dry-run".to_string()),
        },
    );
    resolved["placement"]["admission"]["capacity"] = capacity.clone();
}

/// The bounded inspection that reports the specific worktrees holding
/// reconstructable dependency dirs (`node_modules`, `vendor`, `target`) — the
/// entries that actually free controller space — scoped to the Cook's
/// repository when one is known.
pub(super) fn worktree_artifact_inspection_action(repository: Option<&str>) -> String {
    repository
        .map(|repository| {
            format!(
                "homeboy cleanup artifacts --path '{}' --all-worktrees --sort size --limit 100",
                repository.replace('\'', "'\\''")
            )
        })
        .unwrap_or_else(|| "homeboy cleanup --include repo-artifacts".to_string())
}

/// The existing local repository whose HEAD tree sizes a planned controller
/// checkout: the attested repository path, the resolved identity's workspace
/// path, or the provisioning intent's repository when it names a path.
pub(super) fn cook_checkout_estimate_source(
    repository_identity: Option<&Value>,
    workspace: &Value,
) -> Option<String> {
    [
        repository_identity.and_then(|identity| identity.get("repository_path")),
        repository_identity.and_then(|identity| identity.get("workspace_path")),
        workspace.pointer("/intent/repo"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .find(|path| Path::new(path).is_dir())
    .map(str::to_string)
}

pub(super) fn cook_preview_resolved_request(args: &AgentTaskCookArgs, placement: Value) -> Value {
    serde_json::json!({
        "repository": cook_provision_repository(args),
        "component": cook_component_id(args),
        "repository_identity": args.repository_identity,
        "worktree": args.to_worktree,
        "base": args.base,
        "base_preparation": {
            "schema": "homeboy/cook-base-preparation/v1",
            "declared_base": args.base,
            "base_sha": null,
            "provenance": "resolution_deferred",
            "remote_freshness": "deferred",
            "topology": null,
        },
        "head": args.head,
        "placement": placement,
        "workspace": null,
        "provider": null,
        "gates": {
            "public": args.gates.verify.len(),
            "private": args.gates.private_verify.len(),
            "toolchain_check": "The gate toolchain check runs when the Cook is submitted",
            // The effective per-gate limits a Lab runner enforces (#15792).
            // On Lab, dependency setup and the gate command share this one
            // time limit (#16938).
            "timeout_seconds": args.gates.effective_gate_timeout_seconds().max(1),
            // Zero means the no-progress deadline is disabled.
            "no_progress_timeout_seconds": args.gates.effective_gate_no_progress_timeout_seconds(),
            // A repro must fail on base and pass on the candidate (#17653).
            "repro": args.repro.repro,
            "repro_warning": (!args.repro.is_declared())
                .then_some(homeboy::agents::agent_tasks::gate::NO_REPRO_GATE_WARNING),
        },
        "publication": {
            "finalize": !args.no_finalize,
            "draft": args.draft_pr,
        },
        "notification_resolution": homeboy::core::notification_route::current_resolution(),
    })
}

pub(super) fn preview_gate_contract_disclosure(
    validation: &crate::commands::agent_task::gate_contract::GateContractValidation,
    public_gates: &[String],
) -> Value {
    let mut entries = Vec::new();
    let mut private_deferred = 0usize;
    for gate in &validation.gates {
        if gate.status != "selection_deferred" {
            continue;
        }
        if !public_gates.iter().any(|public| public == &gate.command) {
            private_deferred += 1;
            continue;
        }
        entries.push(serde_json::json!({
            "kind": gate.kind,
            "status": gate.status,
            "filter_interpretation": gate.filter_interpretation,
            "filter": gate.filter,
            "selected_count": gate.selected_count,
            "selected_ids": gate.selected_ids,
            "validation": gate.validation,
        }));
    }
    serde_json::json!({
        "schema": "homeboy/cook-preview-gate-validation/v1",
        "deferred_public": entries,
        "deferred_private_count": private_deferred,
    })
}

pub(super) fn record_preview_phase(progress: &mut Vec<Value>, phase: &'static str) {
    let event = serde_json::json!({
        "event": "cook_preview_progress",
        "phase": phase,
    });
    eprintln!("{event}");
    progress.push(event);
}

pub(super) fn with_preview_heartbeat<T>(
    progress: &mut Vec<Value>,
    phase: &'static str,
    operation: impl FnOnce() -> T,
) -> T {
    struct StopHeartbeat(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
    impl Drop for StopHeartbeat {
        fn drop(&mut self) {
            *self.0 .0.lock().expect("preview heartbeat state") = true;
            self.0 .1.notify_one();
        }
    }

    let state = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let started = std::time::Instant::now();
    let result = std::thread::scope(|scope| {
        let state_for_heartbeat = Arc::clone(&state);
        let events_for_heartbeat = Arc::clone(&events);
        scope.spawn(move || loop {
            let (done, wake) = state_for_heartbeat
                .1
                .wait_timeout(
                    state_for_heartbeat
                        .0
                        .lock()
                        .expect("preview heartbeat state"),
                    PREVIEW_PROGRESS_HEARTBEAT,
                )
                .expect("preview heartbeat wait");
            if *done {
                break;
            }
            if wake.timed_out() {
                let event = serde_json::json!({
                    "event": "cook_preview_heartbeat",
                    "phase": phase,
                    "elapsed_ms": started.elapsed().as_millis(),
                });
                eprintln!("{event}");
                events_for_heartbeat
                    .lock()
                    .expect("preview heartbeat events")
                    .push(event);
            }
        });

        let stop = StopHeartbeat(Arc::clone(&state));
        let result = operation();
        drop(stop);
        result
    });
    progress.extend(events.lock().expect("preview heartbeat events").drain(..));
    result
}

pub(super) const MAX_PREVIEW_REPLAY_ARGS: usize = 128;
pub(super) const MAX_PREVIEW_REPLAY_BYTES: usize = 16 * 1024;

#[derive(Debug)]
pub(super) struct PreviewReplayArgv {
    pub(super) argv: Vec<String>,
    pub(super) requires: Vec<String>,
}

pub(super) fn cook_preview_replay_argv(args: &AgentTaskCookArgs) -> PreviewReplayArgv {
    let process_argv = std::env::args().collect::<Vec<_>>();
    if process_argv
        .windows(2)
        .any(|parts| parts == ["agent-task", "cook"])
        && process_argv.iter().any(|part| part == "--preview")
    {
        let mut replay = std::iter::once("homeboy".to_string())
            .chain(
                process_argv
                    .into_iter()
                    .skip(1)
                    .filter(|part| part != "--preview"),
            )
            .collect::<Vec<_>>();
        rewrite_cook_identity_replay_argv(&mut replay, args);
        append_preview_lifecycle_replay_argv(&mut replay, args);
        append_prepared_base_replay_argv(&mut replay, args);
        return finalize_cook_preview_replay(replay, args);
    }

    finalize_cook_preview_replay(cook_replay_argv(args), args)
}

pub(super) fn finalize_cook_preview_replay(
    replay: impl IntoIterator<Item = String>,
    args: &AgentTaskCookArgs,
) -> PreviewReplayArgv {
    let mut replay = redact_preview_replay_argv(replay);
    if args
        .prompt_snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.source == "stdin")
    {
        replay.requires.push(
            "replay requires the original non-empty prompt on stdin for `--prompt -`".to_string(),
        );
    }
    if homeboy::core::parsed_command_preflight::captured_result()
        .is_none_or(|result| result.placement.requested != homeboy_lab_contract::Placement::Local)
    {
        replay.requires.push(
            "runner placement admission is deferred; replay revalidates connected runner readiness before execution"
                .to_string(),
        );
    }
    replay
}

// Unit callers do not have the original process argv. Keep their fallback
// useful for embedding while the CLI path above preserves every supplied
// advanced flag exactly.
pub(crate) fn cook_replay_argv(args: &AgentTaskCookArgs) -> Vec<String> {
    let mut argv = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];
    if let Some(prompt) = &args.dispatch.prompt {
        argv.extend(["--prompt".to_string(), prompt.clone()]);
    }
    if let Some(goal) = &args.goal {
        argv.extend(["--goal".to_string(), goal.clone()]);
    }
    if let Some(repo) = &args.dispatch.repo {
        argv.extend(["--repo".to_string(), repo.clone()]);
    }
    if let Some(component) = &args.component {
        argv.extend(["--component".to_string(), component.clone()]);
    }
    if let Some(cwd) = &args.dispatch.cwd {
        argv.extend(["--cwd".to_string(), cwd.clone()]);
    }
    if let Some(task_url) = &args.dispatch.task_url {
        argv.extend(["--task-url".to_string(), task_url.clone()]);
    }
    if let Some(backend) = &args.dispatch.backend {
        argv.extend(["--backend".to_string(), backend.clone()]);
    }
    if let Some(selector) = &args.dispatch.selector {
        argv.extend(["--selector".to_string(), selector.clone()]);
    }
    if let Some(model) = &args.dispatch.model {
        argv.extend(["--model".to_string(), model.clone()]);
    }
    if let Some(run_id) = &args.dispatch.run_id {
        argv.extend(["--run-id".to_string(), run_id.clone()]);
    }
    if let Some(run_id) = &args.attempt_run_id {
        argv.extend(["--attempt-run-id".to_string(), run_id.clone()]);
    }
    if let Some(worktree) = &args.to_worktree {
        argv.extend(["--to-worktree".to_string(), worktree.clone()]);
    }
    for gate in &args.gates.verify {
        argv.extend(["--verify".to_string(), gate.clone()]);
    }
    if let Some(base) = &args.base {
        argv.extend(["--base".to_string(), base.clone()]);
    }
    append_prepared_base_replay_argv(&mut argv, args);
    if let Some(head) = &args.head {
        argv.extend(["--head".to_string(), head.clone()]);
    }
    if args.no_finalize {
        argv.push("--no-finalize".to_string());
    }
    if args.draft_pr {
        argv.push("--draft-pr".to_string());
    }
    if args.publish_incomplete_draft {
        argv.push("--publish-incomplete-draft".to_string());
    }
    if args.no_tracker_note {
        argv.push("--no-tracker-note".to_string());
    }
    argv
}

pub(super) fn append_prepared_base_replay_argv(argv: &mut Vec<String>, args: &AgentTaskCookArgs) {
    if let Some(sha) = &args.prepared_base_sha {
        let flag = "--prepared-base-sha";
        let mut index = 0;
        while index < argv.len() {
            if argv[index] == flag {
                if let Some(value) = argv.get_mut(index + 1) {
                    *value = sha.clone();
                    return;
                }
            } else if argv[index].starts_with("--prepared-base-sha=") {
                argv[index] = format!("{flag}={sha}");
                return;
            }
            index += 1;
        }
        argv.extend([flag.to_string(), sha.clone()]);
    }
}

pub(super) fn dispatch_command_for_cook(
    args: &AgentTaskCookArgs,
    dispatch: DispatchArgs,
) -> dispatch_service::AgentTaskDispatchCommand {
    let mut command: dispatch_service::AgentTaskDispatchCommand = dispatch.into();
    command.component = cook_component_id(args).map(str::to_string);
    command
}

pub(crate) fn rewrite_cook_identity_replay_argv(
    replay: &mut Vec<String>,
    args: &AgentTaskCookArgs,
) {
    let mut saw_repo = false;
    let mut saw_component = false;
    let mut index = 0;
    while index < replay.len() {
        if replay[index] == "--provider-argv" {
            index += 2;
            continue;
        }
        if replay[index] == "--repo" {
            if let (Some(repo), Some(value)) =
                (args.dispatch.repo.as_ref(), replay.get_mut(index + 1))
            {
                *value = repo.clone();
                saw_repo = true;
            }
            index += 2;
            continue;
        }
        if replay[index].starts_with("--repo=") {
            if let Some(repo) = &args.dispatch.repo {
                replay[index] = format!("--repo={repo}");
                saw_repo = true;
            }
        } else if replay[index] == "--component" {
            if let (Some(component), Some(value)) =
                (args.component.as_ref(), replay.get_mut(index + 1))
            {
                *value = component.clone();
                saw_component = true;
            }
            index += 2;
            continue;
        } else if replay[index].starts_with("--component=") {
            if let Some(component) = &args.component {
                replay[index] = format!("--component={component}");
                saw_component = true;
            }
        }
        index += 1;
    }
    if !saw_repo {
        if let Some(repo) = &args.dispatch.repo {
            replay.extend(["--repo".to_string(), repo.clone()]);
        }
    }
    if !saw_component {
        if let Some(component) = &args.component {
            replay.extend(["--component".to_string(), component.clone()]);
        }
    }
}

pub(super) fn bind_cook_preview_lifecycle(args: &mut AgentTaskCookArgs) {
    args.requested_run_id = args.dispatch.run_id.clone();
    args.requested_attempt_run_id = args.attempt_run_id.clone();
    let requested_cook_id = args.requested_run_id.clone();
    let owner_run_ref = args.requested_attempt_run_id.clone().unwrap_or_else(|| {
        requested_cook_id.as_deref().map_or_else(
            || format!("agent-task-{}", uuid::Uuid::new_v4()),
            |cook_id| agent_task_lifecycle::cook_attempt_run_id(cook_id, 1),
        )
    });
    args.dispatch.run_id = Some(requested_cook_id.unwrap_or_else(|| owner_run_ref.clone()));
    args.attempt_run_id = Some(owner_run_ref);
}

pub(super) fn preview_replay_has_flag(argv: &[String], flag: &str) -> bool {
    argv.iter()
        .any(|argument| argument == flag || argument.starts_with(&format!("{flag}=")))
}

pub(super) fn append_preview_lifecycle_replay_argv(
    argv: &mut Vec<String>,
    args: &AgentTaskCookArgs,
) {
    for (flag, value) in [
        ("--run-id", args.requested_run_id.as_ref()),
        ("--attempt-run-id", args.requested_attempt_run_id.as_ref()),
    ] {
        let Some(value) = value else {
            continue;
        };
        if preview_replay_has_flag(argv, flag) {
            continue;
        }
        argv.extend([flag.to_string(), value.clone()]);
    }
}

pub(super) fn redact_preview_replay_argv(
    argv: impl IntoIterator<Item = String>,
) -> PreviewReplayArgv {
    let (units, mut requires) = clap_replay_units(argv.into_iter().collect());
    let mut replay = Vec::new();
    let mut bytes = 0;
    for unit in units {
        let unit_bytes = unit.iter().map(String::len).sum::<usize>();
        if replay.len() + unit.len() > MAX_PREVIEW_REPLAY_ARGS
            || bytes + unit_bytes > MAX_PREVIEW_REPLAY_BYTES
        {
            requires.push("replay was truncated at the safety budget; re-add omitted complete flag/value units from the original command".to_string());
            break;
        }
        let (unit, requirement) = redact_replay_unit(unit);
        replay.extend(unit);
        bytes += unit_bytes;
        if let Some(requirement) = requirement {
            requires.push(requirement);
        }
    }
    PreviewReplayArgv {
        argv: replay,
        requires,
    }
}

pub(super) fn clap_replay_units(argv: Vec<String>) -> (Vec<Vec<String>>, Vec<String>) {
    let value_flags = cook_value_flags();
    let mut units = Vec::new();
    let mut requires = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let value = &argv[index];
        if value == "--preview" {
            index += 1;
        } else if let Some((_flag, _)) = value.split_once('=') {
            units.push(vec![value.clone()]);
            index += 1;
        } else if value_flags.contains(value) {
            if let Some(next) = argv.get(index + 1).filter(|next| !next.starts_with("--")) {
                units.push(vec![value.clone(), next.clone()]);
                index += 2;
            } else {
                requires.push(format!("{value} was omitted because its value is absent; provide the original value before replaying"));
                index += 1;
            }
        } else {
            units.push(vec![value.clone()]);
            index += 1;
        }
    }
    (units, requires)
}

pub(super) fn cook_value_flags() -> std::collections::BTreeSet<String> {
    let command = crate::cli_surface::Cli::command_with_scoped_lab_args();
    let mut flags = std::collections::BTreeSet::new();
    let agent_task = command
        .find_subcommand("agent-task")
        .expect("agent-task command exists in generated Clap metadata");
    let cook = agent_task
        .find_subcommand("cook")
        .expect("Cook command exists in generated Clap metadata");
    for command in [&command, agent_task, cook] {
        for arg in command
            .get_arguments()
            .filter(|arg| arg.get_action().takes_values())
        {
            if let Some(flag) = arg.get_long() {
                flags.insert(format!("--{flag}"));
            }
            if let Some(aliases) = arg.get_all_aliases() {
                flags.extend(aliases.into_iter().map(|alias| format!("--{alias}")));
            }
        }
    }
    flags
}

pub(super) fn redact_replay_unit(unit: Vec<String>) -> (Vec<String>, Option<String>) {
    let flag = unit
        .first()
        .map(String::as_str)
        .unwrap_or_default()
        .split('=')
        .next()
        .unwrap_or_default();
    let sensitive = flag == "--private-verify"
        || flag == "--private-verify-file"
        || flag == "--gate-env"
        || flag == "--runner-env"
        || flag == "--lab-env-json"
        || matches!(
            flag,
            "--provider-config" | "--provider-argv" | "--client-context" | "--secret-env"
        );
    if !sensitive {
        return (unit, None);
    }
    let placeholder = if flag == "--gate-env" {
        "REDACTED=<redacted:--gate-env>".to_string()
    } else {
        format!("<redacted:{flag}>")
    };
    let replay = if unit.len() == 2 {
        vec![flag.to_string(), placeholder]
    } else {
        vec![format!("{flag}={placeholder}")]
    };
    (replay, Some(format!("{flag} was redacted; replace its parseable placeholder with the original value before replaying")))
}

pub(super) fn preview_placement_policy_with_admission(replay_args: &[String]) -> Value {
    let mut policy = preview_placement_policy(replay_args);
    apply_resolved_placement(&mut policy);
    if policy["requested"] == "local" {
        return policy;
    }
    let decision = homeboy::core::parsed_command_preflight::captured_result()
        .map(|result| crate::commands::infra::route::cook_admission_decision(&result))
        .unwrap_or_else(crate::commands::infra::route::CookAdmissionDecision::unchecked);
    policy["admission"] = render_cook_admission(decision);
    policy
}

pub(super) fn render_cook_admission(
    decision: crate::commands::infra::route::CookAdmissionDecision,
) -> Value {
    use crate::commands::infra::route::CookAdmissionDecision;
    let mut admission = serde_json::json!({
        "schema": "homeboy/cook-preview-placement-admission/v1",
        "revalidate_before_execution": true,
        "blockers": [],
    });
    match decision {
        CookAdmissionDecision::Admitted => admission["state"] = serde_json::json!("admissible"),
        CookAdmissionDecision::Blocked {
            reason,
            next_action,
        } => {
            admission["state"] = serde_json::json!("blocked");
            admission["blockers"] = serde_json::json!([reason]);
            admission["remaining_blocker"] = serde_json::json!(reason);
            admission["replay_prerequisite"] = serde_json::json!(reason);
            if let Some(action) = next_action {
                admission["next_action"] = serde_json::json!(action);
            }
        }
        CookAdmissionDecision::Indeterminate {
            reason,
            next_action,
        } => {
            admission["state"] = serde_json::json!("indeterminate");
            admission["deferred_to"] = serde_json::json!("execution_placement_admission");
            admission["reason"] = serde_json::json!(reason);
            admission["replay_prerequisite"] = serde_json::json!(reason);
            admission["next_action"] = serde_json::json!(next_action);
        }
    }
    // Report only an existing daemon: reading status opens the observation
    // store, and preview admission must not create state.
    let daemon_exists = homeboy::core::paths::daemon_state_file().is_ok_and(|path| path.exists());
    if daemon_exists {
        if let Ok(status) = homeboy::core::daemon::read_status() {
            admission["daemon"] = status.admission_report();
        }
    }
    admission
}

/// Merge the already-resolved placement decision into preview's placement
/// projection.
///
/// This process already ran the same Lab-readiness resolution that a real
/// dispatch would use — `parsed_command_preflight::capture_result` completes
/// once, before any command (preview included) executes — so surfacing it
/// here costs no additional live I/O. Preview otherwise reported only the
/// *requested* placement, which silently diverged from what actually ran
/// whenever `auto`/`lab-or-local` degraded to local execution (#14729).
pub(super) fn apply_resolved_placement(policy: &mut Value) {
    let Some(result) = homeboy::core::parsed_command_preflight::captured_result() else {
        return;
    };
    let directive = result.placement;
    let selected = match directive.selected {
        homeboy_lab_contract::EffectiveExecutionPlacement::Local => "local",
        homeboy_lab_contract::EffectiveExecutionPlacement::Lab => "lab",
    };
    policy["selected"] = serde_json::json!(selected);
    if let Some(runner) = directive.runner.as_ref() {
        policy["selected_runner"] = serde_json::json!(runner.runner_id);
    }
    if let Some(reason) = directive.fallback.reason.as_deref() {
        policy["fallback_reason"] = serde_json::json!(reason);
    }
}

pub(super) fn preview_placement_policy(argv: &[String]) -> Value {
    let argv = crate::command_capability::homeboy_owned_args(argv);
    let placement = homeboy::core::parsed_command_preflight::captured_result()
        .map(|result| cli_placement_str(result.placement.requested))
        .unwrap_or("auto");
    let runner =
        homeboy::core::parsed_command_preflight::captured_result().and_then(|result| match result
            .input
            .runner
        {
            homeboy::core::parsed_command_preflight::RunnerIntent::Explicit(id) => Some(id),
            _ => None,
        });
    serde_json::json!({
        "requested": placement,
        "runner": runner,
        "detach_after_handoff": !argv.iter().take_while(|value| value.as_str() != "--").any(|value| value == "--wait"),
        "route_executed": false,
    })
}
