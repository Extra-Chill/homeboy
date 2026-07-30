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

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{run_owner_pid, running_status_note, ArtifactRecord, RunRecord, RunStatus};
use crate::artifact_address::{ArtifactAddress, ArtifactAddressKind};
use crate::artifact_preview::{html_preview_entrypoints, ArtifactPreviewEntrypoint};
use crate::artifact_ref::{artifact_ref_from_record, ArtifactRef, EvidenceRef};
use crate::artifacts::{generic_matrix_summary_from_artifacts, GenericMatrixSummary};
use crate::evidence_manifest::{
    BlockingCondition, BlockingSeverity, EvidenceConfidence, EvidenceManifest,
    EvidenceManifestSource, EvidenceManifestState, RunRef, TrackerRef, EVIDENCE_MANIFEST_SCHEMA,
};
use crate::observation::disk_budget::DiskBudget;

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
    pub homeboy_provenance: EvidenceHomeboyProvenance,
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
    /// The run's interpretation contract: what the evidence means and what is
    /// blocking. Resolved from a producer-authored manifest when one is
    /// attached, otherwise derived from this run record. Always present — check
    /// `evidence_manifest.source` to tell an assertion from a derivation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_manifest: Option<EvidenceManifest>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence_manifest_errors: Vec<String>,
}

/// Command-supplied inputs for [`build_run_evidence_report`].
///
/// The run record, its enriched artifacts, and the artifact root are the only
/// observation-store reads a caller has to perform (via
/// [`crate::observation::runs_service`]); everything else in the report
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
    let homeboy_provenance = evidence_homeboy_provenance(&run);
    let agent_task_lifecycle_event = evidence_agent_task_lifecycle_event(&run.metadata_json);
    let matrix_summary = evidence_matrix_summary(&run, &artifacts);
    let (authored_manifest, evidence_manifest_errors) = evidence_manifest(&run, &artifacts);
    // Tracker refs stay derived from the *authored* manifest only. A derived
    // manifest copies this list, so folding it back in would double every ref.
    let tracker_refs = evidence_tracker_refs(&run.metadata_json, authored_manifest.as_ref());
    let stale_reason = running_status_note(&run);
    let evidence_manifest = Some(authored_manifest.unwrap_or_else(|| {
        derive_evidence_manifest(EvidenceManifestDerivation {
            run: &run,
            artifacts: artifacts.as_slice(),
            failure: &failure,
            evidence_links: evidence_links.as_slice(),
            tracker_refs: tracker_refs.as_slice(),
            stale_reason: stale_reason.as_deref(),
        })
    }));
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
        homeboy_provenance,
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
pub struct EvidenceHomeboyProvenance {
    pub schema: &'static str,
    pub identities: Vec<EvidenceHomeboyIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct EvidenceHomeboyIdentity {
    pub role: &'static str,
    pub owner: &'static str,
    pub source: &'static str,
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_job_id: Option<String>,
    pub purpose: &'static str,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory_publication: Option<DirectoryArtifactPublicationGuidance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview_entrypoints: Vec<ArtifactPreviewEntrypoint>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct DirectoryArtifactPublicationGuidance {
    pub status: String,
    pub note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
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
    /// Runner-owned terminal diagnostics projected into controller evidence.
    /// Missing for local and pre-projection records to preserve their schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_failure: Option<RunnerFailureEvidence>,
}

/// Versioned, controller-owned projection of a runner terminal failure.
/// Unknown or malformed legacy metadata is omitted from evidence rather than
/// making `runs evidence` unreadable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerFailureEvidence {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub exit_code: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    pub stderr_tail: String,
    /// Digest input is the original runner stderr bytes, before redaction.
    pub stderr_sha256: String,
    pub runner_id: String,
    pub runner_job_id: String,
    pub runner_job_logs_command: String,
    pub remote_command_result_command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_snapshot: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_materialization_plan: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_job_projection: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_record: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration_provenance: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<RunnerFailureArtifactRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerFailureArtifactRef {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub sha256: String,
    pub size_bytes: i64,
}

impl RunnerFailureEvidence {
    pub const SCHEMA: &'static str = "homeboy/runner-exec-failure-projection/v1";

