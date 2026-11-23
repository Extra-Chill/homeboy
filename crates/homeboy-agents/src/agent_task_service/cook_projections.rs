//! Cook completion and observation projections.

use super::cook::*;

/// Cook lifecycle and terminality projected from the closed report vocabulary
/// and the producing exit's declared disposition.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct RunLifecycleProjection {
    pub lifecycle_status: ControlPlaneState,
    pub terminal: bool,
    pub retryable: bool,
}

impl RunLifecycleProjection {
    /// Classify a report whose exit declared a [`CookDisposition`].
    pub fn from_status_and_disposition(status: CookStatus, disposition: CookDisposition) -> Self {
        Self::new(
            homeboy_core::run_lifecycle_status::RunLifecycleStatus::from(status),
            disposition.is_terminal(),
        )
    }

    /// Classify a report that has no declared disposition — a batch cell whose
    /// child never produced one, for instance.
    ///
    pub fn from_status(status: &str) -> Self {
        let Some(status) = CookStatus::from_status(status) else {
            return Self::new(ControlPlaneState::Unknown, true);
        };
        let terminal = !status.is_in_flight();
        Self::new(
            homeboy_core::run_lifecycle_status::RunLifecycleStatus::from(&status),
            terminal,
        )
    }

    fn new(lifecycle_status: ControlPlaneState, terminal: bool) -> Self {
        Self {
            lifecycle_status,
            terminal,
            // Gated on terminality so a Cook still being carried by a durable
            // owner is never advertised as something to retry.
            retryable: terminal && lifecycle_status.is_retryable(),
        }
    }
}

impl serde::Serialize for AgentTaskCookReport {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        /// Mirrors [`AgentTaskCookReport`] field for field, in order, with the
        /// same `skip_serializing_if` rules. Borrowed so serialization stays
        /// allocation-free.
        #[derive(serde::Serialize)]
        struct Wire<'a> {
            schema: &'static str,
            cook_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            latest_run_id: Option<&'a String>,
            #[serde(skip_serializing_if = "slice_is_empty")]
            history_run_ids: &'a [String],
            #[serde(skip_serializing_if = "slice_is_empty")]
            invocation_run_ids: &'a [String],
            status: &'a CookStatus,
            inherited_failures: usize,
            disposition: CookDisposition,
            attempts: &'a [AgentTaskCookAttemptReport],
            #[serde(skip_serializing_if = "Option::is_none")]
            finalization: Option<&'a Value>,
            #[serde(skip_serializing_if = "Option::is_none")]
            intentional_no_change: Option<&'a AgentTaskCookIntentionalNoChangeReport>,
            #[serde(skip_serializing_if = "Option::is_none")]
            selected_candidate: Option<&'a Value>,
            #[serde(skip_serializing_if = "Option::is_none")]
            stop_reason: Option<&'a String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            terminal_phase: Option<&'a String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            terminal_failure_classification: Option<&'a String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            primary_failure: Option<&'a AgentTaskCookPrimaryFailure>,
            #[serde(skip_serializing_if = "slice_is_empty")]
            provider_route_notes: &'a [String],
            #[serde(skip_serializing_if = "Option::is_none")]
            failure_context: Option<&'a AgentTaskCookFailureContext>,
            #[serde(skip_serializing_if = "Option::is_none")]
            completion: Option<AgentTaskCookCompletion>,
        }

        let completion = self.completion();
        let wire = Wire {
            schema: self.schema,
            cook_id: &self.cook_id,
            latest_run_id: self.latest_run_id.as_ref(),
            history_run_ids: &self.history_run_ids,
            invocation_run_ids: &self.invocation_run_ids,
            status: &self.status,
            inherited_failures: self.inherited_failure_count(),
            disposition: self.disposition,
            attempts: &self.attempts,
            finalization: self.finalization.as_ref(),
            intentional_no_change: self.intentional_no_change.as_ref(),
            selected_candidate: self.selected_candidate.as_ref(),
            stop_reason: self.stop_reason.as_ref(),
            terminal_phase: self.terminal_phase.as_ref(),
            terminal_failure_classification: self.terminal_failure_classification.as_ref(),
            primary_failure: self.primary_failure.as_ref(),
            provider_route_notes: &self.provider_route_notes,
            failure_context: self.failure_context.as_ref(),
            completion,
        };
        serde::Serialize::serialize(&wire, serializer)
    }
}

