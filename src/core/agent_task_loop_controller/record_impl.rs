use super::*;
use crate::core::agent_task_loop_runner_policy::{
    blocked_runner_decision, runner_policy_for_action,
};
use crate::core::{Error, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

impl AgentTaskLoopControllerRecord {
    pub fn new(
        loop_id: impl Into<String>,
        phase: impl Into<String>,
        config_version: impl Into<String>,
    ) -> Self {
        let now = now_timestamp();
        Self {
            schema: AGENT_TASK_LOOP_CONTROLLER_SCHEMA.to_string(),
            loop_id: sanitize_loop_id(&loop_id.into()),
            phase: phase.into(),
            state: AgentTaskLoopControllerState::Running,
            config_version: config_version.into(),
            parent_loop_id: None,
            parent_action_id: None,
            parent_entity_id: None,
            created_at: now.clone(),
            updated_at: now,
            entities: BTreeMap::new(),
            dedupe_keys: BTreeMap::new(),
            task_lineage: Vec::new(),
            gate_bundles: Vec::new(),
            gate_results: Vec::new(),
            terminal_outcomes: Vec::new(),
            waits: Vec::new(),
            subcontrollers: Vec::new(),
            feedback: Vec::new(),
            pr_ownerships: Vec::new(),
            next_actions: Vec::new(),
            history: Vec::new(),
            metadata: Value::Null,
        }
    }

    pub fn upsert_entity(
        &mut self,
        entity_type: impl Into<String>,
        key: impl Into<String>,
        parent_entity_ids: Vec<String>,
        metadata: Value,
    ) -> String {
        let entity_type = entity_type.into();
        let key = key.into();
        let dedupe_key = entity_dedupe_key(&entity_type, &key);
        if let Some(existing) = self.dedupe_keys.get(&dedupe_key) {
            if let Some(entity_id) = &existing.entity_id {
                return entity_id.clone();
            }
        }

        let entity_id = format!("{}:{}", entity_type, sanitize_loop_id(&key));
        let entity = AgentTaskLoopEntity {
            entity_id: entity_id.clone(),
            entity_type,
            key,
            dedupe_key: dedupe_key.clone(),
            state: None,
            human_ready: false,
            parent_entity_ids,
            run_refs: Vec::new(),
            artifact_refs: Vec::new(),
            provenance: Vec::new(),
            metadata,
        };
        self.entities.insert(entity_id.clone(), entity);
        self.dedupe_keys.insert(
            dedupe_key.clone(),
            AgentTaskLoopDedupeRecord {
                dedupe_key,
                action: "entity".to_string(),
                entity_id: Some(entity_id.clone()),
                run_id: None,
                external_ref: None,
                created_at: now_timestamp(),
                reason: Some("entity key registered".to_string()),
            },
        );
        self.touch();
        entity_id
    }

    pub fn apply_event(
        &mut self,
        event: AgentTaskLoopExternalEvent,
    ) -> Vec<AgentTaskLoopPolicyActionRecord> {
        let recorded_at = now_timestamp();
        self.history.push(AgentTaskLoopHistoryEvent {
            event_id: event.event_id.clone(),
            event_type: event.event_type.clone(),
            recorded_at,
            entity_id: event.entity_id.clone(),
            payload: event.payload.clone(),
        });

        for wait in &mut self.waits {
            if wait.status != AgentTaskLoopWaitStatus::Open || wait.event_type != event.event_type {
                continue;
            }
            let entity_matches = wait.entity_id.is_none() || wait.entity_id == event.entity_id;
            let external_matches =
                wait.external_ref.is_none() || wait.external_ref == event.event_key;
            if entity_matches && external_matches {
                wait.status = AgentTaskLoopWaitStatus::Satisfied;
                wait.satisfied_by_event_id = Some(event.event_id.clone());
            }
        }

        if self.open_wait_count() == 0 && self.state == AgentTaskLoopControllerState::Waiting {
            self.state = AgentTaskLoopControllerState::Running;
        }

        let mut actions = Vec::new();
        if let Some(policy) = event
            .payload
            .get("policy")
            .and_then(|value| serde_json::from_value::<AgentTaskLoopPolicy>(value.clone()).ok())
        {
            actions = self.evaluate_policy(&policy, Some(&event));
        }
        self.touch();
        actions
    }

    pub fn evaluate_policy(
        &mut self,
        policy: &AgentTaskLoopPolicy,
        event: Option<&AgentTaskLoopExternalEvent>,
    ) -> Vec<AgentTaskLoopPolicyActionRecord> {
        let mut records = Vec::new();
        for transition in &policy.transitions {
            if !self.transition_matches(transition, event) {
                continue;
            }
            for action in &transition.actions {
                records.push(self.record_action(
                    action.clone(),
                    format!(
                        "policy {} transition {} matched",
                        policy.policy_id, transition.transition_id
                    ),
                ));
            }
        }
        self.touch();
        records
    }

    fn transition_matches(
        &self,
        transition: &AgentTaskLoopTransition,
        event: Option<&AgentTaskLoopExternalEvent>,
    ) -> bool {
        if transition
            .from_phase
            .as_deref()
            .is_some_and(|phase| phase != self.phase)
        {
            return false;
        }
        if let Some(expected_event_type) = &transition.on_event_type {
            if event.map(|event| event.event_type.as_str()) != Some(expected_event_type.as_str()) {
                return false;
            }
        }
        let Some(expr) = &transition.when_json_path else {
            return true;
        };
        let Ok(path) = serde_json_path::JsonPath::parse(expr) else {
            return false;
        };
        let context = json!({
            "controller": self,
            "event": event,
        });
        path.query(&context)
            .all()
            .into_iter()
            .any(jsonpath_match_is_truthy)
    }

    pub fn record_action(
        &mut self,
        action: AgentTaskLoopPolicyAction,
        reason: impl Into<String>,
    ) -> AgentTaskLoopPolicyActionRecord {
        let reason = reason.into();
        let dedupe_key = action_dedupe_key(&action);
        let status = if let Some(dedupe_key) = &dedupe_key {
            if self.dedupe_keys.contains_key(dedupe_key) {
                AgentTaskLoopActionStatus::AlreadySatisfied
            } else {
                self.dedupe_keys.insert(
                    dedupe_key.clone(),
                    AgentTaskLoopDedupeRecord {
                        dedupe_key: dedupe_key.clone(),
                        action: action_name(&action).to_string(),
                        entity_id: action_entity_id(&action),
                        run_id: None,
                        external_ref: None,
                        created_at: now_timestamp(),
                        reason: Some(reason.clone()),
                    },
                );
                AgentTaskLoopActionStatus::Pending
            }
        } else {
            AgentTaskLoopActionStatus::Pending
        };

        let action_id = format!("action-{}", self.next_actions.len() + 1);
        self.apply_action_side_effects(&action, status, &action_id);
        let record = AgentTaskLoopPolicyActionRecord {
            action_id,
            action,
            status,
            reason,
            created_at: now_timestamp(),
            dedupe_key,
            diagnostics: Vec::new(),
        };
        self.next_actions.push(record.clone());
        self.touch();
        record
    }

    pub(crate) fn block_action_for_runner_policy(
        &mut self,
        action_id: &str,
        status: AgentTaskLoopActionStatus,
        diagnostic: AgentTaskLoopActionDiagnostic,
    ) -> Result<()> {
        if !matches!(
            status,
            AgentTaskLoopActionStatus::BlockedRunnerUnavailable
                | AgentTaskLoopActionStatus::BlockedRemoteMaterialization
                | AgentTaskLoopActionStatus::BlockedLocalFallbackDenied
        ) {
            return Err(Error::validation_invalid_argument(
                "status",
                "runner policy blocks must use a blocked action status",
                Some(format!("{status:?}")),
                None,
            ));
        }

        let action = self
            .next_actions
            .iter_mut()
            .find(|action| action.action_id == action_id)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "action_id",
                    format!("loop action '{action_id}' does not exist"),
                    Some(action_id.to_string()),
                    None,
                )
            })?;
        action.status = status;
        action.reason = diagnostic.message.clone();
        action.diagnostics.push(diagnostic.clone());
        self.history.push(AgentTaskLoopHistoryEvent {
            event_id: format!("runner-policy-block-{}", self.history.len() + 1),
            event_type: "runner_policy.blocked".to_string(),
            recorded_at: now_timestamp(),
            entity_id: action_entity_id(&action.action),
            payload: json!({
                "action_id": action_id,
                "status": status,
                "diagnostic": diagnostic,
            }),
        });
        self.touch();
        Ok(())
    }

    pub fn resolve_action_runner_policy<F>(
        &self,
        action: &AgentTaskLoopPolicyAction,
        mut runner_availability: F,
    ) -> AgentTaskLoopRunnerPolicyDecision
    where
        F: FnMut(&str) -> AgentTaskLoopRunnerAvailability,
    {
        let policy = runner_policy_for_action(action);
        let fallback = policy.local_fallback.unwrap_or_else(|| {
            if policy.runner.is_some() {
                AgentTaskLoopLocalFallbackPolicy::Denied
            } else {
                AgentTaskLoopLocalFallbackPolicy::Allowed
            }
        });

        let Some(runner) = policy.runner else {
            return match fallback {
                AgentTaskLoopLocalFallbackPolicy::Allowed => AgentTaskLoopRunnerPolicyDecision {
                    target: Some(AgentTaskLoopRunnerExecutionTarget::Local),
                    blocked_status: None,
                    diagnostic: None,
                },
                AgentTaskLoopLocalFallbackPolicy::Denied => blocked_runner_decision(
                    AgentTaskLoopActionStatus::BlockedLocalFallbackDenied,
                    None,
                    "controller action denies local fallback but did not declare a runner",
                    Value::Null,
                ),
            };
        };

        match runner_availability(&runner) {
            AgentTaskLoopRunnerAvailability::Available => AgentTaskLoopRunnerPolicyDecision {
                target: Some(AgentTaskLoopRunnerExecutionTarget::Runner(runner)),
                blocked_status: None,
                diagnostic: None,
            },
            AgentTaskLoopRunnerAvailability::Unavailable { reason } => match fallback {
                AgentTaskLoopLocalFallbackPolicy::Allowed => AgentTaskLoopRunnerPolicyDecision {
                    target: Some(AgentTaskLoopRunnerExecutionTarget::Local),
                    blocked_status: None,
                    diagnostic: Some(AgentTaskLoopActionDiagnostic {
                        code: "runner_unavailable_local_fallback_allowed".to_string(),
                        message: reason,
                        runner: Some(runner),
                        details: Value::Null,
                    }),
                },
                AgentTaskLoopLocalFallbackPolicy::Denied => blocked_runner_decision(
                    AgentTaskLoopActionStatus::BlockedRunnerUnavailable,
                    Some(runner),
                    reason,
                    Value::Null,
                ),
            },
            AgentTaskLoopRunnerAvailability::MaterializationBlocked { reason } => match fallback {
                AgentTaskLoopLocalFallbackPolicy::Allowed => AgentTaskLoopRunnerPolicyDecision {
                    target: Some(AgentTaskLoopRunnerExecutionTarget::Local),
                    blocked_status: None,
                    diagnostic: Some(AgentTaskLoopActionDiagnostic {
                        code: "remote_materialization_blocked_local_fallback_allowed".to_string(),
                        message: reason,
                        runner: Some(runner),
                        details: Value::Null,
                    }),
                },
                AgentTaskLoopLocalFallbackPolicy::Denied => blocked_runner_decision(
                    AgentTaskLoopActionStatus::BlockedRemoteMaterialization,
                    Some(runner),
                    reason,
                    Value::Null,
                ),
            },
        }
    }

    pub fn mark_human_ready(&mut self, entity_id: &str, reason: Option<String>) -> Result<()> {
        let entity = self.entities.get_mut(entity_id).ok_or_else(|| {
            Error::validation_invalid_argument(
                "entity_id",
                format!("loop entity '{entity_id}' does not exist"),
                Some(entity_id.to_string()),
                None,
            )
        })?;
        entity.human_ready = true;
        entity.state = Some("human_ready".to_string());
        self.state = AgentTaskLoopControllerState::HumanReady;
        self.history.push(AgentTaskLoopHistoryEvent {
            event_id: format!("human-ready-{}", self.history.len() + 1),
            event_type: "human_ready".to_string(),
            recorded_at: now_timestamp(),
            entity_id: Some(entity_id.to_string()),
            payload: json!({ "reason": reason }),
        });
        self.touch();
        Ok(())
    }

    // Part of the loop-controller API exercised only by tests; production wiring is pending.
    #[cfg(test)]
    pub(crate) fn route_finding_packet(
        &mut self,
        finding: AgentTaskLoopFindingPacket,
        request_template: Value,
    ) -> AgentTaskLoopPolicyActionRecord {
        let dedupe_key = format!(
            "finding:{}",
            finding
                .reproduction_key
                .as_deref()
                .unwrap_or(&finding.finding_id)
        );
        if self.dedupe_keys.contains_key(&dedupe_key) {
            return self.record_action(
                AgentTaskLoopPolicyAction::RouteFinding {
                    finding,
                    dedupe_key,
                    entity_id: None,
                    request_template,
                },
                "finding packet route already satisfied",
            );
        }

        let entity_id = self.upsert_entity(
            "finding",
            finding
                .reproduction_key
                .as_deref()
                .unwrap_or(&finding.finding_id),
            Vec::new(),
            json!({
                "severity": finding.severity.clone(),
                "owner": finding.owner.clone(),
                "source_transformer": finding.source_transformer.clone(),
            }),
        );
        if let Some(entity) = self.entities.get_mut(&entity_id) {
            entity.artifact_refs.extend(finding.lineage.clone());
            entity
                .provenance
                .extend(finding.lineage.iter().map(|artifact| {
                    AgentTaskLoopProvenanceRef {
                        kind: artifact
                            .kind
                            .clone()
                            .unwrap_or_else(|| "artifact".to_string()),
                        uri: artifact.uri.clone(),
                        caused_by: Some(finding.finding_id.clone()),
                    }
                }));
        }
        self.record_action(
            AgentTaskLoopPolicyAction::RouteFinding {
                finding,
                dedupe_key,
                entity_id: Some(entity_id),
                request_template,
            },
            "finding packet routed to follow-up task",
        )
    }

    // Part of the loop-controller API exercised only by tests; production wiring is pending.
    #[cfg(test)]
    pub(crate) fn record_candidate_patch_validation(
        &mut self,
        candidate: AgentTaskLoopCandidatePatch,
        validation: AgentTaskLoopCandidateValidation,
        limits: AgentTaskLoopCandidateLoopLimits,
    ) -> AgentTaskLoopPolicyActionRecord {
        let entity_id = self.upsert_entity(
            "candidate_patch",
            &candidate.candidate_id,
            candidate.finding_id.clone().into_iter().collect(),
            json!({
                "worktree": candidate.worktree.clone(),
                "attempt": candidate.attempt,
                "finding_id": candidate.finding_id.clone(),
            }),
        );
        if let Some(entity) = self.entities.get_mut(&entity_id) {
            entity.artifact_refs.push(candidate.patch.clone());
            entity.artifact_refs.extend(candidate.lineage.clone());
            entity.artifact_refs.extend(validation.evidence.clone());
        }

        self.record_action(
            AgentTaskLoopPolicyAction::ValidateCandidatePatch {
                candidate,
                validation,
                limits,
            },
            "candidate patch validation recorded",
        )
    }

    pub fn record_pr_ownership_status(
        &mut self,
        request: &AgentTaskPrOwnershipRequest,
        entity_id: Option<String>,
        status: AgentTaskPrOwnershipStatusUpdate,
    ) -> AgentTaskPrOwnershipRecord {
        let state = pr_ownership_state_from_status(&status, request);
        let record = AgentTaskPrOwnershipRecord {
            ownership_id: request.ownership_id.clone(),
            entity_id: entity_id.clone(),
            state,
            base: request.base.clone(),
            head: request.head.clone(),
            pr_number: status.pr_number.or(request.pr_number),
            pr_url: status.pr_url.clone().or_else(|| request.pr_url.clone()),
            head_sha: status.head_sha.clone(),
            ci_state: status.ci_state.clone(),
            ci_summary: status.ci_summary.clone(),
            review_decision: status.review_decision.clone(),
            merge_state: status.merge_state.clone(),
            retry_count: status.retry_count,
            max_retries: request.max_retries,
            merge_required: request.merge_required,
            last_checked_at: Some(now_timestamp()),
            evidence: status.evidence.clone(),
        };

        if let Some(existing) = self
            .pr_ownerships
            .iter_mut()
            .find(|existing| existing.ownership_id == record.ownership_id)
        {
            *existing = record.clone();
        } else {
            self.pr_ownerships.push(record.clone());
        }

        if let Some(entity_id) = &entity_id {
            if let Some(entity) = self.entities.get_mut(entity_id) {
                entity.state = Some(format!("pr_{:?}", state).to_ascii_lowercase());
                entity.metadata = merge_json_object(
                    entity.metadata.clone(),
                    json!({
                        "pr_ownership": {
                            "ownership_id": record.ownership_id,
                            "pr_number": record.pr_number,
                            "pr_url": record.pr_url,
                            "head": record.head,
                            "ci_state": record.ci_state,
                            "merge_state": record.merge_state,
                            "review_decision": record.review_decision,
                            "state": state,
                        }
                    }),
                );
            }
        }

        self.history.push(AgentTaskLoopHistoryEvent {
            event_id: format!("pr-ownership-{}", self.history.len() + 1),
            event_type: "github.pr.ownership_status".to_string(),
            recorded_at: now_timestamp(),
            entity_id,
            payload: json!({ "ownership": record }),
        });
        self.touch();
        record
    }

    pub fn record_terminal_outcome(
        &mut self,
        status: AgentTaskLoopTerminalStatus,
        reason: impl Into<String>,
        action_id: Option<String>,
        entity_id: Option<String>,
        details: Value,
    ) -> AgentTaskLoopTerminalOutcome {
        let outcome = AgentTaskLoopTerminalOutcome {
            outcome_id: format!("terminal-outcome-{}", self.terminal_outcomes.len() + 1),
            status,
            reason: reason.into(),
            action_id,
            entity_id,
            details,
            recorded_at: now_timestamp(),
        };
        self.history.push(AgentTaskLoopHistoryEvent {
            event_id: format!("terminal-outcome-{}", self.history.len() + 1),
            event_type: "controller.terminal_outcome.recorded".to_string(),
            recorded_at: now_timestamp(),
            entity_id: outcome.entity_id.clone(),
            payload: json!({ "outcome": outcome.clone() }),
        });
        self.terminal_outcomes.push(outcome.clone());
        self.touch();
        outcome
    }

    fn apply_action_side_effects(
        &mut self,
        action: &AgentTaskLoopPolicyAction,
        status: AgentTaskLoopActionStatus,
        action_id: &str,
    ) {
        if status == AgentTaskLoopActionStatus::AlreadySatisfied {
            return;
        }
        match action {
            AgentTaskLoopPolicyAction::RouteFinding {
                entity_id: Some(entity_id),
                ..
            } => {
                if let Some(entity) = self.entities.get_mut(entity_id) {
                    entity.state = Some("routed".to_string());
                }
            }
            AgentTaskLoopPolicyAction::RouteFinding {
                entity_id: None, ..
            } => {}
            AgentTaskLoopPolicyAction::ValidateCandidatePatch {
                candidate,
                validation,
                limits,
            } => {
                let entity_id = format!(
                    "candidate_patch:{}",
                    sanitize_loop_id(&candidate.candidate_id)
                );
                if let Some(entity) = self.entities.get_mut(&entity_id) {
                    match validation.status {
                        AgentTaskLoopCandidateValidationStatus::Passed => {
                            entity.state = Some("validated".to_string());
                            entity.human_ready = true;
                            self.state = AgentTaskLoopControllerState::HumanReady;
                        }
                        AgentTaskLoopCandidateValidationStatus::Failed
                            if candidate.attempt >= limits.max_attempts =>
                        {
                            entity.state = Some("retry_limit_reached".to_string());
                            entity.human_ready = true;
                            self.state = AgentTaskLoopControllerState::HumanReady;
                        }
                        AgentTaskLoopCandidateValidationStatus::Failed => {
                            entity.state = Some("needs_retry".to_string());
                        }
                    }
                }
            }
            AgentTaskLoopPolicyAction::OwnPrUntilGreen {
                ownership,
                entity_id,
            } => {
                let key = format!(
                    "{}#{}",
                    ownership.head,
                    ownership.pr_number.unwrap_or_default()
                );
                let pr_entity_id = entity_id.clone().unwrap_or_else(|| {
                    self.upsert_entity(
                        "pull_request",
                        key,
                        Vec::new(),
                        json!({
                            "ownership_id": ownership.ownership_id,
                            "base": ownership.base,
                            "head": ownership.head,
                            "pr_number": ownership.pr_number,
                            "pr_url": ownership.pr_url,
                        }),
                    )
                });
                self.record_pr_ownership_status(
                    ownership,
                    Some(pr_entity_id),
                    AgentTaskPrOwnershipStatusUpdate::tracking(),
                );
            }
            AgentTaskLoopPolicyAction::WaitForEvent(wait) => {
                self.state = AgentTaskLoopControllerState::Waiting;
                if !self
                    .waits
                    .iter()
                    .any(|existing| existing.wait_key == wait.wait_key)
                {
                    self.waits.push(wait.clone());
                }
            }
            AgentTaskLoopPolicyAction::SpawnController {
                dedupe_key,
                loop_id,
                entity_id,
                request,
                ..
            }
            | AgentTaskLoopPolicyAction::SpawnSubloop {
                dedupe_key,
                loop_id,
                entity_id,
                request,
                ..
            } => {
                self.record_subcontroller_ref(
                    loop_id,
                    dedupe_key,
                    entity_id.clone(),
                    Some(action_id.to_string()),
                    None,
                    Vec::new(),
                    request.clone(),
                );
            }
            AgentTaskLoopPolicyAction::WaitForController {
                loop_id,
                entity_id,
                wait_key,
                terminal_states,
            } => {
                self.state = AgentTaskLoopControllerState::Waiting;
                let wait_key = wait_key
                    .clone()
                    .unwrap_or_else(|| controller_wait_key(loop_id));
                let terminal_states = controller_terminal_states(terminal_states);
                self.record_subcontroller_ref(
                    loop_id,
                    &format!("controller:{loop_id}"),
                    entity_id.clone(),
                    None,
                    Some(wait_key.clone()),
                    terminal_states.clone(),
                    Value::Null,
                );
                if !self
                    .waits
                    .iter()
                    .any(|existing| existing.wait_key == wait_key)
                {
                    self.waits.push(AgentTaskLoopWait {
                        wait_key,
                        event_type: "controller.terminal".to_string(),
                        entity_id: entity_id.clone(),
                        external_ref: Some(loop_id.clone()),
                        timeout_at: None,
                        escalation_policy: None,
                        status: AgentTaskLoopWaitStatus::Open,
                        satisfied_by_event_id: None,
                    });
                }
            }
            AgentTaskLoopPolicyAction::MarkHumanReady { entity_id, reason } => {
                let _ = self.mark_human_ready(entity_id, reason.clone());
            }
            _ => {}
        }
    }

    pub(crate) fn open_wait_count(&self) -> usize {
        self.waits
            .iter()
            .filter(|wait| wait.status == AgentTaskLoopWaitStatus::Open)
            .count()
    }

    pub(crate) fn touch(&mut self) {
        self.updated_at = now_timestamp();
    }

    #[allow(clippy::too_many_arguments)]
    fn record_subcontroller_ref(
        &mut self,
        loop_id: &str,
        dedupe_key: &str,
        entity_id: Option<String>,
        parent_action_id: Option<String>,
        wait_key: Option<String>,
        terminal_states: Vec<AgentTaskLoopControllerState>,
        request: Value,
    ) {
        if let Some(existing) = self
            .subcontrollers
            .iter_mut()
            .find(|existing| existing.dedupe_key == dedupe_key || existing.loop_id == loop_id)
        {
            existing.entity_id = existing.entity_id.clone().or(entity_id);
            existing.parent_action_id = existing.parent_action_id.clone().or(parent_action_id);
            existing.wait_key = existing.wait_key.clone().or(wait_key);
            if existing.terminal_states.is_empty() {
                existing.terminal_states = terminal_states;
            }
            if existing.request.is_null() {
                existing.request = request;
            }
            existing.updated_at = now_timestamp();
            return;
        }

        let now = now_timestamp();
        self.subcontrollers.push(AgentTaskLoopSubcontrollerRef {
            loop_id: sanitize_loop_id(loop_id),
            dedupe_key: dedupe_key.to_string(),
            entity_id,
            parent_loop_id: Some(self.loop_id.clone()),
            parent_action_id,
            wait_key,
            terminal_states,
            state: None,
            created_at: now.clone(),
            updated_at: now,
            request,
        });
    }
}

