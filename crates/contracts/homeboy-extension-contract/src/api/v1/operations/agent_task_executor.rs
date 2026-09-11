//! Typed Extension API v1 registration surface for agent-task executors.
//!
//! An extension declares executors under `agent_runtimes[].agent_task_executors[]`.
//! Those declarations are provider-shaped data, so the values themselves stay
//! private to the manifest and to the agent-task layer that resolves them. What
//! this operation exposes is the registration fact: which executor identities an
//! installed, compatible extension advertises, and whether each one is
//! resolvable.
//!
//! Everything an executor needs in order to *run* — argv, commands, runtime and
//! extension paths, secret and environment declarations, materialization
//! contracts, and provider-specific options — is deliberately absent from these
//! types. A caller that can read this inventory learns identity, ownership,
//! compatibility, and readiness, and nothing that would let it reconstruct an
//! execution.

use serde::{Deserialize, Serialize};

use super::{ExtensionApiOperationFailure, ExtensionApiVersion};

pub const AGENT_TASK_EXECUTOR_CAPABILITY_PREFIX: &str = "agent-task-executor.";
pub const EXTENSION_API_AGENT_TASK_EXECUTOR_INVENTORY_REQUEST_SCHEMA: &str =
    "homeboy/extension-api-agent-task-executor-inventory-request/v1";
pub const EXTENSION_API_AGENT_TASK_EXECUTOR_INVENTORY_RESPONSE_SCHEMA: &str =
    "homeboy/extension-api-agent-task-executor-inventory-response/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiAgentTaskExecutorInventoryRequest {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
}

/// Whether one declared executor can be selected.
///
/// `Duplicate` is not a variant of `Invalid`: a duplicate declaration can be
/// perfectly well-formed and still unusable, and the two cases have different
/// remediations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiAgentTaskExecutorValidation {
    Valid,
    Invalid,
    Duplicate,
}

/// Why an inventory entry is not resolvable. Carried alongside the descriptor
/// so a caller never has to parse a human-readable message to branch.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionApiAgentTaskExecutorDiagnosticKind {
    InvalidDeclaration,
    DuplicateId,
    ExtensionIncompatible,
    ExtensionInvalid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiAgentTaskExecutorDiagnostic {
    pub kind: ExtensionApiAgentTaskExecutorDiagnosticKind,
    pub message: String,
}

/// The safe projection of one declared executor.
///
/// `id` and `backend` are the selection identity. `owning_extension` and
/// `runtime_id` are the registration identity — the pair an install-time gate
/// checks a declaration against, so a provider from another extension or
/// runtime can never satisfy it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiAgentTaskExecutorDescriptor {
    pub id: String,
    pub backend: String,
    pub owning_extension: String,
    pub runtime_id: String,
    /// Declared capability tokens, which are provider-agnostic labels rather
    /// than execution inputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Whether the executor declares a readiness probe at all. The probe's
    /// command stays private; only its existence and budget are public.
    pub declares_readiness_probe: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness_timeout_ms: Option<u64>,
    pub resolvable: bool,
    pub validation: ExtensionApiAgentTaskExecutorValidation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<ExtensionApiAgentTaskExecutorDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionApiAgentTaskExecutorInventoryResponse {
    pub schema: String,
    pub api_version: ExtensionApiVersion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executors: Vec<ExtensionApiAgentTaskExecutorDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ExtensionApiOperationFailure>,
}

impl ExtensionApiAgentTaskExecutorInventoryResponse {
    /// Registration identities an installed extension actually advertises.
    ///
    /// Install and repair compare declared executors against this set, so the
    /// gate and ordinary dispatch agree on what "discoverable" means.
    pub fn resolvable(&self) -> impl Iterator<Item = &ExtensionApiAgentTaskExecutorDescriptor> {
        self.executors.iter().filter(|executor| executor.resolvable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> ExtensionApiAgentTaskExecutorDescriptor {
        ExtensionApiAgentTaskExecutorDescriptor {
            id: "opencode.agent-task-executor".to_string(),
            backend: "opencode".to_string(),
            owning_extension: "homeboy-opencode".to_string(),
            runtime_id: "opencode".to_string(),
            capabilities: vec!["workspace_permission_root/v1".to_string()],
            declares_readiness_probe: true,
            readiness_timeout_ms: Some(20_000),
            resolvable: true,
            validation: ExtensionApiAgentTaskExecutorValidation::Valid,
            diagnostic: None,
        }
    }

    #[test]
    fn a_descriptor_exposes_registration_identity_and_no_execution_inputs() {
        assert_eq!(
            serde_json::to_value(descriptor()).expect("descriptor JSON"),
            serde_json::json!({
                "id": "opencode.agent-task-executor",
                "backend": "opencode",
                "owning_extension": "homeboy-opencode",
                "runtime_id": "opencode",
                "capabilities": ["workspace_permission_root/v1"],
                "declares_readiness_probe": true,
                "readiness_timeout_ms": 20000,
                "resolvable": true,
                "validation": "valid"
            })
        );
    }

    #[test]
    fn an_unresolvable_entry_keeps_its_identity_and_states_why() {
        let mut duplicate = descriptor();
        duplicate.resolvable = false;
        duplicate.validation = ExtensionApiAgentTaskExecutorValidation::Duplicate;
        duplicate.diagnostic = Some(ExtensionApiAgentTaskExecutorDiagnostic {
            kind: ExtensionApiAgentTaskExecutorDiagnosticKind::DuplicateId,
            message: "declared by multiple sources".to_string(),
        });

        let encoded = serde_json::to_value(&duplicate).expect("descriptor JSON");

        // Identity survives so the conflict is reportable, but the entry is not
        // selectable and says which class of problem it is.
        assert_eq!(encoded["id"], "opencode.agent-task-executor");
        assert_eq!(encoded["resolvable"], false);
        assert_eq!(encoded["validation"], "duplicate");
        assert_eq!(encoded["diagnostic"]["kind"], "duplicate_id");
    }

    #[test]
    fn only_resolvable_entries_are_offered_for_selection() {
        let mut duplicate = descriptor();
        duplicate.id = "duplicate".to_string();
        duplicate.resolvable = false;
        duplicate.validation = ExtensionApiAgentTaskExecutorValidation::Duplicate;

        let response = ExtensionApiAgentTaskExecutorInventoryResponse {
            schema: EXTENSION_API_AGENT_TASK_EXECUTOR_INVENTORY_RESPONSE_SCHEMA.to_string(),
            api_version: crate::api::v1::EXTENSION_API_V1,
            executors: vec![descriptor(), duplicate],
            failure: None,
        };

        let resolvable: Vec<_> = response
            .resolvable()
            .map(|executor| executor.id.as_str())
            .collect();

        assert_eq!(resolvable, ["opencode.agent-task-executor"]);
    }
}