/// Resolve a Cook reader ID to its plain subject record.
/// Callers classify its outcome with [`agent_task_lifecycle::CookRunState`].
pub fn cook_observation_in_store(
    store: &AgentTaskLifecycleStore,
    id: &str,
) -> Result<agent_task_lifecycle::AgentTaskRunRecord> {
    let subject = agent_task_lifecycle::resolve_cook_reader_run_id_in_store(store, id)?;
    store.read_record_bounded(&subject)
}

pub(super) fn cook_success(
    terminal: bool,
    verified: bool,
    no_finalize: bool,
    pr_finalized: bool,
) -> bool {
    terminal && verified && (no_finalize || pr_finalized)
}

impl AgentTaskCookReport {
    /// Count only the latest promoted candidate's accepted red gates; failed
    /// earlier attempts must not taint a repaired candidate's completion.
    pub fn inherited_failure_count(&self) -> usize {
        self.attempts
            .iter()
            .rev()
            .find_map(|attempt| attempt.promotion.as_ref())
            .map_or_else(
                || {
                    self.finalization
                        .as_ref()
                        .and_then(|value| value["gate_results"].as_array())
                        .map_or(0, |gates| {
                            gates
                                .iter()
                                .filter(|gate| gate["status"] == "accepted_inherited_failure")
                                .count()
                        })
                },
                |promotion| {
                    promotion.deterministic_gates.iter().filter(|gate| {
                gate.status == crate::agent_task_gate::AgentTaskGateStatus::AcceptedInheritedFailure
            }).count()
                },
            )
    }

    /// Successful Cook completion, rather than provider or launcher success.
    pub fn completed_successfully(&self) -> bool {
        let status = self.status;
        cook_success(
            self.disposition.is_terminal(),
            !status.is_in_flight() && status.is_success_exit(),
            matches!(
                self.status.as_str(),
                "green_no_finalize" | "intentional_no_change"
            ),
            self.completion()
                .is_some_and(|completion| completion.pr_finalized),
        )
    }

    /// The closed-vocabulary classification of this report.
    pub fn lifecycle(&self) -> RunLifecycleProjection {
        RunLifecycleProjection::from_status_and_disposition(self.status, self.disposition)
    }

    /// Project the Cook's end-to-end PR completion separately from provider
    /// output and raw legacy status strings.
    pub fn completion(&self) -> Option<AgentTaskCookCompletion> {
        let finalization_requested = !matches!(
            self.status.as_str(),
            "green_no_finalize" | "intentional_no_change"
        );
        cook_completion_with_stores(
            self.report_stores
                .as_ref()
                .map(|(recipe, lifecycle)| (recipe, lifecycle)),
            self.selected_candidate.as_ref(),
            finalization_requested,
            self.finalization.as_ref(),
            self.latest_run_id.as_deref(),
        )
    }
}

/// A finalization receipt is completion evidence only when it names the PR it
/// published. Older receipt shapes remain serialized unchanged, but cannot make
/// a Cook look complete without that durable identity.
pub fn cook_finalization_is_pr_receipt(finalization: &Value) -> bool {
    super::cook_delivery::publication_receipt(finalization).published
}

/// Project durable candidate and publication facts for both immediate Cook
/// reports and record readers.
pub fn cook_completion(
    selected_candidate: Option<&Value>,
    finalization_requested: bool,
    finalization: Option<&Value>,
    finalization_run_id: Option<&str>,
) -> Option<AgentTaskCookCompletion> {
    cook_completion_with_stores(
        None,
        selected_candidate,
        finalization_requested,
        finalization,
        finalization_run_id,
    )
}

