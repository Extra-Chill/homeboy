//! Run evidence report shaping.
//!
//! Extracted from the `commands::runs::evidence` adapter so the stable
//! `runs evidence` report (metadata buckets, artifact index, heartbeat,
//! retention guidance, failure summary, evidence links, and embedded
//! evidence manifest) is owned by a reusable core service rather than the
//! CLI command module.
//!
//! The command adapter now only:
//!   * opens the store and resolves the run,
//!   * builds its `RunSummary` and disk-budget inputs, and
//!   * maps [`RunEvidenceReport`] into its `RunsOutput` enum.
//!
//! All artifact indexing, metadata bucketing, failure classification,
//! evidence-link derivation, and manifest resolution lives here. Output is
//! byte-for-byte equivalent to the previous inline command implementation.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::{run_owner_pid, running_status_note, ArtifactRecord, RunRecord};
use crate::core::artifact_address::{ArtifactAddress, ArtifactAddressKind};
use crate::core::artifact_preview::{html_preview_entrypoints, ArtifactPreviewEntrypoint};
use crate::core::artifact_ref::{ArtifactRef, EvidenceRef};
use crate::core::artifacts::{generic_matrix_summary_from_artifacts, GenericMatrixSummary};
use crate::core::evidence_manifest::{EvidenceManifest, TrackerRef, EVIDENCE_MANIFEST_SCHEMA};
use crate::core::observation::disk_budget::DiskBudget;

/// Default retention window (days) surfaced in evidence retention guidance.
pub const DEFAULT_RETENTION_DAYS: i64 = 30;

/// Fully shaped `runs evidence` report.
///
/// Generic over the run-summary type `S` so the command adapter can embed
/// its own `RunSummary` (which carries CLI-only enrichment) without leaking
/// that type into core. Serialization is identical regardless of `S`.
#[derive(Serialize)]
pub struct RunEvidenceReport<S: Serialize> {
    pub command: &'static str,
    pub run_id: String,
    pub run: S,
    pub homeboy_version: Option<String>,
    pub metadata: EvidenceMetadata,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracker_refs: Vec<TrackerRef>,
    pub heartbeat: EvidenceHeartbeat,
    pub artifact_index: EvidenceArtifactIndex,
    pub retention: EvidenceRetention,
    pub failure: EvidenceFailureSummary,
    pub disk_budget: DiskBudget,
    pub evidence_links: Vec<EvidenceLink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_task_lifecycle_event: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix_summary: Option<GenericMatrixSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_manifest: Option<EvidenceManifest>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence_manifest_errors: Vec<String>,
}

/// Command-supplied inputs for [`build_run_evidence_report`].
///
/// The run record, its enriched artifacts, and the artifact root are the only
/// observation-store reads a caller has to perform (via
/// [`crate::core::observation::runs_service`]); everything else in the report
/// is derived here. `command`, `run_summary`, and `disk_budget` stay
/// caller-owned so the CLI can embed its own enriched `RunSummary` and platform
/// disk probing without leaking those concerns into core.
pub struct RunEvidenceReportInputs<S: Serialize> {
    pub command: &'static str,
    pub run: RunRecord,
    pub run_summary: S,
    pub artifacts: Vec<ArtifactRecord>,
    pub artifact_root: PathBuf,
    pub disk_budget: DiskBudget,
}

