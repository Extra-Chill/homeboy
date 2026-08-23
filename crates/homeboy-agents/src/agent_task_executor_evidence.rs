//! Persist and link the latest raw executor request/result as first-class
//! agent-task run evidence.
//!
//! Every dispatched agent task encodes a raw executor request (the JSON piped
//! to the provider command's stdin) and receives a raw executor result (the
//! provider outcome JSON). Historically these only existed transiently inside
//! runner temp directories (`homeboy-...-agent-task-input-*/input.json`), so
//! debugging required spelunking those directories by guessing names.
//!
//! This module writes the *latest* raw request and result to a stable,
//! per-task evidence directory and links them back onto the outcome's
//! `evidence_refs` so `homeboy runs evidence <run>`, `agent-task status`, and
//! controller output can surface direct references without guessing temp paths.
//!
//! Redaction preserves secrets (api keys, tokens, auth headers) while retaining
//! the operationally important fields: component contracts, runtime/component
//! paths, model/provider metadata, and typed artifact expectations all survive
//! the redaction pass because [`RedactionPolicy`] only rewrites known-sensitive
//! keys and leaves the rest of the JSON intact.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::agent_task::{AgentTaskEvidenceRef, AgentTaskExecutorRequest, AgentTaskOutcome};
use homeboy_core::redaction::RedactionPolicy;

/// Evidence kind for the latest raw executor request (input piped to the
/// provider command). Surfaced as a first-class run evidence ref.
pub const EXECUTOR_INPUT_EVIDENCE_KIND: &str = "executor-input";

/// Evidence kind for the latest raw executor result (normalized provider
/// outcome). Surfaced as a first-class run evidence ref.
pub const EXECUTOR_RESULT_EVIDENCE_KIND: &str = "executor-result";

/// Evidence kinds for Homeboy-owned provider runtime evidence.
pub const EXECUTOR_ARTIFACT_ROOT_EVIDENCE_KIND: &str = "executor-artifact-root";
pub const PROVIDER_SESSION_EVIDENCE_KIND: &str = "provider-session";
pub const RUNTIME_STDOUT_EVIDENCE_KIND: &str = "provider-runtime-stdout";
pub const RUNTIME_STDERR_EVIDENCE_KIND: &str = "provider-runtime-stderr";
pub const RUNTIME_PROGRESS_EVIDENCE_KIND: &str = "provider-runtime-progress";
pub const RUNTIME_EVIDENCE_KIND: &str = "executor-runtime-evidence";

/// File name for the persisted latest raw executor request.
pub const EXECUTOR_INPUT_FILE: &str = "executor-input.json";

/// File name for the persisted latest raw executor result.
pub const EXECUTOR_RESULT_FILE: &str = "executor-result.json";

/// File name for the redacted runtime evidence index.
pub const RUNTIME_EVIDENCE_FILE: &str = "executor-runtime-evidence.json";

const STRUCTURED_PROGRESS_EVENT_LIMIT: usize = 128;

/// Persist the latest raw executor request and result for `request`/`outcome`
/// and append linking evidence refs onto the outcome.
///
/// This is best-effort: persistence failures never change the executor outcome
/// status. When a file is written, a direct `executor-input` / `executor-result`
/// evidence ref is added so operators can inspect exactly what was sent to and
/// returned from the executor. The redacted request always retains component
/// contracts, runtime/component paths, model/provider metadata, and typed
/// artifact expectations.
pub(crate) fn link_latest_executor_evidence(
    request: &AgentTaskExecutorRequest,
    outcome: &mut AgentTaskOutcome,
    run_id: Option<&str>,
) {
    let policy = RedactionPolicy::default();
    let dir = executor_evidence_dir(run_id, &request.task_id);
    if fs::create_dir_all(&dir).is_err() {
        return;
    }

    if let Some(uri) = persist_evidence_file(
        &dir.join(EXECUTOR_INPUT_FILE),
        &redacted_request_value(request, &policy),
    ) {
        push_unique_evidence_ref(
            outcome,
            AgentTaskEvidenceRef {
                kind: EXECUTOR_INPUT_EVIDENCE_KIND.to_string(),
                uri,
                label: Some("latest raw executor input".to_string()),
            },
        );
    }

    if let Some(uri) = persist_evidence_file(
        &dir.join(EXECUTOR_RESULT_FILE),
        &redacted_outcome_value(outcome, &policy),
    ) {
        push_unique_evidence_ref(
            outcome,
            AgentTaskEvidenceRef {
                kind: EXECUTOR_RESULT_EVIDENCE_KIND.to_string(),
                uri,
                label: Some("latest raw executor result".to_string()),
            },
        );
    }

    link_runtime_evidence(request, outcome, &dir, &policy);
}

