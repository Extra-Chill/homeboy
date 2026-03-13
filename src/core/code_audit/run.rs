//! Audit workflow orchestration — runs audit, handles fix/baseline/comparison modes.
//!
//! Mirrors `core/extension/lint/run.rs` and `core/extension/test/run.rs` — the command
//! layer provides CLI args, this module owns all business logic and returns structured results.

use crate::code_audit::{self, baseline, CodeAuditResult};
use crate::git;
use crate::refactor::{
    auto::{self, AutofixMode},
    run_audit_refactor, AuditConvergenceScoring, AuditVerificationToggles,
};
use std::path::Path;

use super::report::{self, AuditCommandOutput, AutoRatchetSummary};

/// Arguments for the main audit workflow — populated by the command layer from CLI flags.
#[derive(Debug, Clone)]
pub struct AuditRunWorkflowArgs {
    pub component_id: String,
    pub source_path: String,
    pub conventions: bool,
    pub fix: bool,
    pub write: bool,
    pub max_iterations: usize,
    pub scoring: AuditConvergenceScoring,
    pub verification: AuditVerificationToggles,
    pub only_kinds: Vec<code_audit::AuditFinding>,
    pub exclude_kinds: Vec<code_audit::AuditFinding>,
    pub only_labels: Vec<String>,
    pub exclude_labels: Vec<String>,
    pub ratchet: bool,
    pub baseline: bool,
    pub ignore_baseline: bool,
    pub changed_since: Option<String>,
    pub json_summary: bool,
    pub preview: bool,
}

/// Result of the main audit workflow — ready for report assembly.
pub struct AuditRunWorkflowResult {
    pub output: AuditCommandOutput,
    pub exit_code: i32,
}

