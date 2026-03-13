//! Audit command output types and builders — owns the unified audit output envelope.
//!
//! All audit sub-workflows (full run, conventions, fix, baseline save, comparison)
//! produce domain-specific results. This module provides the output types and
//! builder functions that assemble results into command-ready output.

use crate::code_audit::{
    baseline, AuditFinding, CodeAuditResult, ConventionReport, DirectoryConvention, Severity,
};
use crate::refactor::{
    auto::{FixResult, FixResultsSummary, PolicySummary},
    AuditRefactorIterationSummary,
};
use serde::Serialize;

use super::run::AuditRunWorkflowResult;

/// Compact CI summary with top findings.
#[derive(Serialize)]
pub struct AuditSummaryOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alignment_score: Option<f32>,
    pub total_findings: usize,
    pub warnings: usize,
    pub info: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_findings: Vec<AuditSummaryFinding>,
    pub exit_code: i32,
}

/// Individual finding in the summary.
#[derive(Serialize)]
pub struct AuditSummaryFinding {
    pub file: String,
    pub convention: String,
    pub kind: AuditFinding,
    pub severity: Severity,
    pub description: String,
    pub suggestion: String,
}

/// Unified output envelope for the audit command.
///
/// Tagged enum — each variant represents a different audit mode.
#[derive(Serialize)]
#[serde(tag = "command")]
pub enum AuditCommandOutput {
    #[serde(rename = "audit")]
    Full(CodeAuditResult),

    #[serde(rename = "audit.conventions")]
    Conventions {
        component_id: String,
        conventions: Vec<ConventionReport>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        directory_conventions: Vec<DirectoryConvention>,
    },

    #[serde(rename = "audit.fix")]
    Fix {
        component_id: String,
        source_path: String,
        status: String,
        #[serde(flatten)]
        fix_result: FixResult,
        #[serde(skip_serializing_if = "Option::is_none")]
        fix_summary: Option<FixResultsSummary>,
        policy_summary: AuditFixPolicySummary,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        iterations: Vec<AuditFixIterationSummary>,
        written: bool,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        hints: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ratchet_summary: Option<AutoRatchetSummary>,
    },

    #[serde(rename = "audit.baseline")]
    BaselineSaved {
        component_id: String,
        path: String,
        findings_count: usize,
        outliers_count: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        alignment_score: Option<f32>,
    },

    #[serde(rename = "audit.compared")]
    Compared {
        #[serde(flatten)]
        result: CodeAuditResult,
        baseline_comparison: baseline::BaselineComparison,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<AuditSummaryOutput>,
    },

    #[serde(rename = "audit.summary")]
    Summary(AuditSummaryOutput),
}

/// Ratchet lifecycle report.
#[derive(Debug, Serialize)]
pub struct AutoRatchetSummary {
    pub resolved_count: usize,
    pub previous_count: usize,
    pub current_count: usize,
    pub baseline_updated: bool,
}

/// Policy filter report.
#[derive(Debug, Serialize)]
pub struct AuditFixPolicySummary {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub selected_only: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<String>,
    pub visible_insertions: usize,
    pub visible_new_files: usize,
    pub auto_apply_insertions: usize,
    pub auto_apply_new_files: usize,
    pub blocked_insertions: usize,
    pub blocked_new_files: usize,
    pub preflight_failures: usize,
}

/// Type alias for iteration summary.
pub type AuditFixIterationSummary = AuditRefactorIterationSummary;

/// Build an audit summary from a result and exit code.
pub fn build_audit_summary(result: &CodeAuditResult, exit_code: i32) -> AuditSummaryOutput {
    let warnings = result
        .findings
        .iter()
        .filter(|f| matches!(f.severity, Severity::Warning))
        .count();
    let info = result
        .findings
        .iter()
        .filter(|f| matches!(f.severity, Severity::Info))
        .count();

    let top_findings = result
        .findings
        .iter()
        .take(20)
        .map(|f| AuditSummaryFinding {
            file: f.file.clone(),
            convention: f.convention.clone(),
            kind: f.kind.clone(),
            severity: f.severity.clone(),
            description: f.description.clone(),
            suggestion: f.suggestion.clone(),
        })
        .collect();

    AuditSummaryOutput {
        alignment_score: result.summary.alignment_score,
        total_findings: result.findings.len(),
        warnings,
        info,
        top_findings,
        exit_code,
    }
}

/// Build fix policy summary from raw policy and CLI args.
pub fn build_fix_policy_summary(
    policy: &PolicySummary,
    only: Vec<String>,
    excluded: Vec<String>,
) -> AuditFixPolicySummary {
    AuditFixPolicySummary {
        selected_only: only,
        excluded,
        visible_insertions: policy.visible_insertions,
        visible_new_files: policy.visible_new_files,
        auto_apply_insertions: policy.auto_apply_insertions,
        auto_apply_new_files: policy.auto_apply_new_files,
        blocked_insertions: policy.blocked_insertions,
        blocked_new_files: policy.blocked_new_files,
        preflight_failures: policy.preflight_failures,
    }
}

/// Build fix hints for blocked/preflight items.
pub fn build_fix_hints(written: bool, summary: &PolicySummary) -> Vec<String> {
    let mut hints = Vec::new();

    if !written && summary.has_blocked_items() {
        hints.push(format!(
            "{} fix(es) are visible but would be blocked on --write because they are safe_with_checks or plan_only.",
            summary.blocked_insertions + summary.blocked_new_files
        ));
    }

    if summary.preflight_failures > 0 {
        hints.push(format!(
            "{} fix(es) failed deterministic preflight checks and will stay preview-only until their validator passes.",
            summary.preflight_failures
        ));
    }

    if written && summary.has_blocked_items() {
        hints.push(format!(
            "Applied only safe_auto fixes. {} fix(es) were left as preview because they need checks or manual review.",
            summary.blocked_insertions + summary.blocked_new_files
        ));
    }

    hints
}

/// Log fix summary to stderr for human-readable output.
pub fn log_fix_summary(result: &FixResult, policy: &PolicySummary, written: bool) {
    let kind_counts = result.finding_counts();
    let total_insertions = result.total_insertions;
    let total_new_files = result.new_files.len();
    let total_skipped = result.skipped.len();

    if total_insertions == 0 && total_new_files == 0 {
        crate::log_status!("fix", "No fixes to apply");
        return;
    }

    let mode = if written { "Applied" } else { "Would apply" };
    crate::log_status!(
        "fix",
        "{mode} {total_insertions} insertion(s) across {} file(s), {} new file(s)",
        result.files_modified,
        total_new_files
    );

    for (kind, count) in &kind_counts {
        crate::log_status!("fix", "  {kind:?}: {count}");
    }

    if total_skipped > 0 {
        crate::log_status!("fix", "Skipped: {total_skipped} file(s)");
    }

    if policy.has_blocked_items() {
        crate::log_status!(
            "fix",
            "Blocked: {} insertion(s), {} new file(s) (safe_with_checks or plan_only)",
            policy.blocked_insertions,
            policy.blocked_new_files
        );
    }

    if policy.preflight_failures > 0 {
        crate::log_status!("fix", "Preflight failures: {}", policy.preflight_failures);
    }
}

/// Build output from a main audit workflow result.
pub fn from_main_workflow(result: AuditRunWorkflowResult) -> (AuditCommandOutput, i32) {
    let exit_code = result.exit_code;
    (result.output, exit_code)
}
