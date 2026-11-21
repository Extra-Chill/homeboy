//! Cook request preparation, admission, and execution transitions.

use super::run::*;

pub(crate) fn record_cook_provision(plan: &mut AgentTaskPlan, provision: Value) {
    plan.metadata["cook_provision"] = provision.clone();
    if let Some(task) = plan.tasks.first_mut() {
        if !task.metadata.is_object() {
            task.metadata = serde_json::json!({});
        }
        task.metadata["worktree_provision"] = provision;
    }
}

/// Validates Cook input authority before destination provisioning, provider
/// discovery, or any other external effect starts.
pub(super) fn validate_cook_source_authority(
    args: &AgentTaskCookArgs,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> homeboy::core::Result<()> {
    if args.no_finalize {
        if let Some(provenance) = provenance {
            provenance
                .require_sources(
                    &["no_finalize"],
                    &[crate::cli_surface::ArgumentSource::CommandLine],
                )
                .map_err(|error| {
                    homeboy::core::Error::validation_invalid_argument(
                        "no_finalize",
                        "--no-finalize must be explicitly authorized on the command line",
                        Some(serde_json::to_string(&error).expect("source policy serializes")),
                        None,
                    )
                })?;
        }
    }
    Ok(())
}

/// Shared ordering boundary for preview, execution, handoff and admission.
/// Repository metadata supplies default gates, but destination/base resolution
/// must not start until the request (including queue-only refusal) is valid.
pub(crate) fn prepare_cook_request(
    args: &mut AgentTaskCookArgs,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> homeboy::core::Result<Option<ResolvedCookRepository>> {
    // Every Cook path prepares here, including a queued admission's replay of
    // its raw argv, so the `--task-url` prompt is derived here too.
    default_cook_prompt_from_task_url(args);
    let repository = resolve_cook_repository(args);
    let component = repository
        .as_ref()
        .ok()
        .and_then(|repo| repo.as_ref())
        .map(|repo| &repo.component);
    apply_component_default_verify_gates(args, component)?;
    validate_cook_request_with_provenance(args, provenance)?;
    repository
}

/// The PR-title rule for the Cook's target component, resolved only when the
/// operator supplied a subject to check.
pub(super) fn cook_title_policy(args: &AgentTaskCookArgs) -> PrTitlePolicy {
    if args.title.is_none() && args.commit_message.is_none() {
        return PrTitlePolicy::Plain;
    }
    let repository = resolve_cook_repository(&mut args.clone()).ok().flatten();
    PrTitlePolicy::for_component(repository.as_ref().map(|repo| &repo.component))
}

/// Validate operator-supplied publication subjects before Cook has side effects.
/// Components whose releases version from commit subjects require conventional
/// subjects; all others accept any non-empty subject.
pub(super) fn validate_cook_subjects(
    title: Option<&str>,
    commit_message: Option<&str>,
    title_policy: PrTitlePolicy,
) -> homeboy::core::Result<()> {
    for (argument, subject) in [
        ("title", title),
        (
            "commit-message",
            commit_message.map(|message| message.lines().next().unwrap_or("")),
        ),
    ] {
        let Some(subject) = subject else {
            continue;
        };
        let message = match title_policy {
            PrTitlePolicy::ConventionalCommit if !homeboy::core::git::is_conventional_subject(subject) => format!(
                "Cook requires a conventional subject for this component: type(scope)?!?: subject; types: {}. Bumps: feat = minor, fix and other types = patch, ! = major",
                homeboy::core::git::CONVENTIONAL_COMMIT_TYPES.join(", ")
            ),
            PrTitlePolicy::Plain if subject.trim().is_empty() => {
                "Cook requires a non-empty, specific subject".to_string()
            }
            _ => continue,
        };
        return Err(homeboy::core::Error::validation_invalid_argument(
            argument,
            message,
            Some(subject.to_string()),
            None,
        ));
    }
    Ok(())
}

pub(crate) fn validate_cook_request_with_provenance(
    args: &AgentTaskCookArgs,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> homeboy::core::Result<()> {
    validate_cook_source_authority(args, provenance)?;
    validate_cook_subjects(
        args.title.as_deref(),
        args.commit_message.as_deref(),
        cook_title_policy(args),
    )?;
    if args.dispatch.cwd.is_none() {
        if let (Some(handle), Some(repository), Some(head)) = (
            args.to_worktree.as_deref(),
            cook_provision_repository(args),
            args.head.as_deref(),
        ) {
            if !Path::new(handle).is_dir() {
                homeboy::core::worktree_provider::validate_native_handle(
                    handle,
                    &repository,
                    head,
                )?;
            }
        }
    }
    if args.dispatch.core.queue_only {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "queue-only",
            "agent-task cook cannot queue its controller-owned lifecycle",
            None,
            None,
        ));
    }
    // Backend policy is an execution prerequisite. Resolve it before validating
    // unrelated prompt, evidence, and gate inputs so Cook reports the blocker
    // an operator must fix first.
    let mut dispatch = dispatch_args_for_cook(args);
    let request = dispatch_service::resolve_dispatch_request(dispatch_command_for_cook(
        args,
        dispatch.clone(),
    ))?;
    if args.goal.is_some() && !args.dispatch.tasks.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task",
            "agent-task cook uses --goal as framing metadata and --prompt as its one source; --goal and --task conflict",
            None,
            Some(vec![
                "Use: homeboy agent-task cook --goal 'Describe the outcome' --prompt @task.txt --to-worktree sample-plugin@fix-issue --verify 'npm test'".to_string(),
            ]),
        ));
    }
    // Resolve @file input before scanning its provider-visible content. The
    // host path is an ingestion detail, not evidence. Stdin remains unread
    // until execution so this preflight cannot consume its prompt bytes.
    if let Some(snapshot) = &args.prompt_snapshot {
        dispatch.prompt = Some(snapshot.content.clone());
        dispatch.prompt_is_literal = true;
    }
    if dispatch.prompt.as_deref().is_some_and(|spec| {
        spec.starts_with('@')
            && homeboy::agents::agent_task_prompts::stored_prompt_ref_id(spec).is_none()
    }) {
        resolve_dispatch_prompt(&mut dispatch)?;
    }
    validate_provider_evidence_inputs(&args.provider_evidence_inputs, dispatch.prompt.as_deref())?;
    dispatch_service::validate_single_cook_prompt_source(
        dispatch.prompt.as_deref(),
        &dispatch.tasks,
        dispatch.core.tasks_json.as_deref(),
    )?;
    if !args.gates.has_deterministic_gate() && !args.repro.is_declared() && !args.no_finalize {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verify",
            "agent-task cook requires at least one deterministic --verify or --private-verify gate before it can commit, push, and open a PR",
            None,
            Some(vec![
                "Provide a deterministic verification gate, e.g. --verify \"homeboy review test <component>\"."
                    .to_string(),
                "Use --no-finalize for a read-only Cook that will not commit, push, or open a PR."
                    .to_string(),
            ]),
        ));
    }
    // The Cook spine enforces this too; preview and admission apply the same
    // static rule before any destination or provider work (#15672).
    homeboy::agents::agent_task_service::validate_reviewer_visible_gates(&args.gates.verify, None)?;
    homeboy::agents::agent_task_service::validate_reviewer_visible_gates(&args.repro.repro, None)?;
    // Resolve against the same filtered rotation policy that compilation uses,
    // while Cook is still in its no-side-effect validation phase.
    let configured_rotations =
        if args.dispatch.model.is_some() && args.dispatch.acknowledge_model_override {
            0
        } else {
            dispatch_service::controller_resolved_execution_policy(&request)
                .rotation
                .as_ref()
                .map(|rotation| {
                    rotation
                        .max_total_attempts()
                        .min(
                            u32::try_from(rotation.entries.len())
                                .unwrap_or(u32::MAX)
                                .saturating_add(1),
                        )
                        .saturating_sub(1)
                })
                .unwrap_or(0)
        };
    homeboy::agents::agent_task_service::resolve_cook_budget(
        args.max_attempts,
        configured_rotations,
        args.dispatch.core.attempts,
        args.dispatch.core.same_provider_retries,
    )?;
    Ok(())
}

