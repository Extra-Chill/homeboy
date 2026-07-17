//! Shared refactor/autofix result types for homeboy.
//!
//! Behavior-free data describing the outcome of applying refactor fixes
//! (`refactor --from lint/test/audit --write`). These live below core so
//! consumers — the refactor engine that produces them and report layers like the
//! extension lint/test commands that carry them — can share the vocabulary
//! without depending on the refactor engine's behavior.

use serde::{Deserialize, Serialize};

/// Applied-change reporting for a refactor run. `refactor --from lint/test/audit
/// --write` are the entrypoints for fixes; this keeps applied-change reporting in
/// one place so commands don't invent parallel output models.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedRefactor {
    pub files_modified: usize,
    pub rerun_recommended: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
}

/// Aggregated summary of the fixes applied in a refactor run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixResultsSummary {
    pub fixes_applied: usize,
    pub files_modified: usize,
    pub rules: Vec<RuleFixCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primitives: Vec<PrimitiveFixCount>,
}

/// Count of fixes applied for a single rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleFixCount {
    pub rule: String,
    pub count: usize,
}

/// Count of fixes applied for a single refactor primitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimitiveFixCount {
    pub primitive: String,
    pub count: usize,
}
