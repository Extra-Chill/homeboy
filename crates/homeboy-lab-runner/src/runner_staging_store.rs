//! Durable runner-side storage and concrete transports for sealed staging.
//!
//! This is deliberately separate from remote process jobs: staging owns the
//! sealed source and workspace authority before a provider-facing job exists.

use std::collections::BTreeMap;
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use homeboy_core::{Error, Result};

use crate::direct_lab_handoff::DirectLabHandoffReceipt;
use crate::runner_staging_operation::{
    RemoteRunnerStagingEnvelope, RemoteRunnerStagingReceipt, RemoteRunnerStagingTransport,
    RunnerSourceArtifact, RunnerStagingArtifacts, SourcePackageEntryKind,
    REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY, REMOTE_RUNNER_SOURCE_ARTIFACT_SYMLINK_CAPABILITY,
    REMOTE_RUNNER_SOURCE_MATERIALIZATION_CAPABILITY, REMOTE_RUNNER_STAGING_CAPABILITY,
    REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA,
};
use crate::{broker_submit_token_for_runner, RunnerSession};

const STORE_SCHEMA: &str = "homeboy/remote-runner-staging-store/v1";

/// Operator override (whole seconds) for the sealed staging request deadline.
pub(crate) const RUNNER_STAGING_TIMEOUT_ENV: &str = "HOMEBOY_RUNNER_STAGING_TIMEOUT_SECS";
/// Connecting to the tunnel or broker is a control-plane step; only the
/// upload-and-persist exchange needs the payload-scaled deadline.
/// Minimum total deadline for a sealed staging request, however small.
const STAGING_TIMEOUT_FLOOR: Duration = Duration::from_secs(60);
/// Upper bound so a wedged receiver cannot hold a Cook admission forever.
const STAGING_TIMEOUT_CAP: Duration = Duration::from_secs(30 * 60);
/// Budget per MiB of serialized request: a conservative 0.5 MiB/s for upload
/// plus receiver-side persistence over SSH or broker links.
const STAGING_SECS_PER_MIB: u64 = 2;

/// Total deadline for one sealed staging request carrying `payload_bytes`.
///
/// A valid positive `override_secs` (from [`RUNNER_STAGING_TIMEOUT_ENV`]) wins
/// outright. Otherwise the deadline is the floor plus a per-MiB allowance,
/// capped. Extra-Chill/homeboy#15863: the request previously reused the 10s
/// control-plane deadline, so any source large enough to need more than ten
/// seconds to upload and persist failed every Lab Cook before the provider ran.
pub(crate) fn staging_request_timeout(
    payload_bytes: usize,
    override_secs: Option<&str>,
) -> Duration {
    if let Some(secs) = override_secs
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
    {
        return Duration::from_secs(secs);
    }
    let mib = (payload_bytes as u64).div_ceil(1024 * 1024);
    let scaled = STAGING_TIMEOUT_FLOOR.saturating_add(Duration::from_secs(
        mib.saturating_mul(STAGING_SECS_PER_MIB),
    ));
    scaled.min(STAGING_TIMEOUT_CAP)
}

/// Attach staging-specific evidence to a sealed staging transport timeout so
/// the failure names the payload and deadline instead of reading as a generic
/// tunnel problem. Non-timeout errors pass through unchanged.
fn staging_timeout_context(mut error: Error, payload_bytes: usize, timeout: Duration) -> Error {
    let is_timeout = error
        .details
        .pointer("/daemon_transport_error/kind")
        .and_then(serde_json::Value::as_str)
        == Some("timeout")
        || error.message.to_ascii_lowercase().contains("timed out");
    if !is_timeout {
        return error;
    }
    if let Some(details) = error.details.as_object_mut() {
        details.insert(
            "staging_upload_timeout".to_string(),
            serde_json::json!({
                "payload_bytes": payload_bytes,
                "timeout_secs": timeout.as_secs(),
                "override_env": RUNNER_STAGING_TIMEOUT_ENV,
            }),
        );
    }
    error.with_hint(format!(
        "Sealed source staging ({payload_bytes} bytes) did not complete within {}s. Raise the deadline with {RUNNER_STAGING_TIMEOUT_ENV}=<secs> or check the runner link throughput.",
        timeout.as_secs()
    ))
}

/// Read the verified source package named by a staging receipt. Execution code
/// uses this before extracting its own workspace; it never receives controller
/// paths or the transfer's inline content.
pub(crate) fn read_staged_source_artifact(
    store_path: impl AsRef<Path>,
    artifact: &RunnerSourceArtifact,
) -> Result<Vec<u8>> {
    artifact.validate()?;
    let path = store_path
        .as_ref()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("source-artifacts")
        .join(&artifact.artifact_id);
    let bytes = fs::read(&path).map_err(|error| {
        Error::validation_invalid_argument(
            "source_artifact",
            "staged source artifact is unavailable",
            Some(artifact.artifact_id.clone()),
            Some(vec![format!(
                "restore or retransmit source artifact at {}",
                path.display()
            )]),
        )
        .with_source(error)
    })?;
    if bytes.len() as u64 != artifact.size_bytes
        || format!("sha256:{:x}", Sha256::digest(&bytes)) != artifact.sha256
    {
        return Err(Error::validation_invalid_argument(
            "source_artifact",
            "staged source artifact no longer matches its immutable descriptor",
            Some(artifact.artifact_id.clone()),
            None,
        ));
    }
    Ok(bytes)
}

/// Materialize a verified package beneath the manifest-declared `workspace`
/// root. Validated relative entries are the only paths joined to `destination`.
pub fn extract_staged_source_artifact(
    store_path: impl AsRef<Path>,
    artifact: &RunnerSourceArtifact,
    destination: impl AsRef<Path>,
) -> Result<PathBuf> {
    let package = read_staged_source_artifact(store_path, artifact)?;
    artifact.package.validate(&package)?;
    let files: BTreeMap<String, serde_json::Value> =
        serde_json::from_slice(&package).map_err(|error| {
            Error::validation_invalid_argument("source_package", error.to_string(), None, None)
        })?;
    let root = destination.as_ref().join(&artifact.package.extraction_root);
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(extraction_conflict(&root)),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(&root).map_err(|error| {
                Error::internal_io(error.to_string(), Some(root.display().to_string()))
            })?
        }
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(root.display().to_string()),
            ))
        }
    }
    for entry in artifact
        .package
        .entries
        .iter()
        .filter(|entry| entry.kind == SourcePackageEntryKind::File)
    {
        let value = files.get(&entry.path).ok_or_else(|| {
            Error::validation_invalid_argument(
                "source_package",
                "source package entry is missing",
                Some(entry.path.clone()),
                None,
            )
        })?;
        let encoded = if artifact.package.schema == "homeboy/source-package-manifest/v1" {
            value.as_str()
        } else {
            value
                .get("content_base64")
                .and_then(serde_json::Value::as_str)
        }
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "source_package",
                "source package file payload is invalid",
                Some(entry.path.clone()),
                None,
            )
        })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "source_package",
                    error.to_string(),
                    Some(entry.path.clone()),
                    None,
                )
            })?;
        if bytes.len() as u64 != entry.size_bytes
            || format!("sha256:{:x}", Sha256::digest(&bytes)) != entry.sha256
        {
            return Err(Error::validation_invalid_argument(
                "source_package",
                "source package entry does not match manifest",
                Some(entry.path.clone()),
                None,
            ));
        }
        let path = root.join(&entry.path);
        ensure_extraction_parent(&root, &entry.path)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(extraction_conflict(&path)),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(path.display().to_string()),
                ))
            }
        }
        fs::write(&path, bytes).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
    }
    for entry in artifact
        .package
        .entries
        .iter()
        .filter(|entry| entry.kind == SourcePackageEntryKind::Symlink)
    {
        #[cfg(not(unix))]
        return Err(Error::validation_invalid_argument(
            "source_package",
            "source package symlinks require a Unix runner",
            Some(entry.path.clone()),
            None,
        ));
        #[cfg(unix)]
        {
            let target = files
                .get(&entry.path)
                .and_then(|value| value.get("target"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "source_package",
                        "source package symlink payload is invalid",
                        Some(entry.path.clone()),
                        None,
                    )
                })?;
            let path = root.join(&entry.path);
            ensure_extraction_parent(&root, &entry.path)?;
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let existing = fs::read_link(&path).map_err(|error| {
                        Error::internal_io(error.to_string(), Some(path.display().to_string()))
                    })?;
                    if existing != Path::new(target) {
                        fs::remove_file(&path).map_err(|error| {
                            Error::internal_io(error.to_string(), Some(path.display().to_string()))
                        })?;
                        std::os::unix::fs::symlink(target, &path).map_err(|error| {
                            Error::internal_io(error.to_string(), Some(path.display().to_string()))
                        })?;
                    }
                }
                Ok(_) => return Err(extraction_conflict(&path)),
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    std::os::unix::fs::symlink(target, &path).map_err(|error| {
                        Error::internal_io(error.to_string(), Some(path.display().to_string()))
                    })?;
                }
                Err(error) => {
                    return Err(Error::internal_io(
                        error.to_string(),
                        Some(path.display().to_string()),
                    ))
                }
            }
        }
    }
    Ok(root)
}

