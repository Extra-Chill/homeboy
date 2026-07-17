//! Compact root-cause failure summaries for `agent-task controller run-from-spec`.
//!
//! Diagnosing a failed controller run used to require manually traversing huge
//! nested JSON envelopes (resume results, action reports, provider/runtime
//! diagnostics, Sandbox evidence) to find the actual blocker, the owning
//! surface, and the durable artifacts/logs that prove what happened (#6220).
//!
//! This module normalizes those nested provider/runtime failures into a small
//! [`ControllerRunFailureSummary`] object that names the phase, the owner
//! surface that failed, the root blocker message, the first actionable
//! diagnostic, durable evidence refs (runner job logs, persisted run evidence,
//! provider artifact bundles), and the next recommended Homeboy command. The
//! orchestrator prints it on every terminal failure so operators never have to
//! hand-extract the root cause again.

use serde::Serialize;
use serde_json::Value;

/// Owner surface that a controller run blocker is attributed to.
///
/// Ordered roughly outside-in so the most specific reachable surface wins when
/// several layers report context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerSurface {
    Homeboy,
    LabRunner,
    ExtensionProvider,
    SelectedRuntime,
    WordPressRuntime,
    ProviderPlugin,
    AgentOutput,
}

impl OwnerSurface {
    fn as_str(self) -> &'static str {
        match self {
            OwnerSurface::Homeboy => "homeboy",
            OwnerSurface::LabRunner => "lab_runner",
            OwnerSurface::ExtensionProvider => "extension_provider",
            OwnerSurface::SelectedRuntime => "selected_runtime",
            OwnerSurface::WordPressRuntime => "wordpress_runtime",
            OwnerSurface::ProviderPlugin => "provider_plugin",
            OwnerSurface::AgentOutput => "agent_output",
        }
    }
}

/// A durable, Homeboy-owned reference to evidence behind a controller failure.
///
/// `kind` classifies the ref (`runner_job_log`, `run_evidence`,
/// `artifact_bundle`, ...) so operators can pick the right follow-up without
/// guessing at URI shapes.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ControllerRunEvidenceRef {
    pub kind: String,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Compact, operator-facing root-cause summary for a failed controller run.
///
/// Built purely from the `run-from-spec` result envelope (resume results,
/// per-action failure summaries, controller status), so it stays in lockstep
/// with whatever the run actually emitted without a second source of truth.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ControllerRunFailureSummary {
    pub schema: &'static str,
    /// Why the run stopped (`action_failed`, `terminal_state`, ...).
    pub stopped_reason: String,
    /// Controller phase that was executing when the run failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Owner surface the blocker is attributed to.
    pub owner_surface: String,
    /// Root blocker message — the single most actionable line.
    pub root_blocker: String,
    /// First actionable diagnostic (may equal `root_blocker` when only one is known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_diagnostic: Option<String>,
    /// Failed action id, when the failure is tied to a specific action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// Provider implicated in the failure, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Provider/runtime failure phase, when known (e.g. `secret_handoff`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_phase: Option<String>,
    /// Durable evidence refs (runner job logs, run evidence, artifact bundles).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<ControllerRunEvidenceRef>,
    /// Provider-owned runtime context for the failed run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_context: Option<Value>,
    /// Provider-owned replay command for reproducing the failed runtime action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_command: Option<String>,
    /// Next recommended Homeboy command to investigate or resume.
    pub next_command: String,
}

pub const CONTROLLER_RUN_FAILURE_SUMMARY_SCHEMA: &str =
    "homeboy/agent-task-loop-controller-run-failure-summary/v1";

