//! Audit workflow orchestration — runs audit, handles fix/baseline/comparison modes.
//!
//! Mirrors `core/extension/lint/run.rs` and `core/extension/test/run.rs` — the command
//! layer provides CLI args, this module owns all business logic and returns structured results.

use crate::core::code_audit::{self, baseline, AuditTiming, AuditWithAnalysis, CodeAuditResult};
use crate::core::git;
use std::collections::HashSet;
use std::path::Path;

use super::report::{self, AuditCommandOutput};

/// Arguments for the main audit workflow — populated by the command layer from CLI flags.
///
/// Fixes are owned by `homeboy refactor --from audit --write`.
/// The audit command is read-only: it finds problems but does not fix them.
#[derive(Debug, Clone)]
pub struct AuditRunWorkflowArgs {
    pub component_id: String,
    pub source_path: String,
    pub reference_paths: Vec<String>,
    pub conventions: bool,
    pub only_kinds: Vec<code_audit::AuditFinding>,
    pub exclude_kinds: Vec<code_audit::AuditFinding>,
    pub only_labels: Vec<String>,
    pub exclude_labels: Vec<String>,
    pub extension_overrides: Vec<String>,
    pub baseline_flags: crate::core::engine::baseline::BaselineFlags,
    pub changed_since: Option<String>,
    pub precomputed_changed_files: Option<Vec<String>>,
    pub json_summary: bool,
    pub include_fixability: bool,
}

/// Result of the main audit workflow — ready for report assembly.
pub struct AuditRunWorkflowResult {
    pub output: AuditCommandOutput,
    pub exit_code: i32,
    pub findings: Vec<code_audit::Finding>,
    pub timing: AuditTiming,
}

/// Run the main audit workflow.
pub fn run_main_audit_workflow(
    args: AuditRunWorkflowArgs,
) -> crate::core::Result<AuditRunWorkflowResult> {
    // Run audit — scoped or full
    let result = run_audit(&args)?;

    // Early return: no-change shortcut already handled by run_audit returning None
    let audit = match result {
        Some(audit) => audit,
        None => {
            return Ok(audit_run_workflow_result(
                AuditCommandOutput::Full {
                    passed: true,
                    result: CodeAuditResult {
                        component_id: args.component_id,
                        source_path: args.source_path,
                        summary: code_audit::AuditSummary {
                            files_scanned: 0,
                            conventions_detected: 0,
                            outliers_found: 0,
                            alignment_score: None,
                            files_skipped: 0,
                            warnings: vec![],
                        },
                        conventions: vec![],
                        directory_conventions: vec![],
                        findings: vec![],
                        duplicate_groups: vec![],
                    },
                    fixability: None,
                    extension_phase_timings: Vec::new(),
                },
                0,
                Vec::new(),
                AuditTiming::default(),
            ));
        }
    };
    let mut result = audit.result;
    let analysis = audit.analysis;
    let timing = audit.timing;

    // --conventions: just show conventions
    if args.conventions {
        let findings = Vec::new();
        return Ok(audit_run_workflow_result(
            AuditCommandOutput::Conventions {
                component_id: result.component_id,
                conventions: result.conventions,
                directory_conventions: result.directory_conventions,
            },
            0,
            findings,
            timing,
        ));
    }

    // --baseline: save current state. Saved baselines record the *full* finding
    // set so they remain a complete reference; --only / --exclude intentionally
    // do not narrow what gets persisted.
    if args.baseline_flags.baseline {
        return run_baseline_save(result, &args, timing);
    }

    // --only / --exclude: scope this run's findings before comparison and
    // report assembly. The CLI flags are parsed in `parse_finding_kinds` and
    // surfaced here as `only_kinds` / `exclude_kinds`; any filter activity
    // also recomputes `summary.outliers_found` so the exit-code logic in
    // `default_audit_exit_code` reflects the filtered view.
    apply_finding_filters(&mut result, &args.only_kinds, &args.exclude_kinds);

    if args.changed_since.is_some() {
        scope_convention_outliers_to_findings(&mut result);
    }

    // Default: compare against baseline or return full result
    run_comparison_workflow(result, &analysis, &args, timing)
}

