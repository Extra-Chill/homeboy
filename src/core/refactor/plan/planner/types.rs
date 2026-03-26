//! types — extracted from planner.rs.

use crate::component::Component;
use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use serde::Serialize;
use std::path::{Path, PathBuf};
use crate::code_audit::CodeAuditResult;
use crate::engine::run_dir::{self, RunDir};
use crate::engine::undo::UndoSnapshot;
use crate::Error;
use std::collections::{BTreeSet, HashSet};
use super::super::verify::AuditConvergenceScoring;


/// Name of the env var pointing to previous command output files.
///
/// When set, `--from audit` reads the cached audit result instead of
/// re-running the audit. The action sets this during `run-homeboy-commands.sh`
/// and it persists across steps via `GITHUB_ENV`.
pub(crate) const OUTPUT_DIR_ENV: &str = "HOMEBOY_OUTPUT_DIR";

#[derive(Debug, Clone)]
pub struct RefactorPlanRequest {
    pub component: Component,
    pub root: PathBuf,
    pub sources: Vec<String>,
    pub changed_since: Option<String>,
    pub only: Vec<crate::code_audit::AuditFinding>,
    pub exclude: Vec<crate::code_audit::AuditFinding>,
    pub settings: Vec<(String, String)>,
    pub lint: LintSourceOptions,
    pub test: TestSourceOptions,
    pub write: bool,
    /// Skip the clean working tree check (for CI or when you know what you're doing)
    pub force: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LintSourceOptions {
    pub selected_files: Option<Vec<String>>,
    pub file: Option<String>,
    pub glob: Option<String>,
    pub errors_only: bool,
    pub sniffs: Option<String>,
    pub exclude_sniffs: Option<String>,
    pub category: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TestSourceOptions {
    pub selected_files: Option<Vec<String>>,
    pub skip_lint: bool,
    pub script_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefactorPlan {
    pub component_id: String,
    pub source_path: String,
    pub sources: Vec<String>,
    pub dry_run: bool,
    pub applied: bool,
    pub merge_strategy: String,
    pub proposals: Vec<FixProposal>,
    pub stages: Vec<PlanStageSummary>,
    pub plan_totals: PlanTotals,
    pub overlaps: Vec<PlanOverlap>,
    pub files_modified: usize,
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
    pub warnings: Vec<String>,
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanStageSummary {
    pub stage: String,
    pub planned: bool,
    pub applied: bool,
    pub fixes_proposed: usize,
    pub files_modified: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_findings: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlanOverlap {
    pub file: String,
    pub earlier_stage: String,
    pub later_stage: String,
    pub resolution: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanTotals {
    pub stages_with_proposals: usize,
    pub total_fixes_proposed: usize,
    pub total_files_selected: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FixProposal {
    pub source: String,
    pub file: String,
    pub rule_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

pub(crate) struct PlannedStage {
    source: String,
    summary: PlanStageSummary,
    fix_results: Vec<FixApplied>,
}
