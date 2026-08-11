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
    REMOTE_RUNNER_STAGING_CAPABILITY, REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA,
};
use crate::{broker_submit_token_for_runner, RunnerSession, RunnerTunnelMode};

const STORE_SCHEMA: &str = "homeboy/remote-runner-staging-store/v1";

/// Read the verified source package named by a staging receipt. Execution code
/// uses this before extracting its own workspace; it never receives controller
/// paths or the transfer's inline content.
pub fn read_staged_source_artifact(
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
    fs::create_dir_all(&root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?;
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
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(error.to_string(), Some(parent.display().to_string()))
            })?;
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
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(error.to_string(), Some(parent.display().to_string()))
            })?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        #[cfg(not(unix))]
        return Err(Error::validation_invalid_argument(
            "source_package",
            "source package symlinks require a Unix runner",
            Some(entry.path.clone()),
            None,
        ));
    }
    Ok(root)
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
    source_artifact: RunnerSourceArtifact,
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
        let state = store.load()?;
        if state.schema != STORE_SCHEMA {
            return Err(Error::validation_invalid_argument(
                "staging_store",
                "unsupported remote runner staging store schema",
                Some(store.path.display().to_string()),
                None,
            ));
        }
        Ok(store)
    }

    pub fn stage_durable(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<RemoteRunnerStagingReceipt> {
        self.stage_durable_with_job_id(envelope, format!("staging-{}", envelope.handoff.run_id))
    }

    pub fn stage_durable_with_job_id(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
        runner_job_id: impl Into<String>,
    ) -> Result<RemoteRunnerStagingReceipt> {
        let runner_job_id = runner_job_id.into();
        self.stage_durable_with_submit(envelope, |_| Ok(runner_job_id))
    }

    /// Lock-held admission state machine: intent -> source_ready -> job_submitted
    /// -> receipt. `submit` must use the handoff idempotency key.
    pub fn stage_durable_with_submit(
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
        let transfer = envelope
            .materialization
            .source_artifact
            .as_ref()
            .expect("envelope validation requires source artifact");
        let bytes = transfer.decode_verified()?;
        let source_artifact = transfer.descriptor();
        let _lock = self.admission_lock()?;
        let mut state = self.load()?;
        let key = &envelope.handoff.idempotency_key;
        if let Some(existing) = state.stages.get(key) {
            if existing.envelope != *envelope {
                return Err(Error::validation_invalid_argument(
                    "idempotency_key",
                    "sealed staging key is already bound to a different envelope",
                    Some(key.clone()),
                    None,
                ));
            }
            self.verify_source_artifact(&source_artifact)?;
            return Ok(existing.receipt.clone());
        }

        state
            .intents
            .entry(key.clone())
            .or_insert_with(|| StagingIntent {
                envelope: envelope.clone(),
                source_artifact: source_artifact.clone(),
                state: "intent_created".to_string(),
                runner_job_id: None,
            });
        self.persist(&state)?;

        // The lock covers materialization and receipt publication. Concurrent
        // requests therefore observe one durable admission, not two writes.
        self.persist_source_artifact(&source_artifact, &bytes)?;
        // Source bytes are now durable before any queue entry can become claimable.
        state.intents.get_mut(key).expect("intent").state = "source_ready".to_string();
        self.persist(&state)?;
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
                self.persist(&state)?;
                job_id
            }
        };
        let artifacts = self.materializer.materialize(envelope)?;
        let receipt = RemoteRunnerStagingReceipt {
            schema: REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA.to_string(),
            handoff: DirectLabHandoffReceipt::accepted(&envelope.handoff, runner_job_id),
            artifacts: RunnerStagingArtifacts {
                source_artifact: Some(source_artifact),
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
        self.persist(&state)?;
        Ok(receipt)
    }

    fn load(&self) -> Result<StagingStoreState> {
        if !self.path.exists() {
            return Ok(StagingStoreState::default());
        }
        serde_json::from_slice(&fs::read(&self.path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read {}", self.path.display())),
            )
        })?)
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("parse {}", self.path.display())),
            )
        })
    }

    fn persist(&self, state: &StagingStoreState) -> Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
        let bytes = serde_json::to_vec(state).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize remote runner staging store".to_string()),
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
                Some(format!(
                    "write staging temporary for {}",
                    self.path.display()
                )),
            )
        })?;
        temporary.as_file().sync_all().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "sync staging temporary for {}",
                    self.path.display()
                )),
            )
        })?;
        temporary.persist(&self.path).map_err(|error| {
            Error::internal_io(
                error.error.to_string(),
                Some(format!("publish {}", self.path.display())),
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

    /// Return verified, runner-owned package bytes for the later execution
    /// layer. The receipt descriptor remains the only authority it needs.
    pub fn read_source_artifact(&self, artifact: &RunnerSourceArtifact) -> Result<Vec<u8>> {
        read_staged_source_artifact(&self.path, artifact)
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
        let parent = path.parent().expect("source artifact has parent");
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
        let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create source artifact in {}", parent.display())),
            )
        })?;
        temporary.write_all(bytes).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("write source artifact {}", artifact.artifact_id)),
            )
        })?;
        temporary.as_file().sync_all().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("sync source artifact {}", artifact.artifact_id)),
            )
        })?;
        temporary.persist_noclobber(&path).map_err(|error| {
            Error::internal_io(
                error.error.to_string(),
                Some(format!("publish source artifact {}", artifact.artifact_id)),
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

    fn admission_lock(&self) -> Result<File> {
        const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
        const LOCK_RETRY: Duration = Duration::from_millis(25);
        let lock_path = self.path.with_extension("lock");
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
                    "runner_id": self.runner_id,
                    "timeout_ms": LOCK_TIMEOUT.as_millis(),
                });
                error.retryable = Some(true);
                return Err(error);
            }
            std::thread::sleep(LOCK_RETRY);
        }
    }
}