    pub fn from_metadata(value: &Value) -> Option<Self> {
        let evidence: Self = serde_json::from_value(value.clone()).ok()?;
        (evidence.schema == Self::SCHEMA
            && !evidence.runner_id.is_empty()
            && !evidence.runner_job_id.is_empty()
            && evidence.stderr_sha256.len() == 64
            && evidence
                .stderr_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            && evidence.artifact_refs.iter().all(|artifact| {
                !artifact.id.is_empty()
                    && !artifact.path.is_empty()
                    && artifact.sha256.len() == 64
                    && artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && artifact.size_bytes >= 0
            }))
        .then_some(evidence)
    }
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

fn evidence_homeboy_provenance(run: &RunRecord) -> EvidenceHomeboyProvenance {
    let mut identities = vec![EvidenceHomeboyIdentity {
        role: "observation_run_binary",
        owner: "command_process_that_started_this_observation_run",
        source: "run.homeboy_version",
        version: run.homeboy_version.clone(),
        runner_id: None,
        runner_job_id: None,
        purpose: "Version recorded by the Homeboy process that created this observation run; in runner workflows this is the child command/run binary, not proof of the controller CLI, active daemon, or configured runner job binary.",
    }];

    if let Some((runner_id, runner_job_id, source)) = runner_job_context(&run.metadata_json) {
        identities.push(EvidenceHomeboyIdentity {
            role: "runner_job_handoff",
            owner: "runner_broker_or_lab_offload",
            source,
            version: None,
            runner_id: Some(runner_id),
            runner_job_id,
            purpose: "Runner job context associated with this observation run. Use runner status/job logs to compare controller_cli, active_daemon, and configured_job_binary identities for the same runner.",
        });
    }

    let warnings = if identities
        .iter()
        .any(|identity| identity.role == "runner_job_handoff")
    {
        vec!["Runner-backed evidence can involve separate controller_cli, active_daemon, configured_job_binary, and observation_run_binary Homeboy identities; do not interpret top-level homeboy_version as daemon or controller provenance.".to_string()]
    } else {
        Vec::new()
    };

    EvidenceHomeboyProvenance {
        schema: "homeboy/homeboy-provenance/v1",
        identities,
        warnings,
    }
}

fn runner_job_context(metadata: &Value) -> Option<(String, Option<String>, &'static str)> {
    if let Some(lab_offload) = metadata.get("lab_offload") {
        if let Some(runner_id) = string_field(lab_offload, "runner_id") {
            return Some((
                runner_id,
                string_field(lab_offload, "runner_job_id"),
                "metadata.lab_offload",
            ));
        }
    }

    if let Some(identity) = evidence_agent_task_lifecycle_event(metadata).and_then(|event| {
        event
            .get("identity")
            .and_then(|identity| identity.as_object())
            .cloned()
    }) {
        if let Some(runner_id) = identity.get("runner_id").and_then(Value::as_str) {
            return Some((
                runner_id.to_string(),
                identity
                    .get("runner_job_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                "metadata.lab.remote_events.agent_task_lifecycle_event.identity",
            ));
        }
    }

    None
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
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
            let directory_publication = directory_publication_guidance(artifact, &address);
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
                directory_publication,
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

pub fn directory_publication_guidance(
    artifact: &ArtifactRecord,
    address: &ArtifactAddress,
) -> Option<DirectoryArtifactPublicationGuidance> {
    if artifact.artifact_type != "directory" {
        return None;
    }

    if address.kind == ArtifactAddressKind::PublicUrl {
        return Some(DirectoryArtifactPublicationGuidance {
            status: "published".to_string(),
            note: "directory artifact is reviewer-facing through the configured public artifact base URL".to_string(),
            public_url: Some(address.value.clone()),
            command: None,
        });
    }

    if crate::execution_contract::is_remote_runner_artifact_path(&artifact.path) {
        return Some(unpublished_directory_guidance(
            "runner_resident",
            "directory artifact is runner-resident; mirror it to the controller artifact store before using it as review evidence",
            artifact,
        ));
    }

    if Path::new(&artifact.path).is_dir() {
        return Some(unpublished_directory_guidance(
            "mirrored",
            "directory artifact is mirrored in the operator-local Homeboy artifact store but is not a reviewer-facing URL",
            artifact,
        ));
    }

    Some(DirectoryArtifactPublicationGuidance {
        status: "not_publishable".to_string(),
        note: "directory artifact bytes are unavailable from this observation record".to_string(),
        public_url: None,
        command: None,
    })
}

fn unpublished_directory_guidance(
    status: &str,
    note: &str,
    artifact: &ArtifactRecord,
) -> DirectoryArtifactPublicationGuidance {
    DirectoryArtifactPublicationGuidance {
        status: status.to_string(),
        note: note.to_string(),
        public_url: None,
        command: public_base_configured()
            .then(|| format!("homeboy runs artifacts {} --pull", artifact.run_id)),
    }
}

fn public_base_configured() -> bool {
    std::env::var(crate::artifact_links::PUBLIC_ARTIFACT_BASE_URL_ENV)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn artifact_ref(artifact: &ArtifactRecord, address: &ArtifactAddress) -> ArtifactRef {
    let mut reference = artifact_ref_from_record(artifact);
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
        || crate::execution_contract::is_remote_runner_artifact_path(&artifact.path)
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
        runner_failure: metadata
            .pointer("/lab/failure")
            .and_then(RunnerFailureEvidence::from_metadata),
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

/// Resolve a **producer-authored** evidence manifest from run metadata or
/// artifacts.
///
/// Returns the parsed manifest (if any) plus any non-fatal parse errors
/// encountered while resolving candidates, preserving the original error
/// message format. `None` means no producer attached one — not that the run has
/// no interpretation; see [`derive_evidence_manifest`].
///
/// The resolved manifest's `source` is stamped from the path it was found on,
/// overwriting whatever the producer serialized. Provenance is the reader's to
/// state, not the producer's to claim.
pub fn evidence_manifest(
    run: &RunRecord,
    artifacts: &[ArtifactRecord],
) -> (Option<EvidenceManifest>, Vec<String>) {
    let mut errors = Vec::new();
    if let Some(value) = run.metadata_json.get("evidence_manifest") {
        match EvidenceManifest::parse_value(value.clone()) {
            Ok(mut manifest) => {
                manifest.source = Some(EvidenceManifestSource::RunMetadata);
                return (Some(manifest), errors);
            }
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
            Ok(mut manifest) => {
                manifest.source = Some(EvidenceManifestSource::Artifact);
                return (Some(manifest), errors);
            }
            Err(err) => errors.push(format!("artifact.{}: {err}", artifact.id)),
        }
    }

    (None, errors)
}

/// Inputs [`derive_evidence_manifest`] reads. Grouped rather than passed
/// positionally so adding a signal later is not a signature break for callers.
pub struct EvidenceManifestDerivation<'a> {
    pub run: &'a RunRecord,
    pub artifacts: &'a [ArtifactRecord],
    pub failure: &'a EvidenceFailureSummary,
    /// Reviewer-visible evidence targets. Emptiness is the signal that a run
    /// recorded artifacts nobody outside this machine can look at.
    pub evidence_links: &'a [EvidenceLink],
    pub tracker_refs: &'a [TrackerRef],
    /// Set when the run holds a `running` record whose owner is gone.
    pub stale_reason: Option<&'a str>,
}

/// Upper bound on derived blockers.
///
/// A manifest is meant to be lifted out of the report and carried around; a run
/// with a thousand gate failures must not turn it into an unbounded payload.
/// Truncation is announced in `interpretation.notes` rather than silent, and the
/// full lists stay available on the report's `failure` block.
const DERIVED_BLOCKING_CONDITION_LIMIT: usize = 20;

/// Compose an interpretation contract from a run record when no producer
/// attached one.
///
/// This is a mechanical reading, not a judgement, and it says so: the result
/// carries `source: derived`, and a consumer that must not act on Homeboy's own
/// reading of its own run can gate on
/// [`EvidenceManifestSource::is_authored`](crate::evidence_manifest::EvidenceManifestSource::is_authored).
///
/// The mapping is conservative in both directions. An unrecognized status label
/// becomes `unknown` rather than being guessed at, and a run recorded as passing
/// that nonetheless carries a critical blocker is reported as `blocked`: a
/// manifest that claims a pass while listing what stopped it is worse than no
/// manifest, because a consumer reads `status.state` and stops there.
pub fn derive_evidence_manifest(inputs: EvidenceManifestDerivation<'_>) -> EvidenceManifest {
    let EvidenceManifestDerivation {
        run,
        artifacts,
        failure,
        evidence_links,
        tracker_refs,
        stale_reason,
    } = inputs;

    let status = RunStatus::from_label(&run.status);
    let mut state = match status {
        Some(RunStatus::Running) => EvidenceManifestState::Pending,
        Some(RunStatus::Pass) => EvidenceManifestState::Passed,
        Some(RunStatus::Fail) | Some(RunStatus::Error) => EvidenceManifestState::Failed,
        Some(RunStatus::Stale) => EvidenceManifestState::Blocked,
        Some(RunStatus::Skipped) | None => EvidenceManifestState::Unknown,
    };
    let terminal = matches!(
        status,
        Some(RunStatus::Pass) | Some(RunStatus::Fail) | Some(RunStatus::Error)
    );

    let mut notes = Vec::new();
    let mut blocking_conditions = derived_blocking_conditions(
        run,
        failure,
        status,
        stale_reason,
        terminal,
        evidence_links.is_empty(),
    );
    if blocking_conditions.len() > DERIVED_BLOCKING_CONDITION_LIMIT {
        notes.push(format!(
            "{} more blocking conditions were omitted; read the run's failure block for the full list.",
            blocking_conditions.len() - DERIVED_BLOCKING_CONDITION_LIMIT
        ));
        blocking_conditions.truncate(DERIVED_BLOCKING_CONDITION_LIMIT);
    }

    if state == EvidenceManifestState::Passed
        && blocking_conditions
            .iter()
            .any(|condition| condition.severity == Some(BlockingSeverity::Critical))
    {
        state = EvidenceManifestState::Blocked;
        notes.push(
            "The run record reports a pass while also recording a critical blocker; the blocker wins."
                .to_string(),
        );
    }

    let confidence = if !terminal || artifacts.is_empty() {
        EvidenceConfidence::Low
    } else if evidence_links.is_empty() {
        EvidenceConfidence::Medium
    } else {
        EvidenceConfidence::High
    };

    notes.push(
        "Derived by Homeboy from the run record; no producer attached an evidence manifest."
            .to_string(),
    );
    if let Some(reason) = stale_reason {
        notes.push(reason.to_string());
    }
    if terminal && artifacts.is_empty() {
        notes.push("The run recorded no artifacts, so it produced nothing reviewable.".to_string());
    } else if terminal && evidence_links.is_empty() {
        notes.push(
            "Every recorded artifact is operator-local; fetch it with `homeboy runs artifact get` before citing it as evidence."
                .to_string(),
        );
    }
    notes.extend(failure.hints.iter().cloned());

    let mut manifest = EvidenceManifest::new(
        state,
        derived_summary(run, failure, artifacts.len(), evidence_links.len()),
    );
    manifest.id = Some(run.id.clone());
    manifest.title = run
        .command
        .clone()
        .or_else(|| Some(format!("{} run {}", run.kind, run.id)));
    manifest.source = Some(EvidenceManifestSource::Derived);
    manifest.status.label = stale_reason.map(str::to_string);
    manifest.status.updated_at = Some(
        run.finished_at
            .clone()
            .unwrap_or_else(|| run.started_at.clone()),
    );
    manifest.interpretation.confidence = Some(confidence);
    manifest.interpretation.notes = notes;
    manifest.tracker_refs = tracker_refs.to_vec();
    manifest.run_refs = vec![RunRef {
        id: run.id.clone(),
        kind: Some(run.kind.clone()),
        component_id: run.component_id.clone(),
        rig_id: run.rig_id.clone(),
        url: None,
    }];
    manifest.artifact_refs = artifacts.iter().map(artifact_ref_from_record).collect();
    manifest.blocking_conditions = blocking_conditions;
    manifest
}

fn derived_summary(
    run: &RunRecord,
    failure: &EvidenceFailureSummary,
    artifact_count: usize,
    evidence_link_count: usize,
) -> String {
    let mut summary = format!(
        "{} run {} recorded status `{}` with {artifact_count} artifact(s) and {evidence_link_count} reviewer-visible evidence link(s).",
        run.kind, run.id, run.status
    );
    if let Some(error) = failure.error.as_deref() {
        summary.push_str(&format!(" Recorded error: {error}"));
    }
    summary
}

fn derived_blocking_conditions(
    run: &RunRecord,
    failure: &EvidenceFailureSummary,
    status: Option<RunStatus>,
    stale_reason: Option<&str>,
    terminal: bool,
    no_reviewable_evidence: bool,
) -> Vec<BlockingCondition> {
    let refs = vec![run.id.clone()];
    let mut conditions = Vec::new();

    for gate in &failure.gate_failures {
        conditions.push(BlockingCondition {
            kind: "gate_failure".to_string(),
            summary: gate.clone(),
            severity: Some(BlockingSeverity::Critical),
            refs: refs.clone(),
        });
    }
    if let Some(error) = failure.error.as_deref() {
        conditions.push(BlockingCondition {
            kind: "run_error".to_string(),
            summary: error.to_string(),
            severity: Some(BlockingSeverity::Critical),
            refs: refs.clone(),
        });
    }
    if let Some(runner_failure) = failure.runner_failure.as_ref() {
        let detail = runner_failure
            .message
            .clone()
            .or_else(|| runner_failure.failure_code.clone())
            .unwrap_or_else(|| runner_failure.stderr_tail.clone());
        conditions.push(BlockingCondition {
            kind: "runner_failure".to_string(),
            summary: format!(
                "Runner job {} failed: {detail}",
                runner_failure.runner_job_id
            ),
            severity: Some(BlockingSeverity::Critical),
            refs: refs.clone(),
        });
    }
    match status {
        Some(RunStatus::Stale) => conditions.push(BlockingCondition {
            kind: "stale_run".to_string(),
            summary: stale_reason
                .map(str::to_string)
                .unwrap_or_else(|| "The run record is stale.".to_string()),
            severity: Some(BlockingSeverity::Critical),
            refs: refs.clone(),
        }),
        Some(RunStatus::Running) => conditions.push(BlockingCondition {
            kind: "run_in_progress".to_string(),
            summary: "The run has not reached a terminal state.".to_string(),
            severity: Some(BlockingSeverity::Info),
            refs: refs.clone(),
        }),
        Some(RunStatus::Skipped) => conditions.push(BlockingCondition {
            kind: "run_skipped".to_string(),
            summary: "The run was skipped, so it proved nothing.".to_string(),
            severity: Some(BlockingSeverity::Warning),
            refs: refs.clone(),
        }),
        None => conditions.push(BlockingCondition {
            kind: "unknown_run_status".to_string(),
            summary: format!(
                "Status `{}` is not a status Homeboy owns, so terminality cannot be assumed.",
                run.status
            ),
            severity: Some(BlockingSeverity::Warning),
            refs: refs.clone(),
        }),
        Some(RunStatus::Pass) | Some(RunStatus::Fail) | Some(RunStatus::Error) => {}
    }
    if terminal && no_reviewable_evidence {
        conditions.push(BlockingCondition {
            kind: "no_reviewable_evidence".to_string(),
            summary: "The run recorded no reviewer-visible evidence target.".to_string(),
            severity: Some(BlockingSeverity::Warning),
            refs,
        });
    }

    conditions
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
    use crate::observation::disk_budget::DiskBudget;

    fn sample_run() -> RunRecord {
        RunRecord {
            id: "run-1".to_string(),
            kind: "trace".to_string(),
            component_id: Some("homeboy".to_string()),
            started_at: "2026-06-12T00:00:00Z".to_string(),
            finished_at: Some("2026-06-12T00:01:00Z".to_string()),
            status: "pass".to_string(),
            command: Some("homeboy trace".to_string()),
            homeboy_version: Some("test-version".to_string()),
            ..Default::default()
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
            created_at: "2026-06-12T00:00:30Z".to_string(),
            ..Default::default()
        }
    }

    fn sample_disk_budget() -> DiskBudget {
        DiskBudget {
            path: "/tmp".to_string(),
            status: "unavailable".to_string(),
            ..Default::default()
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
        assert_eq!(
            report.retention.default_retention_days,
            DEFAULT_RETENTION_DAYS
        );
        // A passing run is not a failure.
        assert!(!report.failure.failed);
        assert_eq!(report.failure.status, "pass");
        // No producer attached a manifest, so the report carries a derived one.
        let manifest = report.evidence_manifest.expect("derived manifest");
        assert_eq!(manifest.source, Some(EvidenceManifestSource::Derived));
        assert_eq!(manifest.status.state, EvidenceManifestState::Passed);
        assert!(report.evidence_manifest_errors.is_empty());
    }

    fn local_file_artifact() -> ArtifactRecord {
        ArtifactRecord {
            id: "summary".to_string(),
            run_id: "run-1".to_string(),
            kind: "summary".to_string(),
            artifact_type: "file".to_string(),
            path: "/tmp/does-not-need-to-exist/summary.json".to_string(),
            created_at: "2026-06-12T00:00:30Z".to_string(),
            ..Default::default()
        }
    }

    fn derived_manifest(run: &RunRecord, artifacts: &[ArtifactRecord]) -> EvidenceManifest {
        let failure = evidence_failure_summary(run);
        let links = evidence_links(artifacts);
        let stale_reason = running_status_note(run);
        derive_evidence_manifest(EvidenceManifestDerivation {
            run,
            artifacts,
            failure: &failure,
            evidence_links: links.as_slice(),
            tracker_refs: &[],
            stale_reason: stale_reason.as_deref(),
        })
    }

    /// A derived manifest must be self-describing: it always validates against
    /// its own contract, so it can be lifted out of the report and handed to any
    /// consumer of `homeboy/evidence-manifest/v1`.
    #[test]
    fn derived_manifest_satisfies_the_contract_for_every_status_label() {
        for status in ["running", "pass", "fail", "error", "skipped", "stale", "?"] {
            let mut run = sample_run();
            run.status = status.to_string();
            let manifest = derived_manifest(&run, &[url_artifact()]);

            manifest
                .validate()
                .unwrap_or_else(|err| panic!("{status}: {err}"));
            assert_eq!(manifest.source, Some(EvidenceManifestSource::Derived));
            assert_eq!(manifest.id.as_deref(), Some("run-1"));
            assert_eq!(manifest.run_refs[0].id, "run-1");
        }
    }

    #[test]
    fn derived_manifest_maps_each_owned_status_to_a_state() {
        let cases = [
            ("running", EvidenceManifestState::Pending),
            ("pass", EvidenceManifestState::Passed),
            ("fail", EvidenceManifestState::Failed),
            ("error", EvidenceManifestState::Failed),
            ("stale", EvidenceManifestState::Blocked),
            ("skipped", EvidenceManifestState::Unknown),
        ];

        for (status, expected) in cases {
            let mut run = sample_run();
            run.status = status.to_string();
            assert_eq!(
                derived_manifest(&run, &[url_artifact()]).status.state,
                expected,
                "{status}"
            );
        }
    }

    /// Fail closed: a label Homeboy does not own is `unknown`, never guessed
    /// into a pass, and it says why.
    #[test]
    fn derived_manifest_refuses_to_interpret_a_status_it_does_not_own() {
        let mut run = sample_run();
        run.status = "mostly_fine".to_string();
        let manifest = derived_manifest(&run, &[url_artifact()]);

        assert_eq!(manifest.status.state, EvidenceManifestState::Unknown);
        assert_eq!(
            manifest.interpretation.confidence,
            Some(EvidenceConfidence::Low)
        );
        assert!(manifest
            .blocking_conditions
            .iter()
            .any(|condition| condition.kind == "unknown_run_status"));
    }

    /// A pass that also recorded a gate failure is contradictory metadata. The
    /// manifest is what a consumer acts on, so the blocker wins.
    #[test]
    fn derived_manifest_downgrades_a_pass_that_carries_a_critical_blocker() {
        let mut run = sample_run();
        run.metadata_json = serde_json::json!({ "gate_failures": ["p95_ms exceeded"] });
        let manifest = derived_manifest(&run, &[url_artifact()]);

        assert_eq!(run.status, "pass");
        assert_eq!(manifest.status.state, EvidenceManifestState::Blocked);
        assert!(manifest.has_blocking_condition(BlockingSeverity::Critical));
        assert!(manifest
            .interpretation
            .notes
            .iter()
            .any(|note| note.contains("the blocker wins")));
    }

    #[test]
    fn derived_manifest_grades_confidence_by_reviewability() {
        let run = sample_run();

        let none = derived_manifest(&run, &[]);
        assert_eq!(
            none.interpretation.confidence,
            Some(EvidenceConfidence::Low)
        );
        assert!(none
            .blocking_conditions
            .iter()
            .any(|condition| condition.kind == "no_reviewable_evidence"));

        let local = derived_manifest(&run, &[local_file_artifact()]);
        assert_eq!(
            local.interpretation.confidence,
            Some(EvidenceConfidence::Medium)
        );
        assert_eq!(local.artifact_refs.len(), 1);

        let reviewable = derived_manifest(&run, &[url_artifact()]);
        assert_eq!(
            reviewable.interpretation.confidence,
            Some(EvidenceConfidence::High)
        );
        assert!(!reviewable
            .blocking_conditions
            .iter()
            .any(|condition| condition.kind == "no_reviewable_evidence"));
    }

    #[test]
    fn derived_manifest_bounds_its_blocking_conditions() {
        let mut run = sample_run();
        run.status = "fail".to_string();
        let gates: Vec<String> = (0..50).map(|index| format!("gate {index}")).collect();
        run.metadata_json = serde_json::json!({ "gate_failures": gates });

        let manifest = derived_manifest(&run, &[url_artifact()]);

        assert_eq!(
            manifest.blocking_conditions.len(),
            DERIVED_BLOCKING_CONDITION_LIMIT
        );
        assert!(manifest
            .interpretation
            .notes
            .iter()
            .any(|note| note.contains("omitted")));
    }

    /// An authored manifest is never overwritten, and the reader stamps where it
    /// came from instead of trusting what the producer serialized.
    #[test]
    fn an_authored_manifest_wins_and_is_stamped_with_its_real_provenance() {
        let mut run = sample_run();
        run.metadata_json = serde_json::json!({
            "evidence_manifest": {
                "schema": "homeboy/evidence-manifest/v1",
                "source": "artifact",
                "status": { "state": "blocked" },
                "interpretation": { "summary": "Reviewer confirmation is required." }
            }
        });

        let report = build_run_evidence_report(RunEvidenceReportInputs {
            command: "runs.evidence",
            run: run.clone(),
            run_summary: serde_json::json!({ "id": run.id }),
            artifacts: vec![url_artifact()],
            artifact_root: PathBuf::from("/tmp/artifacts"),
            disk_budget: sample_disk_budget(),
        });

        let manifest = report.evidence_manifest.expect("authored manifest");
        assert_eq!(manifest.status.state, EvidenceManifestState::Blocked);
        assert_eq!(
            manifest.interpretation.summary,
            "Reviewer confirmation is required."
        );
        // Claimed `artifact`, resolved from run metadata. The reader decides.
        assert_eq!(manifest.source, Some(EvidenceManifestSource::RunMetadata));
    }

    /// A derived manifest copies the report's tracker refs, so the report must
    /// not fold them back in — that would double every ref.
    #[test]
    fn a_derived_manifest_does_not_double_the_reports_tracker_refs() {
        let mut run = sample_run();
        run.metadata_json = serde_json::json!({
            "tracker_refs": [{ "kind": "issue", "id": "HB-42" }]
        });

        let report = build_run_evidence_report(RunEvidenceReportInputs {
            command: "runs.evidence",
            run: run.clone(),
            run_summary: serde_json::json!({ "id": run.id }),
            artifacts: vec![url_artifact()],
            artifact_root: PathBuf::from("/tmp/artifacts"),
            disk_budget: sample_disk_budget(),
        });

        assert_eq!(report.tracker_refs.len(), 1);
        let manifest = report.evidence_manifest.expect("derived manifest");
        assert_eq!(manifest.tracker_refs.len(), 1);
        assert_eq!(manifest.tracker_refs[0].id, "HB-42");
    }

    /// A malformed authored manifest must not be silently replaced by a derived
    /// one that hides the producer's bug: the error is still reported.
    #[test]
    fn a_malformed_authored_manifest_still_reports_its_error() {
        let mut run = sample_run();
        run.metadata_json = serde_json::json!({
            "evidence_manifest": {
                "schema": "homeboy/evidence-manifest/v1",
                "status": { "state": "passed" },
                "interpretation": { "summary": "" }
            }
        });

        let report = build_run_evidence_report(RunEvidenceReportInputs {
            command: "runs.evidence",
            run: run.clone(),
            run_summary: serde_json::json!({ "id": run.id }),
            artifacts: vec![url_artifact()],
            artifact_root: PathBuf::from("/tmp/artifacts"),
            disk_budget: sample_disk_budget(),
        });

        assert_eq!(report.evidence_manifest_errors.len(), 1);
        assert!(report.evidence_manifest_errors[0].contains("metadata.evidence_manifest"));
        let manifest = report.evidence_manifest.expect("derived manifest");
        assert_eq!(manifest.source, Some(EvidenceManifestSource::Derived));
    }
}
