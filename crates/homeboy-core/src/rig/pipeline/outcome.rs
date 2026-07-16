//! Pipeline outcome types aggregated by the runner.

use serde::Serialize;

/// Result of one pipeline step.
#[derive(Debug, Clone, Serialize)]
pub struct PipelineStepOutcome {
    /// Step kind (`service`, `command`, `symlink`, `check`).
    pub kind: String,
    /// Human-readable label for the step.
    pub label: String,
    /// `"pass"`, `"fail"`, or `"skip"`.
    pub status: String,
    /// Error message when `status = "fail"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineOutcome {
    pub name: String,
    pub steps: Vec<PipelineStepOutcome>,
    pub passed: usize,
    pub failed: usize,
}

impl PipelineOutcome {
    pub fn is_success(&self) -> bool {
        self.failed == 0
    }
}