/// Versioned request carried by either direct daemon or reverse broker transport.
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

/// Direct and reverse routing share the same runner-owned store. The channel is
/// intentionally not part of the persisted receipt, so either channel replays
/// the exact accepted result after a reconnect.
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
                REMOTE_RUNNER_STAGING_CAPABILITY | REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY
            ) || (cfg!(unix) && capability == REMOTE_RUNNER_SOURCE_ARTIFACT_SYMLINK_CAPABILITY))
    }
    fn stage_durable(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<RemoteRunnerStagingReceipt> {
        self.store.stage_durable(envelope)
    }
}

pub type DirectRunnerStagingTransport<M> = RunnerStagingTransport<M>;
pub type ReverseRunnerStagingTransport<M> = RunnerStagingTransport<M>;

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
    let capabilities = production_capabilities(&session)?;
    Ok(ProductionRunnerStagingTransport {
        runner_id: runner_id.to_string(),
        session,
        capabilities,
    })
}

pub struct ProductionRunnerStagingTransport {
    runner_id: String,
    session: RunnerSession,
    capabilities: Vec<String>,
}

impl RemoteRunnerStagingTransport for ProductionRunnerStagingTransport {
    fn is_connected(&self) -> bool {
        true
    }

    fn supports_capability(&self, capability: &str) -> bool {
        self.capabilities
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
        let response = match self.session.mode {
            RunnerTunnelMode::DirectSsh => {
                let data = crate::execution::daemon_api_post_json_for_session(
                    &self.session,
                    "/runner/staging",
                    &body,
                )?;
                crate::execution::canonical_daemon_body(&data, "sealed staging daemon response")
                    .cloned()?
            }
            RunnerTunnelMode::Reverse => {
                let broker_url = self.session.broker_url.as_deref().ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "runner_connection",
                        "reverse runner has no broker URL",
                        Some(self.runner_id.clone()),
                        None,
                    )
                })?;
                let client = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .map_err(|error| {
                        Error::internal_unexpected(format!("build staging broker client: {error}"))
                    })?;
                crate::broker_http::post_json(
                    &client,
                    broker_url,
                    "/runner/staging",
                    body,
                    "submit sealed runner staging",
                    broker_submit_token_for_runner(&self.runner_id)?.as_deref(),
                )?
            }
        };
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