/// Link provider-neutral runtime evidence retained under the executor's owned
/// artifact root. The index records absent optional streams explicitly, while
/// individual file refs remain directly hydratable when they exist.
fn link_runtime_evidence(
    request: &AgentTaskExecutorRequest,
    outcome: &mut AgentTaskOutcome,
    evidence_dir: &Path,
    policy: &RedactionPolicy,
) {
    let artifact_root = request.artifacts_path.canonicalize().ok();
    let runtime_files = artifact_root
        .as_deref()
        .map(discover_runtime_files)
        .unwrap_or_default();
    if request.request.executor.backend == "opencode" {
        hydrate_structured_runtime_evidence(outcome, &runtime_files, policy);
    }
    let sessions = provider_session_metadata(outcome);
    let index = json!({
        "artifact_root": artifact_root.as_ref().map(|path| format!("file://{}", path.display())),
        "provider_runtime_identities": sessions,
        "runtime_stdout": runtime_files.get(RUNTIME_STDOUT_EVIDENCE_KIND),
        "runtime_stderr": runtime_files.get(RUNTIME_STDERR_EVIDENCE_KIND),
        "runtime_progress": runtime_files.get(RUNTIME_PROGRESS_EVIDENCE_KIND),
    });
    let Some(index_uri) = persist_evidence_file(
        &evidence_dir.join(RUNTIME_EVIDENCE_FILE),
        &policy.redact_json(&index),
    ) else {
        return;
    };

    push_unique_evidence_ref(
        outcome,
        AgentTaskEvidenceRef {
            kind: RUNTIME_EVIDENCE_KIND.to_string(),
            uri: index_uri.clone(),
            label: Some("provider runtime evidence index".to_string()),
        },
    );
    if let Some(root) = artifact_root {
        push_unique_evidence_ref(
            outcome,
            AgentTaskEvidenceRef {
                kind: EXECUTOR_ARTIFACT_ROOT_EVIDENCE_KIND.to_string(),
                uri: format!("file://{}", root.display()),
                label: Some("executor artifact root".to_string()),
            },
        );
    }
    if !sessions.is_empty() {
        push_unique_evidence_ref(
            outcome,
            AgentTaskEvidenceRef {
                kind: PROVIDER_SESSION_EVIDENCE_KIND.to_string(),
                uri: index_uri,
                label: Some("provider-native session metadata".to_string()),
            },
        );
    }
    for (kind, paths) in runtime_files {
        for path in paths {
            push_unique_evidence_ref(
                outcome,
                AgentTaskEvidenceRef {
                    kind: kind.clone(),
                    uri: format!("file://{}", path.display()),
                    label: Some(kind.replace("provider-", "").replace('-', " ")),
                },
            );
        }
    }
}

/// Derive compact progress from structured runtime stdout. The full transcript
/// remains in its original artifact; this sidecar contains lifecycle facts only.
fn hydrate_structured_runtime_evidence(
    outcome: &mut AgentTaskOutcome,
    runtime_files: &BTreeMap<String, Vec<PathBuf>>,
    policy: &RedactionPolicy,
) {
    let Some(stdout_paths) = runtime_files.get(RUNTIME_STDOUT_EVIDENCE_KIND) else {
        return;
    };
    let mut session_ids = Vec::new();
    let mut events = Vec::new();
    let mut dropped_events = 0;
    for path in stdout_paths {
        let Ok(file) = fs::File::open(path) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(session_id) = structured_session_id(&event) {
                if !session_ids.iter().any(|existing| existing == session_id) {
                    session_ids.push(session_id.to_string());
                }
            }
            if let Some(progress) = structured_progress_event(&event) {
                if events.len() < STRUCTURED_PROGRESS_EVENT_LIMIT {
                    events.push(progress);
                } else {
                    dropped_events += 1;
                }
            }
        }
    }
    if session_ids.len() != 1 {
        return;
    }

    let session_id = session_ids.pop().expect("one stable session id");
    if outcome.metadata.is_null() {
        outcome.metadata = json!({});
    }
    let Some(metadata) = outcome.metadata.as_object_mut() else {
        return;
    };
    metadata.insert(
        "opencode_session".to_string(),
        json!({ "status": "discovered", "id": session_id }),
    );
    metadata.insert(
        "opencode_progress".to_string(),
        json!({
            "emitted": events.len(),
            "coalesced_or_dropped": dropped_events,
            "last_type": events.last().and_then(|event| event.get("type")).and_then(Value::as_str).unwrap_or(""),
        }),
    );

    let progress = events
        .iter()
        .map(|event| serde_json::to_string(&policy.redact_json(event)))
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .map(|lines| format!("{}\n", lines.join("\n")));
    if let (Some(progress), Some(progress_paths)) =
        (progress, runtime_files.get(RUNTIME_PROGRESS_EVIDENCE_KIND))
    {
        for path in progress_paths {
            // A provider-produced progress stream is authoritative. The derived
            // compact stream only fills the empty artifact reserved for it.
            if fs::metadata(path).is_ok_and(|metadata| metadata.len() == 0) {
                let _ = fs::write(path, &progress);
            }
        }
    }
}

