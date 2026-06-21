use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskOutcome,
    AgentTaskOutcomeStatus, AgentTaskRequest, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_OUTCOME_SCHEMA,
};
use crate::core::agent_task_provider::{
    role_aliases_for_executor, timeout_artifact_discovery_for_executor, wildcard_match,
    AgentTaskProviderArtifactPattern, AgentTaskProviderRoleAliases,
    AgentTaskProviderTimeoutArtifactDiscovery,
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
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Default)]
pub(crate) struct TimeoutArtifactDiscovery {
    pub(crate) artifacts: Vec<AgentTaskArtifact>,
    pub(crate) evidence_refs: Vec<AgentTaskEvidenceRef>,
    pub(crate) diagnostics: Vec<AgentTaskDiagnostic>,
    pub(crate) outcome: Option<AgentTaskOutcome>,
}

impl TimeoutArtifactDiscovery {
    pub(crate) fn discover(request: &AgentTaskRequest) -> Self {
        let role_aliases = role_aliases_for_executor(
            &request.executor.backend,
            request.executor.selector.as_deref(),
        );
        let timeout_discovery = timeout_artifact_discovery_for_executor(
            &request.executor.backend,
            request.executor.selector.as_deref(),
        );
        Self::discover_with_contract(request, &role_aliases, &timeout_discovery)
    }

    fn discover_with_contract(
        request: &AgentTaskRequest,
        role_aliases: &AgentTaskProviderRoleAliases,
        timeout_discovery: &AgentTaskProviderTimeoutArtifactDiscovery,
    ) -> Self {
        let mut discovery = Self::default();
        for path in artifact_discovery_paths(request, timeout_discovery) {
            discovery.scan_path(&path, request, role_aliases, timeout_discovery);
        }
        discovery
    }

    #[cfg(test)]
    fn discover_with_role_aliases(
        request: &AgentTaskRequest,
        role_aliases: &AgentTaskProviderRoleAliases,
    ) -> Self {
        Self::discover_with_contract(
            request,
            role_aliases,
            &AgentTaskProviderTimeoutArtifactDiscovery::default(),
        )
    }

    pub(crate) fn has_runtime_evidence(&self) -> bool {
        self.runtime_evidence_count() > 0
    }

    fn scan_path(
        &mut self,
        path: &Path,
        request: &AgentTaskRequest,
        role_aliases: &AgentTaskProviderRoleAliases,
        timeout_discovery: &AgentTaskProviderTimeoutArtifactDiscovery,
    ) {
        let Ok(metadata) = fs::metadata(path) else {
            return;
        };

        if metadata.is_file() {
            self.record_file(path, request, role_aliases, timeout_discovery);
            return;
        }

        if !metadata.is_dir() {
            return;
        }

        let runtime_evidence_count_before = self.runtime_evidence_count();
        self.scan_directory_files(path, request, role_aliases, timeout_discovery, 0, &mut 0);
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
            label: None,
            role: None,
            semantic_key: None,
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

    fn record_file(
        &mut self,
        path: &Path,
        request: &AgentTaskRequest,
        role_aliases: &AgentTaskProviderRoleAliases,
        timeout_discovery: &AgentTaskProviderTimeoutArtifactDiscovery,
    ) {
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

        let Some((kind, mime, metadata)) =
            artifact_shape_from_path(path, role_aliases, timeout_discovery)
        else {
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
            label: None,
            role: None,
            semantic_key: None,
            path: Some(path.display().to_string()),
            url: None,
            mime,
            size_bytes,
            sha256,
            metadata,
        });
    }

