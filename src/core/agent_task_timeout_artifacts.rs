use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskOutcome,
    AgentTaskOutcomeStatus, AgentTaskRequest, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_OUTCOME_SCHEMA,
};

const EXPECTED_RUNTIME_EVIDENCE_FILES: &[&str] = &[
    "transcript.json",
    "agent-result.json",
    "agent_result.json",
    "patch.diff",
    "patch.patch",
    "*.log",
    "*.txt",
];

#[derive(Default)]
pub(crate) struct TimeoutArtifactDiscovery {
    pub(crate) artifacts: Vec<AgentTaskArtifact>,
    pub(crate) evidence_refs: Vec<AgentTaskEvidenceRef>,
    pub(crate) diagnostics: Vec<AgentTaskDiagnostic>,
    pub(crate) outcome: Option<AgentTaskOutcome>,
}

impl TimeoutArtifactDiscovery {
    pub(crate) fn discover(request: &AgentTaskRequest) -> Self {
        let mut discovery = Self::default();
        for path in artifact_discovery_paths(request) {
            discovery.scan_path(&path, request);
        }
        discovery
    }

    pub(crate) fn has_runtime_evidence(&self) -> bool {
        self.runtime_evidence_count() > 0
    }

    fn scan_path(&mut self, path: &Path, request: &AgentTaskRequest) {
        let Ok(metadata) = fs::metadata(path) else {
            return;
        };

        if metadata.is_file() {
            self.record_file(path, request);
            return;
        }

        if !metadata.is_dir() {
            return;
        }

        let runtime_evidence_count_before = self.runtime_evidence_count();
        self.scan_directory_files(path, request, 0, &mut 0);
        self.record_directory_if_useful(
            path,
            self.runtime_evidence_count() > runtime_evidence_count_before,
        );
    }

    fn runtime_evidence_count(&self) -> usize {
        self.artifacts
            .iter()
            .filter(|artifact| artifact.kind != "preflight_evidence")
            .count()
            + self.evidence_refs.len()
            + usize::from(self.outcome.is_some())
    }

    fn record_directory_if_useful(&mut self, path: &Path, has_runtime_evidence: bool) {
        let Some(id) = artifact_id_from_path(path) else {
            return;
        };
        if !has_runtime_evidence {
            self.record_empty_runtime_bundle(path);
            return;
        }

        self.artifacts.push(AgentTaskArtifact {
            schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id,
            kind: "runtime_bundle".to_string(),
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().to_string()),
            path: Some(path.display().to_string()),
            url: None,
            mime: None,
            size_bytes: None,
            sha256: None,
            metadata: serde_json::json!({ "discovered_from": "timeout_artifact_scan" }),
        });
    }

    fn record_empty_runtime_bundle(&mut self, path: &Path) {
        if !is_runtime_bundle_dir(path) {
            return;
        }
        self.diagnostics.push(AgentTaskDiagnostic {
            class: "empty_runtime_bundle".to_string(),
            message: "timeout artifact scan found an empty runtime bundle directory".to_string(),
            data: serde_json::json!({
                "path": path.display().to_string(),
                "missing_expected_files": EXPECTED_RUNTIME_EVIDENCE_FILES,
            }),
        });
    }

    fn record_file(&mut self, path: &Path, request: &AgentTaskRequest) {
        if let Some(outcome) = read_discovered_outcome(path, request) {
            append_unique_artifacts(&mut self.artifacts, outcome.artifacts.clone());
            append_unique_evidence_refs(&mut self.evidence_refs, outcome.evidence_refs.clone());
            self.evidence_refs.push(AgentTaskEvidenceRef {
                kind: "agent_result".to_string(),
                uri: path.display().to_string(),
                label: Some("discovered agent result".to_string()),
            });
            self.outcome = Some(outcome);
            return;
        }

        let Some(kind) = artifact_kind_from_path(path) else {
            return;
        };
        let Some(id) = artifact_id_from_path(path) else {
            return;
        };
        let (size_bytes, sha256) = file_size_and_sha256(path);
        self.artifacts.push(AgentTaskArtifact {
            schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id,
            kind,
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().to_string()),
            path: Some(path.display().to_string()),
            url: None,
            mime: mime_from_path(path),
            size_bytes,
            sha256,
            metadata: serde_json::json!({ "discovered_from": "timeout_artifact_scan" }),
        });
    }

    fn scan_directory_files(
        &mut self,
        path: &Path,
        request: &AgentTaskRequest,
        depth: usize,
        visited: &mut usize,
    ) {
        if depth > 3 || *visited >= 500 {
            return;
        }

        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            if *visited >= 500 {
                return;
            }
            *visited += 1;
            let child = entry.path();
            let Ok(child_metadata) = entry.metadata() else {
                continue;
            };
            if child_metadata.is_file() {
                self.record_file(&child, request);
            } else if child_metadata.is_dir() {
                let runtime_evidence_count_before = self.runtime_evidence_count();
                self.scan_directory_files(&child, request, depth + 1, visited);
                self.record_directory_if_useful(
                    &child,
                    self.runtime_evidence_count() > runtime_evidence_count_before,
                );
            }
        }
    }
}

