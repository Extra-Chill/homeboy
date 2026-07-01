use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::agent_task_scheduler::{AgentTaskAggregate, AgentTaskPlan};
use crate::core::agent_tasks::AgentTaskEvidenceRef;
use crate::core::observation::ObservationStore;
use crate::core::redaction::{self, RedactionPolicy};
use crate::core::Result;

const EVIDENCE_TEXT_LIMIT: usize = 16 * 1024;

pub fn offloaded_status_remediation(run_id: &str) -> Result<Option<Value>> {
    let Some(run) = ObservationStore::open_initialized()?.get_run(run_id)? else {
        return Ok(None);
    };
    let Some(runner_id) = metadata_string(
        &run.metadata_json,
        &[&["runner_id"], &["identity", "runner_id"]],
    )
    .filter(|runner_id| !runner_id.trim().is_empty()) else {
        return Ok(None);
    };
    let runner_job_id = metadata_string(
        &run.metadata_json,
        &[
            &["runner_job_id"],
            &["job_id"],
            &["identity", "runner_job_id"],
        ],
    );

    Ok(Some(runner_status_remediation(
        run_id,
        &runner_id,
        runner_job_id.as_deref(),
    )))
}

fn runner_status_remediation(run_id: &str, runner_id: &str, runner_job_id: Option<&str>) -> Value {
    let command_prefix = format!("homeboy --runner {runner_id} agent-task");
    let mut commands = vec![
        format!("{command_prefix} status {run_id}"),
        format!("{command_prefix} logs {run_id} --full"),
        format!("{command_prefix} artifacts {run_id}"),
    ];
    if let Some(job_id) = runner_job_id.filter(|job_id| !job_id.trim().is_empty()) {
        commands.push(format!(
            "homeboy runner job logs {runner_id} {job_id} --full"
        ));
    }

    json!({
        "schema": "homeboy/agent-task-status-remediation/v1",
        "status": "runner_status_required",
        "run_id": run_id,
        "runner_id": runner_id,
        "runner_job_id": runner_job_id,
        "message": "Local observation metadata does not contain an agent-task run record; query the runner that owns this durable run.",
        "commands": commands,
        "remediation": {
            "status": format!("{command_prefix} status {run_id}"),
            "logs": format!("{command_prefix} logs {run_id} --full"),
            "artifacts": format!("{command_prefix} artifacts {run_id}"),
        },
    })
}

fn metadata_string(metadata: &Value, paths: &[&[&str]]) -> Option<String> {
    paths.iter().find_map(|path| {
        let mut current = metadata;
        for segment in *path {
            current = current.get(*segment)?;
        }
        current.as_str().map(str::to_string)
    })
}

pub fn persist_provider_boundary_replay_evidence(report: &Value) -> Option<String> {
    let run_id = report.get("run_id")?.as_str().unwrap_or("unknown-run");
    let task_id = report
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown-task");
    let dir = std::env::temp_dir()
        .join("homeboy-agent-task-evidence")
        .join(sanitize_evidence_path_part(run_id));
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!(
        "provider-boundary-replay-{}.json",
        sanitize_evidence_path_part(task_id)
    ));
    fs::write(&path, serde_json::to_vec_pretty(report).ok()?).ok()?;
    Some(format!("file://{}", path.display()))
}

