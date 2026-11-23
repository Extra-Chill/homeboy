//! The caller-facing Cook summary every Cook read surface emits (#18817).
//!
//! `agent-task status`, `status --watch`, `diagnose` and the Cook launch output
//! all carry the same `homeboy/cook-summary/v1` object under the top-level
//! `cook_summary` key, as does the terminal notification payload, so
//! `jq '.cook_summary.status'` reads every surface the same way.

use homeboy::agents::agent_task_lifecycle::AgentTaskRunRecord;
use homeboy::agents::agent_task_service as agent_task_service_direct;
use homeboy_control_plane_contract::{CookSummary, COOK_SUMMARY_KEY};
use serde_json::Value;

/// The summary projected from a durable run record.
pub(crate) fn of_record(record: &AgentTaskRunRecord) -> CookSummary {
    agent_task_service_direct::cook_summary(record)
}

/// The summary of the run `run_id` names, when its record is readable.
pub(crate) fn of_run(run_id: &str) -> Option<CookSummary> {
    agent_task_service_direct::cook_summary_for_run(run_id)
}

/// The summary a surface reports when it has no durable record to project:
/// it reads the surface's own status, pull request URL and stop reason.
pub(crate) fn fallback(value: &Value) -> CookSummary {
    let text = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let status = text("/status")
        .or_else(|| text("/state"))
        .unwrap_or("unknown");
    let pr_url = text("/pr_url")
        .or_else(|| text("/finalization/pr_url"))
        .map(str::to_string);
    let reason = text("/stop_reason")
        .or_else(|| text("/reason"))
        .unwrap_or("no durable run record to project");
    let terminal = !matches!(
        status,
        "queued" | "running" | "in_flight" | "continuing" | "awaiting_ci"
    );
    CookSummary::unobserved(status, terminal, pr_url, reason)
}

/// Write `summary` (or the surface's fallback) under `cook_summary`.
pub(crate) fn attach(value: &mut Value, summary: Option<CookSummary>) {
    if !value.is_object() {
        return;
    }
    let summary = summary.unwrap_or_else(|| fallback(value));
    value[COOK_SUMMARY_KEY] =
        serde_json::to_value(summary).expect("a Cook summary always serializes");
}

/// Attach the fallback only when the surface carries no summary yet.
pub(crate) fn ensure(value: &mut Value) {
    if value.get(COOK_SUMMARY_KEY).is_none_or(Value::is_null) {
        attach(value, None);
    }
}