/// Re-resolve the workspace authority from runner-owned staging state before a
/// claimed job executes. Queue metadata is a projection, not the authority.
pub fn verify_staged_workspace_materialization(
    store_path: impl AsRef<Path>,
    workspace: &crate::runner_staging_operation::ControllerWorkspaceMaterialization,
) -> Result<()> {
    workspace.validate()?;
    let state = load_stage(&stage_record_path(store_path.as_ref(), &workspace.run_id))?;
    let stage = state.stages.get(&workspace.run_id).ok_or_else(|| {
        Error::validation_invalid_argument(
            "staged_workspace_materialization",
            "runner staging store has no durable workspace authority for this run",
            Some(workspace.run_id.clone()),
            None,
        )
    })?;
    stage.envelope.validate()?;
    if stage.envelope.handoff.run_id != workspace.run_id
        || stage.envelope.materialization.workspace.as_ref() != Some(workspace)
    {
        return Err(Error::validation_invalid_argument(
            "staged_workspace_materialization",
            "queued workspace materialization does not match runner-owned staging authority",
            Some(workspace.run_id.clone()),
            None,
        ));
    }
    Ok(())
}

fn extraction_conflict(path: &Path) -> Error {
    Error::validation_invalid_argument(
        "source_package",
        "source package extraction entry conflicts with an existing non-symlink type",
        Some(path.display().to_string()),
        None,
    )
}

fn ensure_extraction_parent(root: &Path, entry_path: &str) -> Result<()> {
    let mut parent = root.to_path_buf();
    for component in entry_path
        .split('/')
        .take(entry_path.split('/').count().saturating_sub(1))
    {
        parent.push(component);
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(extraction_conflict(&parent)),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                fs::create_dir(&parent).map_err(|error| {
                    Error::internal_io(error.to_string(), Some(parent.display().to_string()))
                })?
            }
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(parent.display().to_string()),
                ))
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredStage {
    envelope: RemoteRunnerStagingEnvelope,
    receipt: RemoteRunnerStagingReceipt,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StagingIntent {
    envelope: RemoteRunnerStagingEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_artifact: Option<RunnerSourceArtifact>,
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runner_job_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StagingStoreState {
    schema: String,
    stages: BTreeMap<String, StoredStage>,
    #[serde(default)]
    intents: BTreeMap<String, StagingIntent>,
}

impl Default for StagingStoreState {
    fn default() -> Self {
        Self {
            schema: STORE_SCHEMA.to_string(),
            stages: BTreeMap::new(),
            intents: BTreeMap::new(),
        }
    }
}

/// The only runner-side mutation used by sealed staging. It receives sealed
/// content and opaque keys, never a controller filesystem path.
pub trait RunnerStagingMaterializer {
    fn materialize(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<RunnerStagingArtifacts>;
}

/// A file-backed, runner-owned staging ledger. One store belongs to one runner;
/// binding the runner before any write prevents a controller from staging under
/// another runner's authority.
pub struct RunnerStagingStore<M> {
    path: PathBuf,
    runner_id: String,
    materializer: M,
}

impl<M: RunnerStagingMaterializer> RunnerStagingStore<M> {
    pub fn open(
        path: impl Into<PathBuf>,
        runner_id: impl Into<String>,
        materializer: M,
    ) -> Result<Self> {
        let store = Self {
            path: path.into(),
            runner_id: runner_id.into(),
            materializer,
        };
        let _lock = store.admission_lock()?;
        store.retire_ledger()?;
        Ok(store)
    }

    pub fn stage_durable(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<RemoteRunnerStagingReceipt> {
        self.stage_durable_with_job_id(envelope, format!("staging-{}", envelope.handoff.run_id))
    }

    pub(crate) fn stage_durable_with_job_id(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
        runner_job_id: impl Into<String>,
    ) -> Result<RemoteRunnerStagingReceipt> {
        let runner_job_id = runner_job_id.into();
        self.stage_durable_with_submit(envelope, |_| Ok(runner_job_id))
    }

    /// Lock-held admission state machine: intent -> source_ready -> job_submitted
    /// -> receipt. `submit` must use the handoff idempotency key.
    pub(crate) fn stage_durable_with_submit(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
        submit: impl FnOnce(&RemoteRunnerStagingEnvelope) -> Result<String>,
    ) -> Result<RemoteRunnerStagingReceipt> {
        envelope.validate()?;
        if envelope.handoff.runner_id != self.runner_id {
            return Err(Error::validation_invalid_argument(
                "runner_authority",
                "sealed staging envelope is not authorized for this runner",
                Some(envelope.handoff.runner_id.clone()),
                None,
            ));
        }
        let source_artifact = envelope
            .materialization
            .source_artifact
            .as_ref()
            .map(|transfer| {
                transfer
                    .decode_verified()
                    .map(|bytes| (transfer.descriptor(), bytes))
            })
            .transpose()?;
        let _lock = self.admission_lock()?;
        let key = &envelope.handoff.idempotency_key;
        let mut state = load_stage(&stage_record_path(&self.path, key))?;
        if let Some(existing) = state.stages.get(key) {
            if existing.envelope != *envelope {
                return Err(Error::validation_invalid_argument(
                    "idempotency_key",
                    "sealed staging key is already bound to a different envelope",
                    Some(key.clone()),
                    None,
                ));
            }
            if let Some((artifact, _)) = &source_artifact {
                self.verify_source_artifact(artifact)?;
            }
            return Ok(existing.receipt.clone());
        }

        state
            .intents
            .entry(key.clone())
            .or_insert_with(|| StagingIntent {
                envelope: envelope.clone(),
                source_artifact: source_artifact
                    .as_ref()
                    .map(|(artifact, _)| artifact.clone()),
                state: "intent_created".to_string(),
                runner_job_id: None,
            });
        self.persist(key, &state)?;

        // The lock covers materialization and receipt publication. Concurrent
        // requests therefore observe one durable admission, not two writes.
        if let Some((artifact, bytes)) = &source_artifact {
            self.persist_source_artifact(artifact, bytes)?;
        }
        // Source bytes are now durable before any queue entry can become claimable.
        state.intents.get_mut(key).expect("intent").state = "source_ready".to_string();
        self.persist(key, &state)?;
        let runner_job_id = match state
            .intents
            .get(key)
            .and_then(|intent| intent.runner_job_id.clone())
        {
            Some(job_id) => job_id,
            None => {
                let job_id = submit(envelope)?;
                let intent = state.intents.get_mut(key).expect("intent");
                intent.runner_job_id = Some(job_id.clone());
                intent.state = "job_submitted".to_string();
                self.persist(key, &state)?;
                job_id
            }
        };
        let artifacts = self.materializer.materialize(envelope)?;
        let receipt = RemoteRunnerStagingReceipt {
            schema: REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA.to_string(),
            handoff: DirectLabHandoffReceipt::accepted(&envelope.handoff, runner_job_id),
            artifacts: RunnerStagingArtifacts {
                source_artifact: source_artifact.map(|(artifact, _)| artifact),
                ..artifacts
            },
        };
        receipt.validate_for(envelope)?;
        state.stages.insert(
            key.clone(),
            StoredStage {
                envelope: envelope.clone(),
                receipt: receipt.clone(),
            },
        );
        state.intents.remove(key);
        self.persist(key, &state)?;
        Ok(receipt)
    }

    /// Upgrades drain runner jobs first; the old ledger is transient state,
    /// not a wire contract to migrate into the new per-stage store.
    fn retire_ledger(&self) -> Result<()> {
        if !self.path.exists() {
            return Ok(());
        }
        let size_bytes = fs::metadata(&self.path)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(self.path.display().to_string()))
            })?
            .len();
        // Inspect only for reporting. Even malformed legacy state is retired;
        // no old envelope is decoded or copied into the live store.
        let stage_count = File::open(&self.path)
            .ok()
            .and_then(|file| {
                serde_json::from_reader::<_, serde_json::Value>(std::io::BufReader::new(file)).ok()
            })
            .and_then(|ledger| {
                ledger
                    .get("stages")
                    .and_then(serde_json::Value::as_object)
                    .map(|stages| stages.len())
            });
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let retired_path = self.path.with_file_name(format!(
            "{}.retired-{timestamp}",
            self.path.file_name().unwrap_or_default().to_string_lossy(),
        ));
        fs::rename(&self.path, &retired_path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(self.path.display().to_string()))
        })?;
        persist_stage(
            &self.path.with_extension("retirement-report.json"),
            &serde_json::json!({
                "retired_path": retired_path,
                "size_bytes": size_bytes,
                "stage_count": stage_count,
            }),
        )
    }

    /// Missing jobs have already been compacted. Undecodable records cannot
    /// execute on this runner and are retired rather than pinning artifacts.
    fn reclaim_terminal(&self, jobs: &homeboy_core::api_jobs::JobStore) -> Result<()> {
        let _lock = self.admission_lock()?;
        let directory = self.path.with_extension("stages");
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(directory.display().to_string()),
                ))
            }
        };
        let mut retained_artifacts = std::collections::BTreeSet::new();
        let mut retired_artifacts = BTreeMap::new();
        let mut incompatible_records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| Error::internal_io(error.to_string(), None))?;
            if !entry
                .file_type()
                .map_err(|error| Error::internal_io(error.to_string(), None))?
                .is_file()
            {
                continue;
            }
            let path = entry.path();
            let state = match load_stage(&path) {
                Ok(state) => state,
                Err(error) => {
                    let directory = self.path.with_extension("stages.incompatible");
                    fs::create_dir_all(&directory).map_err(|error| {
                        Error::internal_io(error.to_string(), Some(directory.display().to_string()))
                    })?;
                    let retired_path = directory.join(entry.file_name());
                    fs::rename(&path, &retired_path).map_err(|error| {
                        Error::internal_io(error.to_string(), Some(path.display().to_string()))
                    })?;
                    incompatible_records.push(serde_json::json!({
                        "record": path, "retired_path": retired_path,
                        "message": error.message, "repair": error.hints,
                    }));
                    continue;
                }
            };
            let terminal = !state.stages.is_empty()
                && state.intents.is_empty()
                && state.stages.values().all(|stage| {
                    uuid::Uuid::parse_str(&stage.receipt.handoff.runner_job_id)
                        .ok()
                        .is_some_and(|id| jobs.get(id).map_or(true, |job| job.status.is_terminal()))
                });
            if !terminal {
                for artifact in state
                    .stages
                    .values()
                    .filter_map(|stage| stage.receipt.artifacts.source_artifact.as_ref())
                    .chain(
                        state
                            .intents
                            .values()
                            .filter_map(|intent| intent.source_artifact.as_ref()),
                    )
                {
                    retained_artifacts.insert(artifact.artifact_id.clone());
                }
                continue;
            }
            for stage in state.stages.values() {
                if let Some(artifact) = &stage.receipt.artifacts.source_artifact {
                    retired_artifacts.insert(artifact.artifact_id.clone(), artifact.clone());
                }
                let authority = &stage.envelope.materialization.authority_id;
                // Never follow a persisted path outside the runner-owned root.
                if authority.starts_with("staging-")
                    && Path::new(authority).components().count() == 1
                {
                    let root = self
                        .path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(authority);
                    match fs::symlink_metadata(&root) {
                        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                            fs::remove_dir_all(&root).map_err(|error| {
                                Error::internal_io(
                                    error.to_string(),
                                    Some(root.display().to_string()),
                                )
                            })?;
                        }
                        Err(error) if error.kind() == ErrorKind::NotFound => {}
                        Ok(_) => {
                            return Err(incompatible_stage(
                                &root,
                                "staging directory is not a directory",
                            ))
                        }
                        Err(error) => {
                            return Err(Error::internal_io(
                                error.to_string(),
                                Some(root.display().to_string()),
                            ))
                        }
                    }
                }
            }
            fs::remove_file(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(path.display().to_string()))
            })?;
        }
        if !incompatible_records.is_empty() {
            persist_stage(
                &self.path.with_extension("reclamation-report.json"),
                &serde_json::json!({ "incompatible_records": incompatible_records }),
            )?;
        }
        // Only live, decodable records can retain content-addressed artifacts.
        {
            for (id, artifact) in retired_artifacts {
                if retained_artifacts.contains(&id) {
                    continue;
                }
                let path = self.source_artifact_path(&artifact)?;
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(Error::internal_io(
                            error.to_string(),
                            Some(path.display().to_string()),
                        ))
                    }
                }
            }
        }
        Ok(())
    }

    fn persist(&self, key: &str, state: &StagingStoreState) -> Result<()> {
        persist_stage(&stage_record_path(&self.path, key), state)
    }

    fn source_artifact_path(&self, artifact: &RunnerSourceArtifact) -> Result<PathBuf> {
        artifact.validate()?;
        Ok(self
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("source-artifacts")
            .join(&artifact.artifact_id))
    }

    fn verify_source_artifact(&self, artifact: &RunnerSourceArtifact) -> Result<()> {
        read_staged_source_artifact(&self.path, artifact).map(|_| ())
    }

    fn persist_source_artifact(&self, artifact: &RunnerSourceArtifact, bytes: &[u8]) -> Result<()> {
        let path = self.source_artifact_path(artifact)?;
        if path.exists() {
            return self.verify_source_artifact(artifact);
        }
        persist_bytes(&path, bytes)
    }

    fn admission_lock(&self) -> Result<File> {
        staging_lock(&self.path, &self.runner_id)
    }
}