pub(crate) fn promotion_provider(args: PromotionProviderArgs) -> CmdResult<Value> {
    let mut request = Vec::new();
    std::io::stdin()
        .take(MAX_PROMOTION_PROVIDER_REQUEST_BYTES + 1)
        .read_to_end(&mut request)
        .map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("read agent-task promotion provider request".to_string()),
            )
        })?;
    if request.len() as u64 > MAX_PROMOTION_PROVIDER_REQUEST_BYTES {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "promotion-provider request",
            format!(
                "promotion provider request exceeds {MAX_PROMOTION_PROVIDER_REQUEST_BYTES} bytes"
            ),
            None,
            None,
        ));
    }
    let request = String::from_utf8(request).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "promotion-provider request",
            format!("promotion provider request is not UTF-8: {error}"),
            None,
            None,
        )
    })?;
    homeboy::agents::agent_task_promotion::apply_materialized_workspace_patch(
        Path::new(&args.workspace),
        &request,
    )
    .and_then(|response| {
        serde_json::from_str(&response).map_err(|error| {
            homeboy::core::Error::internal_json(
                error.to_string(),
                Some("serialize agent-task promotion provider response".to_string()),
            )
        })
    })
    .map(|value| (value, 0))
}

pub(super) fn run_cook_with_executor(
    args: AgentTaskCookArgs,
    executor: SharedAgentTaskExecutor,
) -> CmdResult<Value> {
    run_cook_with_executor_and_dispatcher(args, executor, None)
}

pub(crate) fn run_cook_with_executor_and_dispatcher(
    args: AgentTaskCookArgs,
    executor: SharedAgentTaskExecutor,
    attempt_dispatcher: Option<
        Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
) -> CmdResult<Value> {
    run_cook_with_executor_and_dispatcher_with_progress(
        args,
        executor,
        attempt_dispatcher,
        None,
        None,
    )
}

