use clap::Args;
use serde::Serialize;
use std::path::Path;

use homeboy::code_audit::{self, baseline, fixer, CodeAuditResult};
use homeboy::git;
use homeboy::utils::autofix::{self, AutofixMode};

use super::args::BaselineArgs;
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct AuditArgs {
    /// Component ID or direct filesystem path to audit
    pub component_id: String,

    /// Only show discovered conventions (skip findings)
    #[arg(long)]
    pub conventions: bool,

    /// Generate fix stubs for outlier files (dry run by default)
    #[arg(long)]
    pub fix: bool,

    /// Apply fixes to disk (requires --fix)
    #[arg(long, requires = "fix")]
    pub write: bool,

    #[command(flatten)]
    pub baseline_args: BaselineArgs,

    /// Override local_path for this audit run (use a workspace clone or temp checkout)
    #[arg(long)]
    pub path: Option<String>,

    /// Only audit files changed since a git ref (branch, tag, or SHA).
    /// Uses merge-base for accurate PR-scoped audits.
    /// Example: --changed-since origin/main
    #[arg(long)]
    pub changed_since: Option<String>,

    /// Include compact machine-readable summary for CI wrappers
    #[arg(long)]
    pub json_summary: bool,
}

#[derive(Serialize)]
pub struct AuditSummaryOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    alignment_score: Option<f32>,
    total_findings: usize,
    warnings: usize,
    info: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    top_findings: Vec<AuditSummaryFinding>,
    exit_code: i32,
}

#[derive(Serialize)]
pub struct AuditSummaryFinding {
    file: String,
    category: String,
    description: String,
    suggestion: String,
}

#[derive(Serialize)]
#[serde(tag = "command")]
pub enum AuditOutput {
    #[serde(rename = "audit")]
    Full(CodeAuditResult),

    #[serde(rename = "audit.conventions")]
    Conventions {
        component_id: String,
        conventions: Vec<homeboy::code_audit::ConventionReport>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        directory_conventions: Vec<homeboy::code_audit::DirectoryConvention>,
    },