fn audit_run_workflow_result(
    output: AuditCommandOutput,
    exit_code: i32,
    findings: Vec<code_audit::Finding>,
    timing: AuditTiming,
) -> AuditRunWorkflowResult {
    AuditRunWorkflowResult {
        output,
        exit_code,
        findings,
        timing,
    }
}

/// Filter `result.findings` by kind allow/deny lists and refresh
/// `summary.outliers_found` so downstream exit-code and fixability logic
/// agrees with what the user sees.
///
/// No-op when both lists are empty (the common case).
fn apply_finding_filters(
    result: &mut CodeAuditResult,
    only_kinds: &[code_audit::AuditFinding],
    exclude_kinds: &[code_audit::AuditFinding],
) {
    if only_kinds.is_empty() && exclude_kinds.is_empty() {
        return;
    }

    result.findings.retain(|f| {
        let allowed = only_kinds.is_empty() || only_kinds.contains(&f.kind);
        let denied = exclude_kinds.contains(&f.kind);
        allowed && !denied
    });

    // Recompute outliers_found from the filtered set. Findings count is the
    // closest proxy available without re-running the per-convention check —
    // this keeps `default_audit_exit_code(...) -> outliers_found > 0`
    // honest under filtering.
    result.summary.outliers_found = result.findings.len();
}

/// Keep the diagnostic convention report aligned with scoped actionable findings.
///
/// Scoped audits already filter `result.findings` to changed files plus impact
/// call sites. The full convention report is still useful for `audit
/// --conventions`, but PR-facing machine output treats `conventions[].outliers`
/// as actionable too. For changed-since audit output, only retain convention
/// outlier deviations that correspond to the scoped finding set.
fn scope_convention_outliers_to_findings(result: &mut CodeAuditResult) {
    let scoped_findings: HashSet<(String, String, code_audit::AuditFinding)> = result
        .findings
        .iter()
        .map(|finding| {
            (
                finding.convention.clone(),
                finding.file.clone(),
                finding.kind.clone(),
            )
        })
        .collect();

    for convention in &mut result.conventions {
        convention.outliers.retain_mut(|outlier| {
            outlier.deviations.retain(|deviation| {
                scoped_findings.contains(&(
                    convention.name.clone(),
                    outlier.file.clone(),
                    deviation.kind.clone(),
                ))
            });

            !outlier.deviations.is_empty()
        });
    }

    result.summary.outliers_found = result.findings.len();
}

/// Run the audit scan (scoped or full). Returns None if changed-since found no files.
fn run_audit(args: &AuditRunWorkflowArgs) -> crate::core::Result<Option<AuditWithAnalysis>> {
    let plan = if args.baseline_flags.baseline {
        code_audit::AuditExecutionPlan::full()
    } else {
        code_audit::AuditExecutionPlan::from_filters(&args.only_kinds, &args.exclude_kinds)
    };

    if let Some(ref git_ref) = args.changed_since {
        let changed = changed_files_for_scope(args, git_ref)?;
        if changed.is_empty() {
            crate::log_status!("audit", "No files changed since {}", git_ref);
            return Ok(None);
        }
        Ok(Some(code_audit::audit_path_scoped_with_plan_and_analysis(
            &args.component_id,
            &args.source_path,
            &changed,
            Some(git_ref),
            &plan,
            &args.reference_paths,
            &args.extension_overrides,
        )?))
    } else {
        Ok(Some(code_audit::audit_path_with_id_with_plan_and_analysis(
            &args.component_id,
            &args.source_path,
            &plan,
            &args.reference_paths,
            &args.extension_overrides,
        )?))
    }
}