pub(crate) fn run_cook_with_executor_and_dispatcher_with_progress(
    mut args: AgentTaskCookArgs,
    executor: SharedAgentTaskExecutor,
    attempt_dispatcher: Option<
        Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
    progress: super::CookProgress<'_>,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> CmdResult<Value> {
    let retained_gate_contract = args.gates.has_retained_contract();
    snapshot_cook_prompt(&mut args)?;
    args.gates.snapshot_file_inputs()?;
    args.repro.snapshot_file_inputs()?;
    let repository = prepare_cook_request(&mut args, provenance)?;
    confirm_model_override(&mut args)?;
    if !args.no_progress {
        if let Some(progress) = progress {
            progress(
                "destination_resolution",
                None,
                None,
                Some("resolving default base and worktree"),
                None,
            )?;
        }
    }
    let args = resolve_cook_destination_with_component_lint_gate(
        args,
        repository.as_ref(),
        retained_gate_contract,
    )?;
    let gate_workspace = args.dispatch.cwd.as_deref().map(Path::new).or_else(|| {
        args.to_worktree
            .as_deref()
            .map(Path::new)
            .filter(|path| path.is_dir())
    });
    let gate_contract_validation = validate_gate_contracts(
        args.gates
            .verify
            .iter()
            .chain(&args.gates.private_verify)
            .cloned(),
        gate_workspace,
        &crate::cli_runtime::current_augmented_command_contract(),
    )?;
    reject_gates_unexecutable_under_resolved_placement(
        args.gates
            .verify
            .iter()
            .chain(&args.gates.private_verify)
            .cloned(),
    )?;
    preflight_cook_gates(&args)?;
    let no_progress = args.no_progress;
    // Deterministic gates exist to make *publication* safe: a green gate is the
    // proof a cook may commit, push, and open a PR. A `--no-finalize` cook does
    // none of those, so a gate is not meaningful there — read-only/exploratory
    // cooks legitimately have nothing to verify. The promotion lifecycle already
    // treats an empty gate set as a vacuously-green run
    // (`run_promotion_gates` -> `PromotionGateRun::without_gates`), so relaxing
    // the requirement here is safe end to end (#7608). Finalizing cooks still
    // require a gate, but now say so with a copy-pasteable example instead of a
    // bare rejection.
    if !no_progress {
        if let Some(progress) = progress {
            progress(
                "destination_provisioning",
                None,
                None,
                Some("checking managed worktree availability"),
                None,
            )?;
        }
    }
    let provision = provision_cook_destination(&args, repository.as_ref(), false)?;

    run_preflight_cook_execution(
        args,
        executor,
        attempt_dispatcher,
        progress,
        gate_contract_validation,
        provision,
        no_progress,
        provenance,
    )
}

/// Validate the input boundary shared by executable Cook routes. Output-file
/// callers run this before creating their detached handoff parent.
pub(crate) fn preflight_cook_execution_request(
    args: &mut AgentTaskCookArgs,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> homeboy::core::Result<()> {
    snapshot_cook_prompt(&mut *args)?;
    args.gates.snapshot_file_inputs()?;
    args.repro.snapshot_file_inputs()?;
    validate_cook_request_with_provenance(args, provenance)?;
    confirm_model_override(args)?;
    Ok(())
}

/// Non-interactive form of the model-override check every Cook admission path
/// must run (#15009). A queued Lab admission is created out of process, with
/// no TTY to prompt against, so it must be refused here at submit time with
/// the same typed error a replay would otherwise only discover later, deep
/// inside a spawned replay worker, after an admission record already exists.
pub(crate) fn require_model_override_acknowledgement_for_cook(
    args: &AgentTaskCookArgs,
) -> homeboy::core::Result<()> {
    if args.dispatch.acknowledge_model_override {
        return Ok(());
    }
    let dispatch: dispatch_service::AgentTaskDispatchCommand =
        resolved_dispatch_args_for_cook(args)?.into();
    let request = dispatch_service::resolve_dispatch_request(dispatch)?;
    dispatch_service::require_model_override_acknowledgement(&request)
}

pub(super) fn confirm_model_override(args: &mut AgentTaskCookArgs) -> homeboy::core::Result<()> {
    if let Err(error) = require_model_override_acknowledgement_for_cook(args) {
        if !crate::commands::utils::tty::require_tty_for_interactive() {
            return Err(error);
        }
        let answer = crate::commands::utils::tty::prompt(&format!(
            "{} Continue with this model? [y/N] ",
            error.message
        ))?;
        if !matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes") {
            return Err(error);
        }
        args.dispatch.acknowledge_model_override = true;
    }
    Ok(())
}

/// How often a queued local Cook dispatch re-checks capacity.
pub(super) const LOCAL_DISPATCH_QUEUE_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// Admit or queue a local Cook provider dispatch against the
/// resource-policy snapshot the controller already captured at preflight
/// (`captured_pressure_severity` in `homeboy-core::lab_routing`, consulted
/// today for pressure-driven Lab promotion but never for local dispatch).
///
/// Returns `Ok(None)` when preflight captured no snapshot at all — a command
/// resource policy never evaluated for this invocation — so admission fails
/// open rather than guessing at evidence it does not have.
pub(super) fn admit_local_cook_dispatch(
    run_id: &str,
    cook_id: &str,
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
) -> homeboy::core::Result<Option<homeboy::core::local_dispatch_admission::ActiveLocalDispatchLease>>
{
    if homeboy::core::resource_policy_context::captured_context().is_none() {
        return Ok(None);
    }
    let data_root = homeboy::core::paths::homeboy_data()?;
    let run_id_for_notice = run_id.to_string();
    admit_local_cook_dispatch_with_clock(
        run_id,
        &data_root,
        || {
            use crate::commands::resources::ResourceRecommendation;
            // Refresh on every poll: a captured hot snapshot never cools on
            // its own. Use the same lightweight pressure probe as preflight.
            let (resources, _) = crate::commands::resources::run_preflight()?;
            let severity = match resources.recommendation {
                ResourceRecommendation::Ok => "ok",
                ResourceRecommendation::Warm => "warm",
                ResourceRecommendation::Hot => "hot",
            };
            Ok((
                severity.to_string(),
                resources.load.one,
                resources.load.cpu_count,
            ))
        },
        LOCAL_DISPATCH_QUEUE_POLL_INTERVAL,
        std::thread::sleep,
        move |reason| {
            eprintln!("Local dispatch queued for {run_id_for_notice}: {reason}.");
            record_local_dispatch_wait(lifecycle_store, cook_id, reason);
        },
    )
    .map(Some)
}

/// Phase a durable Cook reports while its local dispatch waits for pressure.
pub(super) const LOCAL_DISPATCH_QUEUED_PHASE: &str = "queued_local_pressure";