/// Assemble the full, stable `runs evidence` report from a loaded run and its
/// enriched artifacts.
///
/// This is the single reusable surface for the evidence report: it composes
/// every metadata bucket, the artifact index, heartbeat, retention guidance,
/// failure summary, evidence links, lifecycle event, matrix summary, embedded
/// manifest, and tracker refs. Consumers outside the CLI (HTTP API, MCP,
/// automation) can build the same report without re-deriving the orchestration
/// from the `commands::runs` adapter. Output is byte-for-byte equivalent to the
/// previous inline command implementation.
pub fn build_run_evidence_report<S: Serialize>(
    inputs: RunEvidenceReportInputs<S>,
) -> RunEvidenceReport<S> {
    let RunEvidenceReportInputs {
        command,
        run,
        run_summary,
        artifacts,
        artifact_root,
        disk_budget,
    } = inputs;

    let metadata = evidence_metadata(&run.metadata_json);
    let artifact_index = evidence_artifact_index(&artifacts);
    let failure = evidence_failure_summary(&run);
    let retention = evidence_retention(&artifact_root, &run.id);
    let evidence_links = evidence_links(&artifacts);
    let agent_task_lifecycle_event = evidence_agent_task_lifecycle_event(&run.metadata_json);
    let matrix_summary = evidence_matrix_summary(&run, &artifacts);
    let (evidence_manifest, evidence_manifest_errors) = evidence_manifest(&run, &artifacts);
    let tracker_refs = evidence_tracker_refs(&run.metadata_json, evidence_manifest.as_ref());
    let stale_reason = running_status_note(&run);
    let heartbeat = EvidenceHeartbeat {
        status: run.status.clone(),
        stale: stale_reason.is_some(),
        stale_reason,
        owner_pid: run_owner_pid(&run),
        updated_at: run
            .finished_at
            .clone()
            .unwrap_or_else(|| run.started_at.clone()),
    };

    RunEvidenceReport {
        command,
        run_id: run.id.clone(),
        run: run_summary,
        homeboy_version: run.homeboy_version.clone(),
        metadata,
        tracker_refs,
        heartbeat,
        artifact_index,
        retention,
        failure,
        disk_budget,
        evidence_links,
        agent_task_lifecycle_event,
        matrix_summary,
        evidence_manifest,
        evidence_manifest_errors,
    }
}

#[derive(Serialize)]
pub struct EvidenceMetadata {
    pub cost: Value,
    pub timing: Value,
    pub version: Value,
    pub host: Value,
    pub runtime: Value,
}

#[derive(Serialize)]
pub struct EvidenceHeartbeat {
    pub status: String,
    pub stale: bool,
    pub stale_reason: Option<String>,
    pub owner_pid: Option<u32>,
    pub updated_at: String,
}

#[derive(Serialize)]
pub struct EvidenceArtifactIndex {
    pub count: usize,
    pub file_count: usize,
    pub directory_count: usize,
    pub url_count: usize,
    pub missing_count: usize,
    pub total_size_bytes: u64,
    pub artifacts: Vec<EvidenceArtifact>,
}

#[derive(Serialize)]
pub struct EvidenceArtifact {
    #[serde(rename = "ref")]
    pub reference: ArtifactRef,
    pub id: String,
    pub kind: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub path: String,
    pub address: ArtifactAddress,
    pub url: Option<String>,
    pub public: bool,
    pub public_url: Option<String>,
    pub relative_to: Option<String>,
    pub fetch_command: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub created_at: String,
    pub exists: bool,
    pub retention_candidate: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview_entrypoints: Vec<ArtifactPreviewEntrypoint>,
}

#[derive(Serialize)]
pub struct EvidenceRetention {
    pub artifact_root: String,
    pub default_retention_days: i64,
    pub cleanup_command: String,
}

#[derive(Serialize)]
pub struct EvidenceFailureSummary {
    pub failed: bool,
    pub status: String,
    pub exit_code: Option<i64>,
    pub error: Option<String>,
    pub failure: Value,
    pub gate_failures: Vec<String>,
    pub hints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_command_failures: Vec<Value>,
}

#[derive(Serialize)]
pub struct EvidenceLink {
    #[serde(rename = "ref")]
    pub reference: EvidenceRef,
    pub kind: String,
    pub target: String,
    pub label: String,
}

/// Build the metadata buckets surfaced by `runs evidence`.
pub fn evidence_metadata(metadata: &Value) -> EvidenceMetadata {
    EvidenceMetadata {
        cost: pick_metadata(metadata, &["cost", "costs", "usage", "token_usage"]),
        timing: pick_metadata(
            metadata,
            &[
                "timing",
                "timings",
                "duration",
                "scenario_metrics",
                "phase_events",
                "phase_summaries",
                "failure_classification",
            ],
        ),
        version: pick_metadata(metadata, &["version", "versions", "homeboy_version"]),
        host: pick_metadata(
            metadata,
            &["host", "hostname", "machine", "resource_policy"],
        ),
        runtime: pick_metadata(metadata, &["runtime", "runner", "ci_context", "rig_state"]),
    }
}

pub fn evidence_agent_task_lifecycle_event(metadata: &Value) -> Option<Value> {
    agent_task_lifecycle_event_value(metadata).cloned()
}