fn sanitize_evidence_path_part(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

#[derive(Serialize)]
pub struct AgentTaskHydratedEvidence {
    pub kind: String,
    pub label: Option<String>,
    pub task_id: Option<String>,
    pub uri: String,
    pub source: String,
    pub status: String,
    pub truncated: bool,
    pub bytes_read: Option<usize>,
    pub omitted_bytes: Option<u64>,
    pub content: Value,
    pub error: Option<String>,
}

pub fn hydrate_evidence_ref(
    run_id: &str,
    evidence_ref: &AgentTaskEvidenceRef,
    task_id: Option<&str>,
    plan: Option<&AgentTaskPlan>,
    aggregate: Option<&AgentTaskAggregate>,
) -> AgentTaskHydratedEvidence {
    let hydrated = if evidence_ref.uri.starts_with("homeboy://agent-task/") {
        hydrate_homeboy_evidence_ref(run_id, &evidence_ref.uri, task_id, plan, aggregate)
    } else if evidence_ref.uri.starts_with("file://") {
        hydrate_file_evidence_ref(&evidence_ref.uri)
    } else if let Some(path) = local_evidence_path(&evidence_ref.uri) {
        hydrate_local_path_evidence_ref(&path)
    } else {
        Ok(HydratedContent {
            source: "unsupported".to_string(),
            truncated: false,
            bytes_read: None,
            omitted_bytes: None,
            content: json!({
                "summary": "Evidence ref is recorded but this URI scheme is not hydratable by agent-task evidence yet.",
                "unsupported_ref": evidence_ref.uri,
                "supported_refs": ["homeboy://agent-task/run/<run-id>/<section>", "file://<absolute-path>", "local filesystem path"],
                "next_action": "Use a file:// URI or local path for evidence stored on this machine; otherwise inspect the producing provider or artifact store for this ref.",
            }),
        })
    };

    match hydrated {
        Ok(content) => AgentTaskHydratedEvidence {
            kind: evidence_ref.kind.clone(),
            label: evidence_ref.label.clone(),
            task_id: task_id.map(str::to_string),
            uri: evidence_ref.uri.clone(),
            source: content.source,
            status: "ok".to_string(),
            truncated: content.truncated,
            bytes_read: content.bytes_read,
            omitted_bytes: content.omitted_bytes,
            content: redaction::redact_json(&content.content),
            error: None,
        },
        Err(error) => AgentTaskHydratedEvidence {
            kind: evidence_ref.kind.clone(),
            label: evidence_ref.label.clone(),
            task_id: task_id.map(str::to_string),
            uri: evidence_ref.uri.clone(),
            source: "error".to_string(),
            status: "error".to_string(),
            truncated: false,
            bytes_read: None,
            omitted_bytes: None,
            content: Value::Null,
            error: Some(redaction::redact_string(&error.message)),
        },
    }
}

struct HydratedContent {
    source: String,
    truncated: bool,
    bytes_read: Option<usize>,
    omitted_bytes: Option<u64>,
    content: Value,
}

fn hydrate_homeboy_evidence_ref(
    run_id: &str,
    uri: &str,
    task_id: Option<&str>,
    plan: Option<&AgentTaskPlan>,
    aggregate: Option<&AgentTaskAggregate>,
) -> Result<HydratedContent> {
    let parsed = parse_agent_task_homeboy_uri(uri)?;
    if parsed.run_id != run_id {
        return Err(crate::core::Error::validation_invalid_argument(
            "evidence_ref",
            format!(
                "evidence ref points at run {} but command is hydrating run {run_id}",
                parsed.run_id
            ),
            Some(uri.to_string()),
            None,
        ));
    }

    let content = match parsed.section.as_str() {
        "plan" => match (plan, task_id.or(parsed.task.as_deref())) {
            (Some(plan), Some(task_id)) => plan
                .tasks
                .iter()
                .find(|task| task.task_id == task_id)
                .map(|task| json!(task))
                .unwrap_or_else(|| json!({ "missing_task": task_id })),
            (Some(plan), None) => json!(plan),
            (None, _) => json!({ "summary": "plan is not available for this run" }),
        },
        "aggregate" => match (aggregate, parsed.outcome.as_deref().or(task_id)) {
            (Some(aggregate), Some(task_id)) => aggregate
                .outcomes
                .iter()
                .find(|outcome| outcome.task_id == task_id)
                .map(|outcome| json!(outcome))
                .unwrap_or_else(|| json!({ "missing_outcome": task_id })),
            (Some(aggregate), None) => json!(aggregate),
            (None, _) => json!({ "summary": "aggregate is not available for this run" }),
        },
        "artifacts" => match (aggregate, task_id.or(parsed.task.as_deref())) {
            (Some(aggregate), Some(task_id)) => aggregate
                .outcomes
                .iter()
                .find(|outcome| outcome.task_id == task_id)
                .map(|outcome| {
                    json!({
                        "task_id": outcome.task_id,
                        "status": outcome.status,
                        "summary": outcome.summary,
                        "artifacts": outcome.artifacts,
                        "typed_artifacts": outcome.typed_artifacts,
                        "evidence_refs": outcome.evidence_refs,
                        "diagnostics": outcome.diagnostics,
                    })
                })
                .unwrap_or_else(|| json!({ "missing_outcome": task_id })),
            _ => json!({ "summary": "outcome artifacts are not available for this run" }),
        },
        "logs" => serde_json::to_value(super::logs(run_id)?)
            .unwrap_or_else(|_| json!({ "summary": "logs could not be serialized" })),
        "status" => serde_json::to_value(super::status(run_id)?)
            .unwrap_or_else(|_| json!({ "summary": "status could not be serialized" })),
        section => json!({
            "summary": format!("homeboy agent-task evidence does not hydrate section '{section}' yet"),
        }),
    };

    Ok(HydratedContent {
        source: "homeboy".to_string(),
        truncated: false,
        bytes_read: None,
        omitted_bytes: None,
        content,
    })
}

fn hydrate_file_evidence_ref(uri: &str) -> Result<HydratedContent> {
    let path = file_uri_path(uri)?;
    hydrate_local_path_evidence_ref(&path)
}

fn hydrate_local_path_evidence_ref(path: &Path) -> Result<HydratedContent> {
    let metadata = fs::metadata(path)
        .map_err(|error| crate::core::Error::internal_io(error.to_string(), None))?;
    if !metadata.is_file() {
        return Err(crate::core::Error::validation_invalid_argument(
            "evidence_ref",
            "file evidence ref does not point at a regular file",
            None,
            None,
        ));
    }

    let bytes =
        fs::read(path).map_err(|error| crate::core::Error::internal_io(error.to_string(), None))?;
    let truncated = bytes.len() > EVIDENCE_TEXT_LIMIT;
    let visible = &bytes[..bytes.len().min(EVIDENCE_TEXT_LIMIT)];
    let text = String::from_utf8_lossy(visible);
    let redacted_text = redaction::redact_string(&text);
    let content = serde_json::from_str::<Value>(&redacted_text)
        .map(|value| json!({ "format": "json", "value": value }))
        .unwrap_or_else(|_| json!({ "format": "text", "text": redacted_text }));

    Ok(HydratedContent {
        source: "file".to_string(),
        truncated,
        bytes_read: Some(visible.len()),
        omitted_bytes: truncated.then_some(bytes.len().saturating_sub(EVIDENCE_TEXT_LIMIT) as u64),
        content,
    })
}

fn local_evidence_path(uri: &str) -> Option<PathBuf> {
    if uri.contains("://") || uri.contains('\0') || uri.trim().is_empty() {
        return None;
    }
    let path = Path::new(uri);
    if path.is_absolute() || path.exists() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

fn file_uri_path(uri: &str) -> Result<PathBuf> {
    let raw = uri.strip_prefix("file://").ok_or_else(|| {
        crate::core::Error::validation_invalid_argument(
            "evidence_ref",
            "file evidence ref must start with file://",
            Some(uri.to_string()),
            None,
        )
    })?;
    if raw.is_empty() || raw.contains('\0') {
        return Err(crate::core::Error::validation_invalid_argument(
            "evidence_ref",
            "file evidence ref path is empty or invalid",
            Some(uri.to_string()),
            None,
        ));
    }
    Ok(Path::new(raw).to_path_buf())
}

struct ParsedAgentTaskUri {
    run_id: String,
    section: String,
    task: Option<String>,
    outcome: Option<String>,
}

fn parse_agent_task_homeboy_uri(uri: &str) -> Result<ParsedAgentTaskUri> {
    let rest = uri
        .strip_prefix("homeboy://agent-task/run/")
        .ok_or_else(|| {
            crate::core::Error::validation_invalid_argument(
                "evidence_ref",
                "unsupported homeboy agent-task evidence ref",
                Some(uri.to_string()),
                None,
            )
        })?;
    let (path, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let mut parts = path.split('/');
    let run_id = parts.next().unwrap_or_default();
    let section = parts.next().unwrap_or_default();
    if run_id.is_empty() || section.is_empty() {
        return Err(crate::core::Error::validation_invalid_argument(
            "evidence_ref",
            "homeboy agent-task evidence ref must include run id and section",
            Some(uri.to_string()),
            None,
        ));
    }

    Ok(ParsedAgentTaskUri {
        run_id: run_id.to_string(),
        section: section.to_string(),
        task: fragment_value(fragment, "task"),
        outcome: fragment_value(fragment, "outcome"),
    })
}

fn fragment_value(fragment: &str, key: &str) -> Option<String> {
    fragment.split('&').find_map(|part| {
        let (candidate, value) = part.split_once('=')?;
        (candidate == key && !value.trim().is_empty()).then(|| value.to_string())
    })
}

pub fn hydrate_evidence_summary(task_id: &str, evidence: &AgentTaskEvidenceRef) -> Option<Value> {
    let path = evidence.uri.strip_prefix("file://")?;
    if !path.ends_with(".json") {
        return None;
    }
    let raw = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let redacted = RedactionPolicy::default().redact_json(&value);
    Some(json!({
        "task_id": task_id,
        "kind": evidence.kind,
        "label": evidence.label,
        "uri": evidence.uri,
        "summary": evidence_json_summary(&redacted),
    }))
}

fn evidence_json_summary(value: &Value) -> Value {
    json!({
        "status": find_string_field(value, &["status", "state"]),
        "failure_classification": find_string_field(value, &["failure_classification", "failure_class", "classification", "class", "code", "kind"]),
        "message": find_string_field(value, &["message", "summary", "error", "detail", "reason"]),
        "command": find_string_field(value, &["command", "cmd", "failing_command"]),
        "exit_code": find_number_field(value, &["exit_code", "exit_status", "status_code"]),
        "stderr_excerpt": find_string_field(value, &["stderr", "stderr_excerpt"]).map(|text| excerpt(&text)),
        "stdout_excerpt": find_string_field(value, &["stdout", "stdout_excerpt"]).map(|text| excerpt(&text)),
        "diagnostics": first_diagnostics(value),
    })
}

fn find_string_field(value: &Value, names: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for name in names {
                if let Some(text) = map.get(*name).and_then(Value::as_str) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
            map.values()
                .find_map(|nested| find_string_field(nested, names))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|nested| find_string_field(nested, names)),
        _ => None,
    }
}

fn find_number_field(value: &Value, names: &[&str]) -> Option<i64> {
    match value {
        Value::Object(map) => {
            for name in names {
                if let Some(number) = map.get(*name).and_then(Value::as_i64) {
                    return Some(number);
                }
            }
            map.values()
                .find_map(|nested| find_number_field(nested, names))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|nested| find_number_field(nested, names)),
        _ => None,
    }
}

fn first_diagnostics(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("diagnostics") {
                return Value::Array(items.iter().take(3).cloned().collect());
            }
            map.values()
                .find_map(|nested| match first_diagnostics(nested) {
                    Value::Array(items) if !items.is_empty() => Some(Value::Array(items)),
                    _ => None,
                })
                .unwrap_or_else(|| Value::Array(Vec::new()))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|nested| match first_diagnostics(nested) {
                Value::Array(items) if !items.is_empty() => Some(Value::Array(items)),
                _ => None,
            })
            .unwrap_or_else(|| Value::Array(Vec::new())),
        _ => Value::Array(Vec::new()),
    }
}

fn excerpt(text: &str) -> String {
    const LIMIT: usize = 600;
    let trimmed = text.trim();
    if trimmed.chars().count() <= LIMIT {
        trimmed.to_string()
    } else {
        let prefix: String = trimmed.chars().take(LIMIT).collect();
        format!("{prefix}...")
    }
}