/// Make a pressure wait visible to `status` and `diagnose`. The phase keeps
/// its first `phase_started_at`, so `elapsed_ms` is the whole wait. A
/// foreground Cook has no durable record yet; its wait is the stderr notice.
pub(super) fn record_local_dispatch_wait(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_id: &str,
    reason: &str,
) {
    if lifecycle_store.read_record(cook_id).is_ok() {
        if let Err(error) = lifecycle_store.record_cook_progress_with_activity(
            cook_id,
            LOCAL_DISPATCH_QUEUED_PHASE,
            0,
            Some(reason),
            None,
        ) {
            homeboy::core::log_status!("cook", "local dispatch wait not recorded: {}", error);
        }
    }
}

/// The testable core of [`admit_local_cook_dispatch`]: the clock and the queue
/// notice and pressure probe are injected for deterministic queue tests.
/// There is no admission timeout: the enclosing workflow owns its deadline.
pub(super) fn admit_local_cook_dispatch_with_clock(
    run_id: &str,
    data_root: &Path,
    mut pressure: impl FnMut() -> homeboy::core::Result<(String, Option<f64>, usize)>,
    poll_interval: Duration,
    mut sleep: impl FnMut(Duration),
    mut on_queue: impl FnMut(&str),
) -> homeboy::core::Result<homeboy::core::local_dispatch_admission::ActiveLocalDispatchLease> {
    use homeboy::core::local_dispatch_admission::{
        evaluate_local_dispatch_admission, try_admit_local_dispatch, LocalDispatchAdmission,
        LocalDispatchAttempt,
    };

    loop {
        let (severity, load_one, cpu_count) = pressure()?;
        let attempt = try_admit_local_dispatch(data_root, run_id, |active| {
            evaluate_local_dispatch_admission(active, &severity, load_one, cpu_count, false)
        })?;
        let decision = match attempt {
            LocalDispatchAttempt::Admitted(lease) => return Ok(lease),
            LocalDispatchAttempt::NotAdmitted(decision) => decision,
        };
        match decision {
            // An admitting decision always takes the slot in the same
            // transaction; re-run admission rather than trust a stale one.
            LocalDispatchAdmission::Admit => continue,
            LocalDispatchAdmission::Queue { reason } => {
                on_queue(&reason);
                sleep(poll_interval);
            }
        }
    }
}

