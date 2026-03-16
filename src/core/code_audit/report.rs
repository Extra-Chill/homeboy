//! Audit command output types and builders — owns the unified audit output envelope.
//!
//! All audit sub-workflows (full run, conventions, fix, baseline save, comparison)
//! produce domain-specific results. This module provides the output types and
//! builder functions that assemble results into command-ready output.

use std::collections::BTreeMap;
use std::path::Path;

use crate::code_audit::{
    baseline, AuditFinding, CodeAuditResult, ConventionReport, DirectoryConvention, Severity,
};
use crate::refactor::{
    auto::{FixResult, FixResultsSummary, FixSafetyTier, PolicySummary},
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixability: Option<AuditFixability>,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        fixability: Option<AuditFixability>,
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

/// Fixability metadata for audit findings — computed without running `--fix`.
///
/// This tells CI wrappers how many findings have automated fixes available
/// and at what safety tier, without actually generating or applying the fixes.
#[derive(Debug, Serialize)]
pub struct AuditFixability {
    /// Total findings that have any kind of automated fix.
    pub fixable_count: usize,
    /// Findings with `Safe` tier — can be auto-applied (preflight runs when applicable).
    pub safe_count: usize,
    /// Findings with `PlanOnly` tier — preview only, needs manual review.
    pub plan_only_count: usize,
    /// Breakdown by finding kind.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub by_kind: BTreeMap<String, FixabilityKindBreakdown>,
}

/// Per-finding-kind fixability breakdown.
#[derive(Debug, Serialize)]
pub struct FixabilityKindBreakdown {
    pub total: usize,
    pub safe: usize,
    pub plan_only: usize,
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
        fixability: None,
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
            "{} fix(es) are visible but would be blocked on --write (preflight failed or plan-only).",
            summary.blocked_insertions + summary.blocked_new_files
        ));
    }

    if summary.preflight_failures > 0 {
        hints.push(format!(
            "{} fix(es) failed preflight checks and will stay preview-only until their validator passes.",
            summary.preflight_failures
        ));
    }

    if written && summary.has_blocked_items() {
        hints.push(format!(
            "Applied safe fixes. {} fix(es) were left as preview (preflight failed or plan-only).",
            summary.blocked_insertions + summary.blocked_new_files
        ));
    }

    hints
}

/// Compute fixability metadata from an audit result without applying fixes.
///
/// Runs the fix generator in dry-run mode and counts how many findings
/// have automated fixes at each safety tier. This is cheap — no writes,
/// no convergence loop, just planning + policy annotation.
pub fn compute_fixability(result: &CodeAuditResult) -> Option<AuditFixability> {
    let source_path = Path::new(&result.source_path);
    if !source_path.is_dir() {
        return None;
    }

    // Generate fix plan (dry-run — never writes)
    let mut fix_result = crate::refactor::plan::generate::generate_audit_fixes(result, source_path);

    if fix_result.fixes.is_empty() && fix_result.new_files.is_empty() {
        return None;
    }

    // Apply policy annotation (dry-run mode: write=false, no filtering)
    let policy = crate::refactor::auto::FixPolicy {
        only: None,
        exclude: Vec::new(),
    };
    let context = crate::refactor::auto::PreflightContext { root: source_path };
    crate::refactor::auto::apply_fix_policy(&mut fix_result, false, &policy, &context);

    // Count by safety tier
    let mut safe_count = 0usize;
    let mut plan_only = 0usize;
    let mut by_kind: BTreeMap<String, FixabilityKindBreakdown> = BTreeMap::new();

    for fix in &fix_result.fixes {
        for insertion in &fix.insertions {
            let kind_key = format!("{:?}", insertion.finding).to_lowercase();
            let entry = by_kind.entry(kind_key).or_insert(FixabilityKindBreakdown {
                total: 0,
                safe: 0,
                plan_only: 0,
            });
            entry.total += 1;

            match insertion.safety_tier {
                FixSafetyTier::Safe => {
                    safe_count += 1;
                    entry.safe += 1;
                }
                FixSafetyTier::PlanOnly => {
                    plan_only += 1;
                    entry.plan_only += 1;
                }
            }
        }
    }

    for new_file in &fix_result.new_files {
        let kind_key = format!("{:?}", new_file.finding).to_lowercase();
        let entry = by_kind.entry(kind_key).or_insert(FixabilityKindBreakdown {
            total: 0,
            safe: 0,
            plan_only: 0,
        });
        entry.total += 1;

        match new_file.safety_tier {
            FixSafetyTier::Safe => {
                safe_count += 1;
                entry.safe += 1;
            }
            FixSafetyTier::PlanOnly => {
                plan_only += 1;
                entry.plan_only += 1;
            }
        }
    }

    let fixable_count = safe_count + plan_only;

    Some(AuditFixability {
        fixable_count,
        safe_count,
        plan_only_count: plan_only,
        by_kind,
    })
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

#[cfg(test)]
#[path = "../../../tests/core/code_audit/report_test.rs"]
mod report_test;