pub(super) fn cook_completion_with_stores(
    stores: Option<(&CookRecipeStore, &AgentTaskLifecycleStore)>,
    selected_candidate: Option<&Value>,
    finalization_requested: bool,
    finalization: Option<&Value>,
    finalization_run_id: Option<&str>,
) -> Option<AgentTaskCookCompletion> {
    let candidate_produced = canonical_candidate_produced(selected_candidate);
    let canonical_finalization = canonical_candidate_finalization_with_stores(
        stores,
        selected_candidate,
        finalization,
        finalization_run_id,
    );
    // A report can carry a receipt before the Cook index is available. Once a
    // canonical selection exists, however, only that selected attempt owns the
    // completion receipt; a newer non-substantive attempt cannot replace it.
    let finalization = canonical_finalization.as_ref().or_else(|| {
        selected_candidate
            .is_none_or(|candidate| candidate["cook_id"].as_str().is_none())
            .then_some(finalization)
            .flatten()
    });
    let pr_finalized = finalization.is_some_and(cook_finalization_is_pr_receipt);
    let recovery_run_id = (!pr_finalized && finalization_requested)
        .then(|| canonical_finalization_recovery_run_id_with_stores(stores, selected_candidate))
        .flatten();
    let state = if pr_finalized {
        "pr_finalized"
    } else if recovery_run_id.is_some() {
        "candidate_awaiting_finalization"
    } else if candidate_produced {
        "candidate_produced"
    } else {
        return None;
    };
    let next_action = (state == "candidate_awaiting_finalization")
        .then_some(recovery_run_id)
        .flatten()
        .map(|run_id| {
            let command = stores.map_or_else(
                || cook_recovery_command(&run_id, &["recover", &run_id]),
                |(_, lifecycle)| {
                    cook_recovery_command_in_store(lifecycle, &run_id, &["recover", &run_id])
                },
            );
            AgentTaskCookRecoveryAction {
                action: "publish".to_string(),
                command,
            }
        });
    Some(AgentTaskCookCompletion {
        schema: "homeboy/agent-task-cook-completion/v1",
        candidate_produced,
        finalization_requested,
        pr_finalized,
        state,
        next_action,
    })
}

