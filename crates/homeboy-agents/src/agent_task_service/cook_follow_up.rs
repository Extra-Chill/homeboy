//! Cook follow-up dispatch and no-change provider rotation.

use super::cook::*;

pub(crate) enum CookFollowUpDispatch {
    Dispatched { run_id: String },
    BudgetExhausted { reason: String },
    PolicyFailure { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CookFollowUpBudgetScope {
    Cook,
    CandidateAdoptionReview,
}

pub(super) fn remediation_tool_policy_error(
    request: &crate::agent_task::AgentTaskRequest,
) -> Option<String> {
    (!request.policy.permits_workspace_read_tool()).then(|| {
        "Cook remediation policy must grant the runner read access to the task workspace before dispatch"
            .to_string()
    })
}

pub(super) fn cook_remediation_same_provider(
    promotion: &AgentTaskPromotionReport,
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
    plan: &AgentTaskPlan,
    durable_provider_executions: Option<&Value>,
    follow_up: &crate::agent_task::AgentTaskExecutor,
) -> Option<bool> {
    // A gate result authenticates the durable candidate and identifies this as
    // same-provider gate remediation, even when a timed-out provider omitted
    // its terminal executor identity. A Homeboy-verified unchanged attempt is
    // likewise authenticated by Homeboy's own checkout proof: its follow-up
    // re-runs the same task, so it is a same-provider retry unless a rotation
    // route is available (`no_change_rotation_route`, #15881).
    (promotion.status.gate_failed() || no_change_remediation(promotion, aggregate))
        .then_some(true)
        .or_else(|| {
            terminal_executor_matches(aggregate, plan, durable_provider_executions, follow_up)
        })
}

/// The provider rotation entry a verified no-change follow-up runs next
/// (#15881): one past the last configured entry whose route the terminal
/// attempt actually ran. A model that abstained almost always abstains again,
/// so the retry budget has to offer the miss to a different provider instead
/// of repeating the route. An explicitly pinned route that no entry describes
/// rotates to the first configured entry. Candidate-adoption review never
/// reaches this decision: its caller pins the adopted route and reports a
/// known same executor. `None` keeps the existing same-provider behaviour: no
/// configured chain, the attempted route is unknown, or every configured
/// entry was already behind the terminal route.
pub(crate) fn no_change_rotation_route(
    promotion: &AgentTaskPromotionReport,
    plan: &AgentTaskPlan,
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
    durable_provider_executions: Option<&Value>,
) -> Option<(AgentTaskProviderRotationPolicy, usize)> {
    let source = plan.tasks.first()?;
    let policy =
        crate::agent_task_scheduler::AgentTaskScheduleSupport::rotation_policy_for_request(
            source,
            plan.options.rotation.as_ref(),
        )?;
    if policy.entries.is_empty() {
        return None;
    }
    let outcome = aggregate
        .outcomes
        .iter()
        .find(|outcome| outcome.task_id == promotion.source.task_id)?;
    let terminal = terminal_executor_identity(outcome, plan, durable_provider_executions)?;
    let entry_matches_route = |entry: &AgentTaskProviderRotationEntry| {
        entry
            .backend
            .as_deref()
            .is_none_or(|entry_backend| Some(entry_backend) == Some(terminal.backend.as_str()))
            && entry
                .selector
                .as_deref()
                .is_none_or(|entry_selector| Some(entry_selector) == terminal.selector.as_deref())
            && entry
                .model
                .as_deref()
                .is_none_or(|entry_model| Some(entry_model) == terminal.model.as_deref())
    };
    let next = policy
        .entries
        .iter()
        .rposition(entry_matches_route)
        .map_or(0, |matched| matched + 1);
    (next < policy.entries.len()).then_some((policy, next))
}

pub(super) const NO_CHANGE_PROVIDER_MESSAGE_LIMIT: usize = 1_000;

/// The operator-facing reason a Cook stopped without a change. It names the
/// no-change outcome and quotes the provider's own final message, so an empty
/// attempt reads as "the provider changed nothing" rather than as a gate,
/// review-form, or ancestry failure (#15294).
pub(super) fn no_change_stop_reason(
    promotion: &AgentTaskPromotionReport,
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
    review_form: Option<&crate::agent_task_review_dossier::AiFilledReviewForm>,
    attempt: u32,
    attempt_limit: u32,
) -> String {
    let outcome = if no_change_remediation(promotion, aggregate) {
        format!(
            "attempt {attempt} of {attempt_limit} produced no change: the provider finished without editing the workspace and Homeboy verified the attempt checkout is unchanged from its base, so no gates ran and nothing was promoted"
        )
    } else {
        format!(
            "attempt {attempt} of {attempt_limit} produced no changed files; the task still requires a change"
        )
    };
    let message = review_form
        .map(|form| form.summary.trim())
        .filter(|summary| !summary.is_empty())
        .map(|summary| {
            let mut bounded: String = summary
                .chars()
                .take(NO_CHANGE_PROVIDER_MESSAGE_LIMIT)
                .collect();
            if bounded.len() < summary.len() {
                bounded.push('…');
            }
            format!("provider's final message: {bounded}")
        })
        .unwrap_or_else(|| "the provider returned no final message".to_string());
    format!("{outcome}; {message}")
}

/// Whether this promotion records an attempt Homeboy proved changed nothing.
pub(super) fn no_change_remediation(
    promotion: &AgentTaskPromotionReport,
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
) -> bool {
    promotion.status == AgentTaskPromotionStatus::NoChanges
        && promotion.changed_files.is_empty()
        && aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == promotion.source.task_id)
            .is_some_and(crate::agent_task::AgentTaskOutcome::is_verified_unchanged_attempt)
}

pub(super) fn budget_authority_matches_scope(
    authority: &Value,
    scope: CookFollowUpBudgetScope,
) -> bool {
    authority["kind"].as_str()
        == Some(match scope {
            CookFollowUpBudgetScope::CandidateAdoptionReview => "candidate_adoption_review",
            CookFollowUpBudgetScope::Cook => return false,
        })
}

pub(super) fn scoped_follow_up_budget(
    scope: CookFollowUpBudgetScope,
    cook_budget: &AgentTaskExecutionBudget,
    cook_usage: ExecutionBudgetUsage,
    persisted_authority: Option<&Value>,
) -> (AgentTaskExecutionBudget, ExecutionBudgetUsage) {
    match scope {
        CookFollowUpBudgetScope::Cook => (cook_budget.clone(), cook_usage),
        CookFollowUpBudgetScope::CandidateAdoptionReview => {
            let authority = persisted_authority.unwrap_or(&Value::Null);
            // Review-only remediation has a fixed upper bound, but a durable
            // authority may intentionally narrow it. Never mint a broader
            // allowance when resuming that authority.
            let bounded = |key, maximum| {
                authority[key]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .map(|value| value.min(maximum))
                    .unwrap_or(maximum)
            };
            (
                AgentTaskExecutionBudget::new(
                    bounded("max_provider_executions", 2),
                    bounded("max_same_provider_retries", 1),
                    bounded("max_provider_rotations", 0),
                ),
                ExecutionBudgetUsage::default(),
            )
        }
    }
}

pub(super) fn review_budget_authority(
    scope: CookFollowUpBudgetScope,
    persisted_authority: Option<&Value>,
) -> Value {
    let (budget, _) = scoped_follow_up_budget(
        scope,
        &AgentTaskExecutionBudget::new(0, 0, 0),
        ExecutionBudgetUsage::default(),
        persisted_authority,
    );
    let kind = match scope {
        CookFollowUpBudgetScope::CandidateAdoptionReview => "candidate_adoption_review",
        CookFollowUpBudgetScope::Cook => unreachable!(),
    };
    let review_plan_provider_executions = persisted_authority
        .and_then(|authority| authority["review_plan_provider_executions"].as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .map(|value| value.min(1))
        .unwrap_or(1);
    serde_json::json!({
        "kind": kind,
        "max_provider_executions": budget.max_provider_executions,
        "max_same_provider_retries": budget.max_same_provider_retries,
        "max_provider_rotations": budget.max_provider_rotations,
        "review_plan_provider_executions": review_plan_provider_executions,
    })
}

pub(super) fn child_execution_budget(
    scope: CookFollowUpBudgetScope,
    budget_limit: &AgentTaskExecutionBudget,
) -> AgentTaskExecutionBudget {
    match scope {
        CookFollowUpBudgetScope::Cook => budget_limit.clone(),
        CookFollowUpBudgetScope::CandidateAdoptionReview => AgentTaskExecutionBudget::new(1, 0, 0),
    }
}

pub(super) fn validate_cook_follow_up_stores(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<()> {
    if recipe_store.data_root() != lifecycle_store.data_root() {
        return Err(Error::validation_invalid_argument(
            "stores",
            "Cook follow-up recipe and lifecycle stores must share one data root",
            Some(format!(
                "recipe={}, lifecycle={}",
                recipe_store.data_root().display(),
                lifecycle_store.data_root().display()
            )),
            None,
        ));
    }
    Ok(())
}

/// Append and dispatch one remediation attempt from an authenticated promoted
/// candidate. Both ordinary Cook feedback and external candidate adoption use
/// this boundary so their budget, provenance, and baseline authority match.
#[expect(
    clippy::too_many_arguments,
    reason = "Cook follow-up retains explicit durable recipe and dispatch identity fields"
)]
pub(crate) fn dispatch_cook_follow_up(
    stores: (&CookRecipeStore, &AgentTaskLifecycleStore),
    options: &CookRequest,
    executor: SharedAgentTaskExecutor,
    cook_id: &str,
    attempt: u32,
    source_run_id: &str,
    plan: &AgentTaskPlan,
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
    promotion: &AgentTaskPromotionReport,
    mut follow_up_request: crate::agent_task::AgentTaskRequest,
    known_same_executor: bool,
    budget_scope: CookFollowUpBudgetScope,
    budget_limit: &AgentTaskExecutionBudget,
    budget_used: ExecutionBudgetUsage,
    remediation_category_usage: &mut ExecutionBudgetUsage,
) -> Result<CookFollowUpDispatch> {
    let (recipe_store, lifecycle_store) = stores;
    let timing = match lifecycle_store.open_observation_initialized() {
        Ok(store) => Some(store),
        Err(error) => {
            homeboy_core::log_status!(
                "observe",
                "retry scheduling timing persistence failed: {}",
                error
            );
            None
        }
    };
    let schedule_phase = timing.as_ref().map(|store| {
        store
            .phase_timer(source_run_id)
            .into_guard("terminal.schedule_next_attempt")
    });
    validate_cook_follow_up_stores(recipe_store, lifecycle_store)?;
    let _driver = recipe_store
        .try_acquire_cook_driver(cook_id)?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_id",
                "Cook is already being driven by another controller",
                Some(cook_id.to_string()),
                None,
            )
            .with_retryable(true)
        })?;
    if let Some(reason) = remediation_tool_policy_error(&follow_up_request) {
        return Ok(CookFollowUpDispatch::PolicyFailure { reason });
    }
    let recipe = recipe_store.load_recipe(cook_id)?;
    let requested_edge = super::cook_lineage::CookAttemptLineage {
        source_run_id: source_run_id.to_string(),
        kind: super::cook_lineage::CookLineageKind::GateFix,
        source_patch_sha256: promotion
            .candidate
            .as_ref()
            .map(|candidate| candidate.tree_sha.clone()),
    };
    if !no_change_remediation(promotion, aggregate)
        && requested_edge
            .source_patch_sha256
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Ok(CookFollowUpDispatch::PolicyFailure {
            reason: "a gate-fix intent requires an authenticated source patch digest".to_string(),
        });
    }
    // A verified no-change attempt consumes a provider rotation, not a
    // same-provider retry (#15881): an abstaining model almost always abstains
    // again, so repeating its route spends the retry budget on the same miss.
    // Only a change task rotates; investigation and form-only follow-ups change
    // nothing by design, and an explicitly pinned adoption route never reaches
    // this decision (`known_same_executor`). The route must be reconstructable
    // from durable evidence (terminal executor identity), otherwise the follow
    // -up keeps the existing same-provider behaviour.
    //
    // The reroute is applied before the durable replay comparison below so a
    // replayed follow-up re-derives exactly the intent that was persisted. When
    // the rotation budget is spent, the budget block below restores the
    // unrotated request and falls back to the same-provider behaviour.
    let unrotated_follow_up_request = follow_up_request.clone();
    let no_change_rotation = (!known_same_executor
        && no_change_remediation(promotion, aggregate)
        && request_requires_substantive_candidate(&follow_up_request))
    .then(|| {
        let durable_provider_executions = lifecycle_store
            .read_record(source_run_id)
            .ok()
            .and_then(|record| record.metadata.get("provider_executions").cloned())
            .filter(|executions| {
                executions
                    .as_array()
                    .is_some_and(|executions| !executions.is_empty())
            });
        no_change_rotation_route(
            promotion,
            plan,
            aggregate,
            durable_provider_executions.as_ref(),
        )
    })
    .flatten();
    if let Some((policy, entry_index)) = no_change_rotation.as_ref() {
        // Hand the follow-up the next configured rotation route and advance the
        // durable routing state so the runner keeps rotating from the right
        // entry, and carries the routes already skipped as unready.
        AgentTaskScheduleSupport::apply_rotation_entry(
            &mut follow_up_request,
            &policy.entries[*entry_index],
            policy,
        );
        if !follow_up_request.metadata.is_object() {
            follow_up_request.metadata = serde_json::json!({});
        }
        follow_up_request.metadata["provider_readiness_routing"] = serde_json::json!({
            "next_rotation_index": entry_index + 1,
            "provider_rotations_used": 1,
        });
        if let Some(skipped) = plan
            .tasks
            .first()
            .and_then(|task| task.metadata.pointer("/provider_readiness_routing/skipped"))
            .cloned()
        {
            follow_up_request.metadata["provider_readiness_routing"]["skipped"] = skipped;
        }
    }
    if let Some(bound) = recipe.attempts.iter().rev().find(|bound| {
        bound.attempt > attempt
            && super::cook_lineage::execution_lineage(bound, &recipe.attempts).as_ref()
                == Some(&requested_edge)
    }) {
        let authority = &bound.plan.tasks[0].inputs["cook_loop"]["execution_budget_authority"];
        let scope_matches = if budget_scope == CookFollowUpBudgetScope::Cook {
            authority.is_null()
        } else {
            budget_authority_matches_scope(authority, budget_scope)
        };
        if !scope_matches {
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "persisted follow-up admission scope differs from replay",
                Some(bound.run_id.clone()),
                None,
            ));
        }
        let mut incoming = follow_up_request.clone();
        incoming.task_id = bound.plan.tasks[0].task_id.clone();
        incoming.inputs["cook_loop"]["artifact_provenance"] = serde_json::json!({
            "source_run_id": source_run_id,
            "source_task_id": promotion.source.task_id,
            "source_candidate_tree_sha": promotion.candidate.as_ref().map(|candidate| &candidate.tree_sha),
        });
        let mut frozen = bound.plan.tasks[0].clone();
        for request in [&mut incoming, &mut frozen] {
            if let Some(inputs) = request.inputs["cook_loop"].as_object_mut() {
                inputs.remove("execution_budget_authority");
                // Derived reviewer scope (#17651); attempts persisted before
                // it existed must still replay.
                inputs.remove("remediation_scope");
            }
        }
        if incoming != frozen {
            return Err(Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "persisted follow-up intent conflicts with replay inputs",
                Some(bound.run_id.clone()),
                None,
            ));
        }
        if agent_task_lifecycle::run_record_exists_readonly_in_store(
            lifecycle_store,
            &bound.run_id,
        )? {
            return Ok(CookFollowUpDispatch::Dispatched {
                run_id: bound.run_id.clone(),
            });
        } else {
            drop(schedule_phase);
            return dispatch_bound_cook_follow_up(
                recipe_store,
                lifecycle_store,
                options,
                executor,
                cook_id,
                bound.clone(),
                source_run_id,
                promotion,
                budget_scope,
                remediation_category_usage,
                ExecutionBudgetUsage::default(),
            );
        }
    }
    let related_attempts = recipe.attempts.iter().filter(|recipe_attempt| {
        super::cook_lineage::execution_lineage(recipe_attempt, &recipe.attempts)
            .is_some_and(|lineage| lineage.source_run_id == source_run_id)
    });
    let persisted_budget_authority = plan
        .tasks
        .first()
        .map(|task| &task.inputs["cook_loop"]["execution_budget_authority"])
        .filter(|authority| budget_authority_matches_scope(authority, budget_scope))
        .cloned();
    let (budget_limit, mut durable_budget_used) = scoped_follow_up_budget(
        budget_scope,
        budget_limit,
        budget_used,
        persisted_budget_authority.as_ref(),
    );
    let mut authority_run_ids = std::collections::BTreeSet::new();
    if let Some(authority) = persisted_budget_authority.as_ref() {
        let mut ancestor_run_id = Some(source_run_id.to_string());
        while let Some(run_id) = ancestor_run_id {
            let Some(recipe_attempt) = recipe
                .attempts
                .iter()
                .find(|recipe_attempt| recipe_attempt.run_id == run_id)
                .filter(|recipe_attempt| {
                    recipe_attempt.plan.tasks[0].inputs["cook_loop"]["execution_budget_authority"]
                        == *authority
                })
            else {
                break;
            };
            authority_run_ids.insert(recipe_attempt.run_id.as_str());
            ancestor_run_id = super::cook_lineage::recipe_attempt_lineage(recipe_attempt)
                .map(|edge| edge.source_run_id);
        }
    }
    for recipe_attempt in recipe.attempts.iter().filter(|recipe_attempt| {
        authority_run_ids.contains(recipe_attempt.run_id.as_str())
            || (persisted_budget_authority.is_none()
                && super::cook_lineage::execution_lineage(recipe_attempt, &recipe.attempts)
                    .is_some_and(|edge| edge.source_run_id == source_run_id))
    }) {
        if let Ok(aggregate) = lifecycle_store.read_aggregate(&recipe_attempt.run_id) {
            durable_budget_used.add(execution_budget_usage(&aggregate));
        }
    }
    if known_same_executor {
        durable_budget_used.same_provider_retries =
            durable_budget_used.same_provider_retries.saturating_add(
                related_attempts
                    .map(|recipe_attempt| recipe_attempt.attempt)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    .try_into()
                    .unwrap_or(u32::MAX),
            );
    }
    let Some(remaining_budget) = budget_remaining(&budget_limit, durable_budget_used) else {
        return Ok(CookFollowUpDispatch::BudgetExhausted {
            reason: "max_provider_executions".to_string(),
        });
    };
    let durable_provider_executions = lifecycle_store
        .read_record(source_run_id)
        .ok()
        .and_then(|record| record.metadata.get("provider_executions").cloned())
        .filter(|executions| {
            executions
                .as_array()
                .is_some_and(|executions| !executions.is_empty())
        });
    let mut reserved_no_change_rotation = None;
    let same_provider = if no_change_rotation.is_some() {
        // When the rotation budget is spent, restore the unrotated request and
        // fall back to the existing same-provider retry: an exhausted route
        // chain must never fail the cook by itself when the provider that
        // abstained is still available.
        match reserve_remediation_budget(&remaining_budget, false) {
            Ok(reservation) => {
                reserved_no_change_rotation = Some(reservation);
                Some(true)
            }
            Err(_) => {
                follow_up_request = unrotated_follow_up_request;
                cook_remediation_same_provider(
                    promotion,
                    aggregate,
                    plan,
                    durable_provider_executions.as_ref(),
                    &follow_up_request.executor,
                )
            }
        }
    } else {
        known_same_executor.then_some(true).or_else(|| {
            cook_remediation_same_provider(
                promotion,
                aggregate,
                plan,
                durable_provider_executions.as_ref(),
                &follow_up_request.executor,
            )
        })
    };
    let Some(same_provider) = same_provider else {
        return Ok(CookFollowUpDispatch::PolicyFailure {
            reason: "cannot classify Cook remediation without terminal executor identity"
                .to_string(),
        });
    };
    let reservation = if let Some(reservation) = reserved_no_change_rotation {
        // The reroute was applied before the durable replay comparison; this
        // only commits the already-reserved rotation budget.
        reservation
    } else {
        match reserve_remediation_budget(&remaining_budget, same_provider) {
            Ok(reservation) => reservation,
            Err(reason) => {
                return Ok(CookFollowUpDispatch::BudgetExhausted {
                    reason: reason.to_string(),
                })
            }
        }
    };
    // This is reviewable lineage, not the process-local baseline capability.
    follow_up_request.inputs["cook_loop"]["artifact_provenance"] = serde_json::json!({
        "source_run_id": source_run_id,
        "source_task_id": promotion.source.task_id,
        "source_candidate_tree_sha": promotion.candidate.as_ref().map(|candidate| &candidate.tree_sha),
    });
    if budget_scope != CookFollowUpBudgetScope::Cook {
        follow_up_request.inputs["cook_loop"]["execution_budget_authority"] =
            review_budget_authority(budget_scope, persisted_budget_authority.as_ref());
    }
    let next_attempt = recipe
        .attempts
        .iter()
        .map(|recipe_attempt| recipe_attempt.attempt)
        .max()
        .unwrap_or(attempt)
        .max(attempt)
        .checked_add(1)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "durable cook attempt sequence is exhausted",
                Some(cook_id.to_string()),
                None,
            )
        })?;
    let next_run_id = agent_task_lifecycle::cook_attempt_run_id(cook_id, next_attempt);
    let mut follow_up_plan = AgentTaskPlan::new(
        format!("{cook_id}-cook-attempt-{next_attempt}"),
        vec![follow_up_request],
    );
    follow_up_plan.options = plan.options.clone();
    crate::caller_context::inherit_ownership(&mut follow_up_plan.metadata, &plan.metadata);
    crate::caller_context::inherit_ownership(
        &mut follow_up_plan.metadata,
        &options.identity.initial_plan.metadata,
    );
    // Gate-feedback is a child execution, not a fresh one-shot policy.
    // Adoption remediation retains its separately bounded plan.
    follow_up_plan.options.execution_budget = child_execution_budget(budget_scope, &budget_limit);
    if budget_scope != CookFollowUpBudgetScope::Cook {
        follow_up_plan.options.retry.max_attempts = 1;
    }
    super::cook_recipe::record_recipe_attempt_with_lineage_in_store(
        recipe_store,
        cook_id,
        next_attempt,
        &next_run_id,
        &follow_up_plan,
        Some(requested_edge),
    )?;
    let bound = recipe_store
        .load_recipe(cook_id)?
        .attempts
        .into_iter()
        .find(|bound| bound.run_id == next_run_id)
        .expect("attempt was atomically recorded with its lineage");
    // The terminal event has synchronously admitted its successor. Receiver
    // staging/acceptance (or an entire local execution) is not scheduling time.
    drop(schedule_phase);
    dispatch_bound_cook_follow_up(
        recipe_store,
        lifecycle_store,
        options,
        executor,
        cook_id,
        bound,
        source_run_id,
        promotion,
        budget_scope,
        remediation_category_usage,
        reservation,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "dispatch consumes one persisted attempt and its existing admission context"
)]
pub(super) fn dispatch_bound_cook_follow_up(
    recipe_store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
    executor: SharedAgentTaskExecutor,
    cook_id: &str,
    bound: super::cook_recipe::AgentTaskCookRecipeAttempt,
    source_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    budget_scope: CookFollowUpBudgetScope,
    remediation_category_usage: &mut ExecutionBudgetUsage,
    reservation: ExecutionBudgetUsage,
) -> Result<CookFollowUpDispatch> {
    let recipe = recipe_store.load_recipe(cook_id)?;
    let lineage =
        super::cook_lineage::execution_lineage(&bound, &recipe.attempts).ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts.lineage",
                "follow-up has no persisted lineage authority",
                Some(bound.run_id.clone()),
                None,
            )
        })?;
    if lineage.source_run_id != source_run_id {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts.lineage",
            "follow-up parent does not match its dispatch context",
            Some(bound.run_id),
            None,
        ));
    }
    if lineage.kind != super::cook_lineage::CookLineageKind::GateFix {
        return Err(Error::validation_invalid_argument(
            "cook_recipe.attempts.lineage",
            "bound attempt is not a remediation",
            Some(bound.run_id),
            None,
        ));
    }
    let next_attempt = bound.attempt;
    let next_run_id = bound.run_id;
    let mut follow_up_plan = bound.plan;
    if attempt_needs_execution_with_store(lifecycle_store, &next_run_id) {
        let baseline = materialize_follow_up_baseline_in_root(
            promotion,
            &lifecycle_store.artifact_root(),
            source_run_id,
            &follow_up_plan.tasks[0].task_id,
        )?;
        // The bound plan already carries the promotion-derived lineage in
        // `cook_loop.artifact_provenance`; the baseline only moves the root.
        follow_up_plan.tasks[0].workspace.root = Some(baseline.path.display().to_string());
        // Follow-up retries intentionally move into an authenticated baseline.
        // Refresh the durable execution attestation before this plan can be
        // persisted or handed to a local or detached provider.
        bind_dispatch_workspace_attestations(&mut follow_up_plan)?;
        ensure_cook_attempt_admitted(lifecycle_store, cook_id)?;
        lifecycle_store.submit_plan_with_current_runtime(&follow_up_plan, &next_run_id)?;
        lifecycle_store.record_cook_attempt(cook_id, next_attempt, &next_run_id)?;
        if budget_scope == CookFollowUpBudgetScope::CandidateAdoptionReview {
            lifecycle_store
                .checkpoint_candidate_adoption_remediation(source_run_id, &next_run_id)?;
        }
        if let Some(dispatcher) = &options.provider_transport.attempt_dispatcher {
            // A detached dispatcher may return before any executor-side
            // lifecycle write, so a controller crash after the runner accepts
            // the retry but before its state advances would re-dispatch on
            // restart. Bracket the dispatch with a durable operation claim keyed
            // by the retry run id: a completed claim means the retry is already
            // dispatched, so a resumed pass continues from it instead of
            // sending a second handoff (#8357).
            // Persist the exact materialized plan first so a continuation resumes
            // this baseline-bound workspace contract, and so the run record the
            // claim is written onto exists.
            let operation_key = retry_dispatch_operation_key(&next_run_id);
            ensure_cook_attempt_admitted(lifecycle_store, cook_id)?;
            let dispatch_intent = serde_json::json!({
                "schema": "homeboy/cook-dispatch-intent/v1",
                "run_id": next_run_id,
                "operation_key": operation_key,
            });
            match agent_task_lifecycle::claim_operation_with_intent_in_store(
                lifecycle_store,
                &next_run_id,
                &operation_key,
                RETRY_DISPATCH_CLAIM_LEASE,
                &dispatch_intent,
            )? {
                agent_task_lifecycle::ClaimOutcome::Acquired(_lease) => {
                    let dispatched = dispatcher.dispatch_attempt(
                        follow_up_plan,
                        &next_run_id,
                        Some(baseline.capability()),
                    );
                    // A dispatcher can hand the baseline to work that outlives
                    // this call: Lab staging materializes it from a separate
                    // controller job after `dispatch_attempt` returns. Dropping
                    // it here removed the worktree under that job, which then
                    // failed its workspace attestation with an IO error. The
                    // checkout is preserved for the dispatched attempt, as the
                    // lifecycle retry path already does; reaping it belongs to
                    // artifact retention (#15364).
                    baseline.preserve_for_retry();
                    dispatched?;
                    // The accepting daemon writes this receipt atomically with
                    // its handoff ownership. A dispatcher return alone is not
                    // acceptance evidence and cannot complete this operation.
                    let receipt = await_dispatch_acceptance_receipt(
                        lifecycle_store,
                        &next_run_id,
                        &operation_key,
                        RETRY_DISPATCH_ACCEPTANCE_WAIT,
                    )?
                    .ok_or_else(|| {
                        Error::internal_unexpected(
                            "retry dispatcher returned without a durable receiver acceptance receipt",
                        )
                        .with_retryable(true)
                    })?;
                    _lease.complete(serde_json::json!({
                        "dispatched_run_id": next_run_id,
                        "dispatch_receipt": receipt,
                    }))?;
                }
                agent_task_lifecycle::ClaimOutcome::LeaseHeld => {
                    // A receiver receipt survives a controller crash after the
                    // handoff succeeded and before this controller completed
                    // its claim. Complete from that acceptance evidence rather
                    // than issuing a second dispatch.
                    if let Some(receipt) =
                        agent_task_lifecycle::dispatch_acceptance_receipt_in_store(
                            lifecycle_store,
                            &next_run_id,
                            &operation_key,
                        )?
                    {
                        lifecycle_store.complete_cook_operation(
                            &next_run_id,
                            &operation_key,
                            serde_json::json!({
                                "dispatched_run_id": next_run_id,
                                "dispatch_receipt": receipt,
                                "recovered": true,
                            }),
                        )?;
                    }
                }
                // The retry was already durably dispatched (completed) or is
                // owned by a concurrent pass (lease held). Either way, do not
                // send a second handoff; the persisted plan and run state carry
                // the existing dispatch forward.
                agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(_) => {}
            }
        } else {
            ensure_cook_attempt_admitted(lifecycle_store, cook_id)?;
            run_loaded_plan_with_derived_cook_baseline_in_store(
                lifecycle_store,
                follow_up_plan,
                Some(&next_run_id),
                executor,
                Some(baseline.capability()),
                Some(cook_attempt_harvest_context(&options.harvest_context)),
            )?;
        }
    }
    // The generated ID is random by design. Link the execution only after its
    // materialized plan is durable, so a resumed controller selects this exact
    // run without replacing its baseline-bound workspace contract.
    if !attempt_needs_execution_with_store(lifecycle_store, &next_run_id) {
        lifecycle_store.record_cook_attempt(cook_id, next_attempt, &next_run_id)?;
    }
    remediation_category_usage.add(reservation);
    Ok(CookFollowUpDispatch::Dispatched {
        run_id: next_run_id,
    })
}

pub(crate) fn ensure_cook_attempt_admitted(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<()> {
    lifecycle_store.with_store_lock(|| {
        if lifecycle_store
            .read_cook_index(cook_id)
            .ok()
            .is_some_and(|index| index.cancellation_fence.is_some())
        {
            return Err(Error::validation_invalid_argument(
                "cook_id",
                "Cook mission was cancelled; retries and follow-ups are not admitted",
                Some(cook_id.to_string()),
                None,
            ));
        }
        Ok(())
    })
}

pub(super) fn adopted_attempt_is_ready_for_cook_continuation(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<Option<String>> {
    let Some(promotion) =
        persisted_promotion_for_attempt_in_store(lifecycle_store, &record.run_id)?
    else {
        return Ok(None);
    };
    let Some(adoption) = record.candidate_adoption.as_ref() else {
        return Ok(None);
    };
    if adoption.state != "completed" {
        return Ok(None);
    }
    let provenance = &promotion.provenance["adoption"];
    if provenance["candidate_ref"].as_str() == Some(adoption.candidate_sha.as_str())
        && provenance["ai_model"].as_str() == Some(adoption.ai_model.as_str())
    {
        return Ok(Some(adoption.ai_model.clone()));
    }
    Ok(None)
}