pub(crate) fn pr_ownership_state_from_status(
    status: &AgentTaskPrOwnershipStatusUpdate,
    request: &AgentTaskPrOwnershipRequest,
) -> AgentTaskPrOwnershipState {
    if status.missing_pr {
        return AgentTaskPrOwnershipState::MissingPr;
    }
    if status
        .merge_state
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case("MERGED"))
    {
        return AgentTaskPrOwnershipState::Merged;
    }
    if status
        .ci_state
        .as_deref()
        .is_some_and(|state| state == "terminal_failed" || state == "stale")
    {
        return if status.retry_count >= request.max_retries {
            AgentTaskPrOwnershipState::RetryLimitReached
        } else {
            AgentTaskPrOwnershipState::ChangesRequested
        };
    }
    if status
        .review_decision
        .as_deref()
        .is_some_and(|decision| decision == "CHANGES_REQUESTED")
    {
        return if status.retry_count >= request.max_retries {
            AgentTaskPrOwnershipState::RetryLimitReached
        } else {
            AgentTaskPrOwnershipState::ChangesRequested
        };
    }
    if status
        .ci_state
        .as_deref()
        .is_some_and(|state| state == "pending" || state == "no_checks" || state == "tracking")
    {
        return AgentTaskPrOwnershipState::WaitingForChecks;
    }
    if status
        .ci_state
        .as_deref()
        .is_some_and(|state| state == "terminal_green")
    {
        return if request.merge_required {
            AgentTaskPrOwnershipState::WaitingForMerge
        } else {
            AgentTaskPrOwnershipState::GreenReady
        };
    }
    AgentTaskPrOwnershipState::Tracking
}
