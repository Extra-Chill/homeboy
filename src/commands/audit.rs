use clap::Args;
use serde::Serialize;
use std::path::Path;

use homeboy::code_audit::{self, baseline, fixer, CodeAuditResult};

use super::CmdResult;

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

    /// Save current audit state as baseline for future comparisons
    #[arg(long)]
    pub baseline: bool,

    /// Skip baseline comparison even if a baseline exists
    #[arg(long)]
    pub ignore_baseline: bool,

    /// Override local_path for this audit run (use a workspace clone or temp checkout)
    #[arg(long)]
    pub path: Option<String>,
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
        #[serde(flatten)]
        fix_result: fixer::FixResult,
        written: bool,
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
    },
}

pub fn run(args: AuditArgs, _global: &super::GlobalArgs) -> CmdResult<AuditOutput> {
    let result = if Path::new(&args.component_id).is_dir() {
        // Raw path mode — use --path override if provided, otherwise use the positional arg
        let effective_path = args.path.as_deref().unwrap_or(&args.component_id);
        code_audit::audit_path(effective_path)?
    } else if let Some(ref path) = args.path {
        // Component mode with --path override — use component ID but audit at the given path
        code_audit::audit_path_with_id(&args.component_id, path)?
    } else {
        code_audit::audit_component(&args.component_id)?
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

        let exit_code = if fix_result.total_insertions > 0 {
            1
        } else {
            0
        };

        return Ok((
            AuditOutput::Fix {
                component_id: result.component_id,
                source_path: result.source_path,
                fix_result,
                written,
            },
            exit_code,
        ));
    }

    // --baseline: save current state
    if args.baseline {
        let saved =
            baseline::save_baseline(&result).map_err(|e| homeboy::Error::internal_unexpected(e))?;

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
    if !args.ignore_baseline {
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

            return Ok((
                AuditOutput::Compared {
                    result,
                    baseline_comparison: comparison,
                },
                exit_code,
            ));
        }
    }

    // No baseline — standard output
    let exit_code = if result.summary.outliers_found > 0 {
        1
    } else {
        0
    };
    Ok((AuditOutput::Full(result), exit_code))
}