/// Build a compact failure summary from a `run-from-spec` result envelope.
///
/// `loop_id`, `stopped_reason`, `results`, and `status` come straight from the
/// envelope the command is about to return. Returns `None` when nothing in the
/// envelope indicates a failure (callers only attach the summary on a non-zero
/// exit).
pub fn build_run_failure_summary(
    loop_id: &str,
    stopped_reason: &str,
    results: &[Value],
    status: &Value,
) -> ControllerRunFailureSummary {
    let failed_action = results.iter().rev().find(|result| {
        result.get("failure_summary").is_some()
            || result.get("status").and_then(Value::as_str) == Some("failed")
            || result
                .get("status")
                .and_then(Value::as_str)
                .map(|status| status.starts_with("blocked_"))
                .unwrap_or(false)
    });

    let failure_summary = failed_action.and_then(|result| result.get("failure_summary"));

    let action_id = failure_summary
        .and_then(|summary| string_field(summary, "action_id"))
        .or_else(|| failed_action.and_then(|result| string_field(result, "action_id")));
    let provider = failure_summary.and_then(|summary| string_field(summary, "provider"));
    let failure_phase = failure_summary.and_then(|summary| string_field(summary, "failure_phase"));

    let phase = failure_summary
        .and_then(|summary| string_field(summary, "phase"))
        .or_else(|| nested_string(status, &["controller", "phase"]))
        .or_else(|| nested_string(status, &["phase"]));

    let action_status = failed_action.and_then(|result| string_field(result, "status"));

    let explicit_diagnostic =
        failure_summary.and_then(|summary| string_field(summary, "diagnostic"));
    let deep_diagnostic = best_diagnostic_message(failed_action);
    let root_blocker = best_root_blocker(
        explicit_diagnostic.as_deref(),
        deep_diagnostic.as_deref(),
        stopped_reason_blocker(stopped_reason, action_status.as_deref()).as_deref(),
    )
    .unwrap_or_else(|| "controller run failed".to_string());

    let first_diagnostic = deep_diagnostic
        .or(explicit_diagnostic)
        .filter(|diagnostic| *diagnostic != root_blocker);

    let owner_surface = classify_owner_surface(
        provider.as_deref(),
        failure_phase.as_deref(),
        action_status.as_deref(),
        &root_blocker,
    );

    let evidence_refs = collect_evidence_refs(loop_id, failed_action, status);
    let runtime_context = failure_summary
        .and_then(|summary| summary.get("runtime_context"))
        .cloned()
        .or_else(|| failed_action.and_then(provider_owned_runtime_context));
    let replay_command = failure_summary
        .and_then(|summary| string_field(summary, "replay_command"))
        .or_else(|| failed_action.and_then(provider_owned_replay_command));

    let next_command = next_command(loop_id, owner_surface, action_status.as_deref());

    ControllerRunFailureSummary {
        schema: CONTROLLER_RUN_FAILURE_SUMMARY_SCHEMA,
        stopped_reason: stopped_reason.to_string(),
        phase,
        owner_surface: owner_surface.as_str().to_string(),
        root_blocker,
        first_diagnostic,
        action_id,
        provider,
        failure_phase,
        evidence_refs,
        runtime_context,
        replay_command,
        next_command,
    }
}

/// Attribute the blocker to the most specific reachable owner surface.
///
/// Uses explicit provider/phase hints first, then falls back to scanning the
/// root blocker text for ecosystem-agnostic surface signals.
fn classify_owner_surface(
    provider: Option<&str>,
    failure_phase: Option<&str>,
    action_status: Option<&str>,
    root_blocker: &str,
) -> OwnerSurface {
    if let Some(status) = action_status {
        if status.starts_with("blocked_") || status.contains("runner") {
            // Runner-policy blocks are a Homeboy-side scheduling decision unless
            // the message clearly points at the remote runner.
            if root_blocker.to_ascii_lowercase().contains("runner") {
                return OwnerSurface::LabRunner;
            }
            return OwnerSurface::Homeboy;
        }
    }

    let haystack = root_blocker.to_ascii_lowercase();
    let phase = failure_phase.unwrap_or("").to_ascii_lowercase();

    if phase.contains("runner")
        || haystack.contains("runner job")
        || haystack.contains("lab runner")
    {
        return OwnerSurface::LabRunner;
    }
    if phase.contains("secret")
        || haystack.contains("secret handoff")
        || haystack.contains("secret")
    {
        return OwnerSurface::ExtensionProvider;
    }
    let provider_hint = provider.unwrap_or("").to_ascii_lowercase();
    if haystack.contains("sandbox")
        || provider_hint.contains("sandbox")
        || phase.contains("sandbox")
        || phase.contains("plugin_activation")
        || haystack.contains("plugin activation")
    {
        return OwnerSurface::SelectedRuntime;
    }
    if haystack.contains("php fatal")
        || haystack.contains("fatal error")
        || haystack.contains("wp-cli")
        || haystack.contains("wordpress")
    {
        return OwnerSurface::WordPressRuntime;
    }
    if haystack.contains("plugin") {
        return OwnerSurface::ProviderPlugin;
    }
    if provider.is_some() {
        return OwnerSurface::ExtensionProvider;
    }
    if haystack.contains("missing required artifact")
        || haystack.contains("artifact")
        || haystack.contains("agent output")
    {
        return OwnerSurface::AgentOutput;
    }

    OwnerSurface::Homeboy
}

