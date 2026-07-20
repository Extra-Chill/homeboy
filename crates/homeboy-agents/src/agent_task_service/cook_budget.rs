//! Execution-budget accounting for cook attempts.
//!
//! Extracted from `cook.rs`: the math that tallies how much of an
//! `AgentTaskExecutionBudget` a cook aggregate has consumed
//! (`execution_budget_usage`), computes what budget remains for a follow-up
//! attempt (`budget_remaining`), and reserves budget for a remediation attempt
//! (`reserve_remediation_budget`). Pure functions over aggregate/budget data —
//! no I/O — which is why they lift cleanly out of the cook orchestration file.

use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskExecutionBudget, AgentTaskState};

use super::cook_pre_execution::provider_rotation_attempts;

#[derive(Debug, Default, Clone, Copy)]
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
