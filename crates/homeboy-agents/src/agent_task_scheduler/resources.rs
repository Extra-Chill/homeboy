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

/// How many whole tasks the resource budget still has room for, or `None` when
/// the budget declares no ceiling.
///
/// This is the one place the "available units divided by per-task units"
/// arithmetic lives. Both the scheduler's adaptive concurrency loop and the
/// batch-cook fanout ceiling read it, so a host that tightens
/// `max_active_units` tightens both by the same rule instead of by two
/// hand-maintained copies that can disagree.
pub(super) fn resource_budget_slots(
    budget: &AgentTaskResourceBudget,
    active_units: u32,
) -> Option<usize> {
    let max_active_units = budget.max_active_units?;
    let default_task_units = budget.default_task_units.max(1);
    let available_units = max_active_units.saturating_sub(active_units);
    Some((available_units / default_task_units) as usize)
}

/// Why the batch fanout chose the concurrency it chose.
///
/// Reported alongside the limit so an operator reading a batch result can tell
/// a deliberate `--max-concurrency 1` from a host config ceiling from a
/// resource budget that quietly scaled the batch down, without re-deriving the
/// decision from inputs that are no longer on hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchConcurrencySource {
    /// An explicit `--max-concurrency` flag.
    Flag,
    /// The host's `/agent_task/max_batch_concurrency` setting.
    Config,
    /// The plan's resource budget had fewer whole task slots than the ceiling.
    ResourceBudget,
    /// Fewer children than any ceiling: the batch is its own limit.
    ChildCount,
}

impl BatchConcurrencySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Config => "config",
            Self::ResourceBudget => "resource_budget",
            Self::ChildCount => "child_count",
        }
    }
}

/// The inputs a batch concurrency ceiling is resolved from.
#[derive(Debug, Clone)]
pub struct BatchConcurrencyInputs<'a> {
    /// Explicit operator override, if one was given.
    pub requested: Option<usize>,
    /// Host config ceiling, if one is set.
    pub configured: Option<usize>,
    /// Fallback ceiling when neither of the above is set.
    pub default_limit: usize,
    /// The batch's resource budget, consulted for whole task slots.
    pub resource_budget: &'a AgentTaskResourceBudget,
    /// Units already committed. A batch coordinator starts at zero.
    pub active_units: u32,
    /// How many children the batch actually has.
    pub child_count: usize,
}

/// The resolved ceiling and the reason for it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BatchConcurrencyDecision {
    pub limit: usize,
    pub source: BatchConcurrencySource,
    pub reason: String,
}