    fn scan_directory_files(
        &mut self,
        path: &Path,
        request: &AgentTaskRequest,
        role_aliases: &AgentTaskProviderRoleAliases,
        timeout_discovery: &AgentTaskProviderTimeoutArtifactDiscovery,
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
                self.record_file(&child, request, role_aliases, timeout_discovery);
            } else if child_metadata.is_dir() {
                let runtime_evidence_count_before = self.runtime_evidence_count();
                self.scan_directory_files(
                    &child,
                    request,
                    role_aliases,
                    timeout_discovery,
                    depth + 1,
                    visited,
                );
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
        && !artifact_has_empty_sha(artifact)
        && artifact.metadata.get("actionable").and_then(Value::as_bool) != Some(false)
}

pub(crate) fn is_empty_patch_artifact(artifact: &AgentTaskArtifact) -> bool {
    artifact_has_patch_shape(artifact)
        && (!artifact_has_content(artifact) || artifact_has_empty_sha(artifact))
}

fn artifact_has_patch_shape(artifact: &AgentTaskArtifact) -> bool {
    artifact.kind == "patch"
        || artifact.kind == "diff"
        || artifact.mime.as_deref() == Some("text/x-patch")
        || artifact.mime.as_deref() == Some("text/x-diff")
        || artifact.metadata.get("role").and_then(Value::as_str) == Some("patch")
}

fn artifact_has_empty_sha(artifact: &AgentTaskArtifact) -> bool {
    artifact.sha256.as_deref() == Some(EMPTY_SHA256) || artifact.sha256.as_deref() == Some("")
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

fn artifact_discovery_paths(
    request: &AgentTaskRequest,
    timeout_discovery: &AgentTaskProviderTimeoutArtifactDiscovery,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_artifact_paths_from_value(&request.metadata, default_path_keys(), &mut paths);
    collect_artifact_paths_from_value(&request.executor.config, default_path_keys(), &mut paths);
    collect_artifact_paths_from_value(
        &request.metadata,
        &timeout_discovery.metadata_path_keys,
        &mut paths,
    );
    collect_artifact_paths_from_value(
        &request.executor.config,
        &timeout_discovery.config_path_keys,
        &mut paths,
    );
    paths.extend(timeout_discovery.paths.iter().map(PathBuf::from));
    for expected in &request.expected_artifacts {
        paths.push(PathBuf::from(expected));
    }
    for declaration in &request.artifact_declarations {
        if let Some(path) = declaration.path.as_deref() {
            paths.push(PathBuf::from(path));
        }
    }
    paths
}

fn default_path_keys() -> &'static [&'static str] {
    &[
        "artifact_root",
        "artifact_path",
        "outcome_path",
        "agent_result_path",
        "artifact_roots",
        "artifact_paths",
        "outcome_paths",
        "agent_result_paths",
    ]
}

fn collect_artifact_paths_from_value(
    value: &Value,
    keys: &[impl AsRef<str>],
    paths: &mut Vec<PathBuf>,
) {
    for key in keys {
        let key = key.as_ref();
        if let Some(path) = value.get(key).and_then(Value::as_str) {
            paths.push(PathBuf::from(path));
        }
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

fn artifact_shape_from_path(
    path: &Path,
    role_aliases: &AgentTaskProviderRoleAliases,
    timeout_discovery: &AgentTaskProviderTimeoutArtifactDiscovery,
) -> Option<(String, Option<String>, Value)> {
    if let Some(pattern) = timeout_discovery
        .artifact_patterns
        .iter()
        .find(|pattern| artifact_pattern_matches(pattern, path))
    {
        return Some((
            pattern.kind.clone(),
            pattern.mime.clone().or_else(|| mime_from_path(path)),
            merge_artifact_metadata(pattern.metadata.clone()),
        ));
    }

    artifact_kind_from_path(path, role_aliases).map(|kind| {
        (
            kind,
            mime_from_path(path),
            serde_json::json!({ "discovered_from": "timeout_artifact_scan" }),
        )
    })
}

fn artifact_kind_from_path(
    path: &Path,
    role_aliases: &AgentTaskProviderRoleAliases,
) -> Option<String> {
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

    for role in [
        "patch",
        "preflight_evidence",
        "runtime_bundle",
        "agent_result",
    ] {
        if role_aliases.artifact_filename_matches_role(role, &file_name)
            || role_aliases.artifact_kind_matches_role(role, &file_name)
        {
            return Some(role.to_string());
        }
    }

    None
}

fn artifact_pattern_matches(pattern: &AgentTaskProviderArtifactPattern, path: &Path) -> bool {
    let Some(file_name) = path
        .file_name()
        .map(|file_name| file_name.to_string_lossy().to_ascii_lowercase())
    else {
        return false;
    };
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    pattern
        .filename_patterns
        .iter()
        .any(|candidate| wildcard_match(&candidate.to_ascii_lowercase(), &file_name))
        || pattern
            .filename_contains
            .iter()
            .any(|candidate| file_name.contains(&candidate.to_ascii_lowercase()))
        || pattern
            .extensions
            .iter()
            .map(|candidate| candidate.trim_start_matches('.').to_ascii_lowercase())
            .any(|candidate| candidate == extension)
}

fn merge_artifact_metadata(metadata: Value) -> Value {
    match metadata {
        Value::Object(mut object) => {
            object.insert(
                "discovered_from".to_string(),
                Value::String("timeout_artifact_scan".to_string()),
            );
            Value::Object(object)
        }
        _ => serde_json::json!({ "discovered_from": "timeout_artifact_scan" }),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use serde_json::{json, Value};

    #[test]
    fn empty_runtime_bundle_preserves_preflight_without_runtime_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_root = temp.path().join("task-1-artifacts");
        let empty_runtime = artifact_root.join("runtime-mpxgndju-f4v9yn");
        fs::create_dir_all(&empty_runtime).expect("empty runtime bundle");
        let preflight_path = artifact_root.join("sample-runtime-preflight.json");
        fs::write(&preflight_path, r#"{"runner":"sample-runtime"}"#).expect("preflight evidence");

        let discovery = TimeoutArtifactDiscovery::discover_with_role_aliases(
            &test_request(json!({
                "artifact_root": artifact_root,
            })),
            &role_aliases(json!({
                "artifact_filenames": {
                    "preflight_evidence": ["sample-runtime-preflight.json"]
                }
            })),
        );

        assert!(!discovery.has_runtime_evidence());
        assert!(discovery.artifacts.iter().any(|artifact| {
            artifact.kind == "preflight_evidence"
                && artifact.path.as_deref() == Some(&preflight_path.to_string_lossy())
        }));
        assert!(discovery.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "empty_runtime_bundle"
                && diagnostic.data.get("path").and_then(Value::as_str)
                    == Some(&empty_runtime.to_string_lossy())
        }));
    }

    #[test]
    fn provider_declared_filename_pattern_maps_to_generic_role() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_root = temp.path().join("task-1-artifacts");
        fs::create_dir_all(&artifact_root).expect("artifact root");
        let evidence_path = artifact_root.join("provider-preflight-evidence.json");
        fs::write(&evidence_path, r#"{"provider":"custom"}"#).expect("preflight evidence");

        let discovery = TimeoutArtifactDiscovery::discover_with_role_aliases(
            &test_request(json!({
                "artifact_root": artifact_root,
            })),
            &role_aliases(json!({
                "artifact_filenames": {
                    "preflight_evidence": ["*-preflight-evidence.json"]
                }
            })),
        );

        assert!(discovery.artifacts.iter().any(|artifact| {
            artifact.kind == "preflight_evidence"
                && artifact.path.as_deref() == Some(&evidence_path.to_string_lossy())
        }));
    }

    #[test]
    fn provider_timeout_contract_adds_discovery_paths_and_typed_artifact_patterns() {
        let temp = tempfile::tempdir().expect("tempdir");
        let provider_root = temp.path().join("provider-artifacts");
        fs::create_dir_all(&provider_root).expect("provider artifact root");
        let metrics_path = provider_root.join("worker-metrics.ndjson");
        fs::write(&metrics_path, r#"{"tokens":12}"#).expect("metrics artifact");

        let timeout_discovery: AgentTaskProviderTimeoutArtifactDiscovery =
            serde_json::from_value(json!({
                "config_path_keys": ["provider_artifact_root"],
                "artifact_patterns": [
                    {
                        "kind": "metrics",
                        "filename_patterns": ["*-metrics.ndjson"],
                        "mime": "application/x-ndjson",
                        "metadata": { "role": "telemetry" }
                    }
                ]
            }))
            .expect("timeout discovery contract");

        let mut request = test_request(json!({}));
        request.executor.config = json!({
            "provider_artifact_root": provider_root,
        });

        let discovery = TimeoutArtifactDiscovery::discover_with_contract(
            &request,
            &AgentTaskProviderRoleAliases::default(),
            &timeout_discovery,
        );

        let artifact = discovery
            .artifacts
            .iter()
            .find(|artifact| artifact.path.as_deref() == Some(&metrics_path.to_string_lossy()))
            .expect("typed metrics artifact");
        assert_eq!(artifact.kind, "metrics");
        assert_eq!(artifact.mime.as_deref(), Some("application/x-ndjson"));
        assert_eq!(
            artifact.metadata.get("role").and_then(Value::as_str),
            Some("telemetry")
        );
        assert_eq!(
            artifact
                .metadata
                .get("discovered_from")
                .and_then(Value::as_str),
            Some("timeout_artifact_scan")
        );
    }

    fn test_request(metadata: Value) -> AgentTaskRequest {
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-1".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "test".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata,
        }
    }

    fn role_aliases(value: Value) -> AgentTaskProviderRoleAliases {
        serde_json::from_value(value).expect("role aliases")
    }
}
