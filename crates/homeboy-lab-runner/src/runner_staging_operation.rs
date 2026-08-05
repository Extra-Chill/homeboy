//! Versioned remote admission for a runner-owned staging lifecycle.
//!
//! The controller turns its private source location into sealed source bytes
//! before this boundary. A remote runner receives neither that path nor an
//! instruction to resolve controller state; it durably owns materialization,
//! staging artifacts, and the replayable receipt.

use serde::{Deserialize, Serialize};

use homeboy_core::{Error, Result};

use crate::direct_lab_handoff::{DirectLabHandoffEnvelope, DirectLabHandoffReceipt};

pub const REMOTE_RUNNER_STAGING_SCHEMA: &str = "homeboy/remote-runner-staging/v1";
pub const REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA: &str = "homeboy/remote-runner-staging-receipt/v1";
pub const REMOTE_RUNNER_STAGING_CAPABILITY: &str = "remote-runner-staging/v1";
pub const SEALED_SOURCE_AUTHORITY_SCHEMA: &str = "homeboy/sealed-source-authority/v1";

/// Opaque, self-contained source authority. The producer seals the source
/// payload before transport; its private filesystem location is never part of
/// this contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SealedSourceAuthority {
    pub schema: String,
    pub content_digest: String,
    pub sealed_payload: String,
}

impl SealedSourceAuthority {
    pub fn new(content_digest: impl Into<String>, sealed_payload: impl Into<String>) -> Self {
        Self {
            schema: SEALED_SOURCE_AUTHORITY_SCHEMA.to_string(),
            content_digest: content_digest.into(),
            sealed_payload: sealed_payload.into(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema != SEALED_SOURCE_AUTHORITY_SCHEMA
            || !self.content_digest.starts_with("sha256:")
            || self.content_digest.len() <= "sha256:".len()
            || self.sealed_payload.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "sealed_source_authority",
                "remote staging requires a v1 sealed source payload and SHA-256 content digest",
                None,
                None,
            ));
        }
        Ok(())
    }
}

/// Names the runner-owned materialization target without exposing a filesystem
/// path. The runner maps this opaque key into its own lifecycle store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerMaterializationAuthority {
    pub authority_id: String,
    pub workspace_key: String,
    pub source: SealedSourceAuthority,
}

impl RunnerMaterializationAuthority {
    fn validate(&self) -> Result<()> {
        if self.authority_id.trim().is_empty()
            || self.workspace_key.trim().is_empty()
            || self.workspace_key.contains('/')
            || self.workspace_key.contains('\\')
        {
            return Err(Error::validation_invalid_argument(
                "runner_materialization_authority",
                "remote staging requires opaque runner-owned authority and workspace keys",
                None,
                None,
            ));
        }
        self.source.validate()
    }
}

/// Complete remote operation input. `handoff.recipe.source_path` is always
/// absent: source authority is explicit and sealed above.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RemoteRunnerStagingEnvelope {
    pub schema: String,
    pub handoff: DirectLabHandoffEnvelope,
    pub materialization: RunnerMaterializationAuthority,
}

impl RemoteRunnerStagingEnvelope {
    pub fn from_direct_handoff(
        handoff: &DirectLabHandoffEnvelope,
        materialization: RunnerMaterializationAuthority,
    ) -> Result<Self> {
        handoff.validate()?;
        let mut handoff = handoff.clone();
        handoff.recipe.source_path = None;
        let envelope = Self {
            schema: REMOTE_RUNNER_STAGING_SCHEMA.to_string(),
            handoff,
            materialization,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != REMOTE_RUNNER_STAGING_SCHEMA
            || self.handoff.recipe.source_path.is_some()
            || self.handoff.schema != crate::direct_lab_handoff::DIRECT_LAB_HANDOFF_SCHEMA
            || self.handoff.run_id.trim().is_empty()
            || self.handoff.runner_id.trim().is_empty()
            || self.handoff.idempotency_key != self.handoff.run_id
            || self.handoff.controller_identity.trim().is_empty()
            || self.handoff.recipe.run_id != self.handoff.run_id
            || self.handoff.recipe.runner_id != self.handoff.runner_id
            || self.handoff.durable_plan.plan_id.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "remote_runner_staging",
                "remote staging requires its v1 schema, bound handoff identities, and no controller-local source path",
                Some(self.handoff.run_id.clone()),
                None,
            ));
        }
        self.handoff.recipe.validate_for_runner_staging()?;
        self.materialization.validate()
    }
}