/// Resolve how many child cooks a batch may run at once.
///
/// An explicit flag is operator authority and is taken as the ceiling. With no
/// flag, the host config ceiling (or the caller's default) applies. Either way
/// the resource budget and the child count can only lower the result, never
/// raise it, so declaring a budget can never be made less protective by a
/// larger ceiling somewhere else.
///
/// The floor is 1: a batch with work to do always makes progress.
pub fn resolve_batch_concurrency(inputs: BatchConcurrencyInputs<'_>) -> BatchConcurrencyDecision {
    let (ceiling, source) = match inputs.requested {
        Some(requested) => (requested.max(1), BatchConcurrencySource::Flag),
        None => (
            inputs.configured.unwrap_or(inputs.default_limit).max(1),
            BatchConcurrencySource::Config,
        ),
    };
    let mut limit = ceiling;
    let mut source = source;
    let mut reason = match source {
        BatchConcurrencySource::Flag => {
            format!("--max-concurrency {ceiling} requested by the caller")
        }
        _ => match inputs.configured {
            Some(configured) => {
                format!("host config /agent_task/max_batch_concurrency {configured}")
            }
            None => format!(
                "built-in default ceiling {} with no --max-concurrency flag or host config",
                inputs.default_limit
            ),
        },
    };

    // A resource budget is a statement about what the host can physically
    // sustain, so it lowers even an explicit flag.
    if let Some(slots) = resource_budget_slots(inputs.resource_budget, inputs.active_units) {
        let slots = slots.max(1);
        if slots < limit {
            limit = slots;
            source = BatchConcurrencySource::ResourceBudget;
            reason = format!(
                "resource budget allows {slots} concurrent tasks (max_active_units={:?} active_units={} default_task_units={}), below the {ceiling} ceiling",
                inputs.resource_budget.max_active_units,
                inputs.active_units,
                inputs.resource_budget.default_task_units.max(1),
            );
        }
    }

    if inputs.child_count > 0 && inputs.child_count < limit {
        reason = format!(
            "batch has {} children, fewer than the effective ceiling {limit}",
            inputs.child_count,
        );
        limit = inputs.child_count;
        source = BatchConcurrencySource::ChildCount;
    }

    BatchConcurrencyDecision {
        limit: limit.max(1),
        source,
        reason,
    }
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

    if let Some(resource_slots) = resource_budget_slots(resource_budget, active_units) {
        let max_active_units = resource_budget
            .max_active_units
            .expect("resource slots are only produced by a budget with a ceiling");
        let default_task_units = resource_budget.default_task_units.max(1);
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

#[cfg(test)]
mod batch_concurrency_tests {
    use super::*;

    fn inputs<'a>(
        budget: &'a AgentTaskResourceBudget,
        child_count: usize,
    ) -> BatchConcurrencyInputs<'a> {
        BatchConcurrencyInputs {
            requested: None,
            configured: None,
            default_limit: 4,
            resource_budget: budget,
            active_units: 0,
            child_count,
        }
    }

    /// The regression this whole path exists for: a large batch on a host with
    /// no flag and no config must not fan out to the core count.
    #[test]
    fn an_unconfigured_batch_is_capped_by_the_built_in_default() {
        let budget = AgentTaskResourceBudget::default();
        let decision = resolve_batch_concurrency(inputs(&budget, 32));
        assert_eq!(decision.limit, 4);
        assert_eq!(decision.source, BatchConcurrencySource::Config);
    }

    #[test]
    fn an_explicit_flag_is_the_ceiling_and_is_reported_as_the_source() {
        let budget = AgentTaskResourceBudget::default();
        let decision = resolve_batch_concurrency(BatchConcurrencyInputs {
            requested: Some(2),
            configured: Some(8),
            ..inputs(&budget, 32)
        });
        assert_eq!(decision.limit, 2);
        assert_eq!(decision.source, BatchConcurrencySource::Flag);
    }

    #[test]
    fn host_config_applies_when_no_flag_is_given() {
        let budget = AgentTaskResourceBudget::default();
        let decision = resolve_batch_concurrency(BatchConcurrencyInputs {
            configured: Some(1),
            ..inputs(&budget, 32)
        });
        assert_eq!(decision.limit, 1);
        assert_eq!(decision.source, BatchConcurrencySource::Config);
    }

    /// A budget is a statement about the host, so it lowers even an explicit
    /// flag. Getting this backwards is how a declared budget stops protecting.
    #[test]
    fn a_resource_budget_lowers_even_an_explicit_flag() {
        let budget = AgentTaskResourceBudget {
            max_active_units: Some(2),
            default_task_units: 1,
            ..AgentTaskResourceBudget::default()
        };
        let decision = resolve_batch_concurrency(BatchConcurrencyInputs {
            requested: Some(8),
            ..inputs(&budget, 32)
        });
        assert_eq!(decision.limit, 2);
        assert_eq!(decision.source, BatchConcurrencySource::ResourceBudget);
        assert!(
            decision.reason.contains("resource budget"),
            "{}",
            decision.reason
        );
    }

    #[test]
    fn a_budget_larger_than_the_ceiling_does_not_raise_it() {
        let budget = AgentTaskResourceBudget {
            max_active_units: Some(64),
            default_task_units: 1,
            ..AgentTaskResourceBudget::default()
        };
        let decision = resolve_batch_concurrency(BatchConcurrencyInputs {
            requested: Some(2),
            ..inputs(&budget, 32)
        });
        assert_eq!(decision.limit, 2);
        assert_eq!(decision.source, BatchConcurrencySource::Flag);
    }

    #[test]
    fn a_small_batch_is_limited_by_its_own_child_count() {
        let budget = AgentTaskResourceBudget::default();
        let decision = resolve_batch_concurrency(BatchConcurrencyInputs {
            requested: Some(8),
            ..inputs(&budget, 2)
        });
        assert_eq!(decision.limit, 2);
        assert_eq!(decision.source, BatchConcurrencySource::ChildCount);
    }

    /// A batch with work to do always makes progress, however tight the budget.
    #[test]
    fn the_floor_is_one_worker() {
        let budget = AgentTaskResourceBudget {
            max_active_units: Some(0),
            default_task_units: 8,
            ..AgentTaskResourceBudget::default()
        };
        let decision = resolve_batch_concurrency(BatchConcurrencyInputs {
            requested: Some(0),
            ..inputs(&budget, 32)
        });
        assert_eq!(decision.limit, 1);
    }

    #[test]
    fn the_source_serializes_as_the_documented_vocabulary() {
        for (source, expected) in [
            (BatchConcurrencySource::Flag, "flag"),
            (BatchConcurrencySource::Config, "config"),
            (BatchConcurrencySource::ResourceBudget, "resource_budget"),
            (BatchConcurrencySource::ChildCount, "child_count"),
        ] {
            assert_eq!(source.as_str(), expected);
            assert_eq!(
                serde_json::to_value(source).expect("source serializes"),
                serde_json::Value::String(expected.to_string()),
            );
        }
    }

    /// The scheduler's adaptive loop and the batch ceiling must divide units
    /// the same way, which is the point of sharing this helper.
    #[test]
    fn budget_slots_are_absent_without_a_ceiling_and_floor_at_zero_when_full() {
        let unbounded = AgentTaskResourceBudget::default();
        assert_eq!(resource_budget_slots(&unbounded, 0), None);

        let bounded = AgentTaskResourceBudget {
            max_active_units: Some(10),
            default_task_units: 4,
            ..AgentTaskResourceBudget::default()
        };
        assert_eq!(resource_budget_slots(&bounded, 0), Some(2));
        assert_eq!(resource_budget_slots(&bounded, 8), Some(0));
        assert_eq!(resource_budget_slots(&bounded, 999), Some(0));
    }
}
