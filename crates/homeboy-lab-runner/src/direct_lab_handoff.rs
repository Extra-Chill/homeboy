//! Versioned runner-owned receipt contract for a detached Lab handoff.
//!
//! The controller may be unavailable after it has compiled a detached workload.
//! This envelope carries the complete immutable staging input to a compatible
//! runner. The transport and runner persistence adapter intentionally live
//! outside this contract so both direct and broker-backed runners use the same
//! idempotency and projection semantics.

use serde::{Deserialize, Serialize};

use homeboy_core::{Error, Result};

use crate::lab_staging_controller::LabStagingRecipe;

pub const DIRECT_LAB_HANDOFF_SCHEMA: &str = "homeboy/direct-lab-handoff/v2";
pub const DIRECT_LAB_HANDOFF_RECEIPT_SCHEMA: &str = "homeboy/direct-lab-handoff-receipt/v1";
pub const DIRECT_LAB_HANDOFF_CAPABILITY: &str = "direct-lab-handoff/v2";

/// Complete runner admission input. It has no controller-local attachment or
/// filesystem references, so the runner can persist it before acknowledging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DirectLabHandoffEnvelope {
    pub schema: String,
    pub run_id: String,
    pub runner_id: String,
    pub idempotency_key: String,
    pub controller_identity: String,
    pub recipe: LabStagingRecipe,
    pub durable_plan: homeboy_agents::agent_task_scheduler::AgentTaskPlan,
}

impl DirectLabHandoffEnvelope {
    pub fn new(
        controller_identity: impl Into<String>,
        recipe: LabStagingRecipe,
        durable_plan: homeboy_agents::agent_task_scheduler::AgentTaskPlan,
    ) -> Self {
        Self {
            schema: DIRECT_LAB_HANDOFF_SCHEMA.to_string(),
            run_id: recipe.run_id.clone(),
            runner_id: recipe.runner_id.clone(),
            idempotency_key: recipe.run_id.clone(),
            controller_identity: controller_identity.into(),
            recipe,
            durable_plan,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != DIRECT_LAB_HANDOFF_SCHEMA
            || self.run_id.trim().is_empty()
            || self.runner_id.trim().is_empty()
            || self.idempotency_key != self.run_id
            || self.controller_identity.trim().is_empty()
            || self.recipe.run_id != self.run_id
            || self.recipe.runner_id != self.runner_id
            || self.durable_plan.plan_id.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "direct_lab_handoff",
                "direct Lab handoff requires its v2 schema, bound identities, run-scoped idempotency key, controller identity, recipe, and durable plan",
                Some(self.run_id.clone()),
                None,
            ));
        }
        self.recipe.validate_for_runner_staging()
    }
}

/// Runner-owned durable acknowledgement. The receipt deliberately names
/// controller projection as deferred: a successful runner admission is not a
/// claim that the unavailable controller has observed it yet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectLabHandoffReceipt {
    pub schema: String,
    pub run_id: String,
    pub runner_id: String,
    pub runner_job_id: String,
    pub idempotency_key: String,
    pub controller_identity: String,
    pub acceptance_state: String,
    pub controller_projection: String,
    pub status_command: String,
    pub cancel_command: String,
    pub evidence_command: String,
}

impl DirectLabHandoffReceipt {
    pub fn accepted(envelope: &DirectLabHandoffEnvelope, runner_job_id: impl Into<String>) -> Self {
        let runner_job_id = runner_job_id.into();
        Self {
            schema: DIRECT_LAB_HANDOFF_RECEIPT_SCHEMA.to_string(),
            run_id: envelope.run_id.clone(),
            runner_id: envelope.runner_id.clone(),
            runner_job_id: runner_job_id.clone(),
            idempotency_key: envelope.idempotency_key.clone(),
            controller_identity: envelope.controller_identity.clone(),
            acceptance_state: "accepted".to_string(),
            controller_projection: "deferred".to_string(),
            // Sealed staging has no runner daemon job yet. The durable run is
            // the status, cancellation, and evidence authority.
            status_command: format!("homeboy agent-task status {}", envelope.run_id),
            cancel_command: format!("homeboy agent-task cancel {}", envelope.run_id),
            evidence_command: format!("homeboy agent-task evidence {} --full", envelope.run_id),
        }
    }