/// Run the main audit workflow.
pub fn run_main_audit_workflow(args: AuditRunWorkflowArgs) -> crate::Result<AuditRunWorkflowResult> {
    // Run audit — scoped or full
    let result = run_audit(&args)?;

    // Early return: no-change shortcut already handled by run_audit returning None
    let result = match result {
        Some(r) => r,
        None => {
            return Ok(AuditRunWorkflowResult {
                output: AuditCommandOutput::Full(CodeAuditResult {
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
                }),
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

    // --fix: generate stubs
    if args.fix {
        return run_fix_workflow(result, &args);
    }

    // --baseline: save current state
    if args.baseline {
        return run_baseline_save(result, &args);
    }

    // Default: compare against baseline or return full result
    run_comparison_workflow(result, &args)
}

/// Run the audit scan (scoped or full). Returns None if changed-since found no files.
fn run_audit(args: &AuditRunWorkflowArgs) -> crate::Result<Option<CodeAuditResult>> {
    if let Some(ref git_ref) = args.changed_since {
        let changed = git::get_files_changed_since(&args.source_path, git_ref)?;
        if changed.is_empty() {
            crate::log_status!("audit", "No files changed since {}", git_ref);
            return Ok(None);
        }
        Ok(Some(code_audit::audit_path_scoped(
            &args.component_id,
            &args.source_path,
            &changed,
            Some(git_ref),
        )?))
    } else {
        Ok(Some(code_audit::audit_path_with_id(
            &args.component_id,
            &args.source_path,
        )?))
    }
}

/// Fix workflow — run audit refactor, handle ratchet, build output.
fn run_fix_workflow(
    result: CodeAuditResult,
    args: &AuditRunWorkflowArgs,
) -> crate::Result<AuditRunWorkflowResult> {
    let written = args.write;
    let refactor_outcome = run_audit_refactor(
        result,
        &args.only_kinds,
        &args.exclude_kinds,
        args.scoring.clone(),
        args.verification.clone(),
        args.max_iterations,
        written,
    )?;
    let current_result = refactor_outcome.current_result;
    let mut final_fix_result = refactor_outcome.fix_result;
    let final_policy_summary = refactor_outcome.policy_summary;
    let iterations = refactor_outcome.iterations;

    // Ratchet lifecycle
    let ratchet_summary = if args.ratchet && written && !args.ignore_baseline {
        process_ratchet(&current_result, args)?
    } else {
        None
    };

    let outcome = auto::standard_outcome(
        if written {
            AutofixMode::Write
        } else {
            AutofixMode::DryRun
        },
        final_fix_result.total_insertions,
        Some(format!("homeboy audit {}", current_result.component_id)),
        report::build_fix_hints(written, &final_policy_summary),
    );

    let exit_code = if final_fix_result.total_insertions > 0 {
        1
    } else {
        0
    };

    // Print human-readable summary to stderr
    report::log_fix_summary(&final_fix_result, &final_policy_summary, written);

    // Bridge to universal fix summary (before strip_code mutates the data)
    let fix_summary = if written && final_fix_result.files_modified > 0 {
        Some(auto::summarize_audit_fix_result(&final_fix_result))
    } else {
        None
    };

    // Strip generated code from JSON output unless --preview is set
    if !args.preview {
        final_fix_result.strip_code();
    }

    let policy_summary = report::build_fix_policy_summary(
        &final_policy_summary,
        args.only_labels.clone(),
        args.exclude_labels.clone(),
    );

    Ok(AuditRunWorkflowResult {
        output: AuditCommandOutput::Fix {
            component_id: current_result.component_id,
            source_path: current_result.source_path,
            status: outcome.status,
            fix_result: final_fix_result,
            fix_summary,
            policy_summary,
            iterations,
            written,
            hints: outcome.hints,
            ratchet_summary,
        },
        exit_code,
    })
}

/// Process ratchet lifecycle — update baseline when findings are resolved.
fn process_ratchet(
    current_result: &CodeAuditResult,
    args: &AuditRunWorkflowArgs,
) -> crate::Result<Option<AutoRatchetSummary>> {
    let existing_baseline = baseline::load_baseline(Path::new(&current_result.source_path));
    let Some(existing_baseline) = existing_baseline else {
        return Ok(None);
    };

    let comparison = baseline::compare(current_result, &existing_baseline);
    if comparison.resolved_fingerprints.is_empty() {
        if comparison.new_items.is_empty() {
            crate::log_status!(
                "ratchet",
                "No findings resolved — baseline unchanged ({} findings)",
                existing_baseline.item_count
            );
        }
        return Ok(None);
    }

    // Findings were eliminated — save updated baseline
    let save_result = if let Some(ref git_ref) = args.changed_since {
        let changed = git::get_files_changed_since(&current_result.source_path, git_ref)
            .unwrap_or_default();
        baseline::save_baseline_scoped(current_result, &changed)
    } else {
        baseline::save_baseline(current_result)
    };

    match save_result {
        Ok(_path) => {
            crate::log_status!(
                "ratchet",
                "Auto-updated baseline: {} finding(s) resolved ({} → {})",
                comparison.resolved_fingerprints.len(),
                existing_baseline.item_count,
                current_result.findings.len()
            );
            Ok(Some(AutoRatchetSummary {
                resolved_count: comparison.resolved_fingerprints.len(),
                previous_count: existing_baseline.item_count,
                current_count: current_result.findings.len(),
                baseline_updated: true,
            }))
        }
        Err(e) => {
            crate::log_status!(
                "ratchet",
                "Warning: failed to auto-update baseline: {}",
                e
            );
            Ok(None)
        }
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

    let baseline_data =
        baseline::load_baseline(Path::new(&result.source_path)).ok_or_else(|| {
            crate::Error::internal_unexpected("Failed to read back saved baseline")
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
    })
}

/// Comparison workflow — compare against file baseline, git-ref baseline, or return full.
fn run_comparison_workflow(
    result: CodeAuditResult,
    args: &AuditRunWorkflowArgs,
) -> crate::Result<AuditRunWorkflowResult> {
    // Try file-based baseline
    if !args.ignore_baseline {
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
        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Summary(report::build_audit_summary(&result, exit_code)),
            exit_code,
        })
    } else {
        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Full(result),
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
        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Summary(report::build_audit_summary(&result, exit_code)),
            exit_code,
        })
    } else {
        let summary = if args.json_summary {
            Some(report::build_audit_summary(&result, exit_code))
        } else {
            None
        };

        Ok(AuditRunWorkflowResult {
            output: AuditCommandOutput::Compared {
                result,
                baseline_comparison: comparison,
                summary,
            },
            exit_code,
        })
    }
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