/// Runner-owned artifact identities created before a provider can execute.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerStagingArtifacts {
    pub lifecycle_id: String,
    pub source_artifact_id: String,
    pub workspace_artifact_id: String,
}

/// Durable runner receipt. Replays return this exact value for the same
/// idempotency key, including runner-owned artifact identities.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteRunnerStagingReceipt {
    pub schema: String,
    pub handoff: DirectLabHandoffReceipt,
    pub artifacts: RunnerStagingArtifacts,
}

impl RemoteRunnerStagingReceipt {
    pub fn validate_for(&self, envelope: &RemoteRunnerStagingEnvelope) -> Result<()> {
        if self.schema != REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA
            || self.artifacts.lifecycle_id.trim().is_empty()
            || self.artifacts.source_artifact_id.trim().is_empty()
            || self.artifacts.workspace_artifact_id.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "remote_runner_staging_receipt",
                "remote staging receipt is missing runner-owned lifecycle artifacts",
                Some(envelope.handoff.run_id.clone()),
                None,
            ));
        }
        self.handoff.validate_for(&envelope.handoff)
    }
}

/// Transport/API boundary. The implementation must atomically persist the
/// envelope, runner lifecycle artifacts, and receipt, or replay its receipt.
/// Provider budget consumption belongs after this admission boundary.
pub trait RemoteRunnerStagingTransport {
    fn is_connected(&self) -> bool;
    fn supports_capability(&self, capability: &str) -> bool;
    fn stage_durable(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<RemoteRunnerStagingReceipt>;
}

/// Validates version and runner availability before invoking the mutation
/// boundary, so an unavailable or incompatible runner consumes no provider
/// budget. Idempotency is owned by `stage_durable` and is checked before its
/// provider execution boundary.
pub fn submit_remote_runner_staging(
    transport: &mut impl RemoteRunnerStagingTransport,
    envelope: &RemoteRunnerStagingEnvelope,
) -> Result<RemoteRunnerStagingReceipt> {
    envelope.validate()?;
    if !transport.is_connected() {
        return Err(Error::validation_invalid_argument(
            "runner_connection",
            format!("runner `{}` is disconnected", envelope.handoff.runner_id),
            Some(envelope.handoff.runner_id.clone()),
            None,
        ));
    }
    if !transport.supports_capability(REMOTE_RUNNER_STAGING_CAPABILITY) {
        return Err(Error::validation_invalid_argument(
            "runner_capabilities",
            format!(
                "runner `{}` does not support {REMOTE_RUNNER_STAGING_CAPABILITY}",
                envelope.handoff.runner_id
            ),
            Some(envelope.handoff.runner_id.clone()),
            None,
        ));
    }
    let receipt = transport.stage_durable(envelope)?;
    receipt.validate_for(envelope)?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::direct_lab_handoff::DirectLabHandoffEnvelope;
    use crate::lab_staging_controller::LabStagingRecipe;
    use homeboy_core::lab_contract::LabCommandContract;

    struct Transport {
        connected: bool,
        compatible: bool,
        calls: usize,
        provider_budget: usize,
        receipts: HashMap<String, RemoteRunnerStagingReceipt>,
    }

    impl RemoteRunnerStagingTransport for Transport {
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
            self.calls += 1;
            if let Some(receipt) = self.receipts.get(&envelope.handoff.idempotency_key) {
                return Ok(receipt.clone());
            }
            // This is the runner-side order: persist staging before provider work.
            let receipt = RemoteRunnerStagingReceipt {
                schema: REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA.to_string(),
                handoff: DirectLabHandoffReceipt::accepted(&envelope.handoff, "runner-job-1"),
                artifacts: RunnerStagingArtifacts {
                    lifecycle_id: "runner-lifecycle-1".to_string(),
                    source_artifact_id: "runner-source-1".to_string(),
                    workspace_artifact_id: "runner-workspace-1".to_string(),
                },
            };
            self.receipts
                .insert(envelope.handoff.idempotency_key.clone(), receipt.clone());
            self.provider_budget += 1;
            Ok(receipt)
        }
    }