pub fn evidence_tracker_refs(
    metadata: &Value,
    manifest: Option<&EvidenceManifest>,
) -> Vec<TrackerRef> {
    let mut refs = metadata_tracker_refs(metadata);
    if let Some(manifest) = manifest {
        refs.extend(manifest.tracker_refs.clone());
    }
    refs
}

fn metadata_tracker_refs(metadata: &Value) -> Vec<TrackerRef> {
    metadata
        .get("tracker_refs")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<TrackerRef>>(value).ok())
        .unwrap_or_default()
}

fn agent_task_lifecycle_event_value(value: &Value) -> Option<&Value> {
    if value.get("schema").and_then(Value::as_str)
        == Some("homeboy/agent-task-run-plan-lifecycle-event/v1")
    {
        return Some(value);
    }
    if let Some(event) = value
        .get("agent_task_lifecycle_event")
        .and_then(agent_task_lifecycle_event_value)
    {
        return Some(event);
    }
    if let Some(event) = value.get("data").and_then(agent_task_lifecycle_event_value) {
        return Some(event);
    }
    value
        .get("lab")
        .and_then(|lab| lab.get("remote_events"))
        .and_then(Value::as_array)
        .and_then(|events| {
            events
                .iter()
                .rev()
                .filter_map(|event| event.get("data"))
                .find_map(agent_task_lifecycle_event_value)
        })
}

fn pick_metadata(metadata: &Value, keys: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for key in keys {
        if let Some(value) = metadata.get(*key) {
            out.insert((*key).to_string(), value.clone());
        }
    }
    Value::Object(out)
}

/// Build the stable artifact index for `runs evidence`.
pub fn evidence_artifact_index(artifacts: &[ArtifactRecord]) -> EvidenceArtifactIndex {
    let mut file_count = 0;
    let mut directory_count = 0;
    let mut url_count = 0;
    let mut missing_count = 0;
    let mut total_size_bytes = 0u64;
    let artifacts = artifacts
        .iter()
        .map(|artifact| {
            let address = ArtifactAddress::from_record(artifact);
            let reference = artifact_ref(artifact, &address);
            let public_url = public_url_from_address(&address);
            let exists = artifact_exists(artifact);
            if !exists {
                missing_count += 1;
            }
            match artifact.artifact_type.as_str() {
                "file" => file_count += 1,
                "directory" => directory_count += 1,
                "url" => url_count += 1,
                _ => {}
            }
            let size = artifact_size_bytes(artifact);
            total_size_bytes = total_size_bytes.saturating_add(size);
            let preview_entrypoints = html_preview_entrypoints(artifact);
            EvidenceArtifact {
                id: reference.id.clone(),
                kind: reference.kind.clone(),
                artifact_type: reference.artifact_type.clone(),
                path: address.value.clone(),
                address,
                url: public_url.clone(),
                public: public_url.is_some(),
                public_url,
                relative_to: artifact_relative_to(artifact),
                fetch_command: artifact_fetch_command(artifact),
                size_bytes: artifact.size_bytes,
                sha256: artifact.sha256.clone(),
                created_at: artifact.created_at.clone(),
                exists,
                retention_candidate: artifact.artifact_type != "url",
                preview_entrypoints,
                reference,
            }
        })
        .collect::<Vec<_>>();

    EvidenceArtifactIndex {
        count: artifacts.len(),
        file_count,
        directory_count,
        url_count,
        missing_count,
        total_size_bytes,
        artifacts,
    }
}

fn artifact_ref(artifact: &ArtifactRecord, address: &ArtifactAddress) -> ArtifactRef {
    let mut reference = ArtifactRef::from_record(artifact);
    reference.path = address.value.clone();
    reference.url = public_url_from_address(address);
    reference.public_url = reference.url.clone();
    reference
}

fn public_url_from_address(address: &ArtifactAddress) -> Option<String> {
    (address.kind == ArtifactAddressKind::PublicUrl).then(|| address.value.clone())
}

fn artifact_relative_to(artifact: &ArtifactRecord) -> Option<String> {
    let address = ArtifactAddress::from_record(artifact);
    if address.reviewer_visible {
        return None;
    }
    if artifact.artifact_type == "file" || artifact.artifact_type == "remote_file" {
        return Some("homeboy observation artifact store".to_string());
    }
    artifact
        .metadata_json
        .get("source")
        .and_then(Value::as_str)
        .map(|source| format!("{source} metadata"))
}