/// Collect durable, Homeboy-owned evidence refs behind the failure.
///
/// Pulls runner job ids, run ids, task ids, and any declared artifact/evidence
/// refs out of the failed action result and the controller status, then renders
/// them as stable `homeboy <command>` / URI references operators can follow.
fn collect_evidence_refs(
    loop_id: &str,
    failed_action: Option<&Value>,
    status: &Value,
) -> Vec<ControllerRunEvidenceRef> {
    let mut refs: Vec<ControllerRunEvidenceRef> = Vec::new();

    // Persisted run evidence: the controller record itself is the durable index.
    push_ref(
        &mut refs,
        ControllerRunEvidenceRef {
            kind: "run_evidence".to_string(),
            uri: format!("homeboy agent-task controller status {loop_id}"),
            label: Some("persisted controller run evidence".to_string()),
        },
    );

    if let Some(action) = failed_action {
        // Runner job logs.
        let runner_id = find_all_strings(action, &["runner_id"])
            .into_iter()
            .next()
            .or_else(|| find_all_strings(status, &["runner_id"]).into_iter().next());
        for job_id in find_all_strings(action, &["runner_job_id", "job_id"]) {
            push_ref(
                &mut refs,
                ControllerRunEvidenceRef {
                    kind: "runner_job_log".to_string(),
                    uri: runner_id
                        .as_ref()
                        .map(|runner_id| format!("homeboy runner job logs {runner_id} {job_id}"))
                        .unwrap_or_else(|| format!("runner-job://{job_id}")),
                    label: Some(format!("runner job {job_id} log")),
                },
            );
        }

        // Per-run evidence keyed by run id.
        for run_id in find_all_strings(action, &["run_id"]) {
            push_ref(
                &mut refs,
                ControllerRunEvidenceRef {
                    kind: "run_evidence".to_string(),
                    uri: format!("homeboy agent-task status {run_id} --full"),
                    label: Some(format!("agent-task run {run_id} evidence")),
                },
            );
        }

        // Provider artifact bundles declared on the failed action.
        for evidence in collect_declared_refs(action) {
            push_ref(&mut refs, evidence);
        }
        for evidence in collect_source_like_refs(action) {
            push_ref(&mut refs, evidence);
        }
    }

    // Evidence index recorded on the controller status (artifact bundles).
    for evidence in collect_declared_refs(status) {
        push_ref(&mut refs, evidence);
    }
    for evidence in collect_source_like_refs(status) {
        push_ref(&mut refs, evidence);
    }

    refs
}

/// Extract declared artifact/evidence refs (`uri`/`url`/`path`) from any nested
/// `artifacts`, `artifact_refs`, or `evidence_refs` arrays.
fn collect_declared_refs(value: &Value) -> Vec<ControllerRunEvidenceRef> {
    let mut out = Vec::new();
    collect_declared_refs_into(value, &mut out);
    out
}