/// Baseline save workflow.
fn run_baseline_save(
    result: CodeAuditResult,
    args: &AuditRunWorkflowArgs,
    timing: AuditTiming,
) -> crate::core::Result<AuditRunWorkflowResult> {
    let findings = result.findings.clone();
    let saved = if let Some(ref git_ref) = args.changed_since {
        let changed = changed_files_for_scope(args, git_ref)?;
        if changed.is_empty() {
            crate::log_status!(
                "baseline",
                "No files changed since {} — baseline unchanged",
                git_ref
            );
        } else {
            crate::log_status!(
                "baseline",
                "Scoped baseline update: {} file(s) in scope",
                changed.len()
            );
        }
        baseline::save_baseline_scoped(&result, &changed)
            .map_err(crate::core::Error::internal_unexpected)?
    } else {
        baseline::save_baseline(&result).map_err(crate::core::Error::internal_unexpected)?
    };

    let baseline_data =
        baseline::load_baseline(Path::new(&result.source_path)).ok_or_else(|| {
            crate::core::Error::internal_unexpected("Failed to read back saved baseline")
        })?;

    if let Some(score) = baseline_data.metadata.alignment_score {
        eprintln!(
            "[audit] Baseline saved to {} ({} findings, {:.0}% alignment)",
            saved.display(),
            baseline_data.item_count,
            score * 100.0
        );
    } else {
        eprintln!(
            "[audit] Baseline saved to {} ({} findings, alignment: N/A)",
            saved.display(),
            baseline_data.item_count,
        );
    }

    Ok(AuditRunWorkflowResult {
        output: AuditCommandOutput::BaselineSaved {
            component_id: result.component_id,
            path: saved.to_string_lossy().to_string(),
            findings_count: baseline_data.item_count,
            outliers_count: baseline_data.metadata.outliers_count,
            alignment_score: baseline_data.metadata.alignment_score,
        },
        exit_code: 0,
        findings,
        timing,
    })
}

/// Comparison workflow — compare against file baseline, git-ref baseline, or return full.
fn run_comparison_workflow(
    result: CodeAuditResult,
    analysis: &code_audit::AuditAnalysisContext,
    args: &AuditRunWorkflowArgs,
    timing: AuditTiming,
) -> crate::core::Result<AuditRunWorkflowResult> {
    // Try file-based baseline
    if !args.baseline_flags.ignore_baseline {
        if let Some(existing_baseline) = baseline::load_baseline(Path::new(&result.source_path)) {
            return build_comparison_output(result, analysis, existing_baseline, args, timing);
        }
    }

    // Try git-ref differential
    if let Some(ref git_ref) = args.changed_since {
        if let Some(ref_baseline) = baseline::load_baseline_from_ref(&result.source_path, git_ref) {
            return build_comparison_output(result, analysis, ref_baseline, args, timing);
        }
    }

    // No baseline at all
    let exit_code = if args.changed_since.is_some() {
        if !result.findings.is_empty() {
            eprintln!(
                "[audit] No baseline found for changed-since audit; showing {} contextual finding(s) in touched scope without blocking",
                result.findings.len()
            );
        }
        0
    } else {
        default_audit_exit_code(&result, false)
    };

    if args.json_summary {
        let findings = result.findings.clone();
        let mut summary = report::build_audit_summary(&result, exit_code);
        summary.fixability = compute_fixability_if_requested(&result, analysis, args);
        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Summary(summary),
            exit_code,
            findings,
            timing,
        })
    } else {
        let fixability = compute_fixability_if_requested(&result, analysis, args);
        let findings = result.findings.clone();
        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Full {
                passed: exit_code == 0,
                result,
                fixability,
                extension_phase_timings: Vec::new(),
            },
            exit_code,
            findings,
            timing,
        })
    }
}

