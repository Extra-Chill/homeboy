//! Agent-task implementation of the loop-spec proof-validation hook.
//!
//! Proof validation for a materialized loop-spec artifact must confirm the
//! embedded `spec` deserializes to the agent-task loop-spec schema and passes
//! the loop-spec and artifact-reference validators. That is agent-task
//! behavior, provided to core's proof validator through the
//! `LoopSpecValidationProvider` hook so proof validation does not depend on the
//! agent-task subsystem directly.

use serde_json::Value;

use crate::agent_task_controller_service::{validate_loop_spec, AgentTaskRepoLoopSpec};
use crate::agent_task_repo_loop_compile::validate_repo_loop_artifact_references;
use homeboy_core::proof::loop_spec_validation::{
    register_loop_spec_validation_provider, LoopSpecValidationDiagnostic,
    LoopSpecValidationProvider,
};

struct AgentTaskLoopSpecValidationProvider;

impl LoopSpecValidationProvider for AgentTaskLoopSpecValidationProvider {
    fn validate_materialized_loop_spec(&self, spec: &Value) -> Vec<LoopSpecValidationDiagnostic> {
        let Ok(spec) = serde_json::from_value::<AgentTaskRepoLoopSpec>(spec.clone()) else {
            return vec![LoopSpecValidationDiagnostic {
                code: "invalid_loop_spec_json".to_string(),
                message: "materialized controller spec does not match the loop spec schema"
                    .to_string(),
            }];
        };
        let mut diagnostics = Vec::new();
        if let Err(error) = validate_loop_spec(&spec) {
            diagnostics.push(LoopSpecValidationDiagnostic {
                code: "invalid_controller_loop_spec".to_string(),
                message: error.message,
            });
        }
        if let Err(error) = validate_repo_loop_artifact_references(&spec) {
            diagnostics.push(LoopSpecValidationDiagnostic {
                code: "invalid_artifact_references".to_string(),
                message: error.message,
            });
        }
        diagnostics
    }
}

/// Register the agent-task loop-spec validation provider. Called once at startup
/// so core's proof validator can validate materialized loop-spec artifacts
/// without depending on the agent-task subsystem.
pub fn register() {
    register_loop_spec_validation_provider(Box::new(AgentTaskLoopSpecValidationProvider));
}
