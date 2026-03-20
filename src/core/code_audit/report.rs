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
use crate::refactor::auto::FixSafetyTier;
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
    Full {
        #[serde(flatten)]
        result: CodeAuditResult,
        #[serde(skip_serializing_if = "Option::is_none")]
        fixability: Option<AuditFixability>,
    },

    #[serde(rename = "audit.conventions")]
    Conventions {
        component_id: String,
        conventions: Vec<ConventionReport>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        directory_conventions: Vec<DirectoryConvention>,
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

/// Fixability metadata for audit findings — computed without applying fixes.
///
/// Tells CI wrappers how many findings have automated fixes available
/// and at what safety tier. Use `refactor --from audit --write` to apply.
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

/// Build output from a main audit workflow result.
pub fn from_main_workflow(result: AuditRunWorkflowResult) -> (AuditCommandOutput, i32) {
    let exit_code = result.exit_code;
    (result.output, exit_code)
}

#[cfg(test)]
#[path = "../../../tests/core/code_audit/report_test.rs"]
mod report_test;
