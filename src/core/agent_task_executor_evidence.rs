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

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::core::agent_task::{AgentTaskEvidenceRef, AgentTaskOutcome, AgentTaskRequest};
use crate::core::redaction::RedactionPolicy;

/// Evidence kind for the latest raw executor request (input piped to the
/// provider command). Surfaced as a first-class run evidence ref.
pub const EXECUTOR_INPUT_EVIDENCE_KIND: &str = "executor-input";

/// Evidence kind for the latest raw executor result (normalized provider
/// outcome). Surfaced as a first-class run evidence ref.
pub const EXECUTOR_RESULT_EVIDENCE_KIND: &str = "executor-result";

/// File name for the persisted latest raw executor request.
pub const EXECUTOR_INPUT_FILE: &str = "executor-input.json";

/// File name for the persisted latest raw executor result.
pub const EXECUTOR_RESULT_FILE: &str = "executor-result.json";

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
    request: &AgentTaskRequest,
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
}

/// Stable, per-run/per-task evidence directory under the system temp dir.
///
/// Recorded runs use their durable run id as the first path segment so repeated
/// fanout child cooks with the same task id keep distinct evidence files.
fn executor_evidence_dir(run_id: Option<&str>, task_id: &str) -> PathBuf {
    std::env::temp_dir()
        .join("homeboy-agent-task-evidence")
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
fn redacted_request_value(request: &AgentTaskRequest, policy: &RedactionPolicy) -> Value {
    match serde_json::to_value(request) {
        Ok(value) => policy.redact_json(&value),
        Err(error) => json!({
            "error": "failed to serialize executor request for evidence",
            "detail": error.to_string(),
            "task_id": request.task_id,
        }),
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
    use crate::core::agent_task::{
        AgentTaskComponentContract, AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcomeStatus,
        AgentTaskPolicy, AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
    };
    use serde_json::Map;
    use std::sync::Mutex;

    static TEMP_DIR_ENV_LOCK: Mutex<()> = Mutex::new(());

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
                load_as: None,
                activate: None,
                extra: Map::new(),
            }],
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: vec!["component_contracts".to_string()],
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        }
    }

    fn test_outcome() -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "neutral-runtime proof".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("token=abc done".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }

    fn with_temp_dir<R>(test: impl FnOnce() -> R) -> R {
        // Isolate writes to a unique temp dir without racing parallel tests that
        // also read `std::env::temp_dir()`.
        let _lock = TEMP_DIR_ENV_LOCK.lock().expect("temp dir env lock");
        let guard = tempfile::tempdir().expect("temp dir");
        let previous = std::env::var_os("TMPDIR");
        std::env::set_var("TMPDIR", guard.path());
        let result = test();
        match previous {
            Some(value) => std::env::set_var("TMPDIR", value),
            None => std::env::remove_var("TMPDIR"),
        }
        result
    }

    #[test]
    fn links_executor_input_and_result_evidence_refs() {
        with_temp_dir(|| {
            let request = test_request();
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
                assert!(Path::new(path).is_file(), "evidence file should exist");
            }
        });
    }

    #[test]
    fn persisted_input_redacts_secrets_but_retains_contracts_paths_and_artifacts() {
        with_temp_dir(|| {
            let request = test_request();
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
        });
    }

    #[test]
    fn re_linking_does_not_duplicate_evidence_refs() {
        with_temp_dir(|| {
            let request = test_request();
            let mut outcome = test_outcome();
            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));
            let first = outcome.evidence_refs.len();
            link_latest_executor_evidence(&request, &mut outcome, Some("run-1"));
            assert_eq!(outcome.evidence_refs.len(), first);
        });
    }

    #[test]
    fn evidence_dir_is_stable_for_a_run_and_task_id() {
        let first = executor_evidence_dir(Some("run/attempt:1"), "task/with weird:chars");
        let second = executor_evidence_dir(Some("run/attempt:1"), "task/with weird:chars");
        assert_eq!(first, second);
        assert!(first
            .to_string_lossy()
            .contains("homeboy-agent-task-evidence"));
    }

    #[test]
    fn repeated_child_runs_with_same_task_id_keep_distinct_evidence_paths() {
        with_temp_dir(|| {
            let request = test_request();
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
