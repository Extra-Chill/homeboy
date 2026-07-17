//! Loop-spec proof-validation hook.
//!
//! Proof validation accepts a materialized agent-task loop-spec artifact
//! (`homeboy/agent-task-loop-spec-materialization/v1`) and must confirm the
//! embedded `spec` deserializes to the loop-spec schema and passes the
//! agent-task loop-spec + artifact-reference validators. That validation is
//! agent-task behavior — it owns the `AgentTaskRepoLoopSpec` schema and its
//! rules — so it is inverted behind this provider: core owns proof dispatch and
//! diagnostic reporting, the agent-task layer owns loop-spec validation.
//!
//! With no provider registered (no agent-task subsystem present) the no-op
//! provider reports that the loop-spec schema cannot be validated, so a proof
//! carrying that schema is flagged rather than silently accepted.

use std::sync::Mutex;

use serde_json::Value;

/// A single loop-spec validation finding: a stable diagnostic code and a
/// human-readable message. The caller attaches the JSON path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopSpecValidationDiagnostic {
    pub code: String,
    pub message: String,
}

/// Validates a materialized agent-task loop-spec `spec` value.
pub trait LoopSpecValidationProvider: Send + Sync {
    /// Validate the embedded loop-spec `spec` JSON, returning one diagnostic per
    /// problem found. An empty vec means the spec is valid.
    fn validate_materialized_loop_spec(&self, spec: &Value) -> Vec<LoopSpecValidationDiagnostic>;
}

struct NoopProvider;

impl LoopSpecValidationProvider for NoopProvider {
    fn validate_materialized_loop_spec(&self, _spec: &Value) -> Vec<LoopSpecValidationDiagnostic> {
        vec![LoopSpecValidationDiagnostic {
            code: "loop_spec_validation_unavailable".to_string(),
            message: "agent-task loop-spec validation is not available: the agent-task subsystem is not present".to_string(),
        }]
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn LoopSpecValidationProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn LoopSpecValidationProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the loop-spec validation provider. Called once at startup by the
/// agent-task layer.
pub fn register_loop_spec_validation_provider(provider: Box<dyn LoopSpecValidationProvider>) {
    let mut slot = provider_slot()
        .lock()
        .expect("loop spec validation provider lock");
    *slot = Some(provider);
}

/// Validate a materialized loop-spec `spec` value via the registered provider
/// (or the no-op provider when the agent-task subsystem is absent).
pub(crate) fn validate_materialized_loop_spec(spec: &Value) -> Vec<LoopSpecValidationDiagnostic> {
    let slot = provider_slot()
        .lock()
        .expect("loop spec validation provider lock");
    match slot.as_deref() {
        Some(provider) => provider.validate_materialized_loop_spec(spec),
        None => NoopProvider.validate_materialized_loop_spec(spec),
    }
}
