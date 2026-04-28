//! Audit workflow orchestration — runs audit, handles fix/baseline/comparison modes.
//!
//! Mirrors `core/extension/lint/run.rs` and `core/extension/test/run.rs` — the command
//! layer provides CLI args, this module owns all business logic and returns structured results.

use crate::code_audit::{self, baseline, CodeAuditResult};
use crate::git;
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
    pub conventions: bool,
    pub only_kinds: Vec<code_audit::AuditFinding>,
    pub exclude_kinds: Vec<code_audit::AuditFinding>,
    pub only_labels: Vec<String>,
    pub exclude_labels: Vec<String>,
    pub baseline_flags: crate::engine::baseline::BaselineFlags,
    pub changed_since: Option<String>,
    pub json_summary: bool,
    pub include_fixability: bool,
}

/// Result of the main audit workflow — ready for report assembly.
pub struct AuditRunWorkflowResult {
    pub output: AuditCommandOutput,
    pub exit_code: i32,
}

/// Run the main audit workflow.
pub fn run_main_audit_workflow(
    args: AuditRunWorkflowArgs,
) -> crate::Result<AuditRunWorkflowResult> {
    // Run audit — scoped or full
    let result = run_audit(&args)?;

    // Early return: no-change shortcut already handled by run_audit returning None
    let mut result = match result {
        Some(r) => r,
        None => {
            return Ok(AuditRunWorkflowResult {
                output: AuditCommandOutput::Full {
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
                },
                exit_code: 0,
            });
        }
    };

    // --conventions: just show conventions
    if args.conventions {
        return Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Conventions {
                component_id: result.component_id,
                conventions: result.conventions,
                directory_conventions: result.directory_conventions,
            },
            exit_code: 0,
        });
    }

    // --baseline: save current state. Saved baselines record the *full* finding
    // set so they remain a complete reference; --only / --exclude intentionally
    // do not narrow what gets persisted.
    if args.baseline_flags.baseline {
        return run_baseline_save(result, &args);
    }

    // --only / --exclude: scope this run's findings before comparison and
    // report assembly. The CLI flags are parsed in `parse_finding_kinds` and
    // surfaced here as `only_kinds` / `exclude_kinds`; any filter activity
    // also recomputes `summary.outliers_found` so the exit-code logic in
    // `default_audit_exit_code` reflects the filtered view.
    apply_finding_filters(&mut result, &args.only_kinds, &args.exclude_kinds);

    // Default: compare against baseline or return full result
    run_comparison_workflow(result, &args)
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

/// Run the audit scan (scoped or full). Returns None if changed-since found no files.
fn run_audit(args: &AuditRunWorkflowArgs) -> crate::Result<Option<CodeAuditResult>> {
    let plan = if args.baseline_flags.baseline {
        code_audit::AuditExecutionPlan::full()
    } else {
        code_audit::AuditExecutionPlan::from_filters(&args.only_kinds, &args.exclude_kinds)
    };

    if let Some(ref git_ref) = args.changed_since {
        let changed = git::get_files_changed_since(&args.source_path, git_ref)?;
        if changed.is_empty() {
            crate::log_status!("audit", "No files changed since {}", git_ref);
            return Ok(None);
        }
        Ok(Some(code_audit::audit_path_scoped_with_plan(
            &args.component_id,
            &args.source_path,
            &changed,
            Some(git_ref),
            &plan,
        )?))
    } else {
        Ok(Some(code_audit::audit_path_with_id_with_plan(
            &args.component_id,
            &args.source_path,
            &plan,
        )?))
    }
}

/// Baseline save workflow.
fn run_baseline_save(
    result: CodeAuditResult,
    args: &AuditRunWorkflowArgs,
) -> crate::Result<AuditRunWorkflowResult> {
    let saved = if let Some(ref git_ref) = args.changed_since {
        let changed = git::get_files_changed_since(&args.source_path, git_ref)?;
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
            .map_err(crate::Error::internal_unexpected)?
    } else {
        baseline::save_baseline(&result).map_err(crate::Error::internal_unexpected)?
    };

    let baseline_data = baseline::load_baseline(Path::new(&result.source_path))
        .ok_or_else(|| crate::Error::internal_unexpected("Failed to read back saved baseline"))?;

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
    })
}

/// Comparison workflow — compare against file baseline, git-ref baseline, or return full.
fn run_comparison_workflow(
    result: CodeAuditResult,
    args: &AuditRunWorkflowArgs,
) -> crate::Result<AuditRunWorkflowResult> {
    // Try file-based baseline
    if !args.baseline_flags.ignore_baseline {
        if let Some(existing_baseline) = baseline::load_baseline(Path::new(&result.source_path)) {
            return build_comparison_output(result, existing_baseline, args);
        }
    }

    // Try git-ref differential
    if let Some(ref git_ref) = args.changed_since {
        if let Some(ref_baseline) = baseline::load_baseline_from_ref(&result.source_path, git_ref) {
            return build_comparison_output(result, ref_baseline, args);
        }
    }

    // No baseline at all
    let exit_code = if args.changed_since.is_some() {
        if !result.findings.is_empty() {
            eprintln!(
                "[audit] {} finding(s) in changed files — no baseline to compare against, treating as pre-existing",
                result.findings.len()
            );
        }
        0
    } else {
        default_audit_exit_code(&result, false)
    };

    if args.json_summary {
        let mut summary = report::build_audit_summary(&result, exit_code);
        summary.fixability = compute_fixability_if_requested(&result, args);
        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Summary(summary),
            exit_code,
        })
    } else {
        let fixability = compute_fixability_if_requested(&result, args);
        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Full {
                passed: exit_code == 0,
                result,
                fixability,
            },
            exit_code,
        })
    }
}

/// Build comparison output from a result and baseline.
fn build_comparison_output(
    result: CodeAuditResult,
    existing_baseline: baseline::AuditBaseline,
    args: &AuditRunWorkflowArgs,
) -> crate::Result<AuditRunWorkflowResult> {
    let comparison = baseline::compare(&result, &existing_baseline);
    let exit_code = if comparison.drift_increased { 1 } else { 0 };

    if comparison.drift_increased {
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
        let mut summary = report::build_audit_summary(&result, exit_code);
        summary.fixability = compute_fixability_if_requested(&result, args);
        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Summary(summary),
            exit_code,
        })
    } else {
        let fixability = compute_fixability_if_requested(&result, args);

        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Compared {
                passed: exit_code == 0,
                result,
                baseline_comparison: comparison,
                summary: None,
                fixability,
            },
            exit_code,
        })
    }
}

fn compute_fixability_if_requested(
    result: &CodeAuditResult,
    args: &AuditRunWorkflowArgs,
) -> Option<report::AuditFixability> {
    args.include_fixability
        .then(|| report::compute_fixability(result))
        .flatten()
}

/// Determine exit code for audit results.
pub fn default_audit_exit_code(result: &CodeAuditResult, is_scoped: bool) -> i32 {
    if is_scoped {
        if result.findings.is_empty() {
            0
        } else {
            1
        }
    } else if result.summary.outliers_found > 0 {
        1
    } else {
        0
    }
}

#[cfg(test)]
#[path = "../../../tests/core/code_audit/run_test.rs"]
mod run_test;
