//! Execution-budget accounting for cook attempts.
//!
//! Extracted from `cook.rs`: the math that tallies how much of an
//! `AgentTaskExecutionBudget` a cook aggregate has consumed
//! (`execution_budget_usage`), computes what budget remains for a follow-up
//! attempt (`budget_remaining`), and reserves budget for a remediation attempt
//! (`reserve_remediation_budget`). Pure functions over aggregate/budget data —
//! no I/O — which is why they lift cleanly out of the cook orchestration file.

use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskExecutionBudget, AgentTaskState};
use homeboy_core::{Error, Result};

use super::cook_pre_execution::provider_rotation_attempts;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExecutionBudgetUsage {
    pub(crate) executions: u32,
    pub(crate) same_provider_retries: u32,
    pub(crate) provider_rotations: u32,
}

impl ExecutionBudgetUsage {
    pub(crate) fn add(&mut self, other: Self) {
        self.executions = self.executions.saturating_add(other.executions);
        self.same_provider_retries = self
            .same_provider_retries
            .saturating_add(other.same_provider_retries);
        self.provider_rotations = self
            .provider_rotations
            .saturating_add(other.provider_rotations);
    }
}

/// The provider-backed portion of a Cook's retry policy. Cook attempts are
/// orchestration slots; every remediation that reaches a provider also needs a
/// total execution slot and, for gate and review-form fixes, a same-provider
/// retry slot. Provider rotation is not a substitute for a form-only retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveCookBudget {
    pub requested_attempts: u32,
    pub provider_executions: u32,
    pub same_provider_remediations: u32,
    pub provider_rotations: u32,
}

pub fn effective_cook_budget(
    max_attempts: u32,
    budget: &AgentTaskExecutionBudget,
) -> EffectiveCookBudget {
    EffectiveCookBudget {
        requested_attempts: max_attempts.max(1),
        provider_executions: budget.max_provider_executions,
        same_provider_remediations: budget.max_same_provider_retries,
        provider_rotations: budget.max_provider_rotations,
    }
}

/// Reject a Cook whose advertised attempt allowance cannot support its
/// provider-backed remediation contract. This runs before a recipe is written,
/// preserving immutable-recipe compatibility while preventing expensive work
/// that could never reach its configured retry allowance.
pub fn validate_effective_cook_budget(
    max_attempts: u32,
    budget: &AgentTaskExecutionBudget,
) -> Result<EffectiveCookBudget> {
    let effective = effective_cook_budget(max_attempts, budget);
    let required_remediations = effective.requested_attempts.saturating_sub(1);
    let correction = format!(
        "Start a new Cook with `--max-attempts {} --max-provider-executions {} --max-same-provider-retries {}`.",
        effective.requested_attempts,
        effective.requested_attempts,
        required_remediations,
    );

    if effective.provider_executions < effective.requested_attempts {
        return Err(Error::validation_invalid_argument(
            "max-provider-executions",
            format!(
                "Cook requests {} attempts but --max-provider-executions {} can fund only {} provider-backed attempt(s). Effective budget: attempts={}, provider_executions={}, same_provider_remediations={}, provider_rotations={}. {}",
                effective.requested_attempts,
                effective.provider_executions,
                effective.provider_executions,
                effective.requested_attempts,
                effective.provider_executions,
                effective.same_provider_remediations,
                effective.provider_rotations,
                correction,
            ),
            None,
            Some(vec![correction]),
        ));
    }
    if effective.same_provider_remediations < required_remediations {
        return Err(Error::validation_invalid_argument(
            "max-same-provider-retries",
            format!(
                "Cook requests {} attempts but --max-same-provider-retries {} cannot fund {} same-provider remediation(s). Gate fixes and required review-form retries preserve the successful provider identity; --max-provider-rotations {} cannot replace them. Effective budget: attempts={}, provider_executions={}, same_provider_remediations={}, provider_rotations={}. {}",
                effective.requested_attempts,
                effective.same_provider_remediations,
                required_remediations,
                effective.provider_rotations,
                effective.requested_attempts,
                effective.provider_executions,
                effective.same_provider_remediations,
                effective.provider_rotations,
                correction,
            ),
            None,
            Some(vec![correction]),
        ));
    }
    Ok(effective)
}

pub(crate) fn execution_budget_usage(aggregate: &AgentTaskAggregate) -> ExecutionBudgetUsage {
    let executions = aggregate
        .events
        .iter()
        .filter(|event| event.state == AgentTaskState::Running)
        .count()
        .try_into()
        .unwrap_or(u32::MAX);
    let same_provider_retries = aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| &outcome.diagnostics)
        .filter(|diagnostic| diagnostic.class == "agent_task.retry_attempt")
        .count()
        .try_into()
        .unwrap_or(u32::MAX);
    let provider_rotations = aggregate
        .outcomes
        .iter()
        .filter_map(provider_rotation_attempts)
        .map(|attempts| attempts.len().saturating_sub(1) as u32)
        .fold(0, u32::saturating_add);
    ExecutionBudgetUsage {
        executions,
        same_provider_retries,
        provider_rotations,
    }
}

pub(crate) fn budget_remaining(
    budget: &AgentTaskExecutionBudget,
    usage: ExecutionBudgetUsage,
) -> Option<AgentTaskExecutionBudget> {
    let max_provider_executions = budget
        .max_provider_executions
        .saturating_sub(usage.executions);
    (max_provider_executions > 0).then(|| {
        AgentTaskExecutionBudget::new(
            max_provider_executions,
            budget
                .max_same_provider_retries
                .saturating_sub(usage.same_provider_retries),
            budget
                .max_provider_rotations
                .saturating_sub(usage.provider_rotations),
        )
    })
}

pub(crate) fn reserve_remediation_budget(
    budget: &AgentTaskExecutionBudget,
    same_provider: bool,
) -> std::result::Result<ExecutionBudgetUsage, &'static str> {
    if budget.max_provider_executions == 0 {
        return Err("max_provider_executions");
    }
    if same_provider {
        if budget.max_same_provider_retries == 0 {
            return Err("max_same_provider_retries");
        }
        return Ok(ExecutionBudgetUsage {
            same_provider_retries: 1,
            ..Default::default()
        });
    }
    if budget.max_provider_rotations == 0 {
        return Err("max_provider_rotations");
    }
    Ok(ExecutionBudgetUsage {
        provider_rotations: 1,
        ..Default::default()
    })
}