pub(crate) fn merge_timeout_outcome(base: &mut AgentTaskOutcome, discovered: AgentTaskOutcome) {
    append_unique_artifacts(&mut base.artifacts, discovered.artifacts);
    append_unique_evidence_refs(&mut base.evidence_refs, discovered.evidence_refs);
    if discovered
        .metadata
        .get("actionable")
        .and_then(Value::as_bool)
        != Some(false)
        && matches!(
            discovered.status,
            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
        )
    {
        base.status = discovered.status;
        base.failure_classification = discovered.failure_classification;
        base.summary = discovered.summary.or_else(|| base.summary.clone());
        base.workflow = discovered.workflow.or_else(|| base.workflow.clone());
        base.follow_up = discovered.follow_up.or_else(|| base.follow_up.clone());
        base.metadata = discovered.metadata;
    }
    base.diagnostics.extend(discovered.diagnostics);
}

pub(crate) fn append_unique_artifacts(
    target: &mut Vec<AgentTaskArtifact>,
    artifacts: Vec<AgentTaskArtifact>,
) {
    for artifact in artifacts {
        let duplicate = target.iter().any(|existing| {
            existing.id == artifact.id
                || (existing.path.is_some() && existing.path == artifact.path)
                || (existing.url.is_some() && existing.url == artifact.url)
        });
        if !duplicate {
            target.push(artifact);
        }
    }
}

pub(crate) fn append_unique_evidence_refs(
    target: &mut Vec<AgentTaskEvidenceRef>,
    evidence_refs: Vec<AgentTaskEvidenceRef>,
) {
    for evidence_ref in evidence_refs {
        if !target
            .iter()
            .any(|existing| existing.kind == evidence_ref.kind && existing.uri == evidence_ref.uri)
        {
            target.push(evidence_ref);
        }
    }
}

pub(crate) fn is_actionable_patch_artifact(artifact: &AgentTaskArtifact) -> bool {
    artifact_has_patch_shape(artifact)
        && artifact_has_content(artifact)
        && artifact.metadata.get("actionable").and_then(Value::as_bool) != Some(false)
}

fn artifact_has_patch_shape(artifact: &AgentTaskArtifact) -> bool {
    artifact.kind == "patch"
        || artifact.kind == "diff"
        || artifact.mime.as_deref() == Some("text/x-patch")
        || artifact.mime.as_deref() == Some("text/x-diff")
        || artifact.metadata.get("role").and_then(Value::as_str) == Some("patch")
}