fn artifact_fetch_command(artifact: &ArtifactRecord) -> Option<String> {
    if artifact.artifact_type == "file" || artifact.artifact_type == "remote_file" {
        return Some(format!(
            "homeboy runs artifact get {} {} -o <path>",
            artifact.run_id, artifact.id
        ));
    }
    None
}

fn artifact_exists(artifact: &ArtifactRecord) -> bool {
    if artifact.artifact_type == "url" {
        return true;
    }
    if artifact.artifact_type == "remote_file"
        || crate::core::runners::is_remote_runner_artifact_path(&artifact.path)
    {
        return true;
    }
    Path::new(&artifact.path).exists()
}

fn artifact_size_bytes(artifact: &ArtifactRecord) -> u64 {
    if let Some(size) = artifact
        .size_bytes
        .and_then(|size| u64::try_from(size).ok())
    {
        return size;
    }
    let path = Path::new(&artifact.path);
    if path.is_file() {
        return fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    }
    if path.is_dir() {
        return directory_size_bytes(path);
    }
    0
}

fn directory_size_bytes(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                directory_size_bytes(&path)
            } else {
                fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0)
            }
        })
        .sum()
}

/// Build the failure summary surfaced by `runs evidence`.
pub fn evidence_failure_summary(run: &RunRecord) -> EvidenceFailureSummary {
    let metadata = &run.metadata_json;
    let exit_code = metadata.get("exit_code").and_then(|value| value.as_i64());
    let error = metadata
        .get("error")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    EvidenceFailureSummary {
        failed: matches!(run.status.as_str(), "fail" | "failed" | "error" | "stale"),
        status: run.status.clone(),
        exit_code,
        error,
        failure: metadata.get("failure").cloned().unwrap_or(Value::Null),
        gate_failures: string_array(metadata.get("gate_failures")),
        hints: string_array(metadata.get("hints")),
        child_command_failures: child_command_failures(metadata),
    }
}

