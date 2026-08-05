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
use crate::{broker_submit_token_for_runner, RunnerSession, RunnerTunnelMode};

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
        )?,
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
            )?
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
        })
    }
}

struct ProductionStagingProvider;

impl homeboy_core::daemon::runner_staging::RunnerStagingProvider for ProductionStagingProvider {
    fn capabilities(&self, _runner_id: &str) -> Result<Vec<String>> {
        Ok(vec![REMOTE_RUNNER_STAGING_CAPABILITY.to_string()])
    }

    fn stage(&self, request: serde_json::Value) -> Result<serde_json::Value> {
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
        let receipt = transport.stage_durable(&request.envelope)?;
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
    use std::sync::{Arc, Mutex};

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
            },
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = staging_endpoint(receipt.clone(), seen.clone(), 2);
        let direct_session = production_session(RunnerTunnelMode::DirectSsh, &endpoint);
        let mut direct = ProductionRunnerStagingTransport {
            runner_id: "runner-1".to_string(),
            session: direct_session,
            capabilities: vec![REMOTE_RUNNER_STAGING_CAPABILITY.to_string()],
        };
        assert_eq!(
            submit_remote_runner_staging(&mut direct, &request).expect("direct"),
            receipt
        );

        let reverse_session = production_session(RunnerTunnelMode::Reverse, &endpoint);
        let mut reverse = ProductionRunnerStagingTransport {
            runner_id: "runner-1".to_string(),
            session: reverse_session,
            capabilities: vec![REMOTE_RUNNER_STAGING_CAPABILITY.to_string()],
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
