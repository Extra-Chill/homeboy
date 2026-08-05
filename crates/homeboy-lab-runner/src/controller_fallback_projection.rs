//! Durable controller fallback and later projection for sealed runner staging.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use homeboy_core::{Error, Result};

use crate::runner_staging_operation::{
    submit_remote_runner_staging, RemoteRunnerStagingEnvelope, RemoteRunnerStagingReceipt,
    RemoteRunnerStagingTransport, RunnerStagingArtifacts,
};

const STORE_SCHEMA: &str = "homeboy/controller-fallback-projection/v1";

/// Controller-visible durable receipt for a runner-owned admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeferredControllerReceipt {
    pub schema: String,
    pub mission_id: String,
    pub runner_receipt: RemoteRunnerStagingReceipt,
    pub controller_projection: String,
}

impl DeferredControllerReceipt {
    fn new(mission_id: impl Into<String>, runner_receipt: RemoteRunnerStagingReceipt) -> Self {
        Self {
            schema: STORE_SCHEMA.to_string(),
            mission_id: mission_id.into(),
            runner_receipt,
            controller_projection: "deferred".to_string(),
        }
    }

    fn validate_for(&self, envelope: &RemoteRunnerStagingEnvelope) -> Result<()> {
        if self.schema != STORE_SCHEMA
            || self.mission_id.trim().is_empty()
            || self.controller_projection != "deferred"
        {
            return Err(Error::validation_invalid_argument(
                "controller_fallback_receipt",
                "deferred controller receipt is malformed",
                Some(envelope.handoff.run_id.clone()),
                None,
            ));
        }
        self.runner_receipt.validate_for(envelope)
    }
}

/// Terminal evidence from the runner-owned store. The controller copies these
/// identities without replacing or re-materializing runner artifacts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerTerminalEvidence {
    pub outcome: String,
    pub artifacts: RunnerStagingArtifacts,
}

/// The one controller-owned finalization projection for a deferred mission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControllerMissionProjection {
    pub mission_id: String,
    pub runner_id: String,
    pub runner_job_id: String,
    pub terminal_outcome: String,
    pub artifacts: RunnerStagingArtifacts,
    pub finalization_owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: String,
    receipts: BTreeMap<String, DeferredControllerReceipt>,
    projections: BTreeMap<String, ControllerMissionProjection>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema: STORE_SCHEMA.to_string(),
            receipts: BTreeMap::new(),
            projections: BTreeMap::new(),
        }
    }
}

/// File-backed controller receipt/projection ledger. Runner admission is
/// atomic in its own store; this ledger only records accepted receipts.
pub struct ControllerFallbackProjectionStore {
    path: PathBuf,
}

impl ControllerFallbackProjectionStore {
    /// Shared controller ledger survives daemon restarts independently of the
    /// runner-owned staging store.
    pub fn open_default() -> Result<Self> {
        Self::open(homeboy_core::paths::homeboy_data()?.join("controller-fallback-projection.json"))
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let store = Self { path: path.into() };
        if store.load()?.schema != STORE_SCHEMA {
            return Err(Error::validation_invalid_argument(
                "controller_fallback_store",
                "unsupported controller fallback projection store schema",
                Some(store.path.display().to_string()),
                None,
            ));
        }
        Ok(store)
    }

