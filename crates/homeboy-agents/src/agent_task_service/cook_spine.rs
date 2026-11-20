//! Cook orchestration spine, moved whole without reordering.

use super::cook::*;

/// The Cook spine, bound to explicit recipe and lifecycle roots. Every durable
/// write on this path resolves through the two passed stores, so a caller can
/// place a Cook's recipe and its lifecycle records wherever it owns them
/// instead of wherever the process environment happens to point (#7505).
pub(super) fn run_cook_spine(
    store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    mut options: CookRequest,
    executor: SharedAgentTaskExecutor,
    side_effects: &mut CookSideEffects<'_>,
    durable_observer: Option<&CookProgressObserver<'_>>,
    mode: CookMode,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    let stores = CookStores {
        recipe: store,
        lifecycle: lifecycle_store,
    };
    // The local detached launcher persists this fence before spawn. Recheck it
    // at each durable/external boundary so a dead launcher cannot revive work.
    lifecycle_store.require_detached_cook_handoff_fence_open(&options.identity.cook_id)?;
    // Validate new Cooks before provider discovery, workspace staging, recipe
    // persistence, or a detached handoff can spend provider work. Historical
    // immutable recipes retain their persisted behavior and receive actionable
    // exhaustion guidance if their legacy budget reaches remediation.
    if !store.recipe_exists(&options.identity.cook_id) {
        validate_effective_cook_budget(
            options.retry_policy.max_attempts,
            &options.identity.initial_plan.options.execution_budget,
        )?;
    }
    if options.gates.verify_ci && options.finalization.no_finalize {
        return Err(Error::validation_invalid_argument(
            "verify_ci",
            "CI-deferred verification requires PR publication",
            None,
            None,
        ));
    }
    if !options.gates.gate_environment.data_inputs.is_empty() {
        crate::agent_task_gate::data_inputs::admit(
            &mut options.gates.gate_environment,
            &crate::agent_task_gate::data_inputs::store_root()?,
        )?;
    }
    project_controller_owned_gate_contract(&mut options);
    project_initial_finalizing_review_form_contract(&mut options)?;
    project_attempt_prompt_template(&mut options);
    validate_reviewer_visible_gates(&options.gates.verify, Some(&options.identity.cook_id))?;
    // A configured provider is controller authority. Resolve it before an
    // external runner can spend a provider attempt; explicit transports are
    // caller-owned overrides and retain their existing behavior.
    let verification_pending_continuation = lifecycle_store
        .read_record(&options.identity.initial_run_id)
        .ok()
        .is_some_and(|record| {
            // A dependency rebase re-arms verification on the recovery
            // checkpoint itself; that phase is recovery authority, not a
            // delivery fact.
            super::CookDeliveryArtifacts::from_run_metadata(&record.metadata)
                .source_checkpoint
                .is_some_and(|checkpoint| checkpoint["phase"] == "verification_pending")
                || super::CookDelivery::from_run_metadata(&record.metadata)
                    .is_some_and(|delivery| delivery.awaits_verification())
        });
    let persisted_finalization = lifecycle_store
        .read_record(&options.identity.initial_run_id)
        .ok()
        .is_some_and(|record| {
            super::CookDelivery::from_run_metadata(&record.metadata)
                .is_some_and(|delivery| delivery.has_publication_receipt())
        });
    // The durable reconstruction boundary must exist before an external provider
    // can accept the first attempt.
    let adopted_model = if persisted_finalization || verification_pending_continuation {
        None
    } else {
        lifecycle_store
            .read_record(&options.identity.initial_run_id)
            .ok()
            .map(|record| adopted_attempt_is_ready_for_cook_continuation(lifecycle_store, &record))
            .transpose()?
            .flatten()
    };
    // A form-only continuation has already appended and persisted its exact
    // attempt. Re-persisting it as a fresh initial recipe would falsely look
    // like an unsafe post-gate correction because the durable lineage now has
    // more attempts than the caller's one-attempt input.
    let mut admission_key = None;
    if !store.recipe_exists(&options.identity.cook_id) {
        lifecycle_store.require_detached_cook_handoff_fence_open(&options.identity.cook_id)?;
        // Held until this admission is durable, so concurrent submitters with
        // one key admit exactly one Cook (#18450).
        if let Some(key) =
            options.identity.initial_plan.metadata[super::COOK_ADMISSION_KEY_METADATA].as_str()
        {
            admission_key = Some(super::cook_admission_key::acquire(
                store,
                lifecycle_store,
                key,
            )?);
        }
    }
    let recipe_materialization = match store.load_recipe(&options.identity.cook_id) {
        Ok(recipe)
            if recipe
                .attempts
                .iter()
                .any(|attempt| attempt.run_id == options.identity.initial_run_id) =>
        {
            InitialRecipeMaterialization::reused(recipe)
        }
        Ok(_) | Err(_) => store.persist_initial_recipe_with_outcome(&options)?,
    };
    let existing_recipe = !recipe_materialization.created;
    let recipe = recipe_materialization.recipe;
    // A recipe can survive an interruption before its first lifecycle record.
    // Resume from the validated durable inputs so ambient transport state cannot
    // turn replay into a conflicting new cook.
    let requested_run_id = options.identity.initial_run_id.clone();
    let requested_record = lifecycle_store.read_record(&requested_run_id).ok();
    let local_placement_override = options.provider_transport.attempt_dispatcher.is_none()
        && requested_record
            .as_ref()
            .is_some_and(|record| transport_admission_reset_available(Some(record)));
    let mut options = if existing_recipe {
        let adoption =
            adopted_model.is_some() || matches!(mode, CookMode::Adopt | CookMode::ContinueTerminal);
        let mut reconstructed = if local_placement_override && !adoption {
            super::reconstruct_options_without_dispatcher(&recipe)?
        } else {
            super::reconstruct_options_with_dispatcher(
                &recipe,
                options.provider_transport.attempt_dispatcher,
            )?
        };
        if let Some(attempt) = recipe
            .attempts
            .iter()
            .find(|attempt| attempt.run_id == requested_run_id)
        {
            reconstructed.identity.initial_run_id = attempt.run_id.clone();
            reconstructed.identity.initial_plan = attempt.plan.clone();
            if lifecycle_store.record_exists(&attempt.run_id)? {
                // The recipe freezes the task-worktree handle, while the
                // durable run plan freezes the baseline-bound continuation.
                // Resolve the former back to its active path: the baseline is
                // execution evidence, not a task-worktree identity.
                let continuation_plan = lifecycle_store.read_controller_plan(&attempt.run_id)?;
                rebind_baseline_continuation_workspace(&mut reconstructed, &continuation_plan)?;
                reconstructed.identity.initial_plan = continuation_plan;
                // Recipe-first correction can survive an interruption before
                // the derived controller plan is rewritten.
                if attempt.plan.metadata["cook_base_resolution"]["source"] == "remote_head"
                    && reconstructed.identity.initial_plan.metadata["cook_base_resolution"]
                        ["source"]
                        == "compatibility_fallback"
                {
                    reconstructed.identity.initial_plan.metadata["cook_base_resolution"] =
                        attempt.plan.metadata["cook_base_resolution"].clone();
                    lifecycle_store.write_controller_plan(
                        &attempt.run_id,
                        &reconstructed.identity.initial_plan,
                    )?;
                }
            }
        }
        reconstructed
    } else {
        // The fresh request is the first attempt's live policy; it must gate
        // where its persisted recipe says, not only after a reconstruction.
        let mut options = options;
        options.gates = super::cook_recipe::admitted_gate_policy(&options)?;
        options
    };
    if existing_recipe {
        recover_compatibility_cook_base(store, lifecycle_store, &mut options)?;
    }
    // Recipe persistence and lifecycle materialization are a recoverable saga.
    // Complete it before any capacity, workspace, or provider-facing work so a
    // controller interruption leaves a status-addressable, resumable attempt.
    let materialized_by_invocation = materialize_initial_cook_attempt_with_stores_outcome(
        store,
        lifecycle_store,
        &options,
        recipe_materialization.created,
    )
    .map_err(|mut error| {
        if !error.details.is_object() {
            error.details = serde_json::json!({});
        }
        error.details["cook_admission_run_id"] =
            Value::String(options.identity.initial_run_id.clone());
        error
    })?;
    // Admission completed the recipe/lifecycle saga. Verify the exact record
    // before publishing it, then make the handle visible before any local
    // workspace, base, or capacity work can block the foreground client.
    let materialized_run = lifecycle_store.read_record(&options.identity.initial_run_id)?;
    if materialized_run.run_id != options.identity.initial_run_id {
        return Err(Error::internal_unexpected(
            "materialized Cook lifecycle record does not match its initial run id",
        ));
    }
    if let Some(admission_key) = admission_key {
        admission_key.claim(&options.identity.cook_id)?;
    }
    // Snapshot before report_cook_progress overwrites the phase. Older Cooks
    // have no started notification marker, so a continuation must not label a
    // previously running or completed attempt as newly started.
    let previously_started = materialized_run
        .metadata
        .get("cook_progress")
        .and_then(|progress| progress.get("phase"))
        .and_then(Value::as_str)
        .is_some_and(|phase| phase != "durable_identity");
    report_cook_progress(
        lifecycle_store,
        durable_observer,
        &options.identity.cook_id,
        &options.identity.initial_run_id,
        "durable_identity",
        1,
        None,
    )?;
    let notify_component = cook_component(&options);
    crate::agent_task_notify::cook_started(
        lifecycle_store,
        &options.identity.cook_id,
        &options.identity.initial_run_id,
        &options.finalization.title,
        notify_component.as_deref(),
        &options.finalization.base,
        options.retry_policy.max_attempts,
        &options
            .identity
            .initial_plan
            .tasks
            .first()
            .map(|task| task.executor.backend.clone())
            .unwrap_or_default(),
        previously_started,
    );
    // Canonicalization and native-worktree discovery can block on provider
    // runtime state. The recipe and lifecycle attempt above must therefore own
    // their result before either check begins.
    let startup_cook_id = options.identity.cook_id.clone();
    let startup_run_id = options.identity.initial_run_id.clone();
    run_cook_startup_phase(
        lifecycle_store,
        durable_observer,
        &startup_cook_id,
        &startup_run_id,
        "workspace_provider_canonicalization",
        || canonicalize_cook_provider_workspace(&mut options),
    )
    .map_err(|mut error| {
        error.details["cook_materialized_by_invocation"] = materialized_by_invocation.into();
        error
    })?;
    if !existing_recipe
        && !persisted_finalization
        && !verification_pending_continuation
        && !cook_workspace_lookup_pending(&options.identity.initial_plan)
        && options.provider_transport.attempt_dispatcher.is_none()
        && options.workspace.source_worktree_path.is_none()
        && options.provider_transport.provider_invocation.is_none()
    {
        run_cook_startup_phase(
            lifecycle_store,
            durable_observer,
            &options.identity.cook_id,
            &options.identity.initial_run_id,
            "workspace_provider_preflight",
            || preflight_initial_cook_workspace_provider(&options),
        )
        .map_err(|mut error| {
            error.details["cook_materialized_by_invocation"] = materialized_by_invocation.into();
            error
        })?;
    }
    // The destination marker and its owner's driver lock are authoritative,
    // including for existing and explicit workspace paths. Yield before dispatch.
    if !persisted_finalization
        && !verification_pending_continuation
        && cook_attempt_needs_execution_with_store(
            lifecycle_store,
            &options.identity.initial_run_id,
        )
        && !cook_workspace_lookup_pending(&options.identity.initial_plan)
        && !cook_has_provider_execution_in_store(lifecycle_store, &options.identity.cook_id)?
    {
        if let Some(report) = supersede_by_live_destination_owner(stores, &options)? {
            return Ok(report);
        }
    }
    // A re-cook of the same issue derives the same native worktree, which can
    // still hold the previous failed Cook's uncommitted candidate. Preserve it
    // under a recoverable ref and start from a clean base, before any
    // workspace validation, adoption, or Lab snapshot observes it (#15775).
    if !persisted_finalization
        && !verification_pending_continuation
        && cook_attempt_needs_execution_with_store(
            lifecycle_store,
            &options.identity.initial_run_id,
        )
        && !cook_workspace_lookup_pending(&options.identity.initial_plan)
        && !cook_has_provider_execution_in_store(lifecycle_store, &options.identity.cook_id)?
        && matches!(
            tracked_promotion_continuation_in_store(lifecycle_store, &options),
            Ok(None)
        )
        // Only a destination owned by a different, terminal, unsuccessful run
        // enters the phase; every other Cook keeps its startup sequence.
        && matches!(
            super::cook_prior_candidate::prior_failed_attempt_owner(lifecycle_store, &options),
            Ok(Some(_))
        )
    {
        run_cook_startup_phase(
            lifecycle_store,
            durable_observer,
            &options.identity.cook_id,
            &options.identity.initial_run_id,
            "prior_attempt_candidate_preservation",
            || {
                super::cook_prior_candidate::preserve_prior_attempt_candidate(
                    lifecycle_store,
                    &options,
                )
            },
        )
        .map_err(|mut error| {
            error.details["cook_materialized_by_invocation"] = materialized_by_invocation.into();
            error
        })?;
    }
    // Identity checks are read-only and precede remote access. State repair
    // itself runs only after base capture, in destination convergence below.
    if cook_attempt_needs_execution_with_store(lifecycle_store, &options.identity.initial_run_id)
        && !cook_workspace_lookup_pending(&options.identity.initial_plan)
    {
        if let Some(source) = options.workspace.source_worktree_path.as_deref() {
            homeboy_core::worktree_provider::require_linked_worktree_mutation_path(
                source,
                &options.workspace.to_worktree,
            )?;
        }
    }
    // Base resolution reaches origin. Keep its transport failure behind the
    // recipe/run saga so retry and replay have durable zero-provider evidence.
    let cook_id = options.identity.cook_id.clone();
    let run_id = options.identity.initial_run_id.clone();
    // Even a runner-owned workspace can wait on this Cook's shared capture
    // lock, so keep the durable attempt live regardless of whether the
    // controller has a local path to pin.
    let base_capture = run_cook_startup_phase(
        lifecycle_store,
        durable_observer,
        &cook_id,
        &run_id,
        "workspace_base_capture",
        || pin_and_persist_initial_cook_workspace_base(store, lifecycle_store, &mut options),
    );
    base_capture.map_err(|error| {
        let mut error = error;
        error.details["cook_materialized_by_invocation"] = Value::Bool(true);
        error
    })?;
    // A persisted recipe can replace the just-validated inputs. Re-check its
    // workspace and candidate topology before it reaches transport preparation
    // or a resumed attempt.
    // A detached transport may own workspace materialization on the runner.
    // Its initial handoff has no controller-local source path to validate yet;
    // requiring one here turns an accepted detached retry into a local failure.
    if cook_attempt_needs_execution_with_store(lifecycle_store, &options.identity.initial_run_id)
        && !cook_workspace_lookup_pending(&options.identity.initial_plan)
        && (options.provider_transport.attempt_dispatcher.is_none()
            || options.workspace.source_worktree_path.is_some())
    {
        let workspace_base = run_cook_startup_phase(
            lifecycle_store,
            durable_observer,
            &options.identity.cook_id,
            &options.identity.initial_run_id,
            "workspace_base_convergence",
            || {
                validate_cook_workspace_with_adopted_candidate(
                    lifecycle_store,
                    &options,
                    verification_pending_continuation
                        || (options.workspace.source_worktree_path.is_some()
                            && !cook_uses_explicit_cwd_workspace(&options)),
                )
            },
        )
        .map_err(|mut error| {
            error.details["cook_materialized_by_invocation"] = materialized_by_invocation.into();
            error
        })?;
        if let Some(workspace_base) = workspace_base {
            match workspace_base {
                CookWorkspaceBaseValidation::Snapshot(snapshot) => {
                    lifecycle_store.record_metadata_value(
                        &options.identity.initial_run_id,
                        "cook_workspace_base_snapshot",
                        snapshot,
                    )?;
                }
                CookWorkspaceBaseValidation::Convergence(evidence) => {
                    record_cook_workspace_base_convergence(
                        lifecycle_store,
                        &options.identity.initial_run_id,
                        evidence,
                    )?;
                }
            }
        }
    }
    validate_cook_candidate_group(&options.identity.initial_plan)?;
    // Surface disk pressure in orchestration terms BEFORE the byte reserve:
    // an advisory below the reserve is recorded on the run, and an exhausted
    // filesystem refuses admission here with measured, attributed evidence
    // rather than leaving an unrelated build to fail opaque on the reserve.
    let disk_pressure = run_cook_startup_phase(
        lifecycle_store,
        durable_observer,
        &options.identity.cook_id,
        &options.identity.initial_run_id,
        "workspace_disk_pressure",
        || preflight_cook_materialization_capacity(lifecycle_store),
    )?;
    if !disk_pressure.is_exhausted() {
        record_cook_disk_pressure_advisory(
            lifecycle_store,
            &options.identity.initial_run_id,
            &disk_pressure,
        )?;
    } else if let Some(error) = disk_pressure.error() {
        let mut error = error;
        error.details["cook_disk_pressure_preflight"] = serde_json::json!({
            "subject": disk_pressure.subject,
            "path": disk_pressure.path,
            "reclaim_actions": [
                "homeboy worktree reclaim --apply",
                "homeboy cleanup --apply",
            ],
        });
        return Err(error);
    }
    // Reserve the source tree's projected copy before the scheduler creates its
    // controller scratch lease or detached workspace. This includes dependency
    // trees (for example node_modules and vendor), whose inode demand can be
    // decisive even when their byte footprint is small.
    let _materialization_capacity = cook_startup_workspace(&options)
        .map(|workspace| {
            run_cook_startup_phase(
                lifecycle_store,
                durable_observer,
                &options.identity.cook_id,
                &options.identity.initial_run_id,
                "workspace_capacity_reservation",
                || reserve_cook_materialization_capacity(lifecycle_store, workspace),
            )
        })
        .transpose()?;
    lifecycle_store.require_detached_cook_handoff_fence_open(&options.identity.cook_id)?;
    let Some(_runtime_generation) = super::execution_step::pin_execution_generation_or_defer(
        lifecycle_store,
        &options.identity.initial_run_id,
    )?
    else {
        return Ok(stores.report(CookReportInput {
            cook_id: options.identity.cook_id.clone(),
            status: CookStatus::Queued.as_str(),
            disposition: CookDisposition::InFlight,
            attempts: Vec::new(),
            finalization: None,
            stop_reason: Some("queued before provisioning behind runtime promotion".into()),
            exit_code: 75,
            invocation_latest_run_id: Some(&options.identity.initial_run_id),
        }));
    };
    if cook_workspace_lookup_pending(&options.identity.initial_plan) {
        report_cook_progress(
            lifecycle_store,
            durable_observer,
            &options.identity.cook_id,
            &options.identity.initial_run_id,
            "worktree_provider_lookup",
            1,
            Some("starting bounded provider workspace lookup"),
        )?;
        // Core owns process-tree supervision; Cook owns the durable lifecycle
        // record. Keep that record live while the external provider command runs.
        let (lookup_stop, lookup_wait) = mpsc::channel();
        let lookup_cook_id = options.identity.cook_id.clone();
        let lookup_run_id = options.identity.initial_run_id.clone();
        // Bound before the `move` closure: naming the store inside it would
        // capture the store itself by value rather than this borrow.
        let lookup_lifecycle_store = &lifecycle_store;
        let lookup_result = std::thread::scope(|scope| {
            scope.spawn(homeboy_core::observation::phase_timing::inherit(
                move || {
                    let mut next_heartbeat = Instant::now() + COOK_HEARTBEAT_INTERVAL;
                    loop {
                        match lookup_wait.recv_timeout(COOK_PROVIDER_TERMINAL_POLL_INTERVAL) {
                            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                        }
                        if lookup_lifecycle_store
                            .read_record(&lookup_run_id)
                            .ok()
                            .is_some_and(|record| {
                                record.state()
                                    == homeboy_lifecycle_contract::RunExecutionState::Cancelled
                            })
                        {
                            break;
                        }
                        if Instant::now() >= next_heartbeat {
                            next_heartbeat = Instant::now() + COOK_HEARTBEAT_INTERVAL;
                            let phase = lookup_lifecycle_store
                                .read_record(&lookup_run_id)
                                .ok()
                                .and_then(|record| {
                                    record.metadata["cook_progress"]["phase"]
                                        .as_str()
                                        .map(str::to_string)
                                })
                                .filter(|phase| {
                                    matches!(
                                        phase.as_str(),
                                        "worktree_provider_lookup" | "worktree_provider_ensure"
                                    )
                                })
                                .unwrap_or_else(|| "worktree_provider_lookup".to_string());
                            let _ = report_cook_progress(
                                lookup_lifecycle_store,
                                durable_observer,
                                &lookup_cook_id,
                                &lookup_run_id,
                                &phase,
                                1,
                                Some("bounded provider workspace lookup is still running"),
                            );
                        }
                    }
                },
            ));
            let result = materialize_pending_cook_workspace(lifecycle_store, &mut options);
            let _ = lookup_stop.send(());
            result
        });
        // The pre-dispatch owner check above cannot see a destination that
        // was still being looked up. A concurrent Cook on the same issue may
        // have claimed it meanwhile, whether this lookup then found its
        // worktree or lost the marker claim: follow that Cook (#18307).
        let lost_claim = lookup_result.as_ref().err().is_none_or(|error| {
            std::error::Error::source(error).is_some_and(|source| {
                source.is::<crate::agent_task_promotion::destination::DestinationOwnerMismatch>()
            })
        });
        if lost_claim {
            if let Some(report) = supersede_by_live_destination_owner(stores, &options)? {
                return Ok(report);
            }
        }
        if let Err(error) = lookup_result {
            if lifecycle_store
                .read_record(&options.identity.initial_run_id)
                .ok()
                .is_some_and(|record| {
                    record.state() == homeboy_lifecycle_contract::RunExecutionState::Cancelled
                })
            {
                return Ok(stores.report(CookReportInput {
                    cook_id: options.identity.cook_id.clone(),
                    status: CookStatus::Cancelled.as_str(),
                    disposition: CookDisposition::Terminal,
                    attempts: Vec::new(),
                    finalization: None,
                    stop_reason: Some(error.to_string()),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&options.identity.initial_run_id),
                }));
            }
            let error = with_pre_execution_phase(error, "worktree_provider_lookup");
            CookExecutionPreparation::new(store, lifecycle_store).record_pre_execution_failure(
                &options.identity.cook_id,
                &options.identity.initial_run_id,
                "worktree_provider_lookup",
                &error,
            )?;
            return Ok(pre_execution_failure_report(
                stores,
                options.identity.cook_id.clone(),
                Vec::new(),
                pre_execution_failure_details(
                    lifecycle_store
                        .read_record(&options.identity.initial_run_id)
                        .ok()
                        .as_ref(),
                    &error,
                ),
                error,
                Some(&options.identity.initial_run_id),
            ));
        }
    }
    record_active_cook_worktree_warning(lifecycle_store, &options)?;
    // Admission through the first dispatch observes each runner identity once.
    let mut admission_identities = crate::agent_task_gate::placement::runner_identity_scope();
    if options.gates.has_npm_run_declaration() {
        let gate_workspace = super::cook_promotion::component_workspace_path(&options)?
            .or_else(|| options.workspace.source_worktree_path.clone())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "workspace",
                    "Cook requires a workspace before gate declaration preflight",
                    Some(options.workspace.to_worktree.clone()),
                    None,
                )
            })?;
        if let Err(error) = options.gates.preflight_declarations(&gate_workspace) {
            let error = with_pre_execution_phase(error, "gate_declaration_preflight");
            record_pre_execution_failure(
                lifecycle_store,
                &options.identity.initial_plan,
                &options.identity.initial_run_id,
                &error,
                "gate_declaration_preflight",
            )?;
            return Ok(pre_execution_failure_report(
                stores,
                options.identity.cook_id.clone(),
                Vec::new(),
                pre_execution_failure_details(
                    agent_task_lifecycle::exact_record_in_store(
                        lifecycle_store,
                        &options.identity.initial_run_id,
                    )
                    .ok()
                    .as_ref(),
                    &error,
                ),
                error,
                Some(&options.identity.initial_run_id),
            ));
        }
    }
    let required_toolchains = options.gates.required_toolchains();
    let preflight = if required_toolchains.is_empty()
        && options.gates.gate_target_probes().is_empty()
        && options.gates.gate_package_artifacts.is_empty()
        && options.gates.gate_environment.extension_inputs.is_empty()
        && options.gates.gate_environment.data_inputs.is_empty()
    {
        Ok(())
    } else {
        let gate_workspace = super::cook_promotion::component_workspace_path(&options)?
            .or_else(|| options.workspace.source_worktree_path.clone())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "workspace",
                    "Cook requires a workspace before gate toolchain preflight",
                    Some(options.workspace.to_worktree.clone()),
                    None,
                )
            })?;
        crate::agent_task_gate::preflight_verify_gate_options(&gate_workspace, &options.gates)
    };
    let preflight = defer_gate_preflight_transport_failure(preflight);
    if let Err(error) = preflight {
        let error = with_pre_execution_phase(error, "gate_toolchain_preflight");
        record_pre_execution_failure(
            lifecycle_store,
            &options.identity.initial_plan,
            &options.identity.initial_run_id,
            &error,
            "gate_toolchain_preflight",
        )?;
        return Ok(pre_execution_failure_report(
            stores,
            options.identity.cook_id.clone(),
            Vec::new(),
            pre_execution_failure_details(
                agent_task_lifecycle::exact_record_in_store(
                    lifecycle_store,
                    &options.identity.initial_run_id,
                )
                .ok()
                .as_ref(),
                &error,
            ),
            error,
            Some(&options.identity.initial_run_id),
        ));
    }
    // Promotion gate setup installs dependencies on this controller with the
    // providers' declared commands (e.g. `pnpm install --frozen-lockfile` for
    // a pnpm lockfile). A missing package manager would otherwise fail only
    // after a provider attempt was paid for, stranding its candidate (#15364).
    if options.gates.hydrate_dependencies && !verification_pending_continuation {
        if let Some(error) = dependency_tool_preflight_error(&options)? {
            let error = with_pre_execution_phase(error, "dependency_tool_preflight");
            record_pre_execution_failure(
                lifecycle_store,
                &options.identity.initial_plan,
                &options.identity.initial_run_id,
                &error,
                "dependency_tool_preflight",
            )?;
            return Ok(pre_execution_failure_report(
                stores,
                options.identity.cook_id.clone(),
                Vec::new(),
                pre_execution_failure_details(
                    agent_task_lifecycle::exact_record_in_store(
                        lifecycle_store,
                        &options.identity.initial_run_id,
                    )
                    .ok()
                    .as_ref(),
                    &error,
                ),
                error,
                Some(&options.identity.initial_run_id),
            ));
        }
    }
    if !verification_pending_continuation
        && (!options.gates.verify.is_empty()
            || !options.gates.private_verify.is_empty()
            || options.gates.test_execution_plan.is_some())
    {
        if let Some(base) = options.workspace.task_base_sha.clone().or_else(|| {
            options.identity.initial_plan.metadata["cook_workspace_base"]["sha"]
                .as_str()
                .map(str::to_string)
        }) {
            let admission = super::cook_baseline::admit_base_gates(
                &options,
                &lifecycle_store.open_observation_initialized()?,
                &base,
            )?;
            lifecycle_store.record_metadata_value(
                &options.identity.initial_run_id,
                "cook_gate_baseline",
                admission.clone(),
            )?;
            if options.gates.baseline_policy == crate::agent_task_gate::BaselinePolicy::Strict
                && admission
                    .as_array()
                    .is_some_and(|gates| gates.iter().any(|gate| gate["status"] == "baseline_red"))
            {
                return Ok(stores.report(CookReportInput {
                    cook_id: options.identity.cook_id.clone(),
                    status: CookStatus::Failed.as_str(),
                    disposition: CookDisposition::Terminal,
                    attempts: Vec::new(),
                    finalization: None,
                    stop_reason: Some(
                        "baseline_red: strict baseline policy stopped before provider dispatch"
                            .to_string(),
                    ),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&options.identity.initial_run_id),
                }));
            }
        }
    }
    // A repro must fail on the pinned base before any provider work starts;
    // one that passes there does not reproduce the issue (#17653).
    if !options.gates.repro.is_empty()
        && !verification_pending_continuation
        && recipe.attempts.len() == 1
        && cook_attempt_needs_execution_with_store(
            lifecycle_store,
            &options.identity.initial_run_id,
        )
        && !cook_has_provider_execution_in_store(lifecycle_store, &options.identity.cook_id)?
        && super::cook_repro::recorded_repro_base_evidence(
            lifecycle_store,
            &options.identity.cook_id,
        )
        .is_none_or(|evidence| !evidence.reproduces())
    {
        let base_sha = options.workspace.task_base_sha.clone().or_else(|| {
            options.identity.initial_plan.metadata["cook_workspace_base"]["sha"]
                .as_str()
                .map(str::to_string)
        });
        let evidence = run_cook_startup_phase(
            lifecycle_store,
            durable_observer,
            &options.identity.cook_id,
            &options.identity.initial_run_id,
            "repro_base_check",
            || {
                let base_sha = base_sha.as_deref().ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "repro",
                        "--repro needs a pinned Cook base to run against before dispatch",
                        Some(options.workspace.to_worktree.clone()),
                        None,
                    )
                })?;
                let evidence = super::cook_repro::run_repro_on_base(&options, base_sha)?;
                super::cook_repro::record_repro_base_evidence(
                    lifecycle_store,
                    &options.identity.cook_id,
                    &options.identity.initial_run_id,
                    &evidence,
                )?;
                Ok(evidence)
            },
        );
        let evidence = match evidence {
            Ok(evidence) => evidence,
            Err(error) => {
                let error = with_pre_execution_phase(error, "repro_base_check");
                record_pre_execution_failure(
                    lifecycle_store,
                    &options.identity.initial_plan,
                    &options.identity.initial_run_id,
                    &error,
                    "repro_base_check",
                )?;
                return Ok(pre_execution_failure_report(
                    stores,
                    options.identity.cook_id.clone(),
                    Vec::new(),
                    pre_execution_failure_details(
                        agent_task_lifecycle::exact_record_in_store(
                            lifecycle_store,
                            &options.identity.initial_run_id,
                        )
                        .ok()
                        .as_ref(),
                        &error,
                    ),
                    error,
                    Some(&options.identity.initial_run_id),
                ));
            }
        };
        if !evidence.reproduces() {
            let stop_reason = super::cook_repro::repro_passes_on_base_stop_reason(&evidence);
            let mut error = with_pre_execution_phase(
                Error::validation_invalid_argument(
                    "repro",
                    stop_reason.clone(),
                    Some(evidence.base_sha.clone()),
                    None,
                ),
                "repro_base_check",
            );
            error.details["classification"] =
                Value::String(super::cook_repro::REPRO_PASSES_ON_BASE.to_string());
            record_pre_execution_failure(
                lifecycle_store,
                &options.identity.initial_plan,
                &options.identity.initial_run_id,
                &error,
                "repro_base_check",
            )?;
            let mut report = stores.report(CookReportInput {
                cook_id: options.identity.cook_id.clone(),
                status: CookStatus::ReproPassesOnBase.as_str(),
                disposition: CookDisposition::Terminal,
                attempts: Vec::new(),
                finalization: None,
                stop_reason: Some(stop_reason),
                exit_code: 1,
                invocation_latest_run_id: Some(&options.identity.initial_run_id),
            });
            report.value.terminal_phase = Some("repro_base_check".to_string());
            report.value.terminal_failure_classification =
                Some(super::cook_repro::REPRO_PASSES_ON_BASE.to_string());
            return Ok(report);
        }
    }
    if let Some(latest_attempt) = recipe.attempts.last() {
        // Recipe attempts retain logical input provenance. The controller plan
        // is the canonical dispatch contract once pending workspace resolution
        // has authenticated and projected a concrete root into it.
        let plan = if lifecycle_store.record_exists(&latest_attempt.run_id)? {
            lifecycle_store.read_controller_plan(&latest_attempt.run_id)?
        } else {
            latest_attempt.plan.clone()
        };
        materialize_cook_attempt_with_stores(
            store,
            lifecycle_store,
            &recipe.cook_id,
            &latest_attempt.run_id,
            &plan,
        )?;
    }
    // Hold the authoritative generation pin before transport preparation can
    // provision a destination or contact a provider. Contention only commits
    // a durable, budget-free successor above.
    if !verification_pending_continuation {
        if let Some(dispatcher) = &options.provider_transport.attempt_dispatcher {
            if let Err(error) = dispatcher.prepare_for_cook() {
                lifecycle_store.record_pre_execution_failure(
                    &options.identity.initial_run_id,
                    &options.identity.initial_plan,
                    dispatcher.pre_execution_failure_phase(),
                    &error,
                )?;
                return Err(error);
            }
        }
    }
    report_cook_progress(
        lifecycle_store,
        durable_observer,
        &options.identity.cook_id,
        &options.identity.initial_run_id,
        "provider_ready",
        1,
        None,
    )?;
    let max_attempts = options.retry_policy.max_attempts.max(1);
    let mut attempts = Vec::new();
    // A retry may already be durably dispatched when this controller resumes.
    // Continue from that exact recorded attempt rather than re-entering the
    // original attempt and re-binding its immutable recipe identity.
    let requested_attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == options.identity.initial_run_id)
        .map(|attempt| attempt.attempt)
        .unwrap_or(1);
    let resumed_run_id = resumable_cook_run_id_in_store(
        lifecycle_store,
        &recipe,
        &options.identity.cook_id,
        &options.identity.initial_run_id,
        requested_attempt,
        verification_pending_continuation,
    );
    let mut run_id = resumed_run_id
        .clone()
        .unwrap_or_else(|| options.identity.initial_run_id.clone());
    let mut next_plan = resumed_run_id
        .is_none()
        .then(|| options.identity.initial_plan.clone());
    let cook_id = options.identity.cook_id.clone();
    let mut budget_limit = None;
    let mut observed_budget_used = ExecutionBudgetUsage::default();
    let mut remediation_category_usage = ExecutionBudgetUsage::default();
    // Transient substrate failures of the current semantic attempt: when the
    // first was observed, and how many replacements they took (#17801).
    let mut transient_failures: Option<(u32, std::time::Instant, usize)> = None;

    let first_attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .map(|attempt| attempt.attempt)
        .unwrap_or(1);
    let attempt_limit = max_attempts;
    let mut attempt = first_attempt;
    while attempt <= attempt_limit {
        // Attempt boundary. Checked before any provider work is dispatched, so
        // an expired budget costs nothing further and the attempt that would
        // have run is never started rather than started and killed.
        if let Some(deadline) = expired_cook_deadline() {
            return Ok(cook_deadline_report(
                stores,
                cook_id,
                attempts,
                &run_id,
                &deadline,
                &format!("attempt {attempt} could be started"),
            ));
        }
        let plan = match next_plan.take() {
            Some(plan) => plan,
            None => lifecycle_store.read_controller_plan(&run_id)?,
        };
        // A lost controller watch must adopt the existing daemon result, not
        // consume another provider attempt or retain the transport failure.
        agent_task_lifecycle::recover_disconnected_runner_in_store(lifecycle_store, &run_id)?;
        let needs_execution = cook_attempt_needs_execution_with_store(lifecycle_store, &run_id);
        if needs_execution {
            // The local detached launcher persists this fence before spawn.
            // Recheck immediately before this attempt can publish provider work.
            lifecycle_store.require_detached_cook_handoff_fence_open(&cook_id)?;
            // `provider_start` is a durable lifecycle transition. Create the
            // record before reporting it so a new Cook run is observable before
            // any preflight or provider work can block.
            if !lifecycle_store.record_exists(&run_id)? {
                lifecycle_store.submit_plan_with_current_runtime(&plan, &run_id)?;
            }
            report_cook_progress(
                lifecycle_store,
                durable_observer,
                &cook_id,
                &run_id,
                if attempt == 1 {
                    "provider_start"
                } else {
                    "retry"
                },
                attempt,
                None,
            )?;
            // Attempt boundaries only. The first attempt is already covered by
            // the started event, and the fifteen-second heartbeat is internal
            // liveness with no decision attached to it.
            if attempt > 1 {
                crate::agent_task_notify::cook_retrying(
                    &cook_id,
                    &run_id,
                    notify_component.as_deref(),
                    attempt,
                    attempt_limit,
                );
            }
            let mut failed_dispatch_plan = None;
            let execution = (|| {
                let workspace_base = if options.provider_transport.attempt_dispatcher.is_none()
                    || options.workspace.source_worktree_path.is_some()
                {
                    validate_cook_workspace_with_adopted_candidate(
                        lifecycle_store,
                        &options,
                        options.workspace.source_worktree_path.is_some()
                            && !cook_uses_explicit_cwd_workspace(&options),
                    )?
                } else {
                    None
                };
                let base_snapshot = match workspace_base {
                    Some(CookWorkspaceBaseValidation::Snapshot(snapshot)) => Some(snapshot),
                    Some(CookWorkspaceBaseValidation::Convergence(evidence)) => {
                        record_cook_workspace_base_convergence(lifecycle_store, &run_id, evidence)?;
                        None
                    }
                    None => None,
                }
                .or_else(|| {
                    lifecycle_store
                        .read_record(&run_id)
                        .ok()
                        .and_then(|record| {
                            record.metadata.get("cook_workspace_base_snapshot").cloned()
                        })
                });
                if options.provider_transport.attempt_dispatcher.is_none() {
                    homeboy_core::cleanup::admit_reconstructable_artifact_work(
                        plan.tasks
                            .iter()
                            .filter_map(|task| task.workspace.root.as_ref())
                            .map(PathBuf::from)
                            .collect(),
                    )
                    .map_err(|error| {
                        with_pre_execution_phase(error, "worktree_capacity_admission")
                    })?;
                }
                let initial_baseline = if attempt == 1 {
                    materialize_initial_candidate_baseline(
                        &plan,
                        options.workspace.source_worktree_path.as_deref(),
                        &run_id,
                    )
                    .map_err(|error| {
                        with_pre_execution_phase(error, "materialize_initial_candidate_baseline")
                    })?
                } else {
                    None
                };
                // For follow-up attempts (attempt > 1), the plan's workspace.root
                // was set by a previous dispatch_cook_follow_up to a baseline
                // worktree path. If that worktree was reaped (e.g. by tmp cleanup,
                // disk-pressure cleanup, or git worktree prune), re-materialize it
                // at the same path so the provider preflight check passes.
                let re_materialized_baseline = if attempt > 1 && initial_baseline.is_none() {
                    let root = plan
                        .tasks
                        .first()
                        .and_then(|t| t.workspace.root.as_deref())
                        .map(std::path::Path::new);
                    match root {
                        Some(path) if !path.exists() => {
                            let source_run_id = plan.tasks[0]
                                .inputs["cook_loop"]["artifact_provenance"]["source_run_id"]
                                .as_str()
                                .ok_or_else(|| {
                                    with_pre_execution_phase(
                                        Error::validation_invalid_argument(
                                            "cook_loop.artifact_provenance.source_run_id",
                                            "follow-up plan missing source run id for baseline re-materialization",
                                            None,
                                            None,
                                        ),
                                        "re_materialize_follow_up_baseline",
                                    )
                                })?;
                            let promotion = persisted_promotion_for_attempt_in_store(
                                lifecycle_store,
                                source_run_id,
                            )?
                            .ok_or_else(|| {
                                with_pre_execution_phase(
                                    Error::validation_invalid_argument(
                                        "promotion",
                                        format!(
                                            "source attempt {source_run_id} has no persisted \
                                                 promotion for baseline re-materialization"
                                        ),
                                        Some(source_run_id.to_string()),
                                        None,
                                    ),
                                    "re_materialize_follow_up_baseline",
                                )
                            })?;
                            let task_id = &plan.tasks[0].task_id;
                            Some(
                                re_materialize_follow_up_baseline(
                                    &promotion,
                                    path,
                                    source_run_id,
                                    task_id,
                                )
                                .map_err(|error| {
                                    with_pre_execution_phase(
                                        error,
                                        "re_materialize_follow_up_baseline",
                                    )
                                })?,
                            )
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                let effective_baseline = initial_baseline
                    .as_ref()
                    .or(re_materialized_baseline.as_ref());
                // Live admission belongs to the scheduler on the execution
                // host. Keep the complete route policy intact so local
                // persistence and external transports retain restart authority.
                let mut dispatch_plan = plan.clone();
                if let Some(snapshot) = base_snapshot {
                    lifecycle_store.record_metadata_value(
                        &run_id,
                        "cook_workspace_base_snapshot",
                        snapshot.clone(),
                    )?;
                    for task in &mut dispatch_plan.tasks {
                        task.metadata["cook_workspace_base_snapshot"] = snapshot.clone();
                    }
                }
                if let Some(baseline) = effective_baseline {
                    for task in &mut dispatch_plan.tasks {
                        // The baseline is immutable evidence for this dispatch,
                        // never the durable workspace a retry continues in.
                        task.metadata["cook_continuation_workspace"] = serde_json::json!({
                            "candidate_source_root": options.workspace.source_worktree_path,
                            "task_workspace": {
                                "root": task.workspace.root.clone(),
                                "kind": task.workspace.kind.clone(),
                                "materialization": task.workspace.materialization.clone(),
                            },
                        });
                        task.workspace.root = Some(baseline.path.display().to_string());
                        task.metadata["cook_initial_candidate_baseline"] = serde_json::json!({
                            "source_root": options.workspace.source_worktree_path,
                            "commit": baseline.capability.commit(),
                            "tree": baseline.capability.tree(),
                        });
                    }
                }
                // A controller-owned baseline replaces the admitted workspace
                // for both local and detached dispatch. Attest that exact
                // replacement before either path can persist or hand it off.
                if effective_baseline.is_some()
                    || options.provider_transport.attempt_dispatcher.is_none()
                {
                    bind_dispatch_workspace_attestations(&mut dispatch_plan)?;
                }
                failed_dispatch_plan = Some(dispatch_plan.clone());
                if let Some(dispatcher) = &options.provider_transport.attempt_dispatcher {
                    admit_explicit_cook_workspace_before_provider(
                        lifecycle_store,
                        &options,
                        &run_id,
                    )?;
                    let dispatched = dispatcher.dispatch_attempt(
                        dispatch_plan,
                        &run_id,
                        effective_baseline.map(CookFollowUpBaseline::capability),
                    );
                    // A detached dispatcher hands the baseline to Lab staging,
                    // which materializes it from a separate controller job
                    // after `dispatch_attempt` returns. Dropping the guard at
                    // the end of this scope ran `git worktree remove` under
                    // that job, failing every first-attempt Lab Cook with
                    // "workspace identity does not match its admission
                    // attestation" (IO error). #15379 fixed the retry path
                    // only; preserve the dispatched baseline here as well and
                    // leave reaping to artifact retention (#15815). A rejected
                    // dispatch hands nothing off, so its baseline is still
                    // dropped (and removed) here as before.
                    if dispatched.is_ok() {
                        if let Some(baseline) = initial_baseline {
                            baseline.preserve_for_retry();
                        }
                        if let Some(baseline) = re_materialized_baseline {
                            baseline.preserve_for_retry();
                        }
                    }
                    dispatched
                } else {
                    admit_explicit_cook_workspace_before_provider(
                        lifecycle_store,
                        &options,
                        &run_id,
                    )?;
                    let (heartbeat_stop, heartbeat_wait) = mpsc::channel();
                    let heartbeat_run_id = run_id.clone();
                    let heartbeat_cook_id = cook_id.clone();
                    // Bind the probe to the workspace this dispatch actually
                    // hands the provider, not to the cook's configured
                    // destination: a follow-up attempt runs in a baseline
                    // worktree, and measuring the wrong tree would report
                    // "no files written" while the provider was editing.
                    let heartbeat_activity = CookActivityProbe::new(
                        dispatch_plan
                            .tasks
                            .first()
                            .and_then(|task| task.workspace.root.as_deref())
                            .map(PathBuf::from)
                            .or_else(|| options.workspace.source_worktree_path.clone()),
                    );
                    // The provider runs as a descendant of this controller
                    // process, so the controller pid is the root of the tree to
                    // sample — and, when a budget orders a stop, the root of
                    // the tree to terminate. `terminate_process_tree` excludes
                    // the calling process, so this ends the provider's work
                    // without ending the controller that has to record why.
                    let heartbeat_owner_pid = std::process::id();
                    // Resolved before the thread so a malformed policy document
                    // fails the attempt loudly. An operator who declared a stop
                    // budget and silently got none believes this host is
                    // protected and is not (#7015).
                    let supervision_policy = resolve_supervision_policy()?;
                    let supervision_backend = dispatch_plan
                        .tasks
                        .first()
                        .map(|task| task.executor.backend.clone());
                    let heartbeat_provider_timeout_ms = dispatch_plan
                        .tasks
                        .first()
                        .map(|task| {
                            crate::agent_task_timeout::effective_provider_timeout_ms(
                                task.limits.timeout_ms.or(dispatch_plan.options.timeout_ms),
                                task.limits.max_runtime_ms,
                            )
                        })
                        .unwrap_or(crate::agent_task_timeout::DEFAULT_PROVIDER_TIMEOUT_MS);
                    // Captured before the spawn: the budget lives in a
                    // thread-local, and the heartbeat runs on a thread of its
                    // own that would otherwise start unbudgeted.
                    let heartbeat_deadline = capture_cook_deadline();
                    // Bound before the `move` closure: naming the store inside
                    // it would capture the store itself by value rather than
                    // this borrow.
                    let heartbeat_lifecycle_store = &lifecycle_store;
                    std::thread::scope(|scope| {
                        scope.spawn(homeboy_core::observation::phase_timing::inherit(move || {
                            let mut supervisor =
                                CookSupervisor::new(supervision_policy, supervision_backend);
                            let provider_started_at = Instant::now();
                            let mut deadline_stop_issued = false;
                            let mut next_heartbeat = Instant::now() + COOK_HEARTBEAT_INTERVAL;
                            loop {
                                let poll_for = next_heartbeat
                                    .saturating_duration_since(Instant::now())
                                    .min(COOK_PROVIDER_TERMINAL_POLL_INTERVAL);
                                match heartbeat_wait.recv_timeout(poll_for) {
                                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                                }
                                // The scheduler records harvesting at provider exit.
                                // Stop provider supervision rather than overwriting
                                // subsequent phases with synthetic harvest heartbeats.
                                if !agent_task_lifecycle::has_active_provider_execution_in_store(heartbeat_lifecycle_store, &heartbeat_run_id,
                                )
                                .unwrap_or(true)
                                {
                                    break;
                                }
                                if Instant::now() < next_heartbeat {
                                    continue;
                                }
                                next_heartbeat = Instant::now() + COOK_HEARTBEAT_INTERVAL;
                                let activity_owner_pid = agent_task_lifecycle::running_owner_pid_in_store(heartbeat_lifecycle_store, &heartbeat_run_id,
                                )
                                .ok()
                                .flatten()
                                .unwrap_or(heartbeat_owner_pid);
                                let activity = heartbeat_activity.sample(activity_owner_pid);
                                let tick = supervisor.observe(&activity);
                                let detail = tick.detail_line();
                                let remaining_ms = heartbeat_provider_timeout_ms
                                    .saturating_sub(provider_started_at.elapsed().as_millis() as u64)
                                    .min(heartbeat_deadline
                                        .deadline()
                                        .map(|deadline| deadline.remaining_ms())
                                        .unwrap_or(u64::MAX));
                                let progress_detail = provider_timeout_heartbeat_detail(
                                    heartbeat_provider_timeout_ms,
                                    remaining_ms,
                                    detail.as_deref(),
                                    &activity,
                                );
                                let _ = report_cook_progress_with_activity(
                                    heartbeat_lifecycle_store,
                                    durable_observer,
                                    &heartbeat_cook_id,
                                    &heartbeat_run_id,
                                    "heartbeat",
                                    attempt,
                                    Some(&progress_detail),
                                    (!activity.is_empty()).then_some(&activity),
                                    None,
                                    None,
                                );
                                // Supervision evidence is written even when the
                                // policy is undeclared: the resource timeline is
                                // how "was this run expensive?" stops being a
                                // question only answerable by having watched it.
                                if !tick.is_empty() {
                                    let _ = agent_task_lifecycle::record_cook_supervision_in_store(
                                        heartbeat_lifecycle_store,
                                        &heartbeat_run_id,
                                        attempt,
                                        (!tick.sample.is_empty())
                                            .then(|| serde_json::to_value(tick.sample))
                                            .and_then(std::result::Result::ok),
                                        tick.decisions
                                            .iter()
                                            .filter_map(|decision| {
                                                serde_json::to_value(decision).ok()
                                            })
                                            .collect(),
                                    );
                                }
                                if tick.stop_now {
                                    let outcome = match homeboy_core::process::terminate_process_tree(
                                        heartbeat_owner_pid,
                                        homeboy_core::process::PROCESS_TREE_SIGTERM_GRACE,
                                    ) {
                                        Ok(termination) => serde_json::json!({
                                            "status": "terminated",
                                            "signal": termination.signal,
                                            "signalled_pids": termination.signalled_pids,
                                            "killed_pids": termination.killed_pids,
                                            "surviving_pids": termination.surviving_pids,
                                            "recovery_commands": termination.recovery_commands,
                                        }),
                                        // A host that cannot terminate a tree
                                        // (no process-tree cancellation, a pid
                                        // that outlives SIGKILL) must say so.
                                        // Recording the order alone would let a
                                        // reader conclude the run was stopped.
                                        Err(error) => serde_json::json!({
                                            "status": "termination_failed",
                                            "error": error.to_string(),
                                            "recovery_commands":
                                                homeboy_core::process::process_tree_recovery_commands(
                                                    heartbeat_owner_pid,
                                                ),
                                        }),
                                    };
                                    let _ =
                                        agent_task_lifecycle::record_cook_supervision_stop_in_store(
                                            heartbeat_lifecycle_store,
                                        &heartbeat_run_id,
                                        attempt,
                                        outcome,
                                        );
                                }
                                // A deadline that only fired at attempt and
                                // gate boundaries would not bound a single
                                // provider execution that runs past it, so the
                                // budget would be advisory for exactly the case
                                // it exists to bound. Expiry therefore takes
                                // the same route a supervision stop does:
                                // terminate the provider's process tree, from
                                // the same thread, recorded through the same
                                // durable evidence. Issued once — a second
                                // SIGKILL adds nothing and would overwrite the
                                // first outcome record.
                                if !deadline_stop_issued
                                    && heartbeat_deadline
                                        .deadline()
                                        .is_some_and(|deadline| deadline.is_expired())
                                {
                                    deadline_stop_issued = true;
                                    let outcome = match homeboy_core::process::terminate_process_tree(
                                        heartbeat_owner_pid,
                                        homeboy_core::process::PROCESS_TREE_SIGTERM_GRACE,
                                    ) {
                                        Ok(termination) => serde_json::json!({
                                            "status": "terminated",
                                            "cause": "cook_deadline",
                                            "signal": termination.signal,
                                            "signalled_pids": termination.signalled_pids,
                                            "killed_pids": termination.killed_pids,
                                            "surviving_pids": termination.surviving_pids,
                                            "recovery_commands": termination.recovery_commands,
                                        }),
                                        // Same reasoning as the supervision
                                        // stop above: a host that cannot
                                        // terminate the tree must say so rather
                                        // than let a reader conclude the
                                        // provider was stopped.
                                        Err(error) => serde_json::json!({
                                            "status": "termination_failed",
                                            "cause": "cook_deadline",
                                            "error": error.to_string(),
                                            "recovery_commands":
                                                homeboy_core::process::process_tree_recovery_commands(
                                                    heartbeat_owner_pid,
                                                ),
                                        }),
                                    };
                                    let _ =
                                        agent_task_lifecycle::record_cook_supervision_stop_in_store(
                                            heartbeat_lifecycle_store,
                                            &heartbeat_run_id,
                                            attempt,
                                            outcome,
                                        );
                                }
                            }
                        }));
                        let baseline = effective_baseline.map(CookFollowUpBaseline::capability);
                        let harvest_context =
                            Some(cook_attempt_harvest_context(&options.harvest_context));
                        let result = if lifecycle_store.read_record(&run_id)?.state()
                            == homeboy_lifecycle_contract::RunExecutionState::Running
                        {
                            run_claimed_loaded_plan_with_derived_cook_baseline_in_store(
                                lifecycle_store,
                                dispatch_plan,
                                &run_id,
                                executor.clone(),
                                baseline,
                                harvest_context,
                            )
                        } else {
                            run_loaded_plan_with_derived_cook_baseline_in_store(
                                lifecycle_store,
                                dispatch_plan,
                                Some(&run_id),
                                executor.clone(),
                                baseline,
                                harvest_context,
                            )
                        }
                        .map(|_| ());
                        let _ = heartbeat_stop.send(());
                        result
                    })
                }
            })();
            if let Err(error) = execution {
                if let Some(dispatch_plan) = failed_dispatch_plan.as_ref() {
                    // Baseline cleanup runs when the dispatch scope exits. Restore
                    // the exact continuation contract only after that cleanup so
                    // retry never loses its controller-owned plan.
                    agent_task_lifecycle::persist_controller_plan_in_store(
                        lifecycle_store,
                        &run_id,
                        dispatch_plan,
                    )?;
                }
                let record = match rooted_status(lifecycle_store, &run_id) {
                    Ok(record)
                        if record.state().is_terminal()
                            || (record.state()
                                == homeboy_lifecycle_contract::RunExecutionState::Running
                                && !super::cook_recipe::has_unambiguous_zero_execution(
                                    &record,
                                )) =>
                    {
                        Some(record)
                    }
                    _ => {
                        // A queue claim marks the controller Running before any
                        // provider receives work. Record its admission failure
                        // through the same owner as an unclaimed Queued attempt.
                        // Accepted/ambiguous execution and terminal winners stay
                        // owned by their existing lifecycle above.
                        let phase = pre_execution_failure_phase(
                            &error,
                            options.provider_transport.attempt_dispatcher.as_deref(),
                        );
                        record_pre_execution_failure(
                            lifecycle_store,
                            &plan,
                            &run_id,
                            &error,
                            phase,
                        )?;
                        rooted_status(lifecycle_store, &run_id).ok()
                    }
                };
                let pre_execution_failure = pre_execution_failure_details(record.as_ref(), &error);
                lifecycle_store.record_cook_attempt(&cook_id, attempt, &run_id)?;
                attempts.push(AgentTaskCookAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: record
                        .as_ref()
                        .map(|record| format!("{:?}", record.state()))
                        .unwrap_or_else(|| "DispatchFailed".to_string()),
                    aggregate_path: record
                        .as_ref()
                        .and_then(|record| record.aggregate_path.clone()),
                    promotion: None,
                    feedback: None,
                });
                let retry = pre_provider_retry(&error, &pre_execution_failure);
                let transient = retry == PreProviderRetry::Wait;
                if retry == PreProviderRetry::Stop {
                    return Ok(pre_execution_failure_report(
                        stores,
                        cook_id,
                        attempts,
                        pre_execution_failure,
                        error,
                        Some(&run_id),
                    ));
                }
                let (started, exempt) = match transient_failures {
                    Some((recorded, started, exempt)) if recorded == attempt => (started, exempt),
                    _ => (std::time::Instant::now(), 0),
                };
                if transient {
                    if !wait_out_transient_failure(
                        lifecycle_store,
                        &cook_id,
                        started,
                        exempt,
                        &error,
                    ) {
                        return Ok(pre_execution_failure_report(stores,
                            cook_id.clone(),
                            attempts,
                            pre_execution_failure,
                            Error::validation_invalid_argument(
                                "transport_retry",
                                format!(
                                    "semantic Cook attempt {attempt} waited {}s through transient infrastructure failures before provider start ({}); run `homeboy agent-task recover {cook_id}` once the substrate recovers",
                                    started.elapsed().as_secs(),
                                    error.message,
                                ),
                                Some(run_id.clone()),
                                None,
                            ),
                            Some(&run_id),
                        ));
                    }
                    transient_failures = Some((attempt, started, exempt + 1));
                } else if !transport_retry_available(store, &cook_id, attempt, exempt)?
                    && !transport_admission_reset_available(record.as_ref())
                {
                    return Ok(pre_execution_failure_report(stores,
                        cook_id,
                        attempts,
                        pre_execution_failure,
                        Error::validation_invalid_argument(
                            "transport_retry",
                            format!(
                                "pre-provider transport retry is exhausted for semantic Cook attempt {attempt}; repair the runner or controller transport before continuing"
                            ),
                            Some(run_id.clone()),
                            None,
                        ),
                        Some(&run_id),
                    ));
                }
                let next_run_id = next_transport_retry_run_id(&run_id);
                store.record_recipe_attempt_replacement(&cook_id, &run_id, &next_run_id)?;
                materialize_cook_attempt_with_stores(
                    store,
                    lifecycle_store,
                    &cook_id,
                    &next_run_id,
                    &plan,
                )?;
                run_id = next_run_id;
                next_plan = Some(plan);
                continue;
            }
        }
        // Provider waits, promotion and gate rounds observe identity afresh.
        drop(admission_identities.take());
        lifecycle_store.record_cook_attempt(&cook_id, attempt, &run_id)?;
        let rooted_promotion_continuation = verification_pending_continuation;
        let mut record = if rooted_promotion_continuation {
            lifecycle_store.read_record(&run_id)?
        } else {
            rooted_status(lifecycle_store, &run_id)?
        };
        // A local controller can disappear after the provider ledger records a
        // terminal result but before the run projection is terminalized. Repair
        // only this Cook attempt, never the active fleet, before deciding whether
        // an absent aggregate is safe to continue from.
        if record.state() == homeboy_lifecycle_contract::RunExecutionState::Running
            && record.is_stale_running()
            && record.aggregate_path.is_none()
        {
            super::reconcile_run_in_store(lifecycle_store, &run_id, false)?;
            record = if rooted_promotion_continuation {
                lifecycle_store.read_record(&run_id)?
            } else {
                rooted_status(lifecycle_store, &run_id)?
            };
        }
        let controller_owned_staging = record.lab_staging_controller_job_id().is_some();
        // This invocation owns the exact attempt, not its mission's latest
        // retry. Cancellation must end the observer and release its pin.
        if record.state() == homeboy_lifecycle_contract::RunExecutionState::Cancelled {
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state()),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(stores.report(CookReportInput {
                cook_id,
                status: CookStatus::Cancelled.as_str(),
                disposition: CookDisposition::Terminal,
                attempts,
                finalization: None,
                stop_reason: Some("exact Cook attempt was cancelled".to_string()),
                exit_code: 1,
                invocation_latest_run_id: Some(&run_id),
            }));
        }
        if matches!(
            record.state(),
            homeboy_lifecycle_contract::RunExecutionState::Queued
                | homeboy_lifecycle_contract::RunExecutionState::Running
        ) && (record.runner_job_id().is_some() || controller_owned_staging)
        {
            // Detached staging or runner handoff has a durable owner. It owns
            // timeout and provider rotation, so Cook must not read a future
            // aggregate before that owner has produced it.
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state()),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(stores.report(CookReportInput {
                cook_id,
                status: "in_flight",
                disposition: CookDisposition::InFlight,
                attempts,
                finalization: None,
                stop_reason: Some("provider attempt accepted by the runner daemon".to_string()),
                exit_code: 0,
                invocation_latest_run_id: Some(&run_id),
            }));
        }
        let plan = lifecycle_store.read_controller_plan(&run_id)?;
        budget_limit.get_or_insert_with(|| plan.options.execution_budget.clone());
        let Some(source_request) = plan.tasks.first().cloned() else {
            return Ok(stores.report(CookReportInput {
                cook_id,
                status: "policy_failure",
                disposition: CookDisposition::Terminal,
                attempts,
                finalization: None,
                stop_reason: Some(
                    "agent-task cook requires a plan with one source task".to_string(),
                ),
                exit_code: 1,
                invocation_latest_run_id: Some(&run_id),
            }));
        };
        validate_cook_candidate_group(&plan)?;
        if let Some(finalization) = super::recorded_finalization_receipt(&record.metadata)
            .filter(|finalization| !finalization.is_null())
            .cloned()
        {
            // A terminal child may outlive its coordinator. Its finalization is
            // the durable completion receipt, so harvesting it must not repeat
            // promotion, gates, or any provider-facing work.
            let (status, published) = super::cook_delivery::publication_status(&finalization);
            let exit_code = if published { 0 } else { 1 };
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state()),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(stores.report(CookReportInput {
                cook_id,
                status: &status,
                disposition: CookDisposition::Terminal,
                attempts,
                finalization: Some(finalization),
                stop_reason: None,
                exit_code,
                invocation_latest_run_id: Some(&run_id),
            }));
        }
        let persisted_terminal_promotion =
            if super::CookDelivery::from_run_metadata(&record.metadata)
                .is_some_and(|delivery| delivery.awaits_verification())
            {
                None
            } else {
                super::cook_promotion::persisted_promotion_for_attempt_in_store(
                    lifecycle_store,
                    &run_id,
                )?
            };
        let aggregate = match lifecycle_store.read_aggregate(&run_id) {
            Ok(aggregate) => aggregate,
            // An aggregate path is authoritative evidence that an aggregate was
            // committed. Its read failure must surface for repair rather than be
            // misclassified as an interruption and bypass immutable output.
            Err(_error) if record.state().is_terminal() && record.aggregate_path.is_none() => {
                if let Some(promotion) = persisted_terminal_promotion {
                    let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
                        source_request,
                        promotion_report: promotion.clone(),
                        attempt,
                        max_attempts: attempt_limit,
                        source_run_id: Some(run_id.clone()),
                        current_diff: gate_feedback_current_diff(&promotion),
                        review_form: None,
                        metadata: serde_json::json!({
                            "previous_failure_set": Value::Null,
                            "intentional_no_change": Value::Null,
                        }),
                    });
                    attempts.push(AgentTaskCookAttemptReport {
                        attempt,
                        run_id: run_id.clone(),
                        run_state: format!("{:?}", record.state()),
                        aggregate_path: record.aggregate_path,
                        promotion: Some(promotion),
                        feedback: Some(feedback.clone()),
                    });
                    let (status, stop_reason, exit_code) = match feedback.status {
                        AgentTaskCookLoopStatus::GreenCompleted
                            if options.finalization.no_finalize =>
                        {
                            (
                                "green_no_finalize",
                                "loaded the terminal persisted promotion; --no-finalize skipped commit, push, and PR finalization",
                                0,
                            )
                        }
                        AgentTaskCookLoopStatus::BaselineRed => (
                            "baseline_red",
                            "loaded the terminal persisted promotion with inherited baseline failures",
                            1,
                        ),
                        AgentTaskCookLoopStatus::NoChanges => (
                            "no_changes",
                            "loaded the terminal persisted no-changes promotion",
                            1,
                        ),
                        AgentTaskCookLoopStatus::AgentReportedIncomplete => (
                            "candidate_recoverable",
                            "loaded the terminal persisted promotion; the agent's review form declares the task incomplete, so it was not published",
                            1,
                        ),
                        AgentTaskCookLoopStatus::NoChangesGateFailed => (
                            "no_changes_gate_failed",
                            "loaded the terminal persisted no-changes gate failure",
                            1,
                        ),
                        _ => (
                            "durable_failure",
                            "loaded the terminal persisted promotion, but finalization evidence is absent from the authoritative source observation",
                            1,
                        ),
                    };
                    return Ok(stores.report(CookReportInput {
                        cook_id,
                        status,
                        disposition: CookDisposition::Terminal,
                        attempts,
                        finalization: None,
                        stop_reason: Some(stop_reason.to_string()),
                        exit_code,
                        invocation_latest_run_id: Some(&run_id),
                    }));
                }
                let phase = pre_artifact_interruption_phase(&record);
                if let Some(stop) =
                    homeboy_runner_contract::find_resource_guard_stop(&record.metadata)
                {
                    // The runner stopped the job at its resource guard: the
                    // same attempt would hit it again, so it is never a
                    // transport retry (#18418).
                    attempts.push(AgentTaskCookAttemptReport {
                        attempt,
                        run_id: run_id.clone(),
                        run_state: format!("{:?}", record.state()),
                        aggregate_path: None,
                        promotion: None,
                        feedback: None,
                    });
                    let mut report = pre_artifact_interruption_report(
                        stores,
                        cook_id,
                        attempts,
                        &run_id,
                        phase,
                        format!(
                            "attempt {attempt}'s runner job was stopped by its resource guard ({}); the Cook stopped instead of retrying it",
                            stop.violation.operator_message()
                        ),
                        1,
                    );
                    apply_resource_guard_stop(
                        &mut report,
                        &record.metadata,
                        &record.run_id,
                        record.runner_id(),
                    );
                    return Ok(report);
                }
                if phase == PreArtifactInterruptionPhase::RunnerResultNotProjected {
                    attempts.push(AgentTaskCookAttemptReport {
                        attempt,
                        run_id: run_id.clone(),
                        run_state: format!("{:?}", record.state()),
                        aggregate_path: None,
                        promotion: None,
                        feedback: None,
                    });
                    return Ok(pre_artifact_interruption_report(stores,
                        cook_id,
                        attempts,
                        &run_id,
                        phase,
                        format!(
                            "attempt {attempt}'s runner job succeeded, but no agent-task aggregate could be projected from its result; the provider's work is retained in the runner result, so the Cook stopped instead of running the provider again. Inspect it with `homeboy runs show {run_id}`"
                        ),
                        1,
                    ));
                }
                // Aggregates normally provide this accounting. A missing
                // aggregate must still carry only ledger-proven executions
                // into later remediation budget decisions.
                observed_budget_used.executions = observed_budget_used
                    .executions
                    .saturating_add(pre_artifact_execution_count(&record));
                attempts.push(AgentTaskCookAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: format!("{:?}", record.state()),
                    aggregate_path: record.aggregate_path.clone(),
                    promotion: None,
                    feedback: None,
                });
                let replace_semantic_attempt =
                    phase == PreArtifactInterruptionPhase::BeforeProviderStart;
                if !replace_semantic_attempt && attempt >= max_attempts {
                    publish_latest_incomplete_draft(&options, lifecycle_store)?;
                    return Ok(pre_artifact_interruption_report(stores,
                        cook_id,
                        attempts,
                        &run_id,
                        phase,
                        format!(
                            "attempt {attempt} is terminal without aggregate evidence ({}) and the Cook retry budget is exhausted; inspect durable provider execution metadata before starting a new Cook",
                            phase.name(),
                        ),
                        1,
                    ));
                }
                let budget_limit = budget_limit
                    .as_ref()
                    .expect("budget is initialized from the loaded attempt plan");
                if budget_remaining(budget_limit, observed_budget_used).is_none() {
                    return Ok(pre_artifact_interruption_report(stores,
                        cook_id,
                        attempts,
                        &run_id,
                        phase,
                        format!(
                            "attempt {attempt} is terminal without aggregate evidence ({}) and its ledger-proven provider execution exhausts the Cook provider budget; inspect the durable attempt before increasing the budget",
                            phase.name(),
                        ),
                        1,
                    ));
                }
                if replace_semantic_attempt
                    && !transport_retry_available(
                        store,
                        &cook_id,
                        attempt,
                        transient_failures
                            .filter(|(recorded, _, _)| *recorded == attempt)
                            .map_or(0, |(_, _, exempt)| exempt),
                    )?
                    && !transport_admission_reset_available(Some(&record))
                {
                    return Ok(pre_artifact_interruption_report(stores,
                        cook_id,
                        attempts,
                        &run_id,
                        phase,
                        format!(
                            "attempt {attempt} stopped before provider start after its bounded transport retry; repair the runner or controller transport before continuing"
                        ),
                        1,
                    ));
                }
                match claim_pre_artifact_interruption_retry_with_stores(
                    (store, lifecycle_store),
                    &cook_id,
                    attempt,
                    &run_id,
                    &plan,
                    replace_semantic_attempt,
                )? {
                    Some((next_attempt, next_run_id)) => {
                        run_id = next_run_id;
                        next_plan = Some(plan);
                        if !replace_semantic_attempt {
                            attempt = next_attempt;
                        }
                        continue;
                    }
                    None => {
                        return Ok(pre_artifact_interruption_report(stores,
                            cook_id,
                            attempts,
                            &run_id,
                            phase,
                            format!(
                                "attempt {attempt} is terminal without aggregate evidence ({}) and another controller is claiming its retry; resume Cook after the durable claim completes",
                                phase.name(),
                            ),
                            0,
                        ));
                    }
                }
            }
            Err(error) => return Err(error),
        };
        if aggregate.outcomes.iter().any(|outcome| {
            outcome.status == crate::agent_task::AgentTaskOutcomeStatus::Failed
                && outcome.diagnostics.iter().any(|diagnostic| {
                    diagnostic.class == "agent_task.workspace_snapshot_invalidated"
                        && diagnostic.data["pre_provider"] == Value::Bool(true)
                })
        }) {
            let error = Error::validation_invalid_argument(
                "workspace",
                "Cook workspace snapshot was invalidated before provider execution",
                Some(run_id.clone()),
                None,
            )
            .with_retryable(true);
            let transitioned = lifecycle_store
                .record_workspace_snapshot_fence_invalidation(&run_id, &plan, &error)?;
            if transitioned.metadata["pre_execution_failure"]["phase"]
                == Value::String("workspace_snapshot_fence".to_string())
                && transitioned.metadata["pre_execution_failure"]["retryable"] == Value::Bool(true)
                && transitioned.metadata["provider_executions_consumed"].as_u64() == Some(0)
            {
                let failure = pre_execution_failure_details(Some(&transitioned), &error);
                attempts.push(AgentTaskCookAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: format!("{:?}", transitioned.state()),
                    aggregate_path: transitioned.aggregate_path,
                    promotion: None,
                    feedback: None,
                });
                return Ok(pre_execution_failure_report(
                    stores,
                    cook_id,
                    attempts,
                    failure,
                    error,
                    Some(&run_id),
                ));
            }
        }
        observed_budget_used.add(execution_budget_usage(&aggregate));
        let mut budget_used = observed_budget_used;
        budget_used.same_provider_retries = budget_used
            .same_provider_retries
            .saturating_add(remediation_category_usage.same_provider_retries);
        budget_used.provider_rotations = budget_used
            .provider_rotations
            .saturating_add(remediation_category_usage.provider_rotations);
        let adopted_continuation = if rooted_promotion_continuation {
            None
        } else {
            adopted_attempt_is_ready_for_cook_continuation(lifecycle_store, &record)?
        };
        // A verification-pending marker is not a promotion report: its
        // operation owner must finish before Cook inspects candidate proof.
        let verification_pending = record
            .metadata
            .pointer("/cook_delivery/artifacts/promotion/status")
            .and_then(Value::as_str)
            == Some("verification_pending");
        let review_form_unavailable = !verification_pending
            && persisted_promotion_for_attempt_in_store(lifecycle_store, &run_id)?
                .is_some_and(|promotion| promotion.applied_with_passing_verdict())
            && !matches!(review_form_from_aggregate(&aggregate), Ok(Some(form)) if form.validate(crate::agent_task_review_dossier::PrTitlePolicy::from_plan(&options.identity.initial_plan)).is_ok());
        let persisted_terminal_status = record.metadata["cook_progress"]["terminal_status"]
            .as_str()
            .map(str::to_string);
        if let Some(status @ ("intentional_no_change" | "no_candidate")) =
            persisted_terminal_status.as_deref()
        {
            let declaration =
                intentional_no_change_from_aggregate(&aggregate).ok_or_else(|| {
                    Error::validation_invalid_argument(
                    "intentional_no_change",
                    "persisted no-change terminal status requires its durable provider declaration",
                    Some(run_id.clone()),
                    None,
                )
                })?;
            let review_form = review_form_from_aggregate(&aggregate)?;
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state()),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            let is_refusal = status == "no_candidate";
            let mut report = stores.report(CookReportInput {
                cook_id,
                status,
                disposition: CookDisposition::Terminal,
                attempts,
                finalization: None,
                stop_reason: Some("Cook restored its exact verified intentional no-change terminal result from durable state".to_string()),
                exit_code: i32::from(is_refusal),
                invocation_latest_run_id: Some(&run_id),
            });
            report.value.intentional_no_change = Some(AgentTaskCookIntentionalNoChangeReport {
                declaration,
                review_form,
            });
            if is_refusal {
                report.value.terminal_phase = Some("candidate_selection".to_string());
                report.value.terminal_failure_classification = Some("no_candidate".to_string());
            }
            return Ok(report);
        }
        // Only the adopted attempt itself carries adoption authority past a
        // terminal state; a failed form-only continuation of it does not.
        use agent_task_lifecycle::CookRunPhase::{Continuing, Succeeded};
        let state = agent_task_lifecycle::CookRunState::of(&record);
        if !(state.recoverable || matches!(state.phase, Succeeded | Continuing))
            && (adopted_continuation.is_none() || record.candidate_adoption.is_none())
            && !review_form_unavailable
        {
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state()),
                aggregate_path: record.aggregate_path.clone(),
                promotion: None,
                feedback: None,
            });
            let remaining_budget = budget_limit
                .as_ref()
                .and_then(|budget| budget_remaining(budget, budget_used));
            let no_candidate_reason = (record.state()
                == homeboy_lifecycle_contract::RunExecutionState::Failed)
                .then(|| incomplete_no_candidate_reason(&aggregate))
                .flatten();
            let status = if record.metadata["pre_execution_failure"].is_object() {
                CookStatus::PreExecutionFailure
            } else if no_candidate_reason.is_some() {
                CookStatus::NoCandidate
            } else {
                terminal_fallback_status(record.state())
            };
            let cancelled = status == CookStatus::Cancelled;
            if !cancelled && attempt >= attempt_limit {
                publish_latest_incomplete_draft(&options, lifecycle_store)?;
            }
            let mut report = stores.report(CookReportInput {
                cook_id,
                status: status.as_str(),
                disposition: CookDisposition::Terminal,
                attempts,
                finalization: None,
                stop_reason: if let Some(reason) = no_candidate_reason {
                    Some(reason)
                } else if cancelled {
                    Some(format!(
                        "Cook was cancelled: {}",
                        record.metadata["cancel_reason"]
                            .as_str()
                            .unwrap_or("cancel requested")
                    ))
                } else {
                    Some(terminal_stop_reason(&run_id, record.state(), &aggregate))
                },
                exit_code: 1,
                invocation_latest_run_id: Some(&run_id),
            });
            if cancelled {
                report.value.terminal_phase = Some("cancellation".to_string());
                report.value.terminal_failure_classification = Some("cancelled".to_string());
            } else if status == CookStatus::NoCandidate {
                report.value.terminal_phase = Some("candidate_selection".to_string());
                report.value.terminal_failure_classification = Some("no_candidate".to_string());
            } else {
                make_provider_timeout_actionable(
                    Some(lifecycle_store),
                    &mut report,
                    &aggregate,
                    &plan,
                    &run_id,
                    remaining_budget,
                    agent_task_lifecycle::has_active_provider_execution_in_store(
                        lifecycle_store,
                        &run_id,
                    )
                    .unwrap_or(true),
                );
                make_provider_rotation_actionable(
                    Some(lifecycle_store),
                    &mut report,
                    &aggregate,
                    &run_id,
                );
                make_startup_without_output_actionable(
                    Some(lifecycle_store),
                    &mut report,
                    &aggregate,
                    &run_id,
                );
                if report.value.terminal_phase.is_none() {
                    if let Some((phase, classification, _)) = pre_provider_diagnostic_cause(
                        record.metadata["provider_executions_consumed"]
                            .as_u64()
                            .unwrap_or(0),
                        aggregate
                            .outcomes
                            .iter()
                            .flat_map(|outcome| &outcome.diagnostics),
                    ) {
                        report.value.terminal_phase = Some(phase);
                        report.value.terminal_failure_classification = Some(classification);
                    }
                }
            }
            apply_resource_guard_stop(
                &mut report,
                &record.metadata,
                &record.run_id,
                record.runner_id(),
            );
            if let Some(failure) = aggregate.outcomes.iter().find_map(|outcome| {
                outcome.diagnostics.iter().find_map(|diagnostic| {
                    let code = diagnostic.data.get("failure_code")?.as_str()?;
                    Some((
                        code,
                        diagnostic
                            .data
                            .get("executed_model")
                            .and_then(Value::as_str),
                        diagnostic
                            .data
                            .get("final_message_excerpt")
                            .and_then(Value::as_str),
                    ))
                })
            }) {
                report.value.terminal_phase = Some("provider".to_string());
                report.value.terminal_failure_classification = Some(failure.0.to_string());
                report.value.stop_reason = Some(format!(
                    "provider finished without acceptable output (failure_code: {}; executed_model: {}; final_message: {})",
                    failure.0,
                    failure.1.unwrap_or("unknown"),
                    failure.2.unwrap_or("no final message")
                ));
            }
            return Ok(report);
        }

        if let Some(declaration) = intentional_no_change_from_aggregate(&aggregate) {
            let review_form = review_form_from_aggregate(&aggregate)?;
            let requires_candidate =
                no_change_requires_substantive_candidate(&declaration, &source_request);
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state()),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            let mut report = stores.report(CookReportInput {
                    cook_id,
                    status: if requires_candidate {
                        "no_candidate"
                    } else {
                        "intentional_no_change"
                    },
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(
                        if declaration.verdict == AgentTaskIntentionalNoChangeVerdict::Blocked {
                            format!(
                        "provider returned a verified blocked verdict; promotion and publication were skipped{}",
                        declaration
                            .next_action
                            .is_empty()
                            .then(String::new)
                            .unwrap_or_else(|| format!("; next action: {}", declaration.next_action))
                    )
                        } else if requires_candidate {
                            format!(
                        "provider completed a verified {} review, but task policy requires a substantive candidate patch",
                        declaration.verdict
                    )
                        } else {
                            format!(
                        "provider completed a verified {} evidence-only review without a candidate patch; promotion and publication were skipped",
                        declaration.verdict
                    )
                        },
                    ),
                    exit_code: i32::from(requires_candidate),
                    invocation_latest_run_id: Some(&run_id),
                },
            );
            report.value.intentional_no_change = Some(AgentTaskCookIntentionalNoChangeReport {
                declaration,
                review_form,
            });
            if requires_candidate {
                report.value.terminal_phase = Some("candidate_selection".to_string());
                report.value.terminal_failure_classification = Some("no_candidate".to_string());
            }
            return Ok(report);
        }

        // Arms that end the Cook without delivery use the spine's state directly.
        let mut delivery = CookDeliveryContext {
            store,
            lifecycle_store,
            options: &options,
            side_effects,
            observer: durable_observer,
            cook_id: &cook_id,
            run_id: &run_id,
            attempt,
            attempts: &mut attempts,
        };
        let promotion = match super::cook_delivery::ensure_verified(&mut delivery, &record)? {
            ControlFlow::Continue(promotion) => promotion,
            ControlFlow::Break(report) => return Ok(report),
        };

        // Keep post-gate feedback, failure recording and successor dispatch on
        // the timeline even when an early return terminalizes the attempt.
        let terminal_timing = match lifecycle_store.open_observation_initialized() {
            Ok(store) => Some(store),
            Err(error) => {
                homeboy_core::log_status!(
                    "observe",
                    "terminal timing persistence failed: {}",
                    error
                );
                None
            }
        };
        let _terminal_phase = terminal_timing
            .as_ref()
            .map(|store| store.run_phase_guard(&run_id, "terminal.decision"));
        let review_form = if review_form_unavailable {
            None
        } else {
            review_form_from_aggregate(&aggregate).ok().flatten()
        };
        let intentional_no_change = intentional_no_change_from_aggregate(&aggregate);
        let previous_failure_set = delivery
            .attempts
            .last()
            .and_then(|attempt| attempt.feedback.as_ref())
            .and_then(|feedback| feedback.follow_up_request.as_ref())
            .and_then(|request| request.inputs.pointer("/cook_loop/failure_set"))
            .cloned()
            .unwrap_or(Value::Null);
        let feedback_promotion = promotion.clone();
        let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request: source_request.clone(),
            promotion_report: feedback_promotion,
            attempt,
            max_attempts: attempt_limit,
            source_run_id: Some(run_id.clone()),
            current_diff: gate_feedback_current_diff(&promotion),
            // The form is publication evidence. A no-finalize Cook preserves
            // the verified patch for review without entering finalization.
            review_form: review_form.clone(),
            metadata: serde_json::json!({
                "previous_failure_set": previous_failure_set,
                "intentional_no_change": intentional_no_change,
                "pending_operator_feedback": crate::agent_task_feedback::has_pending(lifecycle_store, &cook_id),
            }),
        });
        let feedback_status = feedback.status;
        let follow_up_request = feedback.follow_up_request.clone();
        delivery.attempts.push(AgentTaskCookAttemptReport {
            attempt,
            run_id: run_id.clone(),
            run_state: format!("{:?}", record.state()),
            aggregate_path: record.aggregate_path,
            promotion: Some(promotion.clone()),
            feedback: Some(feedback.clone()),
        });

        match feedback_status {
            AgentTaskCookLoopStatus::IntentionalNoChange => {
                let declaration = feedback
                    .intentional_no_change
                    .expect("intentional no-change feedback carries its declaration");
                if !options.finalization.no_finalize && !promotion.changed_files.is_empty() {
                    let mut report = super::cook_delivery::publish_accepted(
                        &mut delivery,
                        &promotion,
                        "verified intentional no-change review accepted an existing candidate",
                        Some("intentional_no_change_finalized_existing_candidate"),
                        None,
                    )?;
                    report.value.intentional_no_change =
                        Some(AgentTaskCookIntentionalNoChangeReport {
                            declaration,
                            review_form,
                        });
                    return Ok(report);
                }
                let mut report = stores.report(CookReportInput {
                    cook_id,
                    status: "intentional_no_change",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(format!(
                        "provider completed a verified {} review without a candidate patch; publication was skipped{}",
                        declaration.verdict,
                        declaration
                            .next_action
                            .is_empty()
                            .then(String::new)
                            .unwrap_or_else(|| format!("; next action: {}", declaration.next_action))
                    )),
                    exit_code: 0,
                    invocation_latest_run_id: Some(&run_id),
                });
                report.value.intentional_no_change = Some(AgentTaskCookIntentionalNoChangeReport {
                    declaration,
                    review_form,
                });
                return Ok(report);
            }
            AgentTaskCookLoopStatus::GreenCompleted => {
                let mut report =
                    super::cook_delivery::ensure_published(&mut delivery, promotion.clone())?;
                if review_form_unavailable {
                    report.value.terminal_phase = Some("review_form".to_string());
                    report.value.stop_reason = Some(
                        "review form unavailable; published the gated candidate as a draft"
                            .to_string(),
                    );
                }
                return Ok(report);
            }
            AgentTaskCookLoopStatus::BaselineRed => {
                if promotion.finalization_eligible() {
                    return super::cook_delivery::publish_accepted(
                        &mut delivery,
                        &promotion,
                        "accepted inherited baseline-red gate",
                        None,
                        Some("finalized with proven inherited baseline-red gate evidence"),
                    );
                }
                let stop_reason = super::cook_recovery::baseline_red_refusal(&cook_id);
                return Ok(stores.report(CookReportInput {
                    cook_id,
                    status: "baseline_red",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(stop_reason),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            AgentTaskCookLoopStatus::NoChanges => {
                if attempt >= attempt_limit {
                    publish_latest_incomplete_draft(&options, lifecycle_store)?;
                }
                return Ok(stores.report(CookReportInput {
                    cook_id,
                    status: "no_changes",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(no_change_stop_reason(
                        &promotion,
                        &aggregate,
                        review_form.as_ref(),
                        attempt,
                        attempt_limit,
                    )),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            AgentTaskCookLoopStatus::AgentReportedIncomplete => {
                let reason = review_form
                    .as_ref()
                    .and_then(|form| form.incomplete_reason())
                    .unwrap_or("no reason recorded")
                    .to_string();
                lifecycle_store.record_cook_delivery_blocker(
                    &run_id,
                    super::cook_delivery::CookDeliveryBlocker {
                        code: super::cook_delivery::CookDeliveryBlockerCode::ReviewFormIncomplete,
                        message: format!(
                            "The agent's review form declares the task incomplete: {reason}"
                        ),
                        paths: Vec::new(),
                        next_action: Some(
                            super::cook_recovery::incomplete_review_recovery_command(&run_id),
                        ),
                        cause: None,
                        evidence: "/cook_delivery/blocker".to_string(),
                        output: None,
                    },
                )?;
                return Ok(stores.report(CookReportInput {
                    cook_id,
                    status: "candidate_recoverable",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(format!(
                        "the agent's review form declares the task incomplete, so the green candidate was kept for recovery and not published: {reason}"
                    )),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            AgentTaskCookLoopStatus::NoChangesGateFailed => {
                if attempt >= attempt_limit {
                    publish_latest_incomplete_draft(&options, lifecycle_store)?;
                }
                return Ok(stores.report(CookReportInput {
                    cook_id,
                    status: "no_changes_gate_failed",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(
                        "provider produced no patch and the pinned candidate failed \
     deterministic verification"
                            .to_string(),
                    ),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            // #14731: a gate that could not execute under the resolved
            // placement is control-plane evidence, not a candidate verdict.
            // The candidate patch is already durably promoted; this is
            // deliberately not `policy_failure` and does not run
            // differential-baseline comparison (no failure exists to
            // compare). Retry re-verifies from the existing candidate rather
            // than spending another provider execution.
            AgentTaskCookLoopStatus::GatesDeferred => {
                let stop_reason = gates_deferred_stop_reason(&promotion, &cook_id);
                return Ok(stores.report(CookReportInput {
                    cook_id,
                    status: "gate_environment_unavailable",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(stop_reason),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            AgentTaskCookLoopStatus::GateEnvironmentUnavailable => {
                let failure = feedback
                    .failed_gates
                    .iter()
                    .find(|gate| {
                        gate.classification
                        == crate::agent_task_gate::AgentTaskGateFailureClassification::Environment
                    })
                    .expect("environment stop carries a failed gate");
                let stop_reason = format!(
                    "stop: environment; gate `{}`: {}. Candidate preserved. After fixing the environment run `homeboy agent-task recover {cook_id}` to re-gate, or `homeboy agent-task recover {cook_id} --candidate-ref HEAD` from the local candidate worktree.",
                    failure.gate_id, failure.summary
                );
                return Ok(stores.report(CookReportInput {
                    cook_id,
                    status: "gate_environment_unavailable",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(stop_reason),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            // #18554: a supervision stop is infrastructure, not a candidate
            // verdict. The gate was already re-gated once on this candidate;
            // stop as unavailable verification with a
            // concrete re-gate step rather than `gate_failed`.
            AgentTaskCookLoopStatus::GateBudgetExceeded => {
                let stop_reason = gate_budget_exceeded_stop_reason(&promotion, &cook_id);
                return Ok(stores.report(CookReportInput {
                    cook_id,
                    status: "gate_environment_unavailable",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(stop_reason),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            AgentTaskCookLoopStatus::GateDeclarationInvalid => {
                return Ok(stores.report(CookReportInput {
                    cook_id,
                    status: "gate_declaration_invalid",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(
                        "a declared deterministic gate is invalid or selects an inadmissible test population; correct the gate declaration and start Cook again"
                            .to_string(),
                    ),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
            AgentTaskCookLoopStatus::RetryRequested => {
                let Some(follow_up_request) = follow_up_request else {
                    return Ok(stores.report(CookReportInput {
                        cook_id,
                        status: "policy_failure",
                        disposition: CookDisposition::Terminal,
                        attempts,
                        finalization: None,
                        stop_reason: Some(
                            "cook feedback requested retry without a follow-up request".to_string(),
                        ),
                        exit_code: 1,
                        invocation_latest_run_id: Some(&run_id),
                    }));
                };
                let budget_limit = budget_limit
                    .as_ref()
                    .expect("budget is initialized from the loaded attempt plan");
                match dispatch_cook_follow_up(
                    (store, lifecycle_store),
                    &options,
                    executor.clone(),
                    &cook_id,
                    attempt,
                    &run_id,
                    &plan,
                    &aggregate,
                    &promotion,
                    follow_up_request,
                    false,
                    CookFollowUpBudgetScope::Cook,
                    budget_limit,
                    budget_used,
                    &mut remediation_category_usage,
                )? {
                    CookFollowUpDispatch::Dispatched {
                        run_id: next_run_id,
                    } => {
                        run_id = next_run_id;
                        attempt = attempt.saturating_add(1);
                    }
                    CookFollowUpDispatch::BudgetExhausted { reason } => {
                        return Ok(stores.report(CookReportInput {
                            cook_id,
                            status: "execution_budget_exhausted",
                            disposition: CookDisposition::Terminal,
                            attempts,
                            finalization: None,
                            stop_reason: Some(exhausted_budget_guidance(
                                attempt_limit,
                                budget_limit,
                                &reason,
                            )),
                            exit_code: 1,
                            invocation_latest_run_id: Some(&run_id),
                        }));
                    }
                    CookFollowUpDispatch::PolicyFailure { reason } => {
                        return Ok(stores.report(CookReportInput {
                            cook_id,
                            status: "policy_failure",
                            disposition: CookDisposition::Terminal,
                            attempts,
                            finalization: None,
                            stop_reason: Some(reason),
                            exit_code: 1,
                            invocation_latest_run_id: Some(&run_id),
                        }));
                    }
                }
            }
            AgentTaskCookLoopStatus::RetriesExhausted => {
                publish_latest_incomplete_draft(&options, lifecycle_store)?;
                return Ok(stores.report(CookReportInput {
                    cook_id,
                    status: "retries_exhausted",
                    disposition: CookDisposition::Terminal,
                    attempts,
                    finalization: None,
                    stop_reason: Some(
                        "deterministic gates stayed red after the configured attempt budget"
                            .to_string(),
                    ),
                    exit_code: 1,
                    invocation_latest_run_id: Some(&run_id),
                }));
            }
        }
    }

    publish_latest_incomplete_draft(&options, lifecycle_store)?;
    Ok(stores.report(CookReportInput {
        cook_id,
        status: "retries_exhausted",
        disposition: CookDisposition::Terminal,
        attempts,
        finalization: None,
        stop_reason: Some("cook attempt budget exhausted".to_string()),
        exit_code: 1,
        invocation_latest_run_id: Some(&run_id),
    }))
}