fn stage_record_path(ledger: &Path, key: &str) -> PathBuf {
    ledger
        .with_extension("stages")
        .join(format!("{:x}.json", Sha256::digest(key.as_bytes())))
}

fn incompatible_stage(path: &Path, reason: impl std::fmt::Display) -> Error {
    Error::validation_invalid_argument(
        "runner_staging_state",
        format!("incompatible runner staging state: {reason}"),
        Some(path.display().to_string()),
        None,
    )
    .with_hint(format!(
        "Remove the incompatible record while the runner is stopped: rm -- '{}'",
        path.display()
    ))
}

fn load_stage(path: &Path) -> Result<StagingStoreState> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(StagingStoreState::default())
        }
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(path.display().to_string()),
            ))
        }
    };
    let state: StagingStoreState =
        serde_json::from_slice(&bytes).map_err(|error| incompatible_stage(path, error))?;
    if state.schema != STORE_SCHEMA {
        return Err(incompatible_stage(path, "unsupported schema"));
    }
    Ok(state)
}

fn persist_stage(path: &Path, state: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(state).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize staging record".to_string()),
        )
    })?;
    persist_bytes(path, &bytes)
}

fn persist_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", parent.display())),
        )
    })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create staging temporary in {}", parent.display())),
        )
    })?;
    temporary.write_all(&bytes).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("write staging temporary for {}", path.display())),
        )
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("sync staging temporary for {}", path.display())),
        )
    })?;
    temporary.persist(path).map_err(|error| {
        Error::internal_io(
            error.error.to_string(),
            Some(format!("publish {}", path.display())),
        )
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("sync {}", parent.display())),
            )
        })
}

fn staging_lock(path: &Path, runner_id: &str) -> Result<File> {
    const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
    const LOCK_RETRY: Duration = Duration::from_millis(25);
    let lock_path = path.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("open {}", lock_path.display())),
            )
        })?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match lock.try_lock_exclusive() {
            Ok(true) => return Ok(lock),
            Ok(false) => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(format!("lock {}", lock_path.display())),
                ));
            }
        }
        if Instant::now() >= deadline {
            let mut error = Error::internal_io(
                format!(
                    "timed out after {}ms waiting for staging admission lock",
                    LOCK_TIMEOUT.as_millis()
                ),
                Some(format!("lock {}", lock_path.display())),
            );
            error.details = serde_json::json!({
                "kind": "runner_staging_lock_timeout",
                "runner_id": runner_id,
                "timeout_ms": LOCK_TIMEOUT.as_millis(),
            });
            error.retryable = Some(true);
            return Err(error);
        }
        std::thread::sleep(LOCK_RETRY);
    }
}

/// Versioned request carried by the direct daemon transport.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RemoteRunnerStagingRequest {
    pub schema: String,
    pub envelope: RemoteRunnerStagingEnvelope,
}

impl RemoteRunnerStagingRequest {
    pub const SCHEMA: &'static str = "homeboy/remote-runner-staging-request/v1";

    pub fn new(envelope: RemoteRunnerStagingEnvelope) -> Self {
        Self {
            schema: Self::SCHEMA.to_string(),
            envelope,
        }
    }
}

/// Transport over the runner-owned store. The channel is intentionally not part
/// of the persisted receipt, so a reconnected transport replays the exact
/// accepted result.
pub struct RunnerStagingTransport<M> {
    pub connected: bool,
    pub compatible: bool,
    pub store: RunnerStagingStore<M>,
}