pub(super) fn run_preflight_cook_execution(
    args: AgentTaskCookArgs,
    executor: SharedAgentTaskExecutor,
    attempt_dispatcher: Option<
        Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
    progress: super::CookProgress<'_>,
    gate_contract_validation: GateContractValidation,
    provision: Value,
    no_progress: bool,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> CmdResult<Value> {
    let mut dispatch_args = resolved_dispatch_args_for_cook(&args)?;
    let requested_cook_id = dispatch_args.run_id.clone();
    if let Some(cook_id) = requested_cook_id.as_deref() {
        dispatch_args.run_id = Some(
            args.attempt_run_id
                .clone()
                .unwrap_or_else(|| agent_task_lifecycle::cook_attempt_run_id(cook_id, 1)),
        );
    }
    let run_id = dispatch_args
        .run_id
        .clone()
        .unwrap_or_else(|| format!("agent-task-{}", uuid::Uuid::new_v4()));
    dispatch_args.run_id = Some(run_id.clone());
    let cook_id = requested_cook_id.clone().unwrap_or_else(|| run_id.clone());
    if !no_progress {
        if let Some(progress) = progress {
            progress(
                "preparing",
                Some(&cook_id),
                Some(&run_id),
                Some("preparing durable Cook inputs"),
                None,
            )?;
        }
    }
    let (run_id, mut initial_plan) = if let Some(attempt_plan) = args.attempt_plan.as_deref() {
        let run_id = dispatch_args.run_id.clone().ok_or_else(|| {
            homeboy::core::Error::internal_unexpected(
                "agent-task cook attempt plan requires an attempt run id".to_string(),
            )
        })?;
        (run_id, agent_task_service::read_plan(attempt_plan)?)
    } else {
        let plan = compile_cook_plan(&args, provision.clone(), None)?;
        (run_id, plan)
    };
    if args.attempt_plan.is_some() {
        record_cook_provision(&mut initial_plan, provision);
        record_cook_goal(&mut initial_plan, args.goal.as_deref());
    }
    initial_plan.metadata["gate_contract_validation"] =
        serde_json::to_value(gate_contract_validation)
            .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
    resolve_cook_execution_budget(&args, &mut initial_plan)?;
    if let Some(preflight) = homeboy::core::parsed_command_preflight::captured_result() {
        if preflight.placement.selected == homeboy_lab_contract::EffectiveExecutionPlacement::Lab {
            let placement = serde_json::json!({
                "selected": "lab",
                "selected_runner": preflight.selected_runner_id,
            });
            preflight_preview_lab_provider_secret_env(&placement, &initial_plan)?;
        }
    }
    if !no_progress {
        eprintln!(
            "{}",
            cook_resolved_policy_disclosure(args.max_attempts, &initial_plan)
        );
        eprintln!("{}", cook_rotation_disclosure(&initial_plan));
        eprintln!("{}", cook_provider_timeout_disclosure(&initial_plan));
        if !args.repro.is_declared() {
            eprintln!(
                "cook: warning: {}",
                homeboy::agents::agent_tasks::gate::NO_REPRO_GATE_WARNING
            );
        }
    }
    if let Some(provenance) = provenance {
        record_cook_argument_provenance(&mut initial_plan, provenance);
    }
    if let Some(key) = &args.admission_key {
        if key.trim().is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "admission-key",
                "--admission-key must not be empty",
                None,
                None,
            ));
        }
        initial_plan.metadata[agent_task_service_direct::COOK_ADMISSION_KEY_METADATA] =
            Value::String(key.clone());
    }
    // Admission itself reads the exported HOMEBOY_LAB_PRIORITY step environment.
    if let Some(priority) = &args.priority {
        initial_plan.metadata["lab_priority"] = Value::String(priority.clone());
    }
    if args.require_acceptance {
        let authority = args.acceptance_authority.clone().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "acceptance-authority",
                "--require-acceptance requires --acceptance-authority",
                None,
                None,
            )
        })?;
        let policy = args.acceptance_policy.clone().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "acceptance-policy",
                "--require-acceptance requires --acceptance-policy",
                None,
                None,
            )
        })?;
        if authority.trim().is_empty() || policy.trim().is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "acceptance",
                "--require-acceptance requires non-empty --acceptance-authority and --acceptance-policy",
                None,
                None,
            ));
        }
        initial_plan.metadata["acceptance"] =
            serde_json::json!({ "authority": authority, "policy": policy });
    }
    // Capture the resolved task workspace before dispatch. The provider may
    // commit and leave a clean tree, so resolving this after it runs would
    // silently widen the promotion range.
    let source_worktree_path = initial_plan
        .tasks
        .first()
        .and_then(|task| task.workspace.root.as_ref())
        .map(std::path::PathBuf::from);
    let task_base_sha = args
        .prepared_base_sha
        .clone()
        .or_else(|| source_worktree_path.as_deref().and_then(git_head_sha));
    if let Some(prepared_base_sha) = &args.prepared_base_sha {
        initial_plan.metadata["cook_prepared_base_sha"] = Value::String(prepared_base_sha.clone());
    }
    // Empty recipe title delegates to the validated agent form after candidate gates.
    let title = args.title.clone().unwrap_or_default();
    // Empty commit message likewise delegates to the validated PR title at
    // finalization: a Cook PR has one commit, and GitHub squash-merges a
    // single-commit PR under that commit's subject (#16052).
    let commit_message = args.commit_message.clone().unwrap_or_default();
    let durable_observer = |event: &agent_task_service::CookProgressEvent<'_>| {
        if no_progress && event.phase != "durable_identity" {
            return Ok(());
        }
        // The observer renders the activity sample here rather than passing the
        // struct on, so foreground clients (TTY, machine log, `--output` file)
        // all describe a running provider with the same bounded sentence.
        let activity = event.activity_summary();
        let phase_wait = format!(
            "elapsed={}ms; wait_owner={}",
            event.elapsed_ms, event.wait_owner
        );
        let terminal_outcome =
            event
                .terminal_success
                .map(|succeeded| if succeeded { "succeeded" } else { "failed" });
        progress
            .map(|progress| {
                progress(
                    event.phase,
                    Some(event.cook_id),
                    Some(event.run_id),
                    terminal_outcome.or_else(|| {
                        event
                            .detail
                            .or(activity.as_deref())
                            .or(Some(phase_wait.as_str()))
                    }),
                    event.terminal_retry_command,
                )
            })
            .unwrap_or(Ok(()))
    };
    let recipe_store = agent_task_service::CookRecipeStore::from_current_data_root()?;
    let lifecycle_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    // A local provider dispatch — not a Lab-routed one — is the workload that
    // saturates this machine, so only local dispatch consults the concurrency
    // ceiling. The lease is held for this whole cook (provider, promotion, and
    // gates all run on this host) and released when the function returns,
    // whether by success or by an early `?` (#14732).
    let _local_dispatch_lease = (!args.preview && attempt_dispatcher.is_none())
        .then(|| admit_local_cook_dispatch(&run_id, &cook_id, &lifecycle_store))
        .transpose()?
        .flatten();
    let request = agent_task_service::CookRequest {
        identity: homeboy::agents::agent_task_service::CookIdentity {
            cook_id,
            initial_run_id: run_id,
            initial_plan,
        },
        workspace: homeboy::agents::agent_task_service::CookWorkspace {
            to_worktree: args.to_worktree.expect("Cook destination is resolved"),
            source_worktree_path,
            task_base_sha,
            source_refs: args.dispatch.task_url.into_iter().collect(),
        },
        provider_transport: homeboy::agents::agent_task_service::CookProviderTransport {
            provider_invocation: (!args.provider_argv.is_empty()).then(|| CommandInvocation {
                argv: args.provider_argv,
                ..Default::default()
            }),
            attempt_dispatcher,
        },
        gates: {
            let mut gates = args.dispatch.cook.values.gates.into();
            args.dispatch.cook.values.repro.apply_to(&mut gates);
            gates
        },
        retry_policy: homeboy::agents::agent_task_service::CookRetryPolicy {
            max_attempts: args.dispatch.cook.values.max_attempts,
        },
        finalization: homeboy::agents::agent_task_service::CookFinalization {
            publish_incomplete_draft: args.dispatch.cook.values.publish_incomplete_draft,
            tracker_note: !args.dispatch.cook.values.no_tracker_note,
            no_finalize: args.no_finalize,
            draft_pr: args.draft_pr,
            base: args.base.expect("Cook base is resolved before execution"),
            head: args.head,
            title,
            commit_message,
            protected_branches: args.protected_branches,
        },
        harvest_context:
            homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()?,
    };
    // Keep the original step process alive while Cook is in flight. The
    // workflow owns this process (including cancellation and its deadline),
    // so waiting here neither submits another step nor admits another Cook.
    let workflow_step =
        std::env::var_os(homeboy::core::daemon::workflow_job::WORKFLOW_INSTANCE_ENV).is_some();
    let result = wait_for_workflow_cook(
        workflow_step,
        |mode| {
            agent_task_service::CookService::run(
                request.clone(),
                agent_task_service::CookRuntime::production_with_observer(
                    executor.clone(),
                    &recipe_store,
                    &lifecycle_store,
                    &durable_observer,
                ),
                mode,
            )
            .map(|result| {
                let disposition = result.value.disposition;
                (result, disposition)
            })
        },
        || std::thread::sleep(LOCAL_DISPATCH_QUEUE_POLL_INTERVAL),
    )?;
    Ok((
        super::status::compact_cook_report(
            cook_report_with_continuation(
                serde_json::to_value(result.value).unwrap_or(Value::Null),
            ),
            args.full,
        ),
        result.exit_code,
    ))
}