    fn envelope() -> RemoteRunnerStagingEnvelope {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run".to_string(),
        ];
        let request = crate::LabOffloadRequest {
            placement_decision: homeboy_core::lab_routing::compatibility_placement_decision(
                homeboy_lab_runner_contract::Placement::Lab,
                Some("runner-1"),
                false,
            ),
            command: Some(crate::LabOffloadCommand {
                command: LabCommandContract::portable("agent-task", None, false, &[]),
                required_extensions: Vec::new(),
                required_capabilities: Vec::new(),
                workload: None,
            }),
            normalized_args: &args,
            placement: homeboy_lab_runner_contract::Placement::Lab,
            detach_after_handoff: true,
            source_path: Some(std::path::Path::new("/controller/private/source")),
            job_overrides: homeboy_core::lab_offload::LabJobOverrides {
                env: HashMap::new(),
                secret_env_names: Vec::new(),
                workspace_root: None,
            },
            ..crate::LabOffloadRequest::for_test(&args)
        };
        let handoff = DirectLabHandoffEnvelope::new(
            "controller-identity",
            LabStagingRecipe::from_request("run-1", "runner-1", &request).expect("recipe"),
            homeboy_agents::agent_task_scheduler::AgentTaskPlan::new("plan-1", Vec::new()),
        );
        RemoteRunnerStagingEnvelope::from_direct_handoff(
            &handoff,
            RunnerMaterializationAuthority {
                authority_id: "authority-1".to_string(),
                workspace_key: "run-1".to_string(),
                source: SealedSourceAuthority::new("sha256:source-1", "sealed-source-payload"),
            },
        )
        .expect("sealed envelope")
    }

    fn transport() -> Transport {
        Transport {
            connected: true,
            compatible: true,
            calls: 0,
            provider_budget: 0,
            receipts: HashMap::new(),
        }
    }

    #[test]
    fn compatible_transport_accepts_self_contained_envelope_and_replays_receipt() {
        let envelope = envelope();
        assert!(envelope.handoff.recipe.source_path.is_none());
        assert!(!serde_json::to_string(&envelope)
            .expect("serialize")
            .contains("/controller/private/source"));
        let mut transport = transport();
        let first = submit_remote_runner_staging(&mut transport, &envelope).expect("accept");
        let replay = submit_remote_runner_staging(&mut transport, &envelope).expect("replay");
        assert_eq!(first, replay);
        assert_eq!(transport.receipts.len(), 1);
        assert_eq!(transport.provider_budget, 1);
        assert_eq!(first.artifacts.lifecycle_id, "runner-lifecycle-1");
    }

    #[test]
    fn incompatible_or_disconnected_transport_refuses_before_provider_budget() {
        let envelope = envelope();
        let mut incompatible = Transport {
            compatible: false,
            ..transport()
        };
        assert!(submit_remote_runner_staging(&mut incompatible, &envelope).is_err());
        assert_eq!(incompatible.calls, 0);
        assert_eq!(incompatible.provider_budget, 0);
        let mut disconnected = Transport {
            connected: false,
            ..transport()
        };
        assert!(submit_remote_runner_staging(&mut disconnected, &envelope).is_err());
        assert_eq!(disconnected.calls, 0);
        assert_eq!(disconnected.provider_budget, 0);
    }
}