    #[serde(rename = "audit.fix")]
    Fix {
        component_id: String,
        source_path: String,
        status: String,
        #[serde(flatten)]
        fix_result: fixer::FixResult,
        written: bool,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        hints: Vec<String>,
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

pub fn run(args: AuditArgs, _global: &GlobalArgs) -> CmdResult<AuditOutput> {
    run_inner(args)
}

fn build_audit_summary(result: &CodeAuditResult, exit_code: i32) -> AuditSummaryOutput {
    let warnings = result
        .findings
        .iter()
        .filter(|f| matches!(f.severity, homeboy::code_audit::Severity::Warning))
        .count();
    let info = result
        .findings
        .iter()
        .filter(|f| matches!(f.severity, homeboy::code_audit::Severity::Info))
        .count();

    let top_findings = result
        .findings
        .iter()
        .take(20)
        .map(|f| AuditSummaryFinding {
            file: f.file.clone(),
            category: f.convention.clone(),
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

fn default_audit_exit_code(result: &CodeAuditResult, is_scoped: bool) -> i32 {
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

fn run_inner(args: AuditArgs) -> CmdResult<AuditOutput> {
    // Resolve component ID and source path
    let (resolved_id, resolved_path) = if Path::new(&args.component_id).is_dir() {
        let effective = args
            .path
            .as_deref()
            .unwrap_or(&args.component_id)
            .to_string();
        let name = Path::new(&effective)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        (name, effective)
    } else if let Some(ref path) = args.path {
        (args.component_id.clone(), path.clone())
    } else {
        let comp = homeboy::component::load(&args.component_id)?;
        homeboy::component::validate_local_path(&comp)?;
        let expanded = shellexpand::tilde(&comp.local_path).to_string();
        (args.component_id.clone(), expanded)
    };

    // Run audit — scoped or full
    let result = if let Some(ref git_ref) = args.changed_since {
        let changed = git::get_files_changed_since(&resolved_path, git_ref)?;
        if changed.is_empty() {
            homeboy::log_status!("audit", "No files changed since {}", git_ref);
            return Ok((
                AuditOutput::Full(code_audit::CodeAuditResult {
                    component_id: resolved_id,
                    source_path: resolved_path,
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
                0,
            ));
        }
        code_audit::audit_path_scoped(&resolved_id, &resolved_path, &changed)?
    } else {
        code_audit::audit_path_with_id(&resolved_id, &resolved_path)?
    };

    // --conventions: just show conventions
    if args.conventions {
        return Ok((
            AuditOutput::Conventions {
                component_id: result.component_id,
                conventions: result.conventions,
                directory_conventions: result.directory_conventions,
            },
            0,
        ));
    }

    // --fix: generate stubs
    if args.fix {
        let root = Path::new(&result.source_path);
        let mut fix_result = fixer::generate_fixes(&result, root);
        let written = args.write;

        if written {
            let mut total_modified = 0;
            if !fix_result.fixes.is_empty() {
                total_modified += fixer::apply_fixes(&mut fix_result.fixes, root);
            }
            if !fix_result.new_files.is_empty() {
                total_modified += fixer::apply_new_files(&mut fix_result.new_files, root);
            }
            fix_result.files_modified = total_modified;
        }

        let outcome = autofix::standard_outcome(
            if written {
                AutofixMode::Write
            } else {
                AutofixMode::DryRun
            },
            fix_result.total_insertions,
            Some(format!("homeboy audit {}", result.component_id)),
            vec![],
        );

        let exit_code = if fix_result.total_insertions > 0 {
            1
        } else {
            0
        };

        return Ok((
            AuditOutput::Fix {
                component_id: result.component_id,
                source_path: result.source_path,
                status: outcome.status,
                fix_result,
                written,
                hints: outcome.hints,
            },
            exit_code,
        ));
    }

    // --baseline: save current state
    if args.baseline_args.baseline {
        let saved =
            baseline::save_baseline(&result).map_err(homeboy::Error::internal_unexpected)?;

        let baseline_data =
            baseline::load_baseline(Path::new(&result.source_path)).ok_or_else(|| {
                homeboy::Error::internal_unexpected("Failed to read back saved baseline")
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

        return Ok((
            AuditOutput::BaselineSaved {
                component_id: result.component_id,
                path: saved.to_string_lossy().to_string(),
                findings_count: baseline_data.item_count,
                outliers_count: baseline_data.metadata.outliers_count,
                alignment_score: baseline_data.metadata.alignment_score,
            },
            0,
        ));
    }

    // Default: run audit, compare against baseline if one exists
    if !args.baseline_args.ignore_baseline {
        if let Some(existing_baseline) = baseline::load_baseline(Path::new(&result.source_path)) {
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

            let summary = if args.json_summary {
                Some(build_audit_summary(&result, exit_code))
            } else {
                None
            };

            return Ok((
                if args.json_summary {
                    AuditOutput::Summary(build_audit_summary(&result, exit_code))
                } else {
                    AuditOutput::Compared {
                        result,
                        baseline_comparison: comparison,
                        summary,
                    }
                },
                exit_code,
            ));
        }
    }

    // No baseline — standard output
    let exit_code = default_audit_exit_code(&result, args.changed_since.is_some());
    if args.json_summary {
        Ok((
            AuditOutput::Summary(build_audit_summary(&result, exit_code)),
            exit_code,
        ))
    } else {
        Ok((AuditOutput::Full(result), exit_code))
    }
}

#[cfg(test)]
mod tests {
    use super::default_audit_exit_code;
    use homeboy::code_audit::{AuditSummary, CodeAuditResult, Finding, Severity};
    use homeboy::code_audit::DeviationKind;

    fn mk_result(outliers_found: usize, findings_len: usize) -> CodeAuditResult {
        CodeAuditResult {
            component_id: "component".to_string(),
            source_path: "/tmp/component".to_string(),
            summary: AuditSummary {
                files_scanned: 1,
                conventions_detected: 1,
                outliers_found,
                alignment_score: Some(1.0),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: (0..findings_len)
                .map(|i| Finding {
                    file: format!("src/file{i}.rs"),
                    convention: "Example".to_string(),
                    severity: Severity::Warning,
                    description: "desc".to_string(),
                    suggestion: "suggest".to_string(),
                    kind: DeviationKind::MissingMethod,
                })
                .collect(),
            duplicate_groups: vec![],
        }
    }

    #[test]
    fn test_default_audit_exit_code_full_uses_outliers() {
        let result = mk_result(2, 0);
        assert_eq!(default_audit_exit_code(&result, false), 1);
    }

    #[test]
    fn test_default_audit_exit_code_scoped_uses_findings() {
        let result = mk_result(71, 0);
        assert_eq!(default_audit_exit_code(&result, true), 0);

        let result = mk_result(0, 1);
        assert_eq!(default_audit_exit_code(&result, true), 1);
    }
}