fn collect_declared_refs_into(value: &Value, out: &mut Vec<ControllerRunEvidenceRef>) {
    match value {
        Value::Object(map) => {
            for key in [
                "artifacts",
                "artifact_refs",
                "typed_artifacts",
                "evidence_refs",
            ] {
                if let Some(Value::Array(items)) = map.get(key) {
                    for item in items {
                        if let Some(reference) = declared_ref_from_item(key, item) {
                            out.push(reference);
                        }
                    }
                }
            }
            for nested in map.values() {
                collect_declared_refs_into(nested, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_declared_refs_into(item, out);
            }
        }
        _ => {}
    }
}

fn declared_ref_from_item(container_key: &str, item: &Value) -> Option<ControllerRunEvidenceRef> {
    let uri = string_field(item, "uri")
        .or_else(|| string_field(item, "url"))
        .or_else(|| string_field(item, "path"))?;
    // The evidence-ref `kind` is its own taxonomy (`artifact_bundle`,
    // `evidence_bundle`, ...) keyed by the declaring container, not by the
    // artifact's intrinsic `kind` (e.g. `log_bundle`). Classify by container so
    // declared provider artifacts surface as `artifact_bundle`.
    let kind = if container_key == "evidence_refs" {
        "evidence_bundle".to_string()
    } else {
        "artifact_bundle".to_string()
    };
    let label = string_field(item, "label")
        .or_else(|| string_field(item, "name"))
        .or_else(|| string_field(item, "role"));
    Some(ControllerRunEvidenceRef { kind, uri, label })
}

fn collect_source_like_refs(value: &Value) -> Vec<ControllerRunEvidenceRef> {
    let mut out = Vec::new();
    collect_source_like_refs_into(value, &mut out);
    out
}

fn collect_source_like_refs_into(value: &Value, out: &mut Vec<ControllerRunEvidenceRef>) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                if let Some(kind) = source_like_ref_kind(key) {
                    collect_source_like_ref_value(kind, key, nested, out);
                }
                collect_source_like_refs_into(nested, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_source_like_refs_into(item, out);
            }
        }
        _ => {}
    }
}

fn source_like_ref_kind(key: &str) -> Option<&'static str> {
    let lower = key.to_ascii_lowercase();
    if lower.contains("provider") && lower.contains("path") {
        Some("provider_source")
    } else if lower.contains("prepared") && lower.contains("source") {
        Some("prepared_source")
    } else if (lower.contains("runtime") || lower.contains("overlay"))
        && (lower.contains("source") || lower.contains("ref"))
    {
        Some("runtime_overlay_source")
    } else {
        None
    }
}

