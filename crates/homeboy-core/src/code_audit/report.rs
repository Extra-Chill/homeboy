//! Audit command output types and builders — owns the unified audit output envelope.
//!
//! All audit sub-workflows (full run, conventions, fix, baseline save, comparison)
//! produce domain-specific results. This module provides the output types and
//! builder functions that assemble results into command-ready output.

use crate::code_audit::findings::finding_kind_key;
use std::collections::BTreeMap;
use std::path::Path;

use crate::code_audit::{
    baseline, AuditFinding, CodeAuditResult, ConventionReport, DirectoryConvention,
    FindingConfidence, Severity,
};
use crate::extension::ExtensionPhaseTiming;
use homeboy_finding::HomeboyFinding;
use serde::Serialize;
use serde_json::Value;

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
    pub finding_groups: Vec<AuditSummaryGroup>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_findings: Vec<HomeboyFinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixability: Option<AuditFixability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<AuditChangedSinceSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_filtering: Option<AuditBaselineFilteringSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unbaselined_findings: Vec<baseline::NewFinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_phase_timings: Vec<ExtensionPhaseTiming>,
    pub exit_code: i32,
}

/// Aggregated finding bucket for compact summaries.
#[derive(Serialize)]
pub struct AuditSummaryGroup {
    pub kind: String,
    pub count: usize,
    pub warnings: usize,
    pub info: usize,
    pub confidence: FindingConfidence,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sample_files: Vec<String>,
    pub drilldown_command: String,
}

#[derive(Default)]
struct AuditSummarySeverityCounts {
    warnings: usize,
    info: usize,
}

struct AuditSummaryGroupAccumulator {
    kind: AuditFinding,
    count: usize,
    severities: AuditSummarySeverityCounts,
    sample_files: Vec<String>,
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

/// Baseline filtering counters for compact audit summaries.
///
/// `total_findings` on [`AuditSummaryOutput`] is the current findings count.
/// These counters make the baseline-filtered blocking scope explicit: known
/// findings may be present while only unbaselined findings affect the exit code.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct AuditBaselineFilteringSummary {
    pub current_findings: usize,
    pub unbaselined_findings: usize,
    pub baseline_known_findings: usize,
    pub baseline_filtered_findings: usize,
    pub baseline_total_findings: usize,
    pub resolved_findings: usize,
    pub drift_delta: i64,
    pub drift_increased: bool,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extension_phase_timings: Vec<ExtensionPhaseTiming>,
        #[serde(
            rename = "_homeboy_actionable",
            skip_serializing_if = "Option::is_none"
        )]
        actionable: Option<Value>,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extension_phase_timings: Vec<ExtensionPhaseTiming>,
        #[serde(
            rename = "_homeboy_actionable",
            skip_serializing_if = "Option::is_none"
        )]
        actionable: Option<Value>,
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

    let mut top_finding_refs: Vec<_> = result.findings.iter().collect();
    top_finding_refs.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
            .then_with(|| finding_kind_key(&a.kind).cmp(&finding_kind_key(&b.kind)))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.description.cmp(&b.description))
    });

    let top_findings = top_finding_refs
        .into_iter()
        .take(20)
        .map(HomeboyFinding::from)
        .collect();
    let finding_groups = build_finding_groups(result);

    AuditSummaryOutput {
        alignment_score: result.summary.alignment_score,
        total_findings: result.findings.len(),
        warnings,
        info,
        finding_groups,
        top_findings,
        fixability: None,
        changed_since: None,
        baseline_filtering: None,
        unbaselined_findings: Vec::new(),
        extension_phase_timings: Vec::new(),
        exit_code,
    }
}

pub fn build_unbaselined_finding_summary(
    comparison: &baseline::BaselineComparison,
) -> Vec<baseline::NewFinding> {
    comparison.new_items.iter().take(20).cloned().collect()
}