impl<M: RunnerStagingMaterializer> RemoteRunnerStagingTransport for RunnerStagingTransport<M> {
    fn is_connected(&self) -> bool {
        self.connected
    }
    fn supports_capability(&self, capability: &str) -> bool {
        self.compatible
            && (matches!(
                capability,
                REMOTE_RUNNER_STAGING_CAPABILITY
                    | REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY
                    | REMOTE_RUNNER_SOURCE_MATERIALIZATION_CAPABILITY
            ) || (cfg!(unix) && capability == REMOTE_RUNNER_SOURCE_ARTIFACT_SYMLINK_CAPABILITY))
    }
    fn stage_durable(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<RemoteRunnerStagingReceipt> {
        self.store.stage_durable(envelope)
    }
}

/// Resolve the connected runner into the production HTTP transport. Capability
/// discovery is read-only and happens before `submit_remote_runner_staging`
/// reaches its mutation boundary.
pub fn resolve_runner_staging_transport(
    runner_id: &str,
) -> Result<ProductionRunnerStagingTransport> {
    let report = crate::status(runner_id)?;
    let session = report.session.filter(|_| report.connected).ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner_connection",
            format!("runner `{runner_id}` is disconnected"),
            Some(runner_id.to_string()),
            None,
        )
    })?;
    let broker_token = broker_submit_token_for_runner(runner_id)?;
    let capabilities = production_capabilities(&session, broker_token.as_deref())?;
    Ok(ProductionRunnerStagingTransport {
        runner_id: runner_id.to_string(),
        session,
        capabilities,
        broker_token,
    })
}

pub struct ProductionRunnerStagingTransport {
    runner_id: String,
    session: RunnerSession,
    capabilities: Vec<String>,
    broker_token: Option<String>,
}

impl RemoteRunnerStagingTransport for ProductionRunnerStagingTransport {
    fn is_connected(&self) -> bool {
        true
    }

    fn supports_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|candidate| {
            candidate == crate::runner_staging_operation::DIRECT_RUNNER_STAGED_EXECUTION_CAPABILITY
        }) && self
            .capabilities
            .iter()
            .any(|candidate| candidate == capability)
    }

    fn stage_durable(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<RemoteRunnerStagingReceipt> {
        if envelope.handoff.runner_id != self.runner_id {
            return Err(Error::validation_invalid_argument(
                "runner_authority",
                "sealed staging envelope does not match the resolved runner",
                Some(envelope.handoff.runner_id.clone()),
                None,
            ));
        }
        let body = serde_json::to_value(RemoteRunnerStagingRequest::new(envelope.clone()))
            .map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize sealed staging request".to_string()),
                )
            })?;
        // The sealed request carries the whole source package, and the
        // receiver persists it before replying, so its deadline must scale
        // with the payload instead of reusing the 10s control-plane deadline
        // (Extra-Chill/homeboy#15863).
        let payload_bytes = serde_json::to_vec(&body)
            .map(|bytes| bytes.len())
            .unwrap_or(0);
        let timeout = staging_request_timeout(
            payload_bytes,
            std::env::var(RUNNER_STAGING_TIMEOUT_ENV).ok().as_deref(),
        );
        let data = crate::execution::daemon_api_post_json_for_session_with_timeout(
            &self.session,
            "/runner/staging/direct",
            &body,
            self.broker_token.as_deref(),
            timeout,
        )
        .map_err(|error| staging_timeout_context(error, payload_bytes, timeout))?;
        let response =
            crate::execution::canonical_daemon_body(&data, "sealed staging daemon response")
                .cloned()?;
        serde_json::from_value(response.get("receipt").cloned().unwrap_or(response)).map_err(
            |error| {
                Error::internal_json(
                    error.to_string(),
                    Some("parse sealed staging receipt".to_string()),
                )
            },
        )
    }
}

fn production_capabilities(
    session: &RunnerSession,
    broker_token: Option<&str>,
) -> Result<Vec<String>> {
    let runner_id = &session.runner_id;
    let data = crate::execution::daemon_api_post_json_for_session_with_broker_token(
        session,
        "/runner/staging/capabilities",
        &serde_json::json!({ "runner_id": runner_id }),
        broker_token,
    )
    .map_err(|error| staging_capability_error(runner_id, error))?;
    let response =
        crate::execution::canonical_daemon_body(&data, "sealed staging capabilities response")
            .cloned()?;
    serde_json::from_value(response.get("capabilities").cloned().unwrap_or_default()).map_err(
        |error| {
            Error::internal_json(
                error.to_string(),
                Some("parse sealed staging capabilities".to_string()),
            )
        },
    )
}

fn staging_capability_error(runner_id: &str, error: Error) -> Error {
    if error
        .details
        .get("http_status")
        .and_then(serde_json::Value::as_u64)
        == Some(404)
    {
        return Error::runner_capability_missing(
            runner_id,
            "sealed runner staging",
            vec![REMOTE_RUNNER_STAGING_CAPABILITY.to_string()],
            Vec::new(),
        );
    }
    error
}

struct SealedPayloadMaterializer {
    root: PathBuf,
}

impl RunnerStagingMaterializer for SealedPayloadMaterializer {
    fn materialize(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<RunnerStagingArtifacts> {
        let stage_root = self.root.join(&envelope.materialization.authority_id);
        fs::create_dir_all(&stage_root).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", stage_root.display())),
            )
        })?;
        fs::write(
            stage_root.join("source.sealed"),
            &envelope.materialization.source.sealed_payload,
        )
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("persist sealed runner source".to_string()),
            )
        })?;
        let workspace = stage_root.join(&envelope.materialization.workspace_key);
        fs::create_dir_all(&workspace).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", workspace.display())),
            )
        })?;
        Ok(RunnerStagingArtifacts {
            lifecycle_id: format!("staging:{}", envelope.handoff.run_id),
            source_artifact_id: format!(
                "sealed:{}",
                envelope.materialization.source.content_digest
            ),
            workspace_artifact_id: format!("workspace:{}", envelope.materialization.workspace_key),
            source_artifact: None,
        })
    }
}

struct ProductionStagingProvider;

impl homeboy_core::daemon::runner_staging::RunnerStagingProvider for ProductionStagingProvider {
    fn stage_direct(
        &self,
        request: serde_json::Value,
        jobs: &homeboy_core::api_jobs::JobStore,
    ) -> Result<serde_json::Value> {
        let response = self.stage(request, jobs)?;
        let receipt: RemoteRunnerStagingReceipt =
            serde_json::from_value(response["receipt"].clone()).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("decode durable staging receipt".to_string()),
                )
            })?;
        let job_id = uuid::Uuid::parse_str(&receipt.handoff.runner_job_id).map_err(|error| {
            Error::internal_unexpected(format!("invalid staged job identity: {error}"))
        })?;
        let root = homeboy_core::paths::runner_session_file(&receipt.handoff.runner_id)?
            .with_file_name(format!("{}-staging", receipt.handoff.runner_id));
        let store = RunnerStagingStore::open(
            root.join("store.json"),
            receipt.handoff.runner_id.clone(),
            SealedPayloadMaterializer { root },
        )?;
        // Serialize replay preparation before any source writes.
        let _preparation = store.admission_lock()?;
        if let Some(envelope) =
            jobs.staged_direct_execution_envelope(job_id, &receipt.handoff.runner_id)?
        {
            homeboy_core::daemon::enqueue_staged_direct_exec(
                jobs,
                job_id,
                envelope.clone(),
                envelope,
                None,
            )?;
        }
        Ok(response)
    }

    fn capabilities(&self, _runner_id: &str) -> Result<Vec<String>> {
        let mut capabilities = vec![
            crate::runner_staging_operation::DIRECT_RUNNER_STAGED_EXECUTION_CAPABILITY.to_string(),
            REMOTE_RUNNER_STAGING_CAPABILITY.to_string(),
            REMOTE_RUNNER_SOURCE_MATERIALIZATION_CAPABILITY.to_string(),
            REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY.to_string(),
        ];
        if cfg!(unix) {
            capabilities.push(REMOTE_RUNNER_SOURCE_ARTIFACT_SYMLINK_CAPABILITY.to_string());
        }
        Ok(capabilities)
    }
}