fn collect_source_like_ref_value(
    kind: &str,
    key: &str,
    value: &Value,
    out: &mut Vec<ControllerRunEvidenceRef>,
) {
    match value {
        Value::String(text) if !text.trim().is_empty() => out.push(ControllerRunEvidenceRef {
            kind: kind.to_string(),
            uri: text.trim().to_string(),
            label: Some(key.to_string()),
        }),
        Value::Object(map) => {
            if let Some(uri) = string_field(value, "uri")
                .or_else(|| string_field(value, "url"))
                .or_else(|| string_field(value, "path"))
                .or_else(|| string_field(value, "ref"))
            {
                out.push(ControllerRunEvidenceRef {
                    kind: kind.to_string(),
                    uri,
                    label: string_field(value, "label").or_else(|| Some(key.to_string())),
                });
            }
            for nested in map.values() {
                collect_source_like_ref_value(kind, key, nested, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_source_like_ref_value(kind, key, item, out);
            }
        }
        _ => {}
    }
}

/// Next recommended Homeboy command, tailored to the owning surface.
fn next_command(loop_id: &str, owner: OwnerSurface, action_status: Option<&str>) -> String {
    if action_status
        .map(|status| status.starts_with("blocked_"))
        .unwrap_or(false)
    {
        return format!(
            "homeboy agent-task controller status {loop_id}  # resolve the runner block, then re-run with --resume"
        );
    }
    match owner {
        OwnerSurface::LabRunner => {
            format!(
                "homeboy runner status  # then `homeboy agent-task controller status {loop_id}`"
            )
        }
        OwnerSurface::SelectedRuntime
        | OwnerSurface::WordPressRuntime
        | OwnerSurface::ProviderPlugin => {
            format!("homeboy agent-task controller status {loop_id}  # inspect provider/runtime evidence refs above")
        }
        _ => format!("homeboy agent-task controller status {loop_id}"),
    }
}

fn stopped_reason_blocker(stopped_reason: &str, action_status: Option<&str>) -> Option<String> {
    match stopped_reason {
        "action_failed" => {
            Some("a controller action failed without an embedded diagnostic".to_string())
        }
        "terminal_state" => action_status.map(|status| {
            format!("controller reached terminal state with failed action status '{status}'")
        }),
        "max_actions_reached" => {
            Some("controller hit the run-from-spec --max-actions cap before completing".to_string())
        }
        "idle" => Some("controller is idle with no pending actions to run".to_string()),
        _ => None,
    }
}

fn best_root_blocker(
    explicit: Option<&str>,
    nested: Option<&str>,
    fallback: Option<&str>,
) -> Option<String> {
    [explicit, nested, fallback]
        .into_iter()
        .flatten()
        .filter(|message| !message.trim().is_empty())
        .min_by_key(|message| diagnostic_message_priority(message))
        .map(str::to_string)
}

fn diagnostic_message_priority(message: &str) -> u8 {
    let lower = message.to_ascii_lowercase();
    if lower.contains("fatal") || lower.contains("exception") || lower.contains("uncaught") {
        0
    } else if lower.contains("stale")
        || lower.contains("not found")
        || lower.contains("missing path")
        || lower.contains("required ability")
        || lower.contains("unavailable")
    {
        1
    } else if lower.contains("missing required")
        || lower.contains("required typed artifact")
        || lower.contains("typed artifact")
        || lower.contains("artifact")
    {
        8
    } else {
        4
    }
}

fn best_diagnostic_message(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let mut candidates = Vec::new();
    collect_diagnostic_messages(value, 0, &mut candidates);
    candidates
        .into_iter()
        .min_by_key(|candidate| {
            (
                diagnostic_message_priority(&candidate.message),
                candidate.depth,
            )
        })
        .map(|candidate| candidate.message)
}

fn provider_owned_runtime_context(value: &Value) -> Option<Value> {
    find_value_field(value, "runtime_context")
}

fn provider_owned_replay_command(value: &Value) -> Option<String> {
    find_string_field(value, "replay_command")
}

fn find_value_field(value: &Value, field: &str) -> Option<Value> {
    match value {
        Value::Object(map) => map.get(field).cloned().or_else(|| {
            map.values()
                .find_map(|value| find_value_field(value, field))
        }),
        Value::Array(items) => items
            .iter()
            .find_map(|value| find_value_field(value, field)),
        _ => None,
    }
}

fn find_string_field(value: &Value, field: &str) -> Option<String> {
    match value {
        Value::Object(map) => map
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                map.values()
                    .find_map(|value| find_string_field(value, field))
            }),
        Value::Array(items) => items
            .iter()
            .find_map(|value| find_string_field(value, field)),
        _ => None,
    }
}

struct DiagnosticCandidate {
    message: String,
    depth: usize,
}

fn collect_diagnostic_messages(value: &Value, depth: usize, out: &mut Vec<DiagnosticCandidate>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(diagnostics)) = map.get("diagnostics") {
                for diagnostic in diagnostics {
                    if let Some(message) = string_field(diagnostic, "message") {
                        out.push(DiagnosticCandidate { message, depth });
                    }
                }
            }
            for nested in map.values() {
                collect_diagnostic_messages(nested, depth + 1, out);
            }
        }
        Value::Array(items) => {
            for nested in items {
                collect_diagnostic_messages(nested, depth + 1, out);
            }
        }
        _ => {}
    }
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
}

fn nested_string(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().map(str::to_string)
}

/// Recursively collect every string value found under any of `fields`.
fn find_all_strings(value: &Value, fields: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    find_all_strings_into(value, fields, &mut out);
    out
}

fn find_all_strings_into(value: &Value, fields: &[&str], out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                if fields.contains(&key.as_str()) {
                    if let Some(found) = nested.as_str() {
                        if !found.trim().is_empty() && !out.iter().any(|seen| seen == found) {
                            out.push(found.to_string());
                        }
                    }
                }
                find_all_strings_into(nested, fields, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                find_all_strings_into(item, fields, out);
            }
        }
        _ => {}
    }
}

fn push_ref(refs: &mut Vec<ControllerRunEvidenceRef>, candidate: ControllerRunEvidenceRef) {
    if !refs.iter().any(|existing| existing.uri == candidate.uri) {
        refs.push(candidate);
    }
}
