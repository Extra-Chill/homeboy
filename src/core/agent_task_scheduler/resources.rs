use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use super::outcome::{render_template_string, render_template_value};
use super::*;

pub(super) fn workspace_is_busy(
    task: &ScheduledTask,
    running: &[RunningTask],
    quarantined: &[QuarantinedTask],
) -> bool {
    let Some(workspace) = task.workspace_key.as_deref() else {
        return false;
    };
    running
        .iter()
        .filter_map(|task| task.workspace_key.as_deref())
        .chain(
            quarantined
                .iter()
                .filter_map(|task| task.workspace_key.as_deref()),
        )
        .any(|running_workspace| workspace_keys_overlap(running_workspace, workspace))
}

/// Returns the first deterministic exclusive-key conflict. Resource keys are
/// caller declarations, never inferred from provider configuration or command
/// text, so the scheduler remains tool-agnostic.
pub(super) fn resource_is_busy(
    task: &ScheduledTask,
    running: &[RunningTask],
) -> Option<(String, String)> {
    let keys = AgentTaskScheduleSupport::exclusive_resource_keys(&task.request);
    for key in keys {
        if let Some(holder) = running.iter().find(|running| {
            running
                .exclusive_resource_keys
                .iter()
                .any(|held| held == &key)
        }) {
            return Some((key, holder.task_id.clone()));
        }
    }
    None
}

impl AgentTaskScheduleSupport {
    pub(super) fn workspace_is_quarantined(
        task: &ScheduledTask,
        quarantined: &[QuarantinedTask],
    ) -> bool {
        let Some(workspace) = task.workspace_key.as_deref() else {
            return false;
        };
        quarantined
            .iter()
            .filter_map(|task| task.workspace_key.as_deref())
            .any(|quarantined_workspace| workspace_keys_overlap(quarantined_workspace, workspace))
    }

    pub(crate) fn workspace_key(request: &AgentTaskRequest) -> Option<String> {
        let root = request.workspace.root.as_deref()?;
        let git_identity = Command::new("git")
            .args([
                "-C",
                root,
                "rev-parse",
                "--show-toplevel",
                "--path-format=absolute",
                "--git-common-dir",
            ])
            .output();
        if let Ok(output) = git_identity {
            if output.status.success() {
                let identity = String::from_utf8_lossy(&output.stdout);
                let mut lines = identity.lines();
                if let (Some(top_level), Some(common_dir)) = (lines.next(), lines.next()) {
                    return Some(format!("git:{top_level}:{common_dir}"));
                }
            }
        }
        Some(format!(
            "path:{}",
            std::fs::canonicalize(root)
                .unwrap_or_else(|_| Path::new(root).to_path_buf())
                .display()
        ))
    }
}

fn workspace_keys_overlap(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let (Some(left), Some(right)) = (left.strip_prefix("path:"), right.strip_prefix("path:"))
    else {
        return false;
    };
    let left = Path::new(left);
    let right = Path::new(right);
    left.starts_with(right) || right.starts_with(left)
}

pub(super) fn select_artifact_payload(
    artifact: &AgentTaskArtifact,
    payload_path: &str,
) -> Option<Value> {
    artifact
        .metadata
        .get("payload")
        .and_then(|payload| payload.pointer(payload_path))
        .cloned()
        .or_else(|| {
            serde_json::to_value(artifact)
                .ok()
                .and_then(|artifact_value| artifact_value.pointer(payload_path).cloned())
        })
}

pub(super) fn executor_key(request: &AgentTaskRequest) -> String {
    match &request.executor.selector {
        Some(selector) => format!("{}:{selector}", request.executor.backend),
        None => request.executor.backend.clone(),
    }
}

pub(super) fn model_key(request: &AgentTaskRequest) -> Option<String> {
    request
        .executor
        .model
        .as_ref()
        .map(|model| match &request.executor.selector {
            Some(selector) => format!("{}:{selector}:{model}", request.executor.backend),
            None => format!("{}:{model}", request.executor.backend),
        })
}

pub(super) fn task_resource_units(
    request: &AgentTaskRequest,
    budget: &AgentTaskResourceBudget,
) -> u32 {
    model_key(request)
        .and_then(|key| budget.per_model_task_units.get(&key).copied())
        .or_else(|| {
            budget
                .per_executor_task_units
                .get(&executor_key(request))
                .copied()
        })
        .unwrap_or_else(|| budget.default_task_units.max(1))
        .max(1)
}

pub(super) fn active_resource_units(running: &[RunningTask]) -> u32 {
    running
        .iter()
        .map(|task| task.resource_units)
        .fold(0, u32::saturating_add)
}

pub(super) fn resource_capacity_available(
    request: &AgentTaskRequest,
    running: &[RunningTask],
    budget: &AgentTaskResourceBudget,
) -> bool {
    let Some(max_active_units) = budget.max_active_units else {
        return true;
    };
    active_resource_units(running).saturating_add(task_resource_units(request, budget))
        <= max_active_units
}