/// Re-drive only the already-admitted Cook. Non-workflow callers retain their
/// one-report contract; workflow steps wait for a terminal disposition.
fn wait_for_workflow_cook<T>(
    workflow_step: bool,
    mut drive: impl FnMut(
        agent_task_service::CookMode,
    )
        -> homeboy::core::Result<(T, homeboy::core::cook_status::CookDisposition)>,
    mut wait: impl FnMut(),
) -> homeboy::core::Result<T> {
    let mut mode = agent_task_service::CookMode::Start;
    loop {
        let (report, disposition) = drive(mode)?;
        if !workflow_step || disposition.is_terminal() {
            return Ok(report);
        }
        wait();
        mode = agent_task_service::CookMode::Resume;
    }
}

#[cfg(test)]
mod workflow_cook_wait_tests {
    use super::*;
    use homeboy::core::cook_status::CookDisposition;

    #[test]
    fn queued_cook_step_waits_then_succeeds() {
        let mut calls = 0;
        let mut waits = 0;
        let outcome = wait_for_workflow_cook(
            true,
            |mode| {
                calls += 1;
                if calls == 1 {
                    assert!(matches!(mode, agent_task_service::CookMode::Start));
                    Ok((("queued", 75), CookDisposition::InFlight))
                } else {
                    assert!(matches!(mode, agent_task_service::CookMode::Resume));
                    Ok((("succeeded", 0), CookDisposition::Terminal))
                }
            },
            || waits += 1,
        )
        .unwrap();
        assert_eq!(outcome, ("succeeded", 0));
        assert_eq!(calls, 2);
        assert_eq!(waits, 1);
    }

    #[test]
    fn foreground_cook_preserves_in_flight_report() {
        let report = wait_for_workflow_cook(
            false,
            |_| Ok((("queued", 75), CookDisposition::InFlight)),
            || panic!("foreground caller must not wait"),
        )
        .unwrap();
        assert_eq!(report, ("queued", 75));
    }
}

pub(crate) fn record_cook_argument_provenance(
    plan: &mut AgentTaskPlan,
    provenance: &crate::cli_surface::CommandArgumentProvenance,
) {
    provenance.project_into(&mut plan.metadata);
}

pub(super) fn dispatch_args_for_cook(args: &AgentTaskCookArgs) -> DispatchArgs {
    let mut dispatch_args = args.dispatch.clone();
    let has_explicit_work = dispatch_args.prompt.is_some()
        || !dispatch_args.tasks.is_empty()
        || dispatch_args.core.tasks_json.is_some();
    if !has_explicit_work {
        dispatch_args.prompt = args.goal.clone();
    }
    dispatch_args
}

/// Resolve a Cook prompt without treating an ingress snapshot as another
/// structured spec. The original `dispatch.prompt` remains source provenance;
/// the typed snapshot supplies literal task content.
pub(super) fn resolved_dispatch_args_for_cook(
    args: &AgentTaskCookArgs,
) -> homeboy::core::Result<DispatchArgs> {
    let mut dispatch = dispatch_args_for_cook(args);
    if let Some(snapshot) = &args.prompt_snapshot {
        dispatch.prompt = Some(snapshot.content.clone());
        dispatch.prompt_is_literal = true;
    } else {
        resolve_dispatch_prompt(&mut dispatch)?;
    }
    Ok(dispatch)
}

pub(super) fn resolve_cook_execution_budget(
    args: &AgentTaskCookArgs,
    plan: &mut AgentTaskPlan,
) -> homeboy::core::Result<()> {
    let core = &args.dispatch.core;
    let pinned_route = args.dispatch.model.is_some() && args.dispatch.acknowledge_model_override;
    let configured_rotations = if pinned_route {
        0
    } else {
        plan.options.execution_budget.max_provider_rotations
    };
    let resolved = homeboy::agents::agent_task_service::resolve_cook_budget(
        args.max_attempts,
        configured_rotations,
        core.attempts,
        core.same_provider_retries,
    )?;
    plan.options.execution_budget =
        homeboy::agents::agent_task_scheduler::AgentTaskExecutionBudget::new(
            resolved.provider_executions,
            resolved.same_provider_remediations,
            resolved.provider_rotations,
        );
    let effective_route = plan.tasks.first().map(|task| {
        serde_json::json!({
            "backend": task.executor.backend,
            "selector": task.executor.selector,
            "model": task.executor.model,
            "model_selection": plan.metadata["model_selection"],
        })
    });
    plan.metadata["cook_retry_policy"] = serde_json::json!({
        "operator_intent": {
            "max_attempts": args.max_attempts,
            "max_provider_executions": core.attempts,
            "max_same_provider_retries": core.same_provider_retries,
        },
        "route": {
            "requested": {
                "backend": args.dispatch.backend,
                "model": args.dispatch.model,
            },
            "effective": effective_route,
            "fallback": {
                "mode": if pinned_route { "pinned" } else { "rotatable" },
                "configured_rotations": plan.options.rotation.as_ref().map_or(0, |rotation| rotation.entries.len()),
                "enabled": resolved.provider_rotations > 0,
                "model_override_acknowledged": pinned_route,
            },
        },
        "resolved": {
            "max_attempts": resolved.requested_attempts,
            "max_provider_executions": resolved.provider_executions,
            "max_same_provider_retries": resolved.same_provider_remediations,
            "max_provider_rotations": resolved.provider_rotations,
        },
        "requested": {
            "max_attempts": resolved.requested_attempts,
            "max_provider_executions": resolved.requested_provider_executions,
            "max_same_provider_retries": resolved.same_provider_remediations,
            "max_provider_rotations": resolved.provider_rotations,
        },
        "effective": {
            "max_attempts": resolved.requested_attempts,
            "max_provider_executions": resolved.provider_executions,
            "max_same_provider_retries": resolved.same_provider_remediations,
            "max_provider_rotations": resolved.provider_rotations,
        },
        "timeouts": {
            "provider_timeout_ms": homeboy::agents::agent_task_timeout::effective_provider_timeout_ms(
                plan.tasks.first().and_then(|task| task.limits.timeout_ms).or(plan.options.timeout_ms),
                plan.tasks.first().and_then(|task| task.limits.max_runtime_ms),
            ),
        },
    });
    Ok(())
}