fn structured_session_id(event: &Value) -> Option<&str> {
    structured_event_type(event)?;
    event
        .get("sessionID")
        .and_then(Value::as_str)
        .or_else(|| event.pointer("/part/sessionID").and_then(Value::as_str))
        .filter(|id| !id.trim().is_empty())
}

fn structured_progress_event(event: &Value) -> Option<Value> {
    let event_type = structured_event_type(event)?;
    let part = event.get("part")?;
    let status = part.pointer("/state/status").and_then(Value::as_str);
    let is_progress = matches!(event_type, "step_start" | "step_finish")
        || event_type == "tool_use" && matches!(status, Some("completed" | "error"));
    if !is_progress {
        return None;
    }
    Some(json!({
        "type": event_type,
        "timestamp": event.get("timestamp"),
        "part_id": part.get("id"),
        "message_id": part.get("messageID"),
        "status": status,
        "tool": part.get("tool"),
    }))
}

fn structured_event_type(event: &Value) -> Option<&str> {
    let event_type = event.get("type")?.as_str()?;
    event.get("part")?.as_object()?;
    matches!(event_type, "step_start" | "step_finish" | "tool_use").then_some(event_type)
}

fn provider_session_metadata(outcome: &AgentTaskOutcome) -> Vec<Value> {
    outcome
        .metadata
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, value)| key.to_ascii_lowercase().contains("session") && !value.is_null())
        .map(|(key, value)| {
            let id = value
                .as_object()
                .and_then(|session| {
                    ["id", "session_id", "sessionId", "sessionID"]
                        .iter()
                        .find_map(|key| session.get(*key).and_then(Value::as_str))
                })
                .filter(|id| !id.trim().is_empty());
            json!({
                "source": key,
                "id": id,
                "details": value,
            })
        })
        .collect()
}

fn discover_runtime_files(root: &Path) -> BTreeMap<String, Vec<PathBuf>> {
    let mut files = BTreeMap::new();
    let Ok(entries) = fs::read_dir(root) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        if !path.starts_with(root) || !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let kind = if name.contains("runtime") && name.contains("stdout") {
            Some(RUNTIME_STDOUT_EVIDENCE_KIND)
        } else if name.contains("runtime") && name.contains("stderr") {
            Some(RUNTIME_STDERR_EVIDENCE_KIND)
        } else if name.contains("progress") {
            Some(RUNTIME_PROGRESS_EVIDENCE_KIND)
        } else {
            None
        };
        if let Some(kind) = kind {
            files
                .entry(kind.to_string())
                .or_insert_with(Vec::new)
                .push(path);
        }
    }
    files
}

fn executor_evidence_dir(run_id: Option<&str>, task_id: &str) -> PathBuf {
    durable_executor_evidence_root()
        .join(sanitize_task_id(run_id.unwrap_or("unrecorded-run")))
        .join(sanitize_task_id(task_id))
}

fn durable_executor_evidence_root() -> PathBuf {
    homeboy_core::artifacts::root()
        .unwrap_or_else(|_| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".homeboy-artifacts")
        })
        .join("agent-task")
        .join("executor-evidence")
}

fn sanitize_task_id(task_id: &str) -> String {
    let sanitized: String = task_id
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
        "unknown-task".to_string()
    } else {
        sanitized
    }
}

/// Redact the executor request for evidence while preserving the operationally
/// important fields. `redact_json` only rewrites known-sensitive keys, so
/// component contracts, runtime/component paths, model/provider metadata, and
/// typed artifact expectations are retained.
fn redacted_request_value(request: &AgentTaskExecutorRequest, policy: &RedactionPolicy) -> Value {
    match serde_json::to_value(request) {
        Ok(mut value) => {
            redact_runtime_tool_env(&mut value, "runtime_tools");
            redact_runtime_tool_env(&mut value, "resolved_runtime_tools");
            policy.redact_json(&value)
        }
        Err(error) => json!({
            "error": "failed to serialize executor request for evidence",
            "detail": error.to_string(),
            "task_id": request.task_id,
        }),
    }
}