fn artifact_has_content(artifact: &AgentTaskArtifact) -> bool {
    if artifact.size_bytes == Some(0) {
        return false;
    }

    artifact
        .path
        .as_deref()
        .and_then(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(true)
}

fn artifact_discovery_paths(request: &AgentTaskRequest) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_artifact_paths_from_value(&request.metadata, &mut paths);
    collect_artifact_paths_from_value(&request.executor.config, &mut paths);
    for expected in &request.expected_artifacts {
        paths.push(PathBuf::from(expected));
    }
    paths
}

fn collect_artifact_paths_from_value(value: &Value, paths: &mut Vec<PathBuf>) {
    for key in [
        "artifact_root",
        "artifact_path",
        "outcome_path",
        "agent_result_path",
    ] {
        if let Some(path) = value.get(key).and_then(Value::as_str) {
            paths.push(PathBuf::from(path));
        }
    }

    for key in [
        "artifact_roots",
        "artifact_paths",
        "outcome_paths",
        "agent_result_paths",
    ] {
        if let Some(values) = value.get(key).and_then(Value::as_array) {
            paths.extend(values.iter().filter_map(Value::as_str).map(PathBuf::from));
        }
    }
}

fn read_discovered_outcome(path: &Path, request: &AgentTaskRequest) -> Option<AgentTaskOutcome> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
        return None;
    }
    let raw = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let mut outcome: AgentTaskOutcome = serde_json::from_str(&raw).ok()?;
    if outcome.task_id != request.task_id {
        return None;
    }
    if outcome.schema != AGENT_TASK_OUTCOME_SCHEMA {
        outcome.schema = AGENT_TASK_OUTCOME_SCHEMA.to_string();
    }
    if outcome.metadata.get("actionable").is_none() {
        if let Some(actionable) = value.get("actionable").and_then(Value::as_bool) {
            outcome.metadata = merge_outcome_metadata_actionable(outcome.metadata, actionable);
        }
    }
    Some(outcome)
}

fn merge_outcome_metadata_actionable(metadata: Value, actionable: bool) -> Value {
    match metadata {
        Value::Object(mut object) => {
            object.insert("actionable".to_string(), Value::Bool(actionable));
            Value::Object(object)
        }
        _ => serde_json::json!({ "actionable": actionable }),
    }
}

fn artifact_kind_from_path(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if matches!(extension.as_str(), "patch" | "diff") {
        return Some("patch".to_string());
    }
    if matches!(extension.as_str(), "zip" | "tar" | "gz" | "tgz") {
        return Some("runtime_bundle".to_string());
    }
    if file_name.contains("transcript") || matches!(extension.as_str(), "log" | "txt") {
        return Some("transcript".to_string());
    }
    if file_name.contains("agent-result") || file_name.contains("agent_result") {
        return Some("agent_result".to_string());
    }
    if file_name == "homeboy-codebox-task-runner.json" {
        return Some("preflight_evidence".to_string());
    }

    None
}

fn artifact_id_from_path(path: &Path) -> Option<String> {
    Some(
        path.file_stem()?
            .to_string_lossy()
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '-'
                }
            })
            .collect(),
    )
}

fn is_runtime_bundle_dir(path: &Path) -> bool {
    path.file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|name| name.starts_with("runtime-") || name.contains("runtime_bundle"))
}

fn mime_from_path(path: &Path) -> Option<String> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
    {
        "patch" => Some("text/x-patch".to_string()),
        "diff" => Some("text/x-diff".to_string()),
        "json" => Some("application/json".to_string()),
        "log" | "txt" => Some("text/plain".to_string()),
        "zip" => Some("application/zip".to_string()),
        _ => None,
    }
}

fn file_size_and_sha256(path: &Path) -> (Option<u64>, Option<String>) {
    let size_bytes = fs::metadata(path).ok().map(|metadata| metadata.len());
    let sha256 = fs::read(path).ok().map(|bytes| {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    });
    (size_bytes, sha256)
}