/// Resolve `--prompt` through the structured-input contract its help already
/// advertises: `@file`, `-` for stdin, and stored `@prompt:<id>` references.
///
/// Cook previously took `--prompt` as a literal string, so a large Markdown
/// prompt had to travel as one shell argument. Backticks, `$` expressions and
/// quotes were then interpreted by the caller's shell before Homeboy ever saw
/// them — in one case executing prompt text as commands (#10100).
///
/// Bytes and newlines are preserved exactly; no trimming.
pub(super) fn resolve_dispatch_prompt(
    dispatch_args: &mut DispatchArgs,
) -> homeboy::core::Result<()> {
    let Some(spec) = dispatch_args.prompt.as_deref() else {
        return Ok(());
    };
    let resolved = homeboy::agents::agent_task_prompts::read_prompt_input(spec)?;
    dispatch_args.prompt = Some(resolved);
    Ok(())
}

/// Snapshot stdin at the Cook ingress boundary. Later compilation may happen in
/// a detached child or retry a plan, neither of which owns the original stream.
pub(crate) fn snapshot_cook_prompt(args: &mut AgentTaskCookArgs) -> homeboy::core::Result<()> {
    default_cook_prompt_from_task_url(args);
    if args.prompt_snapshot.is_some()
        || args.attempt_plan.is_some()
        || !args
            .dispatch
            .prompt
            .as_deref()
            .is_some_and(|spec| spec.trim() == "-")
    {
        return Ok(());
    }

    let content = homeboy::agents::agent_task_prompts::read_prompt_input("-")?;
    if content.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "prompt",
            "agent-task cook --prompt - received empty stdin",
            None,
            Some(vec!["Pipe a non-empty prompt, for example: homeboy agent-task cook --prompt - < task.md".to_string()]),
        ));
    }
    let size_bytes = content.len();
    let sha256 = format!(
        "sha256:{}",
        homeboy_engine_primitives::content_hash::sha256_hex(content.as_bytes())
    );
    args.prompt_snapshot = Some(super::args::CookPromptSnapshot {
        content,
        source: "stdin".to_string(),
        sha256,
        size_bytes,
    });
    Ok(())
}

/// Without `--prompt` or `--goal`, Cook runs the `--task-url` task exactly as
/// cook-batch children do. The prompt is a pure function of the task URL, so it
/// is derived on every path, including a controller that carries a compiled
/// attempt plan. An explicit `--goal` is the dispatched prompt instead (#18467).
pub(super) fn default_cook_prompt_from_task_url(args: &mut AgentTaskCookArgs) {
    if args.dispatch.prompt.is_some() || args.prompt_snapshot.is_some() || args.goal.is_some() {
        return;
    }
    if let Some(task_url) = args.dispatch.task_url.as_deref() {
        args.dispatch.prompt = Some(super::cook_batch::default_task_prompt(task_url));
    }
}

/// Preview uses the same literal snapshot contract as live Cook, but bounds the
/// original stream before the read-only planner begins.
pub(super) fn snapshot_cook_prompt_bounded(
    args: &mut AgentTaskCookArgs,
    timeout: Duration,
) -> homeboy::core::Result<()> {
    if args.prompt_snapshot.is_some() || args.attempt_plan.is_some() {
        return Ok(());
    }
    let content = homeboy::agents::agent_task_prompts::read_prompt_input_bounded("-", timeout)?;
    if content.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "prompt",
            "agent-task cook --prompt - received empty stdin",
            None,
            Some(vec!["Pipe a non-empty prompt, for example: homeboy agent-task cook --prompt - < task.md".to_string()]),
        ));
    }
    let size_bytes = content.len();
    let sha256 = format!(
        "sha256:{}",
        homeboy_engine_primitives::content_hash::sha256_hex(content.as_bytes())
    );
    args.prompt_snapshot = Some(super::args::CookPromptSnapshot {
        content,
        source: "stdin".to_string(),
        sha256,
        size_bytes,
    });
    Ok(())
}