fn production_capabilities(session: &RunnerSession) -> Result<Vec<String>> {
    let runner_id = &session.runner_id;
    let response = match session.mode {
        RunnerTunnelMode::DirectSsh => crate::execution::daemon_api_post_json_for_session(
            session,
            "/runner/staging/capabilities",
            &serde_json::json!({ "runner_id": runner_id }),
        )
        .map_err(|error| staging_capability_error(runner_id, error))?,
        RunnerTunnelMode::Reverse => {
            let broker_url = session.broker_url.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "runner_connection",
                    "reverse runner has no broker URL",
                    Some(runner_id.clone()),
                    None,
                )
            })?;
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .map_err(|error| {
                    Error::internal_unexpected(format!("build staging broker client: {error}"))
                })?;
            crate::broker_http::post_json(
                &client,
                broker_url,
                "/runner/staging/capabilities",
                serde_json::json!({ "runner_id": runner_id }),
                "read sealed staging capabilities",
                broker_submit_token_for_runner(runner_id)?.as_deref(),
            )
            .map_err(|error| staging_capability_error(runner_id, error))?
        }
    };
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
    fn capabilities(&self, _runner_id: &str) -> Result<Vec<String>> {
        let mut capabilities = vec![
            REMOTE_RUNNER_STAGING_CAPABILITY.to_string(),
            REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY.to_string(),
        ];
        if cfg!(unix) {
            capabilities.push(REMOTE_RUNNER_SOURCE_ARTIFACT_SYMLINK_CAPABILITY.to_string());
        }
        Ok(capabilities)
    }

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
            .expect("validated source artifact")
            .descriptor();
        let receipt = transport
            .store
            .stage_durable_with_submit(&request.envelope, |envelope| {
                let job = jobs.submit_remote_runner_job(
                    homeboy_core::api_jobs::RemoteRunnerJobRequest {
                        runner_id: envelope.handoff.runner_id.clone(),
                        project_id: None,
                        operation: "runner_staged_execution".to_string(),
                        command: envelope.handoff.recipe.normalized_args.clone(),
                        cwd: None,
                        env: std::collections::HashMap::new(),
                        secret_env_names: Vec::new(),
                        secret_env_plan: Default::default(),
                        env_materialization: None,
                        capture_patch: envelope.handoff.recipe.capture_patch,
                        source_snapshot: None,
                        path_materialization_plan: None,
                        require_paths: Vec::new(),
                        extension_env_providers: Vec::new(),
                        lab_runner_workload: None,
                        lifecycle: None,
                        workspace_claim_binding: None,
                        workspace_owner_lease: None,
                        metadata: Some(serde_json::json!({
                            "submission_key": envelope.handoff.idempotency_key,
                            "staged_source_artifact": source_artifact,
                        })),
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
    fn direct_and_reverse_transport_replay_one_runner_owned_receipt() {
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
        let mut reverse: ReverseRunnerStagingTransport<_> = transport(&path);
        let replay =
            submit_remote_runner_staging(&mut reverse, &request.envelope).expect("reverse replay");
        assert_eq!(first, replay);
        assert_eq!(reverse.store.materializer.calls, 0);
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
        symlink("nested\\file", source_root.join("file-link")).expect("file link");
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
        let session = production_session(RunnerTunnelMode::DirectSsh, &format!("http://{address}"));
        let error = production_capabilities(&session).expect_err("old runner refusal");
        assert_eq!(error.code, homeboy_core::ErrorCode::RunnerCapabilityMissing);
        assert_eq!(
            error.details["missing_capabilities"][0],
            REMOTE_RUNNER_STAGING_CAPABILITY
        );
    }

    #[test]
    fn production_direct_and_reverse_transports_dispatch_the_versioned_endpoint() {
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
        let direct_session = production_session(RunnerTunnelMode::DirectSsh, &endpoint);
        let mut direct = ProductionRunnerStagingTransport {
            runner_id: "runner-1".to_string(),
            session: direct_session,
            capabilities: vec![
                REMOTE_RUNNER_STAGING_CAPABILITY.to_string(),
                REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY.to_string(),
            ],
        };
        assert_eq!(
            submit_remote_runner_staging(&mut direct, &request).expect("direct"),
            receipt
        );

        let reverse_session = production_session(RunnerTunnelMode::Reverse, &endpoint);
        let mut reverse = ProductionRunnerStagingTransport {
            runner_id: "runner-1".to_string(),
            session: reverse_session,
            capabilities: vec![
                REMOTE_RUNNER_STAGING_CAPABILITY.to_string(),
                REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY.to_string(),
            ],
        };
        assert_eq!(
            submit_remote_runner_staging(&mut reverse, &request).expect("reverse"),
            receipt
        );
        let paths = seen.lock().expect("paths");
        assert_eq!(paths.as_slice(), ["/runner/staging", "/runner/staging"]);
    }

    fn production_session(mode: RunnerTunnelMode, endpoint: &str) -> RunnerSession {
        RunnerSession {
            runner_id: "runner-1".to_string(),
            mode: mode.clone(),
            role: crate::RunnerSessionRole::Controller,
            server_id: None,
            controller_id: Some("controller-1".to_string()),
            broker_url: (mode == RunnerTunnelMode::Reverse).then(|| endpoint.to_string()),
            remote_daemon_address: None,
            local_port: None,
            local_url: (mode == RunnerTunnelMode::DirectSsh).then(|| endpoint.to_string()),
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
        seen: Arc<Mutex<Vec<String>>>,
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
                let headers = std::str::from_utf8(&request[..header_end]).expect("headers");
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
                seen.lock()
                    .expect("record")
                    .push(first.split_whitespace().nth(1).expect("path").to_string());
                let body = serde_json::json!({ "success": true, "data": { "body": { "receipt": receipt } } }).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).expect("response");
            }
        });
        format!("http://{address}")
    }
}
