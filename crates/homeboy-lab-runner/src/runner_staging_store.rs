//! Durable runner-side storage and concrete transports for sealed staging.
//!
//! This is deliberately separate from remote process jobs: staging owns the
//! sealed source and workspace authority before a provider-facing job exists.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use homeboy_core::{Error, Result};

use crate::direct_lab_handoff::DirectLabHandoffReceipt;
use crate::runner_staging_operation::{
    RemoteRunnerStagingEnvelope, RemoteRunnerStagingReceipt, RemoteRunnerStagingTransport,
    RunnerStagingArtifacts, REMOTE_RUNNER_STAGING_CAPABILITY, REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA,
};

const STORE_SCHEMA: &str = "homeboy/remote-runner-staging-store/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredStage {
    envelope: RemoteRunnerStagingEnvelope,
    receipt: RemoteRunnerStagingReceipt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StagingStoreState {
    schema: String,
    stages: BTreeMap<String, StoredStage>,
}

impl Default for StagingStoreState {
    fn default() -> Self {
        Self {
            schema: STORE_SCHEMA.to_string(),
            stages: BTreeMap::new(),
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
        envelope.validate()?;
        if envelope.handoff.runner_id != self.runner_id {
            return Err(Error::validation_invalid_argument(
                "runner_authority",
                "sealed staging envelope is not authorized for this runner",
                Some(envelope.handoff.runner_id.clone()),
                None,
            ));
        }
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
            return Ok(existing.receipt.clone());
        }

        // Materialization happens before this receipt is acknowledged and only
        // after the runner, schema, and source authority have all been checked.
        let artifacts = self.materializer.materialize(envelope)?;
        let receipt = RemoteRunnerStagingReceipt {
            schema: REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA.to_string(),
            handoff: DirectLabHandoffReceipt::accepted(
                &envelope.handoff,
                format!("staging-{}", envelope.handoff.run_id),
            ),
            artifacts,
        };
        receipt.validate_for(envelope)?;
        state.stages.insert(
            key.clone(),
            StoredStage {
                envelope: envelope.clone(),
                receipt: receipt.clone(),
            },
        );
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
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("create {}", parent.display())),
                )
            })?;
        }
        let temporary = self.path.with_extension("staging.tmp");
        let bytes = serde_json::to_vec(state).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize remote runner staging store".to_string()),
            )
        })?;
        fs::write(&temporary, bytes).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("write {}", temporary.display())),
            )
        })?;
        fs::rename(&temporary, &self.path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("publish {}", self.path.display())),
            )
        })
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
        self.compatible && capability == REMOTE_RUNNER_STAGING_CAPABILITY
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

#[cfg(test)]
mod tests {
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
}