impl ProductionStagingProvider {
    fn stage(
        &self,
        request: serde_json::Value,
        jobs: &homeboy_core::api_jobs::JobStore,
    ) -> Result<serde_json::Value> {
        let request: RemoteRunnerStagingRequest =
            serde_json::from_value(request).map_err(|error| {
                Error::validation_invalid_argument(
                    "remote_runner_staging",
                    error.to_string(),
                    None,
                    None,
                )
            })?;
        if request.schema != RemoteRunnerStagingRequest::SCHEMA {
            return Err(Error::validation_invalid_argument(
                "remote_runner_staging",
                "unsupported sealed staging request schema",
                Some(request.schema),
                None,
            ));
        }
        let session_path =
            homeboy_core::paths::runner_session_file(&request.envelope.handoff.runner_id)?;
        let root =
            session_path.with_file_name(format!("{}-staging", request.envelope.handoff.runner_id));
        let store = RunnerStagingStore::open(
            root.join("store.json"),
            request.envelope.handoff.runner_id.clone(),
            SealedPayloadMaterializer { root },
        )?;
        store.reclaim_terminal(jobs)?;
        let mut transport = RunnerStagingTransport {
            connected: true,
            compatible: true,
            store,
        };
        let source_artifact = request
            .envelope
            .materialization
            .source_artifact
            .as_ref()
            .map(crate::runner_staging_operation::SourceArtifactTransfer::descriptor);
        let workspace = request.envelope.materialization.workspace.clone();
        let receipt = transport
            .store
            .stage_durable_with_submit(&request.envelope, |envelope| {
                let submission_key = envelope.handoff.idempotency_key.clone();
                let envelope_id = format!(
                    "remote-runner:{}:runner_staged_execution",
                    envelope.handoff.runner_id
                );
                let mut execution = homeboy_runner_contract::RunnerExecutionEnvelope::planned(
                    &envelope_id,
                    "remote_runner_job_request",
                );
                execution.source.ref_id = Some(envelope_id);
                execution.dispatch = Some(homeboy_runner_contract::RunnerExecutionDispatch {
                    runner_id: envelope.handoff.runner_id.clone(),
                    project_id: None,
                    operation: "runner_staged_execution".to_string(),
                    command: envelope.handoff.recipe.normalized_args.clone(),
                    cwd: workspace
                        .as_ref()
                        .map(|workspace| workspace.remote_cwd.clone()),
                    env: envelope.handoff.recipe.job_override_env.clone(),
                    source_snapshot: None,
                    require_paths: Vec::new(),
                    extension_env_providers: Vec::new(),
                });
                execution.metadata = serde_json::json!({
                    "submission_key": submission_key,
                    "staged_source_artifact": source_artifact,
                    "staged_workspace_materialization": workspace,
                });
                execution.mutation_policy.capture_patch = envelope.handoff.recipe.capture_patch;
                execution.secret_env = Some(
                    homeboy_core::secret_env_plan::SecretEnvPlan::from_secret_env_names(
                        envelope.handoff.recipe.secret_env_names.clone(),
                    ),
                );
                execution.lifecycle = Some(homeboy_runner_contract::RunnerJobLifecycleMetadata {
                    source: Some("sealed-staging".to_string()),
                    kind: Some("runner_staged_execution".to_string()),
                    durable_run_id: Some(envelope.handoff.run_id.clone()),
                    ..Default::default()
                });
                let job = jobs.submit_runner_api_request(
                    homeboy_runner_contract::RunnerApiSubmitRequest {
                        schema: homeboy_runner_contract::RUNNER_API_SUBMIT_REQUEST_SCHEMA
                            .to_string(),
                        api_version: homeboy_runner_contract::RUNNER_API_V1,
                        submission_key,
                        envelope: execution,
                        workspace_claim_binding: None,
                        workspace_owner_lease: None,
                    },
                )?;
                Ok(job.id.to_string())
            })?;
        Ok(serde_json::json!({ "receipt": receipt }))
    }
}

