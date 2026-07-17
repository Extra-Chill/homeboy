//! Gate-feedback candidate-baseline hook.
//!
//! Before core's worktree-safety logic clears a *dirty* worktree for reuse, it
//! can accept the worktree if the dirt is exactly a promoted agent-task
//! gate-feedback candidate — verified against that candidate's durable patch
//! artifact and recorded diff. That verification is agent-task behavior (it
//! understands the gate-feedback candidate promotion artifact), so it is
//! inverted behind this provider: core owns worktree-safety gating, the
//! agent-task layer owns candidate-baseline verification.
//!
//! With no provider registered (no agent-task subsystem present) the no-op
//! provider verifies nothing, so a dirty worktree is never cleared on the basis
//! of a gate-feedback baseline — the safe default.

use std::path::Path;
use std::sync::Mutex;

use serde_json::Value;

use crate::Result;

/// Verifies that a dirty worktree is exactly the promoted gate-feedback
/// candidate described by its durable agent-task artifact.
pub trait GateFeedbackBaselineProvider: Send + Sync {
    /// Confirm the worktree at `root` matches the gate-feedback candidate in
    /// `baseline`, returning the verified current diff on success. Errors when
    /// the worktree does not match the recorded candidate.
    fn validate_gate_feedback_candidate_baseline(
        &self,
        root: &Path,
        baseline: &Value,
    ) -> Result<String>;
}

struct NoopProvider;

impl GateFeedbackBaselineProvider for NoopProvider {
    fn validate_gate_feedback_candidate_baseline(
        &self,
        _root: &Path,
        _baseline: &Value,
    ) -> Result<String> {
        Err(crate::Error::validation_invalid_argument(
            "gate_feedback_candidate_baseline",
            "gate-feedback candidate-baseline verification is not available: the agent-task subsystem is not present",
            None,
            None,
        ))
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn GateFeedbackBaselineProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn GateFeedbackBaselineProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the gate-feedback candidate-baseline provider. Called once at
/// startup by the agent-task layer.
pub fn register_gate_feedback_baseline_provider(provider: Box<dyn GateFeedbackBaselineProvider>) {
    let mut slot = provider_slot()
        .lock()
        .expect("gate feedback baseline provider lock");
    *slot = Some(provider);
}

/// Verify a gate-feedback candidate baseline via the registered provider (or the
/// no-op provider, which verifies nothing, when the agent-task subsystem is
/// absent).
pub(crate) fn validate_gate_feedback_candidate_baseline(
    root: &Path,
    baseline: &Value,
) -> Result<String> {
    let slot = provider_slot()
        .lock()
        .expect("gate feedback baseline provider lock");
    match slot.as_deref() {
        Some(provider) => provider.validate_gate_feedback_candidate_baseline(root, baseline),
        None => NoopProvider.validate_gate_feedback_candidate_baseline(root, baseline),
    }
}
