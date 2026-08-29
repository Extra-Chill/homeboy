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
    let dir = executor_evidence_dir(&request.artifact_store_root, run_id, &request.task_id);
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
    let structured_error = runtime_files
        .get(RUNTIME_STDOUT_EVIDENCE_KIND)
        .and_then(|paths| {
            structured_error_from_runtime_files(&request.request.executor.backend, paths)
        });
    if let Some(classification) = structured_error
        .as_ref()
        .and_then(crate::agent_task_provider::normalized_error_failure_classification)
    {
        // This is a provider-side account rejection, not a code-level failure.
        // Rotation remains an explicit scheduler policy decision (#13691).
        outcome.failure_classification = Some(classification);
    }
    let index = json!({
        "artifact_root": artifact_root.as_ref().map(|path| format!("file://{}", path.display())),
        "provider_runtime_identities": sessions,
        "runtime_stdout": runtime_files.get(RUNTIME_STDOUT_EVIDENCE_KIND),
        "runtime_stderr": runtime_files.get(RUNTIME_STDERR_EVIDENCE_KIND),
        "runtime_progress": runtime_files.get(RUNTIME_PROGRESS_EVIDENCE_KIND),
        // Adapter-normalized terminal provider error, redacted. Persisted at
        // execution time so read paths never need vendor knowledge (#13703).
        "structured_error": structured_error,
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

/// Normalize a terminal structured provider error out of the runtime stdout
/// capture files, backend-aware through the provider adapter registry. The
/// latest capture file wins: it belongs to the final attempt. Only the
/// adapter's normalized, redacted output is persisted.
fn structured_error_from_runtime_files(backend: &str, paths: &[PathBuf]) -> Option<Value> {
    for path in paths.iter().rev() {
        let Some(tail) =
            crate::agent_task_provider::structured_error::read_runtime_stream_tail(path)
        else {
            continue;
        };
        if let Some(error) =
            crate::agent_task_provider::structured_error::normalize_runtime_stream_error(
                Some(backend),
                &tail.raw,
            )
        {
            return Some(error);
        }
    }
    None
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

fn executor_evidence_dir(
    artifact_store_root: &Path,
    run_id: Option<&str>,
    task_id: &str,
) -> PathBuf {
    artifact_store_root
        .join("agent-task")
        .join("executor-evidence")
        .join(sanitize_task_id(run_id.unwrap_or("unrecorded-run")))
        .join(sanitize_task_id(task_id))
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
    use crate::agent_task::{
        AgentTaskComponentContract, AgentTaskExecutor, AgentTaskFailureClassification,
        AgentTaskLimits, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest,
        AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA,
    };
    use serde_json::Map;

    fn test_request() -> AgentTaskRequest {
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "neutral-runtime proof".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "example-provider".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: Some("claude-sonnet".to_string()),
                config: json!({
                    "runtime_component_paths": ["/runner/components/sample-runtime"],
                    "api_key": "sk-super-secret",
                }),
            },
            instructions: "prove the typed artifact handoff".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: vec![AgentTaskComponentContract {
                slug: Some("sample-runtime".to_string()),
                path: Some("/runner/components/sample-runtime".to_string()),
                extra: Map::new(),
            }],
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: vec!["component_contracts".to_string()],
            artifact_declarations: Vec::new(),
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
            metadata: Value::Null,
        }
    }

    fn test_outcome() -> AgentTaskOutcome {
        AgentTaskOutcome {
            task_id: "neutral-runtime proof".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("token=abc done".to_string()),
            ..Default::default()
        }
    }

    fn with_artifact_root<R>(test: impl FnOnce(&Path) -> R) -> R {
        let guard = tempfile::tempdir().expect("artifact root");
        test(guard.path())
    }

    #[test]
    fn links_executor_input_and_result_evidence_refs() {
        with_artifact_root(|_| {
            let request = executor_test_request();
            let mut outcome = test_outcome();
            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));

            let kinds: Vec<&str> = outcome
                .evidence_refs
                .iter()
                .map(|evidence| evidence.kind.as_str())
                .collect();
            assert!(kinds.contains(&EXECUTOR_INPUT_EVIDENCE_KIND));
            assert!(kinds.contains(&EXECUTOR_RESULT_EVIDENCE_KIND));

            for evidence in &outcome.evidence_refs {
                let path = evidence
                    .uri
                    .strip_prefix("file://")
                    .expect("file uri prefix");
                assert!(Path::new(path).exists(), "evidence path should exist");
            }
        });
    }

    fn executor_test_request() -> AgentTaskExecutorRequest {
        let request = test_request();
        // Use a deterministic isolated `.../runner/artifacts/task` directory
        // instead of the host `std::env::temp_dir()`. The provenance assertions
        // check the retained path ends in that stable segment; the ambient temp
        // dir is host-dependent (and, once canonicalized, never contained the
        // asserted path), making the test pass or fail by host configuration
        // rather than behavior (#8964). The tempdir is intentionally leaked so the
        // captured path stays valid for the lifetime of the test process.
        let root = tempfile::Builder::new()
            .prefix("hb-executor-artifacts-")
            .tempdir()
            .expect("executor artifacts root")
            .keep();
        let artifacts_path = root.join("runner").join("artifacts").join("task");
        std::fs::create_dir_all(&artifacts_path).expect("create isolated artifacts path");
        AgentTaskExecutorRequest {
            artifacts_root_identity: crate::agent_task_provider::artifact_finalization::ExecutorArtifactRootIdentity::capture_with_finalized_root(&artifacts_path, root.join("executor-finalized")).expect("artifact root identity"),
            artifacts_path,
            artifact_store_root: root,
            artifacts_path_provenance: crate::agent_task::AgentTaskArtifactsPathProvenance {
                owner: "homeboy".to_string(),
                locality: "runner".to_string(),
                plan_id: "plan-1".to_string(),
                run_id: Some("run-1".to_string()),
                task_id: request.task_id.clone(),
                attempt: 1,
            },
            request,
            resolved_runtime_tools: Vec::new(),
        }
    }

    #[test]
    fn persisted_input_redacts_secrets_but_retains_contracts_paths_and_artifacts() {
        with_artifact_root(|_| {
            let request = executor_test_request();
            let mut outcome = test_outcome();
            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));

            let input_ref = outcome
                .evidence_refs
                .iter()
                .find(|evidence| evidence.kind == EXECUTOR_INPUT_EVIDENCE_KIND)
                .expect("executor input evidence");
            let path = input_ref
                .uri
                .strip_prefix("file://")
                .expect("file uri prefix");
            let raw = fs::read_to_string(path).expect("read input evidence");

            // Secret redacted...
            assert!(!raw.contains("sk-super-secret"));
            assert!(raw.contains("[REDACTED]"));
            // ...while component contracts, runtime/component paths, model, and
            // typed artifact expectations are retained.
            assert!(raw.contains("/runner/components/sample-runtime"));
            assert!(raw.contains("runtime_component_paths"));
            assert!(raw.contains("claude-sonnet"));
            assert!(raw.contains("component_contracts"));
            assert!(raw.contains("/runner/artifacts/task"));
            assert!(raw.contains("artifacts_path_provenance"));
            assert!(raw.contains("\"locality\": \"runner\""));
        });
    }

    #[test]
    fn links_retained_runtime_files_and_redacted_provider_session_metadata() {
        with_artifact_root(|_| {
            let request = executor_test_request();
            fs::write(
                request.artifacts_path.join("provider-runtime-stdout.log"),
                "runtime stdout",
            )
            .expect("write stdout");
            fs::write(
                request.artifacts_path.join("provider-runtime-stderr.log"),
                "runtime stderr",
            )
            .expect("write stderr");
            fs::write(
                request.artifacts_path.join("provider-progress.jsonl"),
                "{}\n",
            )
            .expect("write progress");
            let mut outcome = test_outcome();
            outcome.status = AgentTaskOutcomeStatus::Failed;
            outcome.metadata = json!({
                "provider_session": { "id": "session-123", "token": "secret-session-token" }
            });

            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));

            for kind in [
                EXECUTOR_ARTIFACT_ROOT_EVIDENCE_KIND,
                PROVIDER_SESSION_EVIDENCE_KIND,
                RUNTIME_STDOUT_EVIDENCE_KIND,
                RUNTIME_STDERR_EVIDENCE_KIND,
                RUNTIME_PROGRESS_EVIDENCE_KIND,
            ] {
                assert!(
                    outcome
                        .evidence_refs
                        .iter()
                        .any(|evidence| evidence.kind == kind),
                    "missing {kind} evidence ref"
                );
            }
            let index = outcome
                .evidence_refs
                .iter()
                .find(|evidence| evidence.kind == RUNTIME_EVIDENCE_KIND)
                .expect("runtime evidence index");
            let raw = fs::read_to_string(index.uri.strip_prefix("file://").expect("file uri"))
                .expect("read runtime evidence index");
            assert!(raw.contains("session-123"));
            assert!(!raw.contains("secret-session-token"));
            assert!(raw.contains("[REDACTED]"));
        });
    }

    #[test]
    fn extracts_opencode_session_and_bounded_progress_from_structured_stdout() {
        with_artifact_root(|_| {
            let request = executor_test_request();
            let mut request = request;
            request.request.executor.backend = "opencode".to_string();
            let fixture = include_str!("fixtures/opencode-structured-transcript.jsonl");
            fs::write(
                request.artifacts_path.join("provider-runtime-stdout.log"),
                fixture,
            )
            .expect("write structured stdout");
            let progress_path = request.artifacts_path.join("provider-progress.jsonl");
            fs::write(&progress_path, "").expect("create progress artifact");
            let mut outcome = test_outcome();
            outcome.metadata = json!({
                "opencode_session": { "status": "not_discovered" },
                "opencode_progress": { "emitted": 0, "coalesced_or_dropped": 0, "last_type": "" }
            });

            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));

            assert_eq!(
                outcome.metadata["opencode_session"],
                json!({ "status": "discovered", "id": "ses_sanitized_progress_fixture" })
            );
            assert_eq!(outcome.metadata["opencode_progress"]["emitted"], 3);
            let progress: Vec<Value> = fs::read_to_string(&progress_path)
                .expect("read progress")
                .lines()
                .map(|line| serde_json::from_str(line).expect("parse progress event"))
                .collect();
            assert_eq!(progress.len(), 3);
            assert_eq!(progress[1]["tool"], "read");
            assert!(!fs::read_to_string(&progress_path)
                .expect("read progress")
                .contains("private transcript content"));
        });
    }

    #[test]
    fn classifies_a_structured_opencode_account_rejection() {
        with_artifact_root(|_| {
            let mut request = executor_test_request();
            request.request.executor.backend = "opencode".to_string();
            fs::write(
                request.artifacts_path.join("provider-runtime-stdout.log"),
                r#"{"type":"error","error":{"name":"APIError","data":{"message":"spending-limit: You have run out of credits.","statusCode":403,"isRetryable":false}}}"#,
            )
            .expect("write stdout");
            let mut outcome = test_outcome();
            outcome.failure_classification = Some(AgentTaskFailureClassification::ExecutionFailed);

            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));

            assert_eq!(
                outcome.failure_classification,
                Some(AgentTaskFailureClassification::ProviderAccountBlocked)
            );
        });
    }

    #[test]
    fn keeps_existing_provider_progress_while_linking_opencode_session() {
        with_artifact_root(|_| {
            let mut request = executor_test_request();
            request.request.executor.backend = "opencode".to_string();
            fs::write(
                request.artifacts_path.join("provider-runtime-stdout.log"),
                include_str!("fixtures/opencode-structured-transcript.jsonl"),
            )
            .expect("write structured stdout");
            let progress_path = request.artifacts_path.join("provider-progress.jsonl");
            fs::write(&progress_path, "{\"provider\":\"progress\"}\n")
                .expect("write provider progress");
            let mut outcome = test_outcome();

            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));

            assert_eq!(
                fs::read_to_string(progress_path).expect("read provider progress"),
                "{\"provider\":\"progress\"}\n"
            );
            assert_eq!(
                outcome.metadata["opencode_session"]["id"],
                "ses_sanitized_progress_fixture"
            );
        });
    }

    #[test]
    fn leaves_unknown_runtime_format_undiscovered_and_progress_empty() {
        with_artifact_root(|_| {
            let request = executor_test_request();
            let mut request = request;
            request.request.executor.backend = "opencode".to_string();
            fs::write(
                request.artifacts_path.join("provider-runtime-stdout.log"),
                "unstructured provider output\n",
            )
            .expect("write stdout");
            let progress_path = request.artifacts_path.join("provider-progress.jsonl");
            fs::write(&progress_path, "").expect("create progress artifact");
            let mut outcome = test_outcome();
            outcome.metadata = json!({
                "opencode_session": { "status": "not_discovered" },
                "opencode_progress": { "emitted": 0, "coalesced_or_dropped": 0, "last_type": "" }
            });

            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));

            assert_eq!(
                outcome.metadata["opencode_session"]["status"],
                "not_discovered"
            );
            assert_eq!(outcome.metadata["opencode_progress"]["emitted"], 0);
            assert!(fs::read_to_string(progress_path)
                .expect("read progress")
                .is_empty());
        });
    }

    #[test]
    fn runtime_evidence_index_records_absent_optional_streams() {
        with_artifact_root(|_| {
            let request = executor_test_request();
            let mut outcome = test_outcome();

            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));

            let index = outcome
                .evidence_refs
                .iter()
                .find(|evidence| evidence.kind == RUNTIME_EVIDENCE_KIND)
                .expect("runtime evidence index");
            let value: Value = serde_json::from_slice(
                &fs::read(index.uri.strip_prefix("file://").expect("file uri"))
                    .expect("read runtime evidence index"),
            )
            .expect("parse runtime evidence index");
            assert_eq!(value["provider_runtime_identities"], json!([]));
            assert_eq!(value["runtime_stdout"], Value::Null);
            assert_eq!(value["runtime_stderr"], Value::Null);
            assert_eq!(value["runtime_progress"], Value::Null);
        });
    }

    #[test]
    fn persisted_input_redacts_runtime_tool_literal_environment_values() {
        let mut request = executor_test_request();
        request.request.runtime_tools = vec![serde_json::from_value(json!({
            "id": "fixture.mcp",
            "command": ["fixture-mcp"],
            "env": { "FIXTURE_MODE": "private-declaration" }
        }))
        .expect("runtime tool")];
        request.resolved_runtime_tools = vec![crate::agent_task::ResolvedAgentTaskRuntimeTool {
            schema: crate::agent_task::RESOLVED_AGENT_TASK_RUNTIME_TOOL_SCHEMA.to_string(),
            id: "fixture.mcp".to_string(),
            transport: "stdio".to_string(),
            executable: "/fixture-mcp".to_string(),
            argv: vec!["/fixture-mcp".to_string()],
            env: [("FIXTURE_MODE".to_string(), "private-resolved".to_string())]
                .into_iter()
                .collect(),
            version: None,
            capabilities: vec!["browser".to_string()],
            capability_probe: Some(crate::agent_task::AgentTaskRuntimeToolProbeEvidence {
                status: "succeeded".to_string(),
                argv: Vec::new(),
            }),
            env_names: vec!["FIXTURE_MODE".to_string()],
            secret_env_names: Vec::new(),
            readiness: crate::agent_task::ResolvedAgentTaskRuntimeToolReadiness {
                status: "ready".to_string(),
                evidence: Some(
                    crate::agent_task::ResolvedAgentTaskRuntimeToolReadinessEvidence {
                        kind: "declared_probe".to_string(),
                        success: true,
                    },
                ),
            },
            lifecycle: Default::default(),
        }];

        let value = redacted_request_value(&request, &RedactionPolicy::default());

        assert!(value.get("runtime_tools").is_none());
        assert_eq!(
            value["resolved_runtime_tools"][0]["env"]["FIXTURE_MODE"],
            "[redacted]"
        );
        assert!(!value.to_string().contains("private-"));
    }

    #[test]
    fn re_linking_does_not_duplicate_evidence_refs() {
        with_artifact_root(|_| {
            let request = executor_test_request();
            let mut outcome = test_outcome();
            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));
            let first = outcome.evidence_refs.len();
            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));
            assert_eq!(outcome.evidence_refs.len(), first);
        });
    }

    #[test]
    fn evidence_dir_is_stable_for_a_run_and_task_id() {
        with_artifact_root(|root| {
            let first = executor_evidence_dir(root, Some("run/attempt:1"), "task/with weird:chars");
            let second =
                executor_evidence_dir(root, Some("run/attempt:1"), "task/with weird:chars");
            assert_eq!(first, second);
            assert!(first
                .to_string_lossy()
                .contains("agent-task/executor-evidence"));
        });
    }

    #[test]
    fn evidence_dir_is_under_durable_artifact_root() {
        with_artifact_root(|artifact_root| {
            let path = executor_evidence_dir(artifact_root, Some("run-1"), "task-1");
            assert!(path.starts_with(artifact_root));
            assert!(!path
                .to_string_lossy()
                .contains("homeboy-agent-task-evidence"));
        });
    }

    #[test]
    fn repeated_child_runs_with_same_task_id_keep_distinct_evidence_paths() {
        with_artifact_root(|_| {
            let request = executor_test_request();
            let mut first_outcome = test_outcome();
            let mut second_outcome = test_outcome();

            link_latest_executor_evidence(
                &request,
                &mut first_outcome,
                Some("cook-homeboy-attempt-1-aaaa1111"),
            );
            link_latest_executor_evidence(
                &request,
                &mut second_outcome,
                Some("cook-homeboy-attempt-1-bbbb2222"),
            );

            let first_input = first_outcome
                .evidence_refs
                .iter()
                .find(|evidence| evidence.kind == EXECUTOR_INPUT_EVIDENCE_KIND)
                .expect("first executor input evidence");
            let second_input = second_outcome
                .evidence_refs
                .iter()
                .find(|evidence| evidence.kind == EXECUTOR_INPUT_EVIDENCE_KIND)
                .expect("second executor input evidence");

            assert_ne!(first_input.uri, second_input.uri);
            assert!(first_input.uri.contains("cook-homeboy-attempt-1-aaaa1111"));
            assert!(second_input.uri.contains("cook-homeboy-attempt-1-bbbb2222"));
            assert!(Path::new(first_input.uri.strip_prefix("file://").unwrap()).is_file());
            assert!(Path::new(second_input.uri.strip_prefix("file://").unwrap()).is_file());
        });
    }
}