pub fn register_runner_staging_provider() {
    homeboy_core::daemon::runner_staging::register_runner_staging_provider(Box::new(
        ProductionStagingProvider,
    ));
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::process::Command;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier, Mutex,
    };

    use super::*;
    use crate::runner_staging_operation::submit_remote_runner_staging;
    use crate::runner_staging_operation::tests_support::envelope;

    #[derive(Default)]
    struct Materializer {
        calls: usize,
    }
    impl RunnerStagingMaterializer for Materializer {
        fn materialize(
            &mut self,
            envelope: &RemoteRunnerStagingEnvelope,
        ) -> Result<RunnerStagingArtifacts> {
            self.calls += 1;
            Ok(RunnerStagingArtifacts {
                lifecycle_id: format!("lifecycle-{}", envelope.handoff.run_id),
                source_artifact_id: format!(
                    "source-{}",
                    envelope.materialization.source.content_digest
                ),
                workspace_artifact_id: format!(
                    "workspace-{}",
                    envelope.materialization.workspace_key
                ),
                source_artifact: None,
            })
        }
    }

    fn transport(path: &Path) -> RunnerStagingTransport<Materializer> {
        RunnerStagingTransport {
            connected: true,
            compatible: true,
            store: RunnerStagingStore::open(path, "runner-1", Materializer::default())
                .expect("store"),
        }
    }

    #[test]
    fn staging_retired_record_does_not_block_new_run() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("store.json");
        let mut initial = transport(&path);
        let request = envelope();
        initial.store.stage_durable(&request).unwrap();
        let record = stage_record_path(&path, &request.handoff.run_id);
        let mut stored: serde_json::Value =
            serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        fs::remove_file(&record).unwrap();
        let stage = stored["stages"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        stage["envelope"]["handoff"]["reuse_compatible_snapshot"] = true.into();
        fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
        let mut restarted = transport(&path);
        let new_run = crate::runner_staging_operation::tests_support::envelope_for_run("new-run");
        restarted.store.stage_durable(&new_run).unwrap();
        assert!(!path.exists());
        assert!(!record.exists(), "legacy stages must not be migrated");
        let report_path = path.with_extension("retirement-report.json");
        let report_bytes = fs::read(&report_path).unwrap();
        let report: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
        assert_eq!(
            report["size_bytes"],
            fs::read(report["retired_path"].as_str().unwrap())
                .unwrap()
                .len()
        );
        assert_eq!(report["stage_count"], 1);
        let retired = PathBuf::from(report["retired_path"].as_str().unwrap());
        assert!(retired
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("store.json.retired-"));
        assert_eq!(
            fs::read(&retired).unwrap(),
            serde_json::to_vec(&stored).unwrap()
        );
        drop(transport(&path));
        assert_eq!(fs::read(&report_path).unwrap(), report_bytes);

        let jobs = homeboy_core::api_jobs::JobStore::open_without_reconciliation(
            temp.path().join("jobs.json"),
        )
        .unwrap();
        let mut compacted = envelope();
        compacted.materialization.authority_id = "staging-compacted".to_string();
        let receipt = restarted
            .store
            .stage_durable_with_job_id(&compacted, uuid::Uuid::new_v4().to_string())
            .unwrap();
        let artifact = receipt.artifacts.source_artifact.unwrap();
        let artifact_path = restarted.store.source_artifact_path(&artifact).unwrap();
        let directory = temp.path().join("staging-compacted");
        fs::create_dir(&directory).unwrap();
        let mut bad =
            crate::runner_staging_operation::tests_support::envelope_for_run("undecodable");
        bad.materialization.source_artifact = compacted.materialization.source_artifact.clone();
        restarted.store.stage_durable(&bad).unwrap();
        let bad_path = stage_record_path(&path, &bad.handoff.idempotency_key);
        let mut bad_record: serde_json::Value =
            serde_json::from_slice(&fs::read(&bad_path).unwrap()).unwrap();
        bad_record["stages"][&bad.handoff.idempotency_key]["envelope"]["handoff"]
            ["reuse_compatible_snapshot"] = true.into();
        fs::write(&bad_path, serde_json::to_vec(&bad_record).unwrap()).unwrap();
        // Remove the new run's reference to the shared test source.
        fs::remove_file(stage_record_path(&path, &new_run.handoff.idempotency_key)).unwrap();
        restarted.store.reclaim_terminal(&jobs).unwrap();
        assert!(!record.exists());
        assert!(!bad_path.exists());
        assert!(!directory.exists());
        assert!(!artifact_path.exists());
        let reclaimed: serde_json::Value = serde_json::from_slice(
            &fs::read(path.with_extension("reclamation-report.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            reclaimed["incompatible_records"][0]["record"],
            bad_path.to_str().unwrap()
        );
        restarted.store.reclaim_terminal(&jobs).unwrap();
    }

    #[test]
    fn staging_terminal_retention_preserves_active_records_and_reclaims_directories() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("store.json");
        let jobs = homeboy_core::api_jobs::JobStore::open_without_reconciliation(
            temp.path().join("jobs.json"),
        )
        .unwrap();
        let terminal = jobs.create("test");
        let active = jobs.create("test");
        let mut store = transport(&path).store;
        let mut request = envelope();
        request.materialization.authority_id = "staging-terminal".to_string();
        store
            .stage_durable_with_job_id(&request, terminal.id.to_string())
            .unwrap();
        let directory = temp.path().join("staging-terminal");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("source.sealed"), "payload").unwrap();
        let active_request =
            crate::runner_staging_operation::tests_support::envelope_for_run("active");
        store
            .stage_durable_with_job_id(&active_request, active.id.to_string())
            .unwrap();
        store.reclaim_terminal(&jobs).unwrap();
        assert!(directory.exists());
        jobs.cancel(terminal.id, "done").unwrap();
        store.reclaim_terminal(&jobs).unwrap();
        assert!(!directory.exists());
        assert!(!stage_record_path(&path, &request.handoff.run_id).exists());
        assert!(stage_record_path(&path, &active_request.handoff.run_id).exists());
        store.reclaim_terminal(&jobs).unwrap();
    }

    #[test]
    fn reconnected_transport_replays_one_runner_owned_receipt() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("staging.json");
        let request = RemoteRunnerStagingRequest::new(envelope());
        assert!(!serde_json::to_string(&request)
            .expect("request")
            .contains("/controller/private/source"));
        let mut direct = transport(&path);
        let first =
            submit_remote_runner_staging(&mut direct, &request.envelope).expect("direct stage");
        assert_eq!(direct.store.materializer.calls, 1);
        let mut reconnected = transport(&path);
        let replay =
            submit_remote_runner_staging(&mut reconnected, &request.envelope).expect("replay");
        assert_eq!(first, replay);
        assert_eq!(reconnected.store.materializer.calls, 0);
    }

    #[test]
    fn restart_replays_and_refusals_happen_before_materialization() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("staging.json");
        let request = envelope();
        let mut initial = transport(&path);
        submit_remote_runner_staging(&mut initial, &request).expect("stage");
        let mut restarted = transport(&path);
        submit_remote_runner_staging(&mut restarted, &request).expect("restart replay");
        assert_eq!(restarted.store.materializer.calls, 0);
        let mut incompatible = transport(&temp.path().join("incompatible.json"));
        incompatible.compatible = false;
        assert!(submit_remote_runner_staging(&mut incompatible, &request).is_err());
        assert_eq!(incompatible.store.materializer.calls, 0);
        let mut disconnected = transport(&temp.path().join("disconnected.json"));
        disconnected.connected = false;
        assert!(submit_remote_runner_staging(&mut disconnected, &request).is_err());
        assert_eq!(disconnected.store.materializer.calls, 0);
    }

    #[test]
    fn canonical_retention_replays_without_rewriting_store() {
        for delete in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("staging.json");
            let mut request = envelope();
            request.handoff.recipe.delete_workspace_on_failure = delete;
            let mut initial = transport(&path);
            let receipt = initial.store.stage_durable(&request).unwrap();
            let path = stage_record_path(&path, &request.handoff.run_id);
            let mut stored: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            let recipe = &mut stored["stages"]
                .as_object_mut()
                .unwrap()
                .values_mut()
                .next()
                .unwrap()["envelope"]["handoff"]["recipe"];
            if !delete {
                recipe
                    .as_object_mut()
                    .unwrap()
                    .remove("delete_workspace_on_failure");
            }
            let before = serde_json::to_vec_pretty(&stored).unwrap();
            fs::write(&path, &before).unwrap();
            let mut restarted = transport(&temp.path().join("staging.json"));
            assert_eq!(restarted.store.stage_durable(&request).unwrap(), receipt);
            assert_eq!(restarted.store.materializer.calls, 0);
            assert_eq!(fs::read(&path).unwrap(), before);
            let state = load_stage(&path).unwrap();
            assert_eq!(
                state
                    .stages
                    .values()
                    .next()
                    .unwrap()
                    .envelope
                    .handoff
                    .recipe
                    .delete_workspace_on_failure,
                delete
            );
        }
    }

    #[test]
    fn obsolete_retention_is_refused_without_rewriting_stages_or_intents() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("staging.json");
        let request = envelope();
        let mut initial = transport(&path);
        initial.store.stage_durable(&request).expect("stage");
        let record = stage_record_path(&path, &request.handoff.run_id);
        let mut stored: serde_json::Value =
            serde_json::from_slice(&fs::read(&record).expect("stored state")).expect("json");
        let staged_envelope = stored["stages"]
            .as_object()
            .expect("stages")
            .values()
            .next()
            .expect("stage")
            .get("envelope")
            .expect("envelope")
            .clone();
        let legacy_recipe = staged_envelope["handoff"]["recipe"].clone();
        let mut legacy_recipe = legacy_recipe.as_object().cloned().expect("recipe object");
        let delete = legacy_recipe
            .remove("delete_workspace_on_failure")
            .expect("current retention field");
        legacy_recipe.insert(
            "preserve_workspace_on_failure".to_string(),
            serde_json::Value::Bool(!delete.as_bool().expect("delete bool")),
        );
        let legacy_recipe = serde_json::Value::Object(legacy_recipe);
        let stage = stored["stages"]
            .as_object_mut()
            .expect("stages")
            .values_mut()
            .next()
            .expect("stage");
        stage["envelope"]["handoff"]["recipe"] = legacy_recipe.clone();
        let mut active_envelope = staged_envelope;
        active_envelope["handoff"]["recipe"] = legacy_recipe.clone();
        stored["intents"] = serde_json::json!({
            "active-intent": {
                "envelope": active_envelope,
                "source_artifact": null,
                "state": "job_submitted",
                "runner_job_id": "runner-job-active"
            }
        });
        let bytes = serde_json::to_vec(&stored).expect("obsolete bytes");
        for section in ["stages", "intents"] {
            let mut invalid = stored.clone();
            let other = if section == "stages" {
                "intents"
            } else {
                "stages"
            };
            invalid[other] = serde_json::json!({});
            let before = serde_json::to_vec(&invalid).unwrap();
            fs::write(&record, &before).unwrap();
            assert!(RunnerStagingStore::open(&path, "runner-1", Materializer::default()).is_ok());
            let mut live_store = transport(&temp.path().join("unused.json"));
            live_store.store.path = path.clone();
            live_store
                .store
                .stage_durable(&request)
                .expect_err("retired recipe refused");
            assert_eq!(live_store.store.materializer.calls, 0);
            assert_eq!(fs::read(&record).unwrap(), before);
        }
        fs::write(&record, &bytes).unwrap();
        assert!(RunnerStagingStore::open(&path, "runner-1", Materializer::default()).is_ok());
        assert!(load_stage(&record).is_err());
        assert_eq!(fs::read(&record).unwrap(), bytes);
    }

    #[test]
    fn rejects_unknown_or_conflicting_retention_fields() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("staging.json");
        let request = envelope();
        let mut initial = transport(&path);
        initial.store.stage_durable(&request).expect("stage");
        let record = stage_record_path(&path, &request.handoff.run_id);
        let mut stored: serde_json::Value =
            serde_json::from_slice(&fs::read(&record).expect("stored state")).expect("json");
        let recipe = stored["stages"]
            .as_object_mut()
            .expect("stages")
            .values_mut()
            .next()
            .expect("stage")
            .get_mut("envelope")
            .expect("envelope")
            .get_mut("handoff")
            .expect("handoff")
            .get_mut("recipe")
            .expect("recipe");
        recipe["preserve_workspace_on_failure"] = serde_json::Value::Bool(true);
        assert!(recipe.get("delete_workspace_on_failure").is_some());
        let conflicting_recipe = recipe.clone();
        let parse_error =
            serde_json::from_value::<crate::lab_staging_controller::LabStagingRecipe>(
                conflicting_recipe.clone(),
            )
            .expect_err("conflicting retention fields must fail");
        assert!(parse_error.to_string().contains("unknown field"));
        fs::write(
            &record,
            serde_json::to_vec(&stored).expect("conflicting bytes"),
        )
        .expect("conflicting store");
        let error = load_stage(&record).expect_err("conflicting retention fields must fail");
        assert_ne!(error.code, homeboy_core::ErrorCode::InternalJsonError);

        let mut unknown_recipe = conflicting_recipe;
        unknown_recipe
            .as_object_mut()
            .expect("unknown recipe object")
            .remove("preserve_workspace_on_failure");
        unknown_recipe["unknown_retention_policy"] = serde_json::Value::Bool(true);
        let unknown_error =
            serde_json::from_value::<crate::lab_staging_controller::LabStagingRecipe>(
                unknown_recipe,
            )
            .expect_err("unknown retention fields must fail");
        assert!(unknown_error.to_string().contains("unknown field"));
    }

    #[derive(Clone)]
    struct ConcurrentMaterializer {
        calls: Arc<AtomicUsize>,
    }

    impl RunnerStagingMaterializer for ConcurrentMaterializer {
        fn materialize(
            &mut self,
            envelope: &RemoteRunnerStagingEnvelope,
        ) -> Result<RunnerStagingArtifacts> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(100));
            Ok(RunnerStagingArtifacts {
                lifecycle_id: format!("lifecycle-{}", envelope.handoff.run_id),
                source_artifact_id: "source-1".to_string(),
                workspace_artifact_id: "workspace-1".to_string(),
                source_artifact: None,
            })
        }
    }

    #[test]
    fn concurrent_admission_materializes_once_and_replays_one_receipt() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("staging.json");
        let request = envelope();
        let mut seed = transport(&path);
        seed.store.stage_durable(&request).expect("seed stage");
        let record = stage_record_path(&path, &request.handoff.run_id);
        let mut stored: serde_json::Value =
            serde_json::from_slice(&fs::read(&record).expect("seed state")).expect("json");
        let stage_key = stored["stages"]
            .as_object()
            .expect("stages")
            .keys()
            .next()
            .expect("stage key")
            .clone();
        let stage = stored["stages"][&stage_key].clone();
        stored["stages"] = serde_json::json!({});
        let mut intents = serde_json::Map::new();
        intents.insert(
            stage_key,
            serde_json::json!({
                "envelope": stage["envelope"].clone(),
                "source_artifact": stage["receipt"]["artifacts"]["source_artifact"].clone(),
                "state": "job_submitted",
                "runner_job_id": stage["receipt"]["handoff"]["runner_job_id"].clone()
            }),
        );
        stored["intents"] = serde_json::Value::Object(intents);
        fs::write(&record, serde_json::to_vec(&stored).expect("pending state"))
            .expect("write intent");
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let request = request.clone();
            let calls = calls.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                let mut store =
                    RunnerStagingStore::open(path, "runner-1", ConcurrentMaterializer { calls })
                        .expect("store");
                barrier.wait();
                store.stage_durable(&request).expect("stage")
            }));
        }
        let first = workers.remove(0).join().expect("first worker");
        let second = workers.remove(0).join().expect("second worker");
        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(&record).expect("final state")).expect("json");
        assert_eq!(state["stages"].as_object().expect("stages").len(), 1);
        assert!(state["intents"].as_object().unwrap().is_empty());
    }

    #[test]
    fn staged_source_artifact_is_retrievable_and_tampering_is_refused() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("staging.json");
        let request = envelope();
        let mut initial = transport(&path);
        let receipt = initial.stage_durable(&request).expect("stage");
        let source = receipt.artifacts.source_artifact.expect("source artifact");
        let extracted = extract_staged_source_artifact(&path, &source, temp.path().join("extract"))
            .expect("extract");
        assert_eq!(
            fs::read(extracted.join("source.bin")).expect("source"),
            b"source package"
        );
        fs::write(
            initial.store.source_artifact_path(&source).expect("path"),
            b"tampered",
        )
        .expect("tamper");
        let mut restarted = transport(&path);
        let error = restarted
            .stage_durable(&request)
            .expect_err("tampered source refusal");
        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::ValidationInvalidArgument
        );
        assert_eq!(restarted.store.materializer.calls, 0);
    }

    #[cfg(unix)]
    #[test]
    fn extraction_materializes_tracked_safe_and_unresolved_symlinks_as_target_text() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp");
        let source_root = temp.path().join("source");
        fs::create_dir_all(source_root.join("nested")).expect("source");
        fs::write(source_root.join("nested/file"), b"safe").expect("file");
        symlink("nested/file", source_root.join("file-link")).expect("file link");
        symlink("missing", source_root.join("missing-link")).expect("missing link");
        for args in [["init"].as_slice(), ["add", "."].as_slice()] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&source_root)
                .status()
                .expect("git")
                .success());
        }
        let transfer = crate::runner_staging_operation::SourceArtifactTransfer::from_directory(
            "source-1",
            &source_root,
        )
        .expect("package");
        let artifact = transfer.descriptor();
        let store = temp.path().join("staging.json");
        let artifact_path = temp
            .path()
            .join("source-artifacts")
            .join(&artifact.artifact_id);
        fs::create_dir_all(artifact_path.parent().expect("parent")).expect("artifact parent");
        fs::write(
            &artifact_path,
            transfer.decode_verified().expect("verified"),
        )
        .expect("artifact");

        let extracted =
            extract_staged_source_artifact(&store, &artifact, temp.path().join("extract"))
                .expect("extract");
        assert_eq!(
            fs::read_link(extracted.join("file-link")).expect("file target"),
            Path::new("nested/file")
        );
        assert_eq!(
            fs::read_link(extracted.join("missing-link")).expect("missing target"),
            Path::new("missing")
        );
        assert_eq!(
            fs::read(extracted.join("file-link")).expect("linked file"),
            b"safe"
        );
        let retried =
            extract_staged_source_artifact(&store, &artifact, temp.path().join("extract"))
                .expect("idempotent retry");
        assert_eq!(retried, extracted);
        fs::remove_file(extracted.join("file-link")).expect("replace link");
        symlink("wrong", extracted.join("file-link")).expect("wrong link");
        extract_staged_source_artifact(&store, &artifact, temp.path().join("extract"))
            .expect("recreate changed link");
        assert_eq!(
            fs::read_link(extracted.join("file-link")).expect("recreated target"),
            Path::new("nested/file")
        );
        fs::remove_file(extracted.join("missing-link")).expect("remove link");
        fs::write(extracted.join("missing-link"), b"conflict").expect("conflicting file");
        assert!(
            extract_staged_source_artifact(&store, &artifact, temp.path().join("extract")).is_err()
        );
    }

    #[test]
    fn missing_staged_source_is_refused_on_receipt_replay() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("staging.json");
        let request = envelope();
        let mut initial = transport(&path);
        let receipt = initial.stage_durable(&request).expect("stage");
        let source = receipt.artifacts.source_artifact.expect("source artifact");
        fs::remove_file(initial.store.source_artifact_path(&source).expect("path"))
            .expect("remove");
        let mut restarted = transport(&path);
        let error = restarted
            .stage_durable(&request)
            .expect_err("missing source refusal");
        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::ValidationInvalidArgument
        );
        assert_eq!(restarted.store.materializer.calls, 0);
    }

    #[test]
    fn source_transfer_rejects_declared_size_above_bound() {
        let mut source = crate::runner_staging_operation::SourceArtifactTransfer::from_bytes(
            "source-package-1",
            b"source package",
        );
        source.size_bytes = crate::runner_staging_operation::MAX_SOURCE_ARTIFACT_BYTES + 1;
        let error = source.decode_verified().expect_err("size bound");
        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::ValidationInvalidArgument
        );
    }

    #[test]
    fn old_runner_capability_endpoint_is_a_typed_pre_mutation_refusal() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).expect("read");
            let body =
                serde_json::json!({ "success": false, "error": "unknown route" }).to_string();
            write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).expect("response");
        });
        let session = production_session(&format!("http://{address}"));
        let error = production_capabilities(&session, None).expect_err("old runner refusal");
        assert_eq!(error.code, homeboy_core::ErrorCode::RunnerCapabilityMissing);
        assert_eq!(
            error.details["missing_capabilities"][0],
            REMOTE_RUNNER_STAGING_CAPABILITY
        );
    }

    #[test]
    fn production_staging_requires_direct_execution_capability() {
        let request = envelope();
        let receipt = RemoteRunnerStagingReceipt {
            schema: REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA.to_string(),
            handoff: DirectLabHandoffReceipt::accepted(&request.handoff, "runner-stage-1"),
            artifacts: RunnerStagingArtifacts {
                lifecycle_id: "lifecycle-1".to_string(),
                source_artifact_id: "source-1".to_string(),
                workspace_artifact_id: "workspace-1".to_string(),
                source_artifact: request
                    .materialization
                    .source_artifact
                    .as_ref()
                    .map(crate::runner_staging_operation::SourceArtifactTransfer::descriptor),
            },
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = staging_endpoint(receipt.clone(), seen.clone(), 2);
        let broker_token = "paired-staging-token".to_string();
        let direct_session = production_session(&endpoint);
        assert!(
            production_capabilities(&direct_session, Some(&broker_token))
                .expect("direct capabilities")
                .contains(&REMOTE_RUNNER_STAGING_CAPABILITY.to_string())
        );
        let mut direct = ProductionRunnerStagingTransport {
            runner_id: "runner-1".to_string(),
            session: direct_session,
            capabilities: vec![
                REMOTE_RUNNER_STAGING_CAPABILITY.to_string(),
                REMOTE_RUNNER_SOURCE_MATERIALIZATION_CAPABILITY.to_string(),
                REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY.to_string(),
            ],
            broker_token: Some(broker_token.clone()),
        };
        let error = submit_remote_runner_staging(&mut direct, &request)
            .expect_err("direct staging has no staged-source consumer");
        assert_eq!(error.code, homeboy_core::ErrorCode::RunnerCapabilityMissing);

        direct.capabilities.push(
            crate::runner_staging_operation::DIRECT_RUNNER_STAGED_EXECUTION_CAPABILITY.to_string(),
        );
        assert_eq!(
            submit_remote_runner_staging(&mut direct, &request).expect("capable direct staging"),
            receipt
        );

        let paths = seen.lock().expect("paths");
        assert_eq!(
            paths
                .iter()
                .map(|(path, _, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["/runner/staging/capabilities", "/runner/staging/direct"]
        );
        assert!(paths.iter().all(|(_, canonical, authorization)| {
            canonical == &broker_token && authorization == &format!("Bearer {broker_token}")
        }));
    }

    #[test]
    fn production_direct_transport_requires_the_direct_execution_capability_and_uses_direct_route()
    {
        let request = envelope();
        let receipt = RemoteRunnerStagingReceipt {
            schema: REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA.to_string(),
            handoff: DirectLabHandoffReceipt::accepted(&request.handoff, "runner-stage-direct"),
            artifacts: RunnerStagingArtifacts {
                lifecycle_id: "lifecycle-direct".to_string(),
                source_artifact_id: "source-direct".to_string(),
                workspace_artifact_id: "workspace-direct".to_string(),
                source_artifact: request
                    .materialization
                    .source_artifact
                    .as_ref()
                    .map(crate::runner_staging_operation::SourceArtifactTransfer::descriptor),
            },
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = staging_endpoint(receipt.clone(), seen.clone(), 1);
        let mut direct = ProductionRunnerStagingTransport {
            runner_id: "runner-1".to_string(),
            session: production_session(&endpoint),
            capabilities: vec![
                REMOTE_RUNNER_STAGING_CAPABILITY.to_string(),
                REMOTE_RUNNER_SOURCE_MATERIALIZATION_CAPABILITY.to_string(),
                REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY.to_string(),
                crate::runner_staging_operation::DIRECT_RUNNER_STAGED_EXECUTION_CAPABILITY
                    .to_string(),
            ],
            broker_token: Some("direct-token".to_string()),
        };

        assert_eq!(
            submit_remote_runner_staging(&mut direct, &request).expect("direct stage"),
            receipt
        );
        assert_eq!(seen.lock().expect("paths")[0].0, "/runner/staging/direct");

        let unsupported = ProductionRunnerStagingTransport {
            runner_id: "runner-1".to_string(),
            session: production_session("http://127.0.0.1:9"),
            capabilities: vec![
                REMOTE_RUNNER_STAGING_CAPABILITY.to_string(),
                REMOTE_RUNNER_SOURCE_MATERIALIZATION_CAPABILITY.to_string(),
                REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY.to_string(),
            ],
            broker_token: None,
        };
        assert!(
            !unsupported.supports_capability(REMOTE_RUNNER_STAGING_CAPABILITY),
            "an old direct daemon must fail capability negotiation before staging"
        );
    }

    fn production_session(endpoint: &str) -> RunnerSession {
        RunnerSession {
            runner_id: "runner-1".to_string(),
            mode: crate::RunnerTunnelMode::DirectSsh,
            role: crate::RunnerSessionRole::Controller,
            server_id: None,
            controller_id: Some("controller-1".to_string()),
            broker_url: None,
            remote_daemon_address: None,
            local_port: None,
            local_url: Some(endpoint.to_string()),
            tunnel_pid: None,
            tunnel_process_start_identity: None,
            proxy_forward: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: None,
            connected_at: "2026-08-05T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    fn staging_endpoint(
        receipt: RemoteRunnerStagingReceipt,
        seen: Arc<Mutex<Vec<(String, String, String)>>>,
        count: usize,
    ) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        std::thread::spawn(move || {
            for _ in 0..count {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = Vec::new();
                let mut chunk = [0; 1024];
                loop {
                    let read = stream.read(&mut chunk).expect("read");
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let header_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .expect("headers")
                    + 4;
                let headers = String::from_utf8(request[..header_end].to_vec()).expect("headers");
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .map(|(_, value)| value.trim())
                    })
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                while request.len() < header_end + length {
                    let read = stream.read(&mut chunk).expect("read body");
                    request.extend_from_slice(&chunk[..read]);
                }
                let first = std::str::from_utf8(&request)
                    .expect("request")
                    .lines()
                    .next()
                    .expect("line");
                let path = first.split_whitespace().nth(1).expect("path").to_string();
                let header = |name: &str| {
                    headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
                                .map(|(_, value)| value.trim().to_string())
                        })
                        .unwrap_or_default()
                };
                seen.lock().expect("record").push((
                    path.clone(),
                    header(homeboy_core::broker_auth::BROKER_TOKEN_HEADER),
                    header("authorization"),
                ));
                let body = if path == "/runner/staging/capabilities" {
                    serde_json::json!({ "success": true, "data": { "body": { "capabilities": [REMOTE_RUNNER_STAGING_CAPABILITY] } } }).to_string()
                } else {
                    serde_json::json!({ "success": true, "data": { "body": { "receipt": receipt } } }).to_string()
                };
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).expect("response");
            }
        });
        format!("http://{address}")
    }

    #[test]
    fn staging_request_timeout_scales_with_payload_within_floor_and_cap() {
        const MIB: usize = 1024 * 1024;
        assert_eq!(staging_request_timeout(0, None), STAGING_TIMEOUT_FLOOR);
        assert_eq!(staging_request_timeout(1, None), Duration::from_secs(62));
        assert_eq!(
            staging_request_timeout(100 * MIB, None),
            Duration::from_secs(260)
        );
        assert_eq!(
            staging_request_timeout(usize::MAX, None),
            STAGING_TIMEOUT_CAP
        );
        // The fixed 10s control-plane deadline is never used for staging.
        assert!(staging_request_timeout(0, None) > Duration::from_secs(10));
    }

    #[test]
    fn staging_request_timeout_honors_a_valid_override_only() {
        assert_eq!(
            staging_request_timeout(0, Some("900")),
            Duration::from_secs(900)
        );
        assert_eq!(
            staging_request_timeout(0, Some(" 5 ")),
            Duration::from_secs(5)
        );
        for invalid in ["0", "-3", "abc", ""] {
            assert_eq!(
                staging_request_timeout(0, Some(invalid)),
                STAGING_TIMEOUT_FLOOR,
                "override {invalid:?} must fall back to the computed deadline"
            );
        }
    }

    #[test]
    fn staging_timeout_context_annotates_timeouts_and_passes_other_errors_through() {
        let timeout = crate::daemon_http_get::daemon_transport_error(
            crate::daemon_http_get::DaemonHttpErrorKind::Timeout,
            "/runner/staging/direct",
            None,
            "query runner daemon",
            "operation timed out",
        );
        let annotated = staging_timeout_context(timeout, 4096, Duration::from_secs(61));
        assert_eq!(
            annotated.details["staging_upload_timeout"],
            serde_json::json!({
                "payload_bytes": 4096,
                "timeout_secs": 61,
                "override_env": RUNNER_STAGING_TIMEOUT_ENV,
            })
        );
        assert!(annotated
            .hints
            .iter()
            .any(|hint| hint.message.contains(RUNNER_STAGING_TIMEOUT_ENV)));

        let refused = crate::daemon_http_get::daemon_transport_error(
            crate::daemon_http_get::DaemonHttpErrorKind::Connect,
            "/runner/staging/direct",
            None,
            "query runner daemon",
            "connection refused",
        );
        let passed = staging_timeout_context(refused, 4096, Duration::from_secs(61));
        assert!(passed.details.get("staging_upload_timeout").is_none());
        assert!(passed.hints.is_empty());
    }

    /// A daemon that answers after `delay` with a successful envelope.
    fn delayed_daemon(delay: Duration) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let Ok(mut stream) = stream else { continue };
                let mut buffer = [0_u8; 16 * 1024];
                let _ = stream.read(&mut buffer);
                std::thread::sleep(delay);
                let body = r#"{"success":true,"data":{"body":{"ok":true}}}"#;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        format!("http://{address}")
    }

    #[test]
    fn direct_daemon_post_uses_the_caller_deadline_not_the_control_plane_default() {
        let payload = serde_json::json!({ "probe": true });

        // Slower than the explicit deadline: times out at the caller's limit.
        let slow = production_session(&delayed_daemon(Duration::from_millis(1500)));
        let error = crate::execution::daemon_api_post_json_for_session_with_timeout(
            &slow,
            "/runner/staging/direct",
            &payload,
            None,
            Duration::from_millis(300),
        )
        .expect_err("a response slower than the deadline must time out");
        assert_eq!(
            error.details.pointer("/daemon_transport_error/kind"),
            Some(&serde_json::json!("timeout"))
        );

        // Within a longer explicit deadline: the same slow daemon succeeds.
        let patient = production_session(&delayed_daemon(Duration::from_millis(1500)));
        let data = crate::execution::daemon_api_post_json_for_session_with_timeout(
            &patient,
            "/runner/staging/direct",
            &payload,
            None,
            Duration::from_secs(5),
        )
        .expect("a slow response within the explicit deadline succeeds");
        assert_eq!(data["body"]["ok"], serde_json::json!(true));
    }
}