    /// Preflight happens inside `submit_remote_runner_staging` before the
    /// transport mutation boundary, so refusals spend no provider budget.
    pub fn submit_detached<T: RemoteRunnerStagingTransport>(
        &self,
        transport: &mut T,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<DeferredControllerReceipt> {
        let receipt = DeferredControllerReceipt::new(
            &envelope.handoff.run_id,
            submit_remote_runner_staging(transport, envelope)?,
        );
        receipt.validate_for(envelope)?;
        let mut state = self.load()?;
        if let Some(existing) = state.receipts.get(&receipt.mission_id) {
            if existing != &receipt {
                return Err(Error::validation_invalid_argument(
                    "idempotency_key",
                    "controller fallback mission is already bound to a different runner receipt",
                    Some(receipt.mission_id),
                    None,
                ));
            }
            return Ok(existing.clone());
        }
        state
            .receipts
            .insert(receipt.mission_id.clone(), receipt.clone());
        self.persist(&state)?;
        Ok(receipt)
    }

    /// Repeated controller startup projects exactly one mission and fails closed
    /// if later runner evidence differs from the first terminal evidence.
    pub fn reconcile_after_controller_restart(
        &self,
        mission_id: &str,
        evidence: RunnerTerminalEvidence,
    ) -> Result<ControllerMissionProjection> {
        if evidence.outcome.trim().is_empty()
            || evidence.artifacts.lifecycle_id.trim().is_empty()
            || evidence.artifacts.source_artifact_id.trim().is_empty()
            || evidence.artifacts.workspace_artifact_id.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "runner_terminal_evidence",
                "runner terminal evidence requires an outcome and all staged artifacts",
                Some(mission_id.to_string()),
                None,
            ));
        }
        let mut state = self.load()?;
        let receipt = state.receipts.get(mission_id).ok_or_else(|| {
            Error::validation_invalid_argument(
                "mission_id",
                "controller cannot project a mission without a deferred runner receipt",
                Some(mission_id.to_string()),
                None,
            )
        })?;
        let projection = ControllerMissionProjection {
            mission_id: mission_id.to_string(),
            runner_id: receipt.runner_receipt.handoff.runner_id.clone(),
            runner_job_id: receipt.runner_receipt.handoff.runner_job_id.clone(),
            terminal_outcome: evidence.outcome,
            artifacts: evidence.artifacts,
            finalization_owner: "controller".to_string(),
        };
        if let Some(existing) = state.projections.get(mission_id) {
            if existing != &projection {
                return Err(Error::validation_invalid_argument(
                    "runner_terminal_evidence",
                    "controller mission already has a different terminal projection",
                    Some(mission_id.to_string()),
                    None,
                ));
            }
            return Ok(existing.clone());
        }
        state
            .projections
            .insert(mission_id.to_string(), projection.clone());
        self.persist(&state)?;
        Ok(projection)
    }

    fn load(&self) -> Result<State> {
        if !self.path.exists() {
            return Ok(State::default());
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

    fn persist(&self, state: &State) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("create {}", parent.display())),
                )
            })?;
        }
        let temporary = self.path.with_extension("tmp");
        let bytes = serde_json::to_vec(state).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize controller fallback projection".to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner_staging_operation::tests_support::{envelope, Transport};
    use tempfile::tempdir;

    fn store() -> ControllerFallbackProjectionStore {
        ControllerFallbackProjectionStore::open(
            tempdir().expect("temp").keep().join("controller.json"),
        )
        .expect("store")
    }

    #[test]
    fn failed_controller_daemon_uses_compatible_runner_once_and_returns_deferred_receipt() {
        let store = store();
        let envelope = envelope();
        let mut runner = Transport::compatible();
        let first = store
            .submit_detached(&mut runner, &envelope)
            .expect("fallback admission");
        let repeated = store
            .submit_detached(&mut runner, &envelope)
            .expect("replay");
        assert_eq!(first, repeated);
        assert_eq!(first.controller_projection, "deferred");
        assert_eq!(runner.provider_budget(), 0);
    }

    #[test]
    fn disconnected_or_incompatible_runner_refuses_before_provider_budget() {
        let envelope = envelope();
        for mut runner in [Transport::incompatible(), Transport::disconnected()] {
            assert!(store().submit_detached(&mut runner, &envelope).is_err());
            assert_eq!(runner.calls(), 0);
            assert_eq!(runner.provider_budget(), 0);
        }
    }

    #[test]
    fn restart_projects_one_controller_mission_and_preserves_runner_artifacts() {
        let store = store();
        let envelope = envelope();
        let mut runner = Transport::compatible();
        let receipt = store
            .submit_detached(&mut runner, &envelope)
            .expect("admit");
        let evidence = RunnerTerminalEvidence {
            outcome: "succeeded".to_string(),
            artifacts: receipt.runner_receipt.artifacts.clone(),
        };
        let projected = store
            .reconcile_after_controller_restart(&receipt.mission_id, evidence.clone())
            .expect("project");
        assert_eq!(projected.artifacts, evidence.artifacts);
        assert_eq!(projected.finalization_owner, "controller");
        assert_eq!(
            store
                .reconcile_after_controller_restart(&receipt.mission_id, evidence)
                .expect("idempotent projection"),
            projected
        );
    }
}