fn child_command_failures(metadata: &Value) -> Vec<Value> {
    metadata
        .get("child_command_failures")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Build the retention guidance block for `runs evidence`.
pub fn evidence_retention(artifact_root: &Path, run_id: &str) -> EvidenceRetention {
    EvidenceRetention {
        artifact_root: artifact_root.display().to_string(),
        default_retention_days: DEFAULT_RETENTION_DAYS,
        cleanup_command: format!(
            "homeboy runs artifact cleanup-persisted --run-id {run_id} --older-than-days {DEFAULT_RETENTION_DAYS}"
        ),
    }
}

/// Build the evidence-link list (reviewer-visible artifact targets).
pub fn evidence_links(artifacts: &[ArtifactRecord]) -> Vec<EvidenceLink> {
    artifacts
        .iter()
        .filter_map(|artifact| {
            let address = ArtifactAddress::from_record(artifact);
            let target = address.reviewer_target()?;
            let mut reference = EvidenceRef::new(&artifact.kind, target, &artifact.kind);
            reference.artifact = Some(artifact_ref(artifact, &address));
            Some(EvidenceLink {
                kind: reference.kind.clone(),
                target: reference.target.clone(),
                label: reference.label.clone(),
                reference,
            })
        })
        .collect()
}

/// Resolve a generic matrix dashboard summary from typed JSON artifacts.
pub fn evidence_matrix_summary(
    run: &RunRecord,
    artifacts: &[ArtifactRecord],
) -> Option<GenericMatrixSummary> {
    generic_matrix_summary_from_artifacts(&run.id, artifacts)
}

/// Resolve an embedded evidence manifest from run metadata or artifacts.
///
/// Returns the parsed manifest (if any) plus any non-fatal parse errors
/// encountered while resolving candidates, preserving the original error
/// message format.
pub fn evidence_manifest(
    run: &RunRecord,
    artifacts: &[ArtifactRecord],
) -> (Option<EvidenceManifest>, Vec<String>) {
    let mut errors = Vec::new();
    if let Some(value) = run.metadata_json.get("evidence_manifest") {
        match EvidenceManifest::parse_value(value.clone()) {
            Ok(manifest) => return (Some(manifest), errors),
            Err(err) => errors.push(format!("metadata.evidence_manifest: {err}")),
        }
    }

    for artifact in artifacts {
        if !is_evidence_manifest_artifact(artifact) {
            continue;
        }
        let value = match fs::read_to_string(&artifact.path)
            .map_err(|err| err.to_string())
            .and_then(|body| serde_json::from_str::<Value>(&body).map_err(|err| err.to_string()))
        {
            Ok(value) => value,
            Err(err) => {
                errors.push(format!("artifact.{}: {err}", artifact.id));
                continue;
            }
        };
        match EvidenceManifest::parse_value(value) {
            Ok(manifest) => return (Some(manifest), errors),
            Err(err) => errors.push(format!("artifact.{}: {err}", artifact.id)),
        }
    }

    (None, errors)
}

fn is_evidence_manifest_artifact(artifact: &ArtifactRecord) -> bool {
    artifact.kind == "evidence_manifest"
        || artifact.metadata_json.get("schema").and_then(Value::as_str)
            == Some(EVIDENCE_MANIFEST_SCHEMA)
}

#[cfg(test)]
mod tests {
    //! Coverage for the reusable full-report builder. The CLI adapter in
    //! `commands::runs::evidence` keeps the integration coverage (JSON shape,
    //! manifest resolution, links); here we prove the standalone composition
    //! surface assembles the same fields so non-CLI consumers can rely on it.

    use super::*;
    use crate::core::observation::disk_budget::DiskBudget;

    fn sample_run() -> RunRecord {
        RunRecord {
            id: "run-1".to_string(),
            kind: "trace".to_string(),
            component_id: Some("homeboy".to_string()),
            started_at: "2026-06-12T00:00:00Z".to_string(),
            finished_at: Some("2026-06-12T00:01:00Z".to_string()),
            status: "pass".to_string(),
            command: Some("homeboy trace".to_string()),
            cwd: None,
            homeboy_version: Some("test-version".to_string()),
            git_sha: None,
            rig_id: None,
            metadata_json: Value::Null,
        }
    }

    fn url_artifact() -> ArtifactRecord {
        ArtifactRecord {
            id: "frontend_url".to_string(),
            run_id: "run-1".to_string(),
            kind: "frontend_url".to_string(),
            artifact_type: "url".to_string(),
            path: "https://example.test/".to_string(),
            url: Some("https://example.test/".to_string()),
            public_url: Some("https://example.test/".to_string()),
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json: Value::Null,
            created_at: "2026-06-12T00:00:30Z".to_string(),
        }
    }

    fn sample_disk_budget() -> DiskBudget {
        DiskBudget {
            path: "/tmp".to_string(),
            available_bytes: None,
            total_bytes: None,
            used_percent: None,
            status: "unavailable".to_string(),
            warning: None,
        }
    }

    #[test]
    fn build_run_evidence_report_composes_stable_fields() {
        let run = sample_run();
        let report = build_run_evidence_report(RunEvidenceReportInputs {
            command: "runs.evidence",
            run: run.clone(),
            run_summary: serde_json::json!({ "id": run.id }),
            artifacts: vec![url_artifact()],
            artifact_root: PathBuf::from("/tmp/artifacts"),
            disk_budget: sample_disk_budget(),
        });

        assert_eq!(report.command, "runs.evidence");
        assert_eq!(report.run_id, "run-1");
        assert_eq!(report.homeboy_version.as_deref(), Some("test-version"));
        // Heartbeat derives from the run record: a finished, passing run is not
        // stale and reports its finished_at as updated_at.
        assert_eq!(report.heartbeat.status, "pass");
        assert!(!report.heartbeat.stale);
        assert!(report.heartbeat.stale_reason.is_none());
        assert_eq!(report.heartbeat.updated_at, "2026-06-12T00:01:00Z");
        // Artifact index reflects the single URL artifact.
        assert_eq!(report.artifact_index.count, 1);
        assert_eq!(report.artifact_index.url_count, 1);
        // Retention guidance embeds the run id and the default window.
        assert!(report.retention.cleanup_command.contains("run-1"));
        assert_eq!(report.retention.default_retention_days, DEFAULT_RETENTION_DAYS);
        // A passing run is not a failure.
        assert!(!report.failure.failed);
        assert_eq!(report.failure.status, "pass");
        // No manifest in metadata or artifacts means a clean (None, empty) result.
        assert!(report.evidence_manifest.is_none());
        assert!(report.evidence_manifest_errors.is_empty());
    }
}
