//! policy_summary — extracted from contracts.rs.

use crate::code_audit::conventions::AuditFinding;
use std::path::Path;


#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicySummary {
    pub visible_insertions: usize,
    pub visible_new_files: usize,
    pub auto_apply_insertions: usize,
    pub auto_apply_new_files: usize,
    pub blocked_insertions: usize,
    pub blocked_new_files: usize,
    pub preflight_failures: usize,
    /// Fixes dropped in write mode because they had no auto-applicable insertions
    /// (e.g., PlanOnly fixes that would waste CI time without being written).
    pub dropped_plan_only: usize,
}