pub fn build_baseline_filtering_summary(
    result: &CodeAuditResult,
    comparison: &baseline::BaselineComparison,
    baseline: &baseline::AuditBaseline,
) -> AuditBaselineFilteringSummary {
    let current_findings = result.findings.len();
    let unbaselined_findings = comparison.new_items.len();
    let baseline_known_findings = current_findings.saturating_sub(unbaselined_findings);

    AuditBaselineFilteringSummary {
        current_findings,
        unbaselined_findings,
        baseline_known_findings,
        baseline_filtered_findings: baseline_known_findings,
        baseline_total_findings: baseline.item_count,
        resolved_findings: comparison.resolved_fingerprints.len(),
        drift_delta: comparison.delta,
        drift_increased: comparison.drift_increased,
    }
}

fn severity_rank(severity: &Severity) -> u8 {
    match severity {
        Severity::Warning => 0,
        Severity::Info => 1,
    }
}

fn build_finding_groups(result: &CodeAuditResult) -> Vec<AuditSummaryGroup> {
    let mut groups: BTreeMap<String, AuditSummaryGroupAccumulator> = BTreeMap::new();

    for finding in &result.findings {
        let kind = finding_kind_key(&finding.kind);
        let group = groups
            .entry(kind)
            .or_insert_with(|| AuditSummaryGroupAccumulator {
                kind: finding.kind.clone(),
                count: 0,
                severities: AuditSummarySeverityCounts::default(),
                sample_files: Vec::new(),
            });

        group.count += 1;
        match finding.severity {
            Severity::Warning => group.severities.warnings += 1,
            Severity::Info => group.severities.info += 1,
        }
        if group.sample_files.len() < 5 && !group.sample_files.contains(&finding.file) {
            group.sample_files.push(finding.file.clone());
        }
    }

    let mut grouped: Vec<_> = groups
        .into_iter()
        .map(|(kind, group)| AuditSummaryGroup {
            drilldown_command: format!(
                "homeboy review audit {} --only {}",
                result.component_id, kind
            ),
            confidence: crate::code_audit::findings::finding_confidence(&group.kind),
            kind,
            count: group.count,
            warnings: group.severities.warnings,
            info: group.severities.info,
            sample_files: group.sample_files,
        })
        .collect();

    grouped.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.kind.cmp(&b.kind)));
    grouped
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

/// Compute fixability metadata from an audit result without applying fixes.
///
/// Runs the fix generator in dry-run mode and counts how many findings
/// have automated fixes at each safety tier. This is cheap — no writes,
/// no convergence loop, just planning + policy annotation.
pub fn compute_fixability(result: &CodeAuditResult) -> Option<AuditFixability> {
    compute_fixability_impl(result, None)
}

pub(crate) fn compute_fixability_with_analysis(
    result: &CodeAuditResult,
    analysis: &crate::code_audit::AuditAnalysisContext,
) -> Option<AuditFixability> {
    compute_fixability_impl(result, Some(analysis))
}

fn compute_fixability_impl(
    result: &CodeAuditResult,
    analysis: Option<&crate::code_audit::AuditAnalysisContext>,
) -> Option<AuditFixability> {
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

    // Plan the fixes and project each into an automation verdict. The refactor
    // engine owns the planning (generate + policy annotation); audit only needs
    // the per-fix (finding, auto_apply) tally. When no refactor provider is
    // registered, this yields no verdicts and fixability is unavailable.
    let fingerprints = analysis
        .map(|analysis| analysis.fingerprints.as_slice())
        .unwrap_or(&[]);
    let verdicts = super::fixability_provider::plan_fixability(
        result,
        &source_path.to_string_lossy(),
        fingerprints,
    );

    if verdicts.is_empty() {
        return None;
    }

    // Count by automation eligibility
    let mut automated_count = 0usize;
    let mut manual_only = 0usize;
    let mut by_kind: BTreeMap<String, FixabilityKindBreakdown> = BTreeMap::new();
    for verdict in &verdicts {
        let kind_key = finding_kind_key(&verdict.finding);
        let entry = by_kind.entry(kind_key).or_insert(FixabilityKindBreakdown {
            total: 0,
            automated: 0,
            manual_only: 0,
        });
        entry.total += 1;

        if verdict.auto_apply {
            automated_count += 1;
            entry.automated += 1;
        } else {
            manual_only += 1;
            entry.manual_only += 1;
        }
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
#[path = "../../../../tests/core/code_audit/report_test.rs"]
mod report_test;
