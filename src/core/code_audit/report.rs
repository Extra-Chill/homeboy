//! Audit command output types and builders — owns the unified audit output envelope.
//!
//! All audit sub-workflows (full run, conventions, fix, baseline save, comparison)
//! produce domain-specific results. This module provides the output types and
//! builder functions that assemble results into command-ready output.

use std::collections::BTreeMap;
use std::path::Path;

use crate::code_audit::{
    baseline, AuditFinding, CodeAuditResult, ConventionReport, DirectoryConvention,
    FindingConfidence, Severity,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<AuditChangedSinceSummary>,
    pub exit_code: i32,
}

/// Changed-since audit classification.
///
/// `introduced_findings` are findings not present in the selected baseline and
/// therefore block the PR. `contextual_findings` are existing findings in the
/// touched/impact scope that are shown for context only.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct AuditChangedSinceSummary {
    pub introduced_findings: usize,
    pub contextual_findings: usize,
}

/// Individual finding in the summary.
#[derive(Serialize)]
pub struct AuditSummaryFinding {
    pub file: String,
    pub convention: String,
    pub kind: AuditFinding,
    pub confidence: FindingConfidence,
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
        passed: bool,
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
        passed: bool,
        #[serde(flatten)]
        result: CodeAuditResult,
        baseline_comparison: baseline::BaselineComparison,
        #[serde(skip_serializing_if = "Option::is_none")]
        changed_since: Option<AuditChangedSinceSummary>,
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
/// versus manual-only fixes. Use `refactor --from audit --write` to apply
/// automation-eligible fixes.
#[derive(Debug, Serialize)]
pub struct AuditFixability {
    /// Total findings that have any kind of automated fix.
    pub fixable_count: usize,
    /// Findings eligible for automated `refactor --from ...` execution.
    pub automated_count: usize,
    /// Findings that are manual-only and require explicit command execution.
    pub manual_only_count: usize,
    /// Breakdown by finding kind.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub by_kind: BTreeMap<String, FixabilityKindBreakdown>,
}

/// Per-finding-kind fixability breakdown.
#[derive(Debug, Serialize)]
pub struct FixabilityKindBreakdown {
    pub total: usize,
    pub automated: usize,
    pub manual_only: usize,
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
            confidence: f.kind.confidence(),
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
        changed_since: None,
        exit_code,
    }
}

pub fn build_changed_since_summary(
    result: &CodeAuditResult,
    comparison: &baseline::BaselineComparison,
) -> AuditChangedSinceSummary {
    let introduced_findings = comparison.new_items.len();
    AuditChangedSinceSummary {
        introduced_findings,
        contextual_findings: result.findings.len().saturating_sub(introduced_findings),
    }
}

/// Serialize an [`AuditFinding`] variant to its serde snake_case key.
///
/// This must match the `#[serde(rename_all = "snake_case")]` on the enum so that
/// `fixability.by_kind` keys align with the finding group keys in JSON output.
/// Using `format!("{:?}", ...)` would produce Debug PascalCase (e.g. `compilerwarning`)
/// which doesn't match the serde output (`compiler_warning`).
pub(crate) fn finding_kind_key(finding: &AuditFinding) -> String {
    serde_json::to_value(finding)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", finding).to_lowercase())
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

    if !result.findings.is_empty()
        && result.findings.iter().all(|finding| {
            matches!(
                finding.kind,
                AuditFinding::GodFile | AuditFinding::HighItemCount | AuditFinding::DirectorySprawl
            )
        })
    {
        // Structural decompose planning can be much more expensive than audit
        // reporting. Keep filtered read-only audits fast; `homeboy refactor`
        // remains the explicit path for planning those changes.
        return None;
    }

    // Generate fix plan (dry-run — never writes)
    let mut fix_result = crate::refactor::plan::generate::generate_audit_fixes(
        result,
        source_path,
        &crate::refactor::auto::FixPolicy::default(),
    );

    if fix_result.fixes.is_empty() && fix_result.new_files.is_empty() {
        return None;
    }

    // Apply policy annotation (dry-run mode: write=false, no filtering)
    let policy = crate::refactor::auto::FixPolicy {
        only: None,
        exclude: Vec::new(),
    };
    let _ = source_path;
    crate::refactor::auto::apply_fix_policy(&mut fix_result, false, &policy);

    // Count by automation eligibility
    let mut automated_count = 0usize;
    let mut manual_only = 0usize;
    let mut by_kind: BTreeMap<String, FixabilityKindBreakdown> = BTreeMap::new();
    let mut count_fixability = |finding: &AuditFinding, auto_apply: bool| {
        let kind_key = finding_kind_key(finding);
        let entry = by_kind.entry(kind_key).or_insert(FixabilityKindBreakdown {
            total: 0,
            automated: 0,
            manual_only: 0,
        });
        entry.total += 1;

        if auto_apply {
            automated_count += 1;
            entry.automated += 1;
        } else {
            manual_only += 1;
            entry.manual_only += 1;
        }
    };

    for fix in &fix_result.fixes {
        for insertion in &fix.insertions {
            count_fixability(&insertion.finding, insertion.auto_apply);
        }
    }

    for new_file in &fix_result.new_files {
        count_fixability(&new_file.finding, new_file.auto_apply);
    }

    let fixable_count = automated_count + manual_only;

    Some(AuditFixability {
        fixable_count,
        automated_count,
        manual_only_count: manual_only,
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