/// Runtime tool literals are execution input, not durable evidence. Keep their
/// names visible for diagnosis while replacing every value before persistence.
fn redact_runtime_tool_env(value: &mut Value, field: &str) {
    let Some(tools) = value.get_mut(field).and_then(Value::as_array_mut) else {
        return;
    };
    for tool in tools {
        let Some(env) = tool.get_mut("env").and_then(Value::as_object_mut) else {
            continue;
        };
        for env_value in env.values_mut() {
            *env_value = Value::String("[redacted]".to_string());
        }
    }
}

fn redacted_outcome_value(outcome: &AgentTaskOutcome, policy: &RedactionPolicy) -> Value {
    match serde_json::to_value(outcome) {
        Ok(value) => policy.redact_json(&value),
        Err(error) => json!({
            "error": "failed to serialize executor outcome for evidence",
            "detail": error.to_string(),
            "task_id": outcome.task_id,
        }),
    }
}

/// Atomically persist `value` to `path` and return a stable `file://` URI when
/// the write succeeds. Returns `None` on any IO failure (best-effort evidence).
fn persist_evidence_file(path: &Path, value: &Value) -> Option<String> {
    let serialized = serde_json::to_vec_pretty(value).ok()?;
    let parent = path.parent()?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()?.to_string_lossy(),
        std::process::id()
    ));
    fs::write(&tmp, &serialized).ok()?;
    if fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    Some(format!("file://{}", path.display()))
}

fn push_unique_evidence_ref(outcome: &mut AgentTaskOutcome, evidence_ref: AgentTaskEvidenceRef) {
    let duplicate = outcome
        .evidence_refs
        .iter()
        .any(|existing| existing.kind == evidence_ref.kind && existing.uri == evidence_ref.uri);
    if !duplicate {
        outcome.evidence_refs.push(evidence_ref);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ARTIFACT_ROOT_LOCK: Mutex<()> = Mutex::new(());

    /// Scopes the process-global artifact-root override to one test.
    ///
    /// Two properties matter, and both exist so that a panicking `#[test]`
    /// cannot fail an unrelated later test on something other than its own
    /// merits:
    ///
    /// 1. The lock is poison-tolerant. A panic inside `test` poisons
    ///    `ARTIFACT_ROOT_LOCK`, and a plain `.expect(...)` then reports every
    ///    subsequent test in this module as a `PoisonError` rather than its
    ///    real result. `homeboy_core::test_support::env_lock` already ignores
    ///    poison for exactly this reason; match it here.
    /// 2. The override is cleared from `Drop`, not on the straight-line return
    ///    path. A panic used to skip the reset and leave the override pointing
    ///    at an already-deleted `TempDir`, so the next test resolved artifacts
    ///    under a missing directory.
    fn with_artifact_root<R>(test: impl FnOnce(&Path) -> R) -> R {
        struct ClearArtifactRootOverride;

        impl Drop for ClearArtifactRootOverride {
            fn drop(&mut self) {
                homeboy_core::set_artifact_root_override(None);
            }
        }

        let _lock = ARTIFACT_ROOT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let guard = tempfile::tempdir().expect("artifact root");
        homeboy_core::set_artifact_root_override(Some(guard.path().to_path_buf()));
        // Declared after `guard` so the override is cleared before the
        // directory it points at is removed, and before the lock is released.
        let _clear_override = ClearArtifactRootOverride;
        test(guard.path())
    }

    #[test]
    fn evidence_dir_is_stable_for_a_run_and_task_id() {
        with_artifact_root(|_| {
            let first = executor_evidence_dir(Some("run/attempt:1"), "task/with weird:chars");
            let second = executor_evidence_dir(Some("run/attempt:1"), "task/with weird:chars");
            assert_eq!(first, second);
            assert!(first
                .to_string_lossy()
                .contains("agent-task/executor-evidence"));
        });
    }

    #[test]
    fn evidence_dir_is_under_durable_artifact_root() {
        with_artifact_root(|artifact_root| {
            let path = executor_evidence_dir(Some("run-1"), "task-1");
            assert!(path.starts_with(artifact_root));
            assert!(!path
                .to_string_lossy()
                .contains("homeboy-agent-task-evidence"));
        });
    }
}