/// A candidate selection is substantive only when the controller named the
/// immutable task and patch artifact it selected. The legacy fallback selection
/// leaves those fields empty for empty, unknown, and conflicting attempts.
pub(super) fn canonical_candidate_produced(selected_candidate: Option<&Value>) -> bool {
    let Some(candidate) = selected_candidate else {
        return false;
    };
    candidate["incomplete"] != true
        && candidate["run_id"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
        && candidate["selected_task_id"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
        && candidate["selected_candidate_sha"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
}

/// Recovery needs the exact selected immutable artifact, a finalizable promotion,
/// required-gate evidence accepted by the Cook recipe, and reviewer metadata.
/// The optional stores keep reads rooted to the installation being reconciled.
pub(super) fn canonical_finalization_recovery_run_id_with_stores(
    stores: Option<(&CookRecipeStore, &AgentTaskLifecycleStore)>,
    selected_candidate: Option<&Value>,
) -> Option<String> {
    if !canonical_candidate_produced(selected_candidate) {
        return None;
    }
    let candidate = selected_candidate?;
    let run_id = candidate["run_id"].as_str()?;
    let task_id = candidate["selected_task_id"].as_str()?;
    let cook_id = candidate["cook_id"].as_str()?;
    let promotion = stores
        .map(|(_, lifecycle)| {
            super::cook_promotion::persisted_promotion_for_attempt_in_store(lifecycle, run_id)
        })
        .unwrap_or_else(|| super::cook_promotion::persisted_promotion_for_attempt(run_id))
        .ok()
        .flatten()?;
    let recovery_run_id =
        super::cook_promotion::canonical_cook_recovery_run_id_with_stores(stores, cook_id)?;
    let title_policy = stores
        .map(|(recipes, _)| recipes.load_recipe(cook_id))
        .unwrap_or_else(|| super::cook_recipe::load_recipe(cook_id))
        .ok()
        .and_then(|recipe| {
            recipe.attempts.first().map(|attempt| {
                crate::agent_task_review_dossier::PrTitlePolicy::from_plan(&attempt.plan)
            })
        })
        .unwrap_or(crate::agent_task_review_dossier::PrTitlePolicy::ConventionalCommit);
    if !canonical_finalization_eligible(
        &promotion,
        review_form_from_aggregate(
            &stores
                .map(|(_, lifecycle)| lifecycle.read_aggregate(&recovery_run_id))
                .unwrap_or_else(|| agent_task_lifecycle::read_attempt_aggregate(&recovery_run_id))
                .ok()?,
        )
        .ok()
        .flatten()
        .is_some_and(|form| form.validate(title_policy).is_ok()),
    ) || promotion.source.run_id.as_deref() != Some(run_id)
        || promotion.source.task_id != task_id
        || promotion
            .candidate
            .as_ref()
            .is_none_or(|candidate| candidate.tree_sha.is_empty())
    {
        return None;
    }
    Some(recovery_run_id)
}

pub(super) fn canonical_finalization_eligible(
    promotion: &AgentTaskPromotionReport,
    has_valid_review_form: bool,
) -> bool {
    promotion.applied_with_passing_verdict() && has_valid_review_form
}

/// Read finalization from the canonical selected attempt or its authenticated
/// form-only continuation. An exact status read may target a later attempt, but
/// only a continuation that carries this candidate's verified promotion may own
/// its reviewer-metadata receipt.
pub(crate) fn canonical_candidate_finalization(
    selected_candidate: Option<&Value>,
    inspected_finalization: Option<&Value>,
    inspected_run_id: Option<&str>,
) -> Option<Value> {
    canonical_candidate_finalization_with_stores(
        None,
        selected_candidate,
        inspected_finalization,
        inspected_run_id,
    )
}

pub(crate) fn canonical_candidate_finalization_with_stores(
    stores: Option<(&CookRecipeStore, &AgentTaskLifecycleStore)>,
    selected_candidate: Option<&Value>,
    inspected_finalization: Option<&Value>,
    inspected_run_id: Option<&str>,
) -> Option<Value> {
    if !canonical_candidate_produced(selected_candidate) {
        return None;
    }
    let candidate = selected_candidate?;
    let cook_id = candidate["cook_id"].as_str()?;
    let run_id = candidate["run_id"].as_str()?;
    if inspected_run_id == Some(run_id) {
        if let Some(finalization) = inspected_finalization {
            return Some(finalization.clone());
        }
    }
    let record = stores
        .map(|(_, lifecycle)| lifecycle.read_record(run_id))
        .unwrap_or_else(|| agent_task_lifecycle::exact_record(run_id))
        .ok()?;
    if record.metadata["cook_id"].as_str() == Some(cook_id) {
        if let Some(finalization) = super::recorded_finalization_receipt(&record.metadata) {
            return Some(finalization.clone());
        }
    }
    None
}

/// Resolve the controller-owned candidate selection for a Cook alias. Callers
/// that publish or recover must use this instead of ranking attempt history.
pub(crate) fn canonical_cook_candidate(cook_id: &str) -> Option<Value> {
    agent_task_lifecycle::select_cook_candidate(cook_id)
        .ok()
        .and_then(|selection| serde_json::to_value(selection).ok())
}

pub(crate) fn canonical_cook_candidate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Option<Value> {
    lifecycle_store
        .select_cook_candidate(cook_id)
        .ok()
        .and_then(|selection| serde_json::to_value(selection).ok())
}

/// `skip_serializing_if` predicate for a borrowed slice field.
///
/// Serde passes the predicate a reference to the field, so a `&'a [T]` field
/// arrives as `&&[T]`. Spelled out rather than relying on `<[T]>::is_empty`
/// resolving through an extra layer of deref.
pub(super) fn slice_is_empty<T>(value: &&[T]) -> bool {
    value.is_empty()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookIntentionalNoChangeReport {
    #[serde(flatten)]
    pub declaration: AgentTaskIntentionalNoChange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_form: Option<crate::agent_task_review_dossier::AiFilledReviewForm>,
}

/// Durable identity and legal recovery surface for a failed Cook.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookFailureContext {
    pub cook_id: String,
    pub latest_run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_candidate_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion_provenance: Option<Value>,
    pub durable_recipe_ref: String,
    pub lifecycle_state: String,
    pub phase: String,
    pub reason_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<Value>,
    /// Bounded, durable admission predicates for a denied historical
    /// continuation. It intentionally contains no provider output or evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_admission: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_claim: Option<Value>,
    pub provider_budget_consumed: bool,
    pub provider_executions_consumed: u64,
    pub recovery_legal: bool,
    pub recovery_reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legal_actions: Vec<AgentTaskCookRecoveryAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<AgentTaskCookRecoveryAction>,
    /// The single step `agent-task recover` performs for this state; the
    /// mutating entries of `legal_actions` are derived from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_step: Option<CookRecoveryStep>,
}

/// An exact command that is legal for the durable Cook state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookRecoveryAction {
    pub action: String,
    pub command: String,
}

/// Bounded primary cause for a terminal provider failure.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookPrimaryFailure {
    pub schema: &'static str,
    pub provider_id: String,
    pub operation: String,
    pub phase: String,
    pub exit_code: i64,
    pub stderr_excerpt: String,
    pub evidence_ref: String,
    pub next_action: AgentTaskCookRecoveryAction,
    /// The typed terminal diagnostic when provider execution, rather than
    /// pre-execution setup, exhausted its available routes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<Value>,
}

pub(super) fn terminal_fallback_status(
    state: homeboy_lifecycle_contract::RunExecutionState,
) -> CookStatus {
    match state {
        homeboy_lifecycle_contract::RunExecutionState::Cancelled => CookStatus::Cancelled,
        _ => CookStatus::ProviderFailure,
    }
}

/// A normal provider stop can still fail the patch contract when the agent
/// explicitly reports that it could not finish. That is a candidate-selection
/// result, not a provider outage. Keep this narrow so other failures retain
/// their provider diagnostics and recovery actions.
/// Why a terminal run stopped: the failed outcome's own cause (for example a
/// refused committed-change harvest), not just the lifecycle state.
pub(super) fn terminal_stop_reason(
    run_id: &str,
    state: homeboy_lifecycle_contract::RunExecutionState,
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
) -> String {
    let cause = aggregate
        .outcomes
        .iter()
        .filter(|outcome| outcome.status != crate::agent_task::AgentTaskOutcomeStatus::Succeeded)
        .find_map(|outcome| {
            outcome
                .summary
                .as_deref()
                .filter(|summary| !summary.trim().is_empty())
        });
    match cause {
        Some(cause) => format!("agent-task run {run_id} ended in state {state:?}: {cause}"),
        None => format!("agent-task run {run_id} ended in state {state:?}"),
    }
}

pub(super) fn incomplete_no_candidate_reason(
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
) -> Option<String> {
    let [outcome] = aggregate.outcomes.as_slice() else {
        return None;
    };
    if outcome.metadata["stop_reason"] != "stop"
        || outcome.metadata["exit_code"]
            .as_i64()
            .is_some_and(|code| code != 0)
        || outcome.candidate.is_some()
        || outcome
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "patch")
        || !outcome.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.required_typed_artifacts_missing"
                && diagnostic.data["missing"] == serde_json::json!(["patch"])
        })
        || outcome.diagnostics.iter().any(|diagnostic| {
            !matches!(
                diagnostic.class.as_str(),
                "provider" | "agent_task.required_typed_artifacts_missing"
            ) || diagnostic.data.get("failure_code").is_some()
                || (diagnostic.class == "agent_task.required_typed_artifacts_missing"
                    && diagnostic.data["missing"] != serde_json::json!(["patch"]))
        })
    {
        return None;
    }
    let form = review_form_from_aggregate(aggregate).ok()??;
    form.incomplete_reason()
        .filter(|reason| !reason.is_empty())
        .map(str::to_string)
}

/// Read the first durable outcome diagnostic for a failed attempt. Batch
/// coordinators use this bounded projection when a Cook report only retains
/// generic failure context; expanded evidence remains attached to the run.
pub fn attempt_primary_failure_diagnostic(run_id: &str) -> Option<Value> {
    let aggregate = agent_task_lifecycle::read_attempt_aggregate(run_id).ok()?;
    aggregate
        .outcomes
        .iter()
        .filter(|outcome| outcome.status == crate::agent_task::AgentTaskOutcomeStatus::Failed)
        .flat_map(|outcome| &outcome.diagnostics)
        .next()
        .and_then(|diagnostic| serde_json::to_value(diagnostic).ok())
}