pub(super) fn adaptive_concurrency_decision(
    policy: Option<&AgentTaskAdaptiveConcurrencyPolicy>,
    configured_max_concurrency: usize,
    queued: usize,
    running: usize,
    resource_budget: &AgentTaskResourceBudget,
    active_units: u32,
    previous_effective_concurrency: Option<usize>,
) -> Option<AgentTaskAdaptiveConcurrencyDecision> {
    let policy = policy?;
    let configured_max_concurrency = configured_max_concurrency.max(1);
    let min_concurrency = policy.min_concurrency.max(1);
    let policy_max_concurrency = policy
        .max_concurrency
        .unwrap_or(configured_max_concurrency)
        .max(min_concurrency);
    let mut effective_concurrency = policy_max_concurrency;
    let mut reason =
        format!("adaptive concurrency held at configured ceiling {policy_max_concurrency}");

    if let Some(runner_capacity) = policy.runner_capacity {
        let available_runner_slots = runner_capacity.saturating_sub(policy.active_leases);
        if available_runner_slots == 0 {
            effective_concurrency = 0;
            reason = format!(
                "paused because active_leases={} consume runner_capacity={runner_capacity}",
                policy.active_leases
            );
        } else if available_runner_slots < effective_concurrency {
            effective_concurrency = available_runner_slots;
            reason = format!(
                "scaled down to available runner slots {available_runner_slots} from runner_capacity={runner_capacity} active_leases={}",
                policy.active_leases
            );
        } else if available_runner_slots > configured_max_concurrency
            && policy_max_concurrency > configured_max_concurrency
        {
            reason = format!(
                "scaled up because runner slots are available: runner_capacity={runner_capacity} active_leases={}",
                policy.active_leases
            );
        }
    }

    if let Some(max_active_units) = resource_budget.max_active_units {
        let default_task_units = resource_budget.default_task_units.max(1);
        let available_units = max_active_units.saturating_sub(active_units);
        let resource_slots = (available_units / default_task_units) as usize;
        if resource_slots == 0 {
            effective_concurrency = 0;
            reason = format!(
                "paused because active_units={active_units} consume max_active_units={max_active_units}"
            );
        } else if resource_slots < effective_concurrency {
            effective_concurrency = resource_slots;
            reason = format!(
                "scaled down to resource slots {resource_slots} from max_active_units={max_active_units} active_units={active_units} default_task_units={default_task_units}"
            );
        }
    }

    if policy
        .pause_on_pressure
        .zip(policy.resource_pressure)
        .map(|(pause_on, pressure)| pressure >= pause_on)
        .unwrap_or(false)
    {
        effective_concurrency = 0;
        reason = format!(
            "paused because resource_pressure={:?} reached pause_on_pressure={:?}",
            policy.resource_pressure.expect("pressure checked"),
            policy.pause_on_pressure.expect("pause threshold checked")
        );
    }

    if policy
        .pause_after_recent_failures
        .map(|threshold| threshold > 0 && policy.recent_failures >= threshold)
        .unwrap_or(false)
    {
        effective_concurrency = 0;
        reason = format!(
            "paused because recent_failures={} reached pause_after_recent_failures={}",
            policy.recent_failures,
            policy.pause_after_recent_failures.unwrap_or_default()
        );
    }

    if policy
        .pause_after_recent_timeouts
        .map(|threshold| threshold > 0 && policy.recent_timeouts >= threshold)
        .unwrap_or(false)
    {
        effective_concurrency = 0;
        reason = format!(
            "paused because recent_timeouts={} reached pause_after_recent_timeouts={}",
            policy.recent_timeouts,
            policy.pause_after_recent_timeouts.unwrap_or_default()
        );
    }

    if effective_concurrency > 0 {
        effective_concurrency = effective_concurrency
            .max(min_concurrency)
            .min(policy_max_concurrency);
    }
    if queued == 0 && running == 0 && effective_concurrency > configured_max_concurrency {
        reason = format!(
            "held because no queued or running tasks need fan-out above configured max {configured_max_concurrency}"
        );
        effective_concurrency = configured_max_concurrency;
    }

    let action = match (previous_effective_concurrency, effective_concurrency) {
        (_, 0) => AgentTaskAdaptiveConcurrencyAction::Paused,
        (Some(previous), current) if current > previous => {
            AgentTaskAdaptiveConcurrencyAction::Increased
        }
        (Some(previous), current) if current < previous => {
            AgentTaskAdaptiveConcurrencyAction::Decreased
        }
        (None, current) if current > configured_max_concurrency => {
            AgentTaskAdaptiveConcurrencyAction::Increased
        }
        (None, current) if current < configured_max_concurrency => {
            AgentTaskAdaptiveConcurrencyAction::Decreased
        }
        _ => AgentTaskAdaptiveConcurrencyAction::Held,
    };

    Some(AgentTaskAdaptiveConcurrencyDecision {
        action,
        effective_concurrency,
        previous_effective_concurrency,
        reason,
        inputs: AgentTaskAdaptiveConcurrencyInputs {
            queued,
            running,
            configured_max_concurrency,
            runner_capacity: policy.runner_capacity,
            active_leases: policy.active_leases,
            queue_depth: policy.queue_depth,
            resource_pressure: policy.resource_pressure,
            max_active_units: resource_budget.max_active_units,
            active_units,
            default_task_units: resource_budget.default_task_units.max(1),
            recent_failures: policy.recent_failures,
            recent_timeouts: policy.recent_timeouts,
        },
    })
}

pub(super) fn render_value_templates(value: &mut Value, bindings: &HashMap<String, Value>) {
    match value {
        Value::String(raw) => {
            if let Some(rendered) = render_template_value(raw, bindings) {
                *value = rendered;
            } else {
                *raw = render_template_string(raw, bindings);
            }
        }
        Value::Array(items) => {
            for item in items {
                render_value_templates(item, bindings);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                render_value_templates(value, bindings);
            }
        }
        _ => {}
    }
}