/// Compile the one durable provider-cell plan used by local Cook and Lab handoff.
pub(crate) fn compile_cook_plan(
    args: &AgentTaskCookArgs,
    provision: Value,
    projected_evidence: Option<&[Value]>,
) -> homeboy::core::Result<AgentTaskPlan> {
    let pending_lookup = matches!(
        provision.get("action").and_then(Value::as_str),
        Some("lookup_pending" | "attestation_pending" | "planned_create" | "planned_unshallow")
    );
    let requested_workspace = (!pending_lookup)
        .then(|| {
            provision
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten();
    if requested_workspace.is_none() && !pending_lookup {
        return Err(homeboy::core::Error::internal_unexpected(
            "Cook destination provisioning did not return a task worktree path".to_string(),
        ));
    }
    if let Some(workspace) = requested_workspace.as_deref() {
        validate_cook_destination_identity(args, Path::new(workspace))?;
    }
    let mut dispatch = resolved_dispatch_args_for_cook(args)?;
    // Provisioning makes an explicit --cwd authoritative, otherwise this is the
    // resolved managed destination. Pass that exact linked worktree downstream.
    dispatch.cwd = None;
    dispatch.workspace = requested_workspace.clone();
    let admitted_evidence = admit_provider_evidence_inputs(&args.provider_evidence_inputs)?;
    // The controller store snapshots evidence before a deferred native
    // workspace is materialized. Its content-addressed paths are independent
    // of the eventual checkout, so keep the same projection in the durable
    // plan for both existing and deferred destinations.
    // Preview already planned and validated these immutable identities. Reuse
    // that projection and its allowlist instead of clearing declarations and
    // rejecting our own rewritten paths (or copying evidence during preview).
    let evidence = match projected_evidence {
        Some(evidence) => evidence.to_vec(),
        None => project_admitted_provider_evidence_inputs(
            &args.provider_evidence_inputs,
            &admitted_evidence,
        )?,
    };
    let projected_paths = projected_provider_evidence_paths(&evidence);
    rewrite_provider_evidence_prompt(
        &mut dispatch.prompt,
        &args.provider_evidence_inputs,
        &admitted_evidence,
        &evidence,
        &projected_paths,
    )?;
    let mut request =
        dispatch_service::resolve_dispatch_request(dispatch_command_for_cook(args, dispatch))?;
    let mut plan = match dispatch_service::build_controller_dispatch_plan(&mut request) {
        Ok(plan) => plan,
        // A managed promotion handle may be intentionally unavailable until
        // after provider execution. Keep the compiled task durable and let
        // promotion report its established controlled policy failure.
        Err(error)
            if request.workspace.as_deref() == args.to_worktree.as_deref()
                && error.message.contains(
                    "neither an existing directory nor a resolvable managed worktree handle",
                ) =>
        {
            request.workspace = None;
            dispatch_service::build_controller_dispatch_plan(&mut request)?
        }
        Err(error) => return Err(error),
    };
    plan.options.candidate_completion = args.candidate_completion;
    record_cook_provision(&mut plan, provision);
    if let Some(identity) = &args.repository_identity {
        plan.metadata["cook_repository_identity"] = identity.clone();
    }
    if let Some(workspace) = requested_workspace.as_deref() {
        agent_task_service_direct::bind_materialized_cook_component_workspace(
            &mut plan,
            Path::new(workspace),
            args.component.as_deref(),
        )?;
    }
    if let Some(resolution) = &args.base_resolution {
        plan.metadata["cook_base_resolution"] = resolution.clone();
    }
    if let Some(resolution) = &args.issue_resolution {
        plan.metadata["cook_issue_resolution"] = resolution.clone();
    }
    if let Some(snapshot) = &args.prompt_snapshot {
        for task in &mut plan.tasks {
            task.metadata["prompt_source"] = serde_json::json!(args.dispatch.prompt);
        }
        plan.metadata["prompt_input_v1"] = serde_json::to_value(snapshot)
            .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
    }
    for task in &mut plan.tasks {
        if pending_lookup {
            continue;
        }
        let root = task.workspace.root.as_deref().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "workspace",
                "Cook requires a bound task worktree",
                None,
                None,
            )
        })?;
        task.metadata["cook_workspace_identity"] = workspace_identity_attestation(Path::new(root))?;
    }
    // Part of the durable plan, so retries and continuations keep the same
    // attempt-workspace dependency policy as the promotion gates.
    for task in &mut plan.tasks {
        task.metadata["cook_hydrate_dependencies"] =
            serde_json::json!(!crate::commands::skip_deps_hydration());
    }
    record_cook_goal(&mut plan, args.goal.as_deref());
    if !args.provider_evidence_inputs.is_empty() {
        for task in &mut plan.tasks {
            if !task.executor.config.is_object() {
                task.executor.config = serde_json::json!({});
            }
            task.executor.config["evidence_inputs"] = serde_json::to_value(&evidence)
                .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
        }
    }
    // Admission must see Cook's pinned-route/rotation budget rather than the
    // broader configured policy. Callers resolve again after admission to
    // refresh the persisted effective-route disclosure.
    resolve_cook_execution_budget(args, &mut plan)?;
    let catalog = homeboy::agents::agent_task_provider::AgentTaskProviderCatalog::discover();
    validate_cook_provider_execution_plan(&plan, &catalog)?;
    Ok(plan)
}

/// Admit a compiled Cook plan for the selected placement and return the plan
/// readiness bound: the first trusted rotation route, with every skipped entry
/// recorded on the task. Compile and preview share the process-local readiness
/// cache, so preview reuses compile's probes instead of re-running them.
pub(super) fn validate_cook_provider_execution_plan(
    plan: &homeboy::agents::agent_tasks::AgentTaskPlan,
    catalog: &homeboy::agents::agent_task_provider::AgentTaskProviderCatalog,
) -> homeboy::core::Result<homeboy::agents::agent_tasks::AgentTaskPlan> {
    let mut readiness_cache =
        homeboy::agents::agent_task_provider::ProviderRuntimeReadinessCache::process_local();
    // A Lab-placed Cook proves provider readiness on the runner with the
    // runner's credentials; controller-local credentials must not reject it.
    let selected_plan = homeboy::agents::agent_task_provider::admit_plan_provider_dispatchability(
        &plan,
        &catalog,
        &mut readiness_cache,
        homeboy_core::parsed_command_preflight::captured_result()
            .as_ref()
            .map(|result| &result.placement),
    )?;
    Ok(selected_plan)
}