    pub fn validate_for(&self, envelope: &DirectLabHandoffEnvelope) -> Result<()> {
        if self.schema != DIRECT_LAB_HANDOFF_RECEIPT_SCHEMA
            || self.run_id != envelope.run_id
            || self.runner_id != envelope.runner_id
            || self.idempotency_key != envelope.idempotency_key
            || self.controller_identity != envelope.controller_identity
            || self.runner_job_id.trim().is_empty()
            || self.acceptance_state != "accepted"
            || self.controller_projection != "deferred"
        {
            return Err(Error::validation_invalid_argument(
                "direct_lab_handoff_receipt",
                "direct Lab handoff receipt does not prove accepted runner ownership for this envelope",
                Some(envelope.run_id.clone()),
                None,
            ));
        }
        Ok(())
    }
}

/// A runner-side durable store. Implementations must atomically persist a new
/// envelope and receipt, or replay the original receipt for the same key.
pub trait DirectLabHandoffReceiver {
    fn supports_capability(&self, capability: &str) -> bool;
    fn accept_durable(
        &mut self,
        envelope: &DirectLabHandoffEnvelope,
    ) -> Result<DirectLabHandoffReceipt>;
}

/// Fail closed before calling the receiver's mutation boundary. A caller uses
/// this after its normal mutation-free runner capability and connectivity
/// preflight, and before any provider budget can be consumed.
pub fn submit_direct_lab_handoff(
    receiver: &mut impl DirectLabHandoffReceiver,
    envelope: &DirectLabHandoffEnvelope,
) -> Result<DirectLabHandoffReceipt> {
    envelope.validate()?;
    if !receiver.supports_capability(DIRECT_LAB_HANDOFF_CAPABILITY) {
        return Err(Error::validation_invalid_argument(
            "runner_capabilities",
            format!(
                "runner `{}` does not support {DIRECT_LAB_HANDOFF_CAPABILITY}",
                envelope.runner_id
            ),
            Some(envelope.runner_id.clone()),
            None,
        ));
    }
    let receipt = receiver.accept_durable(envelope)?;
    receipt.validate_for(envelope)?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::lab_contract::LabCommandContract;
    use std::collections::HashMap;

    struct Receiver {
        compatible: bool,
        calls: usize,
        receipts: HashMap<String, DirectLabHandoffReceipt>,
    }

    impl DirectLabHandoffReceiver for Receiver {
        fn supports_capability(&self, capability: &str) -> bool {
            self.compatible && capability == DIRECT_LAB_HANDOFF_CAPABILITY
        }

        fn accept_durable(
            &mut self,
            envelope: &DirectLabHandoffEnvelope,
        ) -> Result<DirectLabHandoffReceipt> {
            self.calls += 1;
            Ok(self
                .receipts
                .entry(envelope.idempotency_key.clone())
                .or_insert_with(|| DirectLabHandoffReceipt::accepted(envelope, "runner-job-1"))
                .clone())
        }
    }

    fn envelope() -> DirectLabHandoffEnvelope {
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
            source_path: Some(std::path::Path::new("/source")),
            job_overrides: homeboy_core::lab_offload::LabJobOverrides {
                env: HashMap::new(),
                secret_env_names: Vec::new(),
                workspace_root: None,
            },
            ..crate::LabOffloadRequest::for_test(&args)
        };
        DirectLabHandoffEnvelope::new(
            "controller-identity",
            LabStagingRecipe::from_request("run-1", "runner-1", &request).expect("recipe"),
            homeboy_agents::agent_task_scheduler::AgentTaskPlan::new("plan-1", Vec::new()),
        )
    }

    #[test]
    fn compatible_runner_returns_deferred_receipt_and_replays_idempotently() {
        let envelope = envelope();
        let mut receiver = Receiver {
            compatible: true,
            calls: 0,
            receipts: HashMap::new(),
        };
        let first = submit_direct_lab_handoff(&mut receiver, &envelope).expect("accept");
        let repeated = submit_direct_lab_handoff(&mut receiver, &envelope).expect("replay");
        assert_eq!(first, repeated);
        assert_eq!(receiver.receipts.len(), 1);
        assert_eq!(first.controller_projection, "deferred");
        assert_eq!(first.status_command, "homeboy agent-task status run-1");
        assert_eq!(first.cancel_command, "homeboy agent-task cancel run-1");
        assert_eq!(
            first.evidence_command,
            "homeboy agent-task evidence run-1 --full"
        );
    }

    #[test]
    fn incompatible_runner_is_refused_before_durable_submission() {
        let envelope = envelope();
        let mut receiver = Receiver {
            compatible: false,
            calls: 0,
            receipts: HashMap::new(),
        };
        let error = submit_direct_lab_handoff(&mut receiver, &envelope).expect_err("refuse");
        assert_eq!(
            error.code,
            homeboy_core::ErrorCode::ValidationInvalidArgument
        );
        assert_eq!(receiver.calls, 0);
        assert!(receiver.receipts.is_empty());
    }
}