/// Build comparison output from a result and baseline.
fn build_comparison_output(
    result: CodeAuditResult,
    analysis: &code_audit::AuditAnalysisContext,
    existing_baseline: baseline::AuditBaseline,
    args: &AuditRunWorkflowArgs,
    timing: AuditTiming,
) -> crate::core::Result<AuditRunWorkflowResult> {
    let mut comparison = baseline::compare(&result, &existing_baseline);
    if let Some(ref git_ref) = args.changed_since {
        let changed = changed_files_for_scope(args, git_ref).unwrap_or_else(|_| {
            result
                .findings
                .iter()
                .map(|finding| finding.file.clone())
                .collect()
        });
        retain_new_items_for_changed_files(&mut comparison, &changed);
    }
    let drift_increased = if args.changed_since.is_none() && !uses_finding_filters(args) {
        comparison
            .new_items
            .iter()
            .any(|item| !is_structural_complexity_fingerprint(&item.fingerprint))
    } else {
        comparison.drift_increased
    };
    comparison.drift_increased = drift_increased;
    let exit_code = if drift_increased { 1 } else { 0 };
    let changed_since_summary = args
        .changed_since
        .as_ref()
        .map(|_| report::build_changed_since_summary(&result, &comparison));

    if let Some(summary) = changed_since_summary {
        if summary.introduced_findings > 0 {
            eprintln!(
                "[audit] DRIFT INCREASED: {} introduced finding(s) since baseline ({} contextual finding(s) already known in touched scope)",
                summary.introduced_findings,
                summary.contextual_findings
            );
        } else if summary.contextual_findings > 0 {
            eprintln!(
                "[audit] No introduced findings; {} contextual finding(s) already known in touched scope",
                summary.contextual_findings
            );
        } else {
            eprintln!("[audit] No introduced findings in touched scope");
        }
    } else if comparison.drift_increased {
        eprintln!(
            "[audit] DRIFT INCREASED: {} new finding(s) since baseline",
            comparison.new_items.len()
        );
    } else if !comparison.resolved_fingerprints.is_empty() {
        eprintln!(
            "[audit] Drift reduced: {} finding(s) resolved since baseline",
            comparison.resolved_fingerprints.len()
        );
    } else {
        eprintln!("[audit] No change from baseline");
    }

    if args.json_summary {
        let findings = result.findings.clone();
        let mut summary = report::build_audit_summary(&result, exit_code);
        summary.fixability = compute_fixability_if_requested(&result, analysis, args);
        summary.changed_since = changed_since_summary;
        summary.baseline_filtering = Some(report::build_baseline_filtering_summary(
            &result,
            &comparison,
            &existing_baseline,
        ));
        summary.unbaselined_findings = report::build_unbaselined_finding_summary(&comparison);
        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Summary(summary),
            exit_code,
            findings,
            timing,
        })
    } else {
        let fixability = compute_fixability_if_requested(&result, analysis, args);
        let findings = result.findings.clone();

        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Compared {
                passed: exit_code == 0,
                result,
                baseline_comparison: comparison,
                changed_since: changed_since_summary,
                summary: None,
                fixability,
                extension_phase_timings: Vec::new(),
            },
            exit_code,
            findings,
            timing,
        })
    }
}

fn retain_new_items_for_changed_files(
    comparison: &mut baseline::BaselineComparison,
    changed_files: &[String],
) {
    let changed_files: HashSet<&str> = changed_files.iter().map(String::as_str).collect();

    comparison.new_items.retain(|item| {
        baseline::file_from_audit_fingerprint(&item.fingerprint)
            .is_some_and(|file| changed_files.contains(file.as_str()))
    });
    comparison.drift_increased = !comparison.new_items.is_empty();
}

fn changed_files_for_scope(
    args: &AuditRunWorkflowArgs,
    git_ref: &str,
) -> crate::core::Result<Vec<String>> {
    match &args.precomputed_changed_files {
        Some(files) => Ok(files.clone()),
        None => git::get_files_changed_since(&args.source_path, git_ref),
    }
}

fn compute_fixability_if_requested(
    result: &CodeAuditResult,
    analysis: &code_audit::AuditAnalysisContext,
    args: &AuditRunWorkflowArgs,
) -> Option<report::AuditFixability> {
    args.include_fixability
        .then(|| report::compute_fixability_with_analysis(result, analysis))
        .flatten()
}

/// Determine exit code for audit results.
fn default_audit_exit_code(result: &CodeAuditResult, is_scoped: bool) -> i32 {
    if is_scoped {
        if result.findings.is_empty() {
            0
        } else {
            1
        }
    } else if result.findings.iter().any(is_blocking_full_audit_finding) {
        1
    } else {
        0
    }
}

fn is_blocking_full_audit_finding(finding: &code_audit::Finding) -> bool {
    !matches!(
        finding.kind,
        code_audit::AuditFinding::GodFile
            | code_audit::AuditFinding::HighItemCount
            | code_audit::AuditFinding::DirectorySprawl
    )
}

fn uses_finding_filters(args: &AuditRunWorkflowArgs) -> bool {
    !args.only_kinds.is_empty()
        || !args.exclude_kinds.is_empty()
        || !args.only_labels.is_empty()
        || !args.exclude_labels.is_empty()
}

fn is_structural_complexity_fingerprint(fingerprint: &str) -> bool {
    fingerprint.starts_with("structural::")
        && (fingerprint.ends_with("::GodFile")
            || fingerprint.ends_with("::HighItemCount")
            || fingerprint.ends_with("::DirectorySprawl"))
}

#[cfg(test)]
#[path = "../../../tests/core/code_audit/run_test.rs"]
mod run_test;
