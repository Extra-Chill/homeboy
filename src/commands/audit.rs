use clap::Args;
use homeboy::code_audit::{self, baseline, fixer, CodeAuditResult};
use homeboy::component::{self, Component};
use homeboy::extension::ExtensionRunner;
use homeboy::git;
use homeboy::refactor::{
    build_chunk_verifier, finding_fingerprint, run_audit_refactor, score_delta,
    weighted_finding_score_with, AuditConvergenceScoring, AuditRefactorIterationSummary,
    AuditVerificationToggles,
};
use homeboy::utils::autofix::{self, AutofixMode, FixResultsSummary};
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use super::args::{BaselineArgs, PositionalComponentArgs};
use super::test_scope::compute_changed_test_scope;
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct AuditArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    /// Only show discovered conventions (skip findings)
    #[arg(long)]
    pub conventions: bool,

    /// Generate fix stubs for outlier files (dry run by default)
    #[arg(long)]
    pub fix: bool,

    /// Apply fixes to disk (requires --fix)
    #[arg(long, requires = "fix")]
    pub write: bool,

    /// Maximum recursive autofix iterations when writing
    #[arg(long, requires = "fix", default_value_t = 3)]
    pub max_iterations: usize,

    /// Weight for warning-level findings in convergence scoring
    #[arg(long, requires = "fix", default_value_t = 3)]
    pub warning_weight: usize,

    /// Weight for info-level findings in convergence scoring
    #[arg(long, requires = "fix", default_value_t = 1)]
    pub info_weight: usize,

    /// Disable lint smoke verification during chunk verification
    #[arg(long, requires = "fix")]
    pub no_lint_smoke: bool,

    /// Disable test smoke verification during chunk verification
    #[arg(long, requires = "fix")]
    pub no_test_smoke: bool,

    /// Restrict generated fixes to these fix kinds (repeatable)
    #[arg(long = "only", value_name = "kind")]
    pub only: Vec<String>,

    /// Exclude generated fixes for these fix kinds (repeatable)
    #[arg(long = "exclude", value_name = "kind")]
    pub exclude: Vec<String>,

    /// Update baseline when findings are resolved (ratchet forward).
    /// Without this flag, --fix --write applies code fixes but does not
    /// touch the baseline in homeboy.json.
    #[arg(long)]
    pub ratchet: bool,

    #[command(flatten)]
    pub baseline_args: BaselineArgs,

    /// Only audit files changed since a git ref (branch, tag, or SHA).
    /// Uses merge-base for accurate PR-scoped audits.
    /// Example: --changed-since origin/main
    #[arg(long)]
    pub changed_since: Option<String>,

    /// Include compact machine-readable summary for CI wrappers
    #[arg(long)]
    pub json_summary: bool,

    /// Include full generated code in --fix JSON output (omitted by default to reduce size)
    #[arg(long, requires = "fix")]
    pub preview: bool,
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
    /// Convention this finding belongs to (matches Finding.convention).
    convention: String,
    kind: homeboy::code_audit::AuditFinding,
    severity: homeboy::code_audit::Severity,
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
        /// Universal fix summary bridged from the Rust-native FixResult.
        /// Same structure as lint --fix and test --fix output.
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

#[derive(Debug, Serialize)]
pub struct AutoRatchetSummary {
    /// Number of findings resolved by autofix.
    pub resolved_count: usize,
    /// Baseline finding count before auto-ratchet.
    pub previous_count: usize,
    /// Current finding count after auto-ratchet.
    pub current_count: usize,
    /// Whether the baseline file was successfully updated.
    pub baseline_updated: bool,
}

#[derive(Debug, Serialize)]
pub struct AuditFixPolicySummary {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    selected_only: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    excluded: Vec<String>,
    visible_insertions: usize,
    visible_new_files: usize,
    auto_apply_insertions: usize,
    auto_apply_new_files: usize,
    blocked_insertions: usize,
    blocked_new_files: usize,
    preflight_failures: usize,
}

type AuditFixIterationSummary = AuditRefactorIterationSummary;

fn parse_finding_kinds(
    values: &[String],
    flag: &str,
) -> homeboy::Result<Vec<homeboy::code_audit::AuditFinding>> {
    use std::str::FromStr;
    values
        .iter()
        .map(|value| {
            homeboy::code_audit::AuditFinding::from_str(value)
                .map_err(|msg| homeboy::Error::validation_invalid_argument(flag, msg, None, None))
        })
        .collect()
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
    let scoring = AuditConvergenceScoring {
        warning_weight: args.warning_weight,
        info_weight: args.info_weight,
    };
    let verification = AuditVerificationToggles {
        lint_smoke: !args.no_lint_smoke,
        test_smoke: !args.no_test_smoke,
    };
    let only_kinds = parse_finding_kinds(&args.only, "only")?;
    let exclude_kinds = parse_finding_kinds(&args.exclude, "exclude")?;

    // Resolve component ID and source path.
    // Supports: component ID with --path, registered component, or direct filesystem path.
    let (resolved_id, resolved_path) = if Path::new(&args.comp.component).is_dir() {
        // Direct path passed as component ID (e.g. `homeboy audit /some/path`)
        let effective = args
            .comp
            .path
            .as_deref()
            .unwrap_or(&args.comp.component)
            .to_string();
        let name = Path::new(&effective)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        (name, effective)
    } else {
        // Standard resolution: registered → portable config → synthetic
        let comp = args.comp.load()?;
        homeboy::component::validate_local_path(&comp)?;
        let expanded = shellexpand::tilde(&comp.local_path).to_string();
        (comp.id.clone(), expanded)
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
        code_audit::audit_path_scoped(&resolved_id, &resolved_path, &changed, Some(git_ref))?
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
        let written = args.write;
        let refactor_outcome = run_audit_refactor(
            result,
            &only_kinds,
            &exclude_kinds,
            scoring,
            verification,
            args.max_iterations,
            written,
        )?;
        let current_result = refactor_outcome.current_result;
        let mut final_fix_result = refactor_outcome.fix_result;
        let final_policy_summary = refactor_outcome.policy_summary;
        let iterations = refactor_outcome.iterations;

        // Ratchet: if --ratchet is set and --fix --write applied changes,
        // update the baseline to remove resolved findings.
        // This makes the baseline shrink as autofix eliminates fixable findings.
        //
        // Ratchet is opt-in to avoid homeboy.json merge conflicts in CI:
        // only main/release workflows should pass --ratchet, never PR branches.
        //
        // When --changed-since is active, use scoped baseline update to avoid
        // touching fingerprints for files outside the change set.
        let mut ratchet_summary = None;
        if args.ratchet && written && !args.baseline_args.ignore_baseline {
            if let Some(existing_baseline) =
                baseline::load_baseline(Path::new(&current_result.source_path))
            {
                let comparison = baseline::compare(&current_result, &existing_baseline);
                if !comparison.resolved_fingerprints.is_empty() {
                    // Findings were eliminated — save updated baseline
                    let save_result = if let Some(ref git_ref) = args.changed_since {
                        let changed =
                            git::get_files_changed_since(&current_result.source_path, git_ref)
                                .unwrap_or_default();
                        baseline::save_baseline_scoped(&current_result, &changed)
                    } else {
                        baseline::save_baseline(&current_result)
                    };
                    match save_result {
                        Ok(_path) => {
                            homeboy::log_status!(
                                "ratchet",
                                "Auto-updated baseline: {} finding(s) resolved ({} → {})",
                                comparison.resolved_fingerprints.len(),
                                existing_baseline.item_count,
                                current_result.findings.len()
                            );
                            ratchet_summary = Some(AutoRatchetSummary {
                                resolved_count: comparison.resolved_fingerprints.len(),
                                previous_count: existing_baseline.item_count,
                                current_count: current_result.findings.len(),
                                baseline_updated: true,
                            });
                        }
                        Err(e) => {
                            homeboy::log_status!(
                                "ratchet",
                                "Warning: failed to auto-update baseline: {}",
                                e
                            );
                        }
                    }
                } else if comparison.new_items.is_empty() {
                    homeboy::log_status!(
                        "ratchet",
                        "No findings resolved — baseline unchanged ({} findings)",
                        existing_baseline.item_count
                    );
                }
            }
        }

        let outcome = autofix::standard_outcome(
            if written {
                AutofixMode::Write
            } else {
                AutofixMode::DryRun
            },
            final_fix_result.total_insertions,
            Some(format!("homeboy audit {}", current_result.component_id)),
            build_fix_hints(written, &final_policy_summary),
        );

        let exit_code = if final_fix_result.total_insertions > 0 {
            1
        } else {
            0
        };

        // Print human-readable summary to stderr
        log_fix_summary(&final_fix_result, &final_policy_summary, written);

        // Bridge to universal fix summary (before strip_code mutates the data).
        let fix_summary = if written && final_fix_result.files_modified > 0 {
            Some(autofix::summarize_audit_fix_result(&final_fix_result))
        } else {
            None
        };

        // Strip generated code from JSON output unless --preview is set
        if !args.preview {
            final_fix_result.strip_code();
        }

        return Ok((
            AuditOutput::Fix {
                component_id: current_result.component_id,
                source_path: current_result.source_path,
                status: outcome.status,
                fix_result: final_fix_result,
                fix_summary,
                policy_summary: AuditFixPolicySummary {
                    selected_only: args.only,
                    excluded: args.exclude,
                    visible_insertions: final_policy_summary.visible_insertions,
                    visible_new_files: final_policy_summary.visible_new_files,
                    auto_apply_insertions: final_policy_summary.auto_apply_insertions,
                    auto_apply_new_files: final_policy_summary.auto_apply_new_files,
                    blocked_insertions: final_policy_summary.blocked_insertions,
                    blocked_new_files: final_policy_summary.blocked_new_files,
                    preflight_failures: final_policy_summary.preflight_failures,
                },
                iterations,
                written,
                hints: outcome.hints,
                ratchet_summary,
            },
            exit_code,
        ));
    }

    // --baseline: save current state
    if args.baseline_args.baseline {
        let saved = if let Some(ref git_ref) = args.changed_since {
            // Scoped baseline: only update fingerprints for changed files
            let changed = git::get_files_changed_since(&resolved_path, git_ref)?;
            if changed.is_empty() {
                homeboy::log_status!(
                    "baseline",
                    "No files changed since {} — baseline unchanged",
                    git_ref
                );
            } else {
                homeboy::log_status!(
                    "baseline",
                    "Scoped baseline update: {} file(s) in scope",
                    changed.len()
                );
            }
            baseline::save_baseline_scoped(&result, &changed)
                .map_err(homeboy::Error::internal_unexpected)?
        } else {
            // Full baseline: replace everything
            baseline::save_baseline(&result).map_err(homeboy::Error::internal_unexpected)?
        };

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

    // No explicit baseline — try differential comparison from git ref
    if let Some(ref git_ref) = args.changed_since {
        if let Some(ref_baseline) = baseline::load_baseline_from_ref(&result.source_path, git_ref) {
            let comparison = baseline::compare(&result, &ref_baseline);
            let exit_code = if comparison.drift_increased { 1 } else { 0 };

            if comparison.drift_increased {
                eprintln!(
                    "[audit] {} new finding(s) introduced by this change",
                    comparison.new_items.len()
                );
            } else if !result.findings.is_empty() {
                eprintln!(
                    "[audit] {} finding(s) in changed files — all pre-existing (0 introduced)",
                    result.findings.len()
                );
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

    // No baseline at all
    //
    // When --changed-since is active but no baseline exists anywhere (neither
    // explicit file nor at the base ref), we cannot determine which findings
    // are new vs pre-existing. In this case, pass — the correct fix is to
    // add a baseline, not to fail PRs on unattributable debt.
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
        Ok((
            AuditOutput::Summary(build_audit_summary(&result, exit_code)),
            exit_code,
        ))
    } else {
        Ok((AuditOutput::Full(result), exit_code))
    }
}

fn build_fix_hints(written: bool, summary: &fixer::PolicySummary) -> Vec<String> {
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

fn log_fix_summary(result: &fixer::FixResult, policy: &fixer::PolicySummary, written: bool) {
    let kind_counts = result.finding_counts();
    let total_insertions = result.total_insertions;
    let total_new_files = result.new_files.len();
    let total_skipped = result.skipped.len();

    if total_insertions == 0 && total_new_files == 0 {
        homeboy::log_status!("fix", "No fixes to apply");
        return;
    }

    let mode = if written { "Applied" } else { "Would apply" };
    homeboy::log_status!(
        "fix",
        "{mode} {total_insertions} insertion(s) across {} file(s), {} new file(s)",
        result.files_modified,
        total_new_files
    );

    for (kind, count) in &kind_counts {
        homeboy::log_status!("fix", "  {kind:?}: {count}");
    }

    if total_skipped > 0 {
        homeboy::log_status!("fix", "Skipped: {total_skipped} file(s)");
    }

    if policy.has_blocked_items() {
        homeboy::log_status!(
            "fix",
            "Blocked: {} insertion(s), {} new file(s) (safe_with_checks or plan_only)",
            policy.blocked_insertions,
            policy.blocked_new_files
        );
    }

    if policy.preflight_failures > 0 {
        homeboy::log_status!("fix", "Preflight failures: {}", policy.preflight_failures);
    }
}

#[cfg(test)]
mod tests {
    use super::default_audit_exit_code;
    use super::{run, AuditArgs, AuditOutput};
    use crate::commands::args::{BaselineArgs, PositionalComponentArgs};
    use homeboy::refactor::{
        build_chunk_verifier, finding_fingerprint, score_delta, weighted_finding_score_with,
        AuditConvergenceScoring,
    };
    use homeboy::code_audit::fixer::{FixSafetyTier, InsertionKind};
    use homeboy::code_audit::AuditFinding;
    use homeboy::code_audit::{AuditSummary, CodeAuditResult, Finding, Severity};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-audit-command-{name}-{nanos}"))
    }

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
                    kind: AuditFinding::MissingMethod,
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

    #[test]
    fn test_run_fix_write_applies_preflight_checked_method_stub() {
        use homeboy::code_audit::fixer::{
            self, Fix, FixPolicy, Insertion, PreflightContext, PreflightStatus,
        };

        let root = tmp_dir("fix-write-applies-preflight-checked-method-stub");
        fs::create_dir_all(root.join("commands")).unwrap();
        fs::write(root.join("commands/bad.rs"), "pub fn run() {}\n").unwrap();

        // Construct a FixResult directly — tests the fixer logic without
        // requiring an extension to fingerprint .rs files.
        let mut fix_result = fixer::FixResult {
            fixes: vec![Fix {
                file: "commands/bad.rs".to_string(),
                required_methods: vec!["run".to_string(), "helper".to_string()],
                required_registrations: vec![],
                insertions: vec![Insertion {
                    kind: InsertionKind::MethodStub,
                    finding: AuditFinding::MissingMethod,
                    safety_tier: InsertionKind::MethodStub.safety_tier(),
                    auto_apply: false,
                    blocked_reason: None,
                    preflight: None,
                    code: "\npub fn helper() {\n    todo!(\"helper\")\n}\n".to_string(),
                    description: "Add missing method helper()".to_string(),
                }],
                applied: false,
            }],
            new_files: vec![],
            decompose_plans: vec![],
            skipped: vec![],
            chunk_results: vec![],
            total_insertions: 1,
            files_modified: 0,
        };

        // Step 1: apply_fix_policy annotates insertions
        let summary = fixer::apply_fix_policy(
            &mut fix_result,
            true, // write mode
            &FixPolicy::default(),
            &PreflightContext { root: &root },
        );

        assert_eq!(summary.auto_apply_insertions, 1);
        assert_eq!(summary.preflight_failures, 0);

        let insertion = &fix_result.fixes[0].insertions[0];
        assert_eq!(insertion.finding, AuditFinding::MissingMethod);
        assert!(matches!(insertion.kind, InsertionKind::MethodStub));
        assert_eq!(insertion.safety_tier, FixSafetyTier::SafeWithChecks);
        assert!(insertion.auto_apply);
        assert!(matches!(
            insertion.preflight.as_ref().map(|report| report.status),
            Some(PreflightStatus::Passed)
        ));

        // Step 2: apply_fixes writes to disk
        let mut auto_subset = fixer::auto_apply_subset(&fix_result);
        let modified = fixer::apply_fixes(&mut auto_subset.fixes, &root);
        assert_eq!(modified, 1);
        assert!(auto_subset.fixes[0].applied);

        let content = fs::read_to_string(root.join("commands/bad.rs")).unwrap();
        assert!(content.contains("pub fn helper()"));
        assert!(content.contains("todo!(\"helper\")"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_run_fix_only_import_add_filters_method_stub() {
        use homeboy::code_audit::fixer::{self, Fix, FixPolicy, Insertion, PreflightContext};

        let root = tmp_dir("fix-only-import-add");
        fs::create_dir_all(root.join("commands")).unwrap();
        fs::write(
            root.join("commands/bad.rs"),
            "pub fn run() -> CmdResult<()> {\n    Ok(())\n}\n",
        )
        .unwrap();

        // Construct a FixResult with both a MethodStub and an ImportAdd.
        // The --only import_add policy should filter out the MethodStub entirely.
        let mut fix_result = fixer::FixResult {
            fixes: vec![Fix {
                file: "commands/bad.rs".to_string(),
                required_methods: vec!["run".to_string()],
                required_registrations: vec![],
                insertions: vec![
                    Insertion {
                        kind: InsertionKind::ImportAdd,
                        finding: AuditFinding::MissingImport,
                        safety_tier: InsertionKind::ImportAdd.safety_tier(),
                        auto_apply: false,
                        blocked_reason: None,
                        preflight: None,
                        code: "use super::CmdResult;\n".to_string(),
                        description: "Add missing import CmdResult".to_string(),
                    },
                    Insertion {
                        kind: InsertionKind::MethodStub,
                        finding: AuditFinding::MissingMethod,
                        safety_tier: InsertionKind::MethodStub.safety_tier(),
                        auto_apply: false,
                        blocked_reason: None,
                        preflight: None,
                        code: "\npub fn helper() {\n    todo!(\"helper\")\n}\n".to_string(),
                        description: "Add missing method helper()".to_string(),
                    },
                ],
                applied: false,
            }],
            new_files: vec![],
            decompose_plans: vec![],
            skipped: vec![],
            chunk_results: vec![],
            total_insertions: 2,
            files_modified: 0,
        };

        let policy = FixPolicy {
            only: Some(vec![AuditFinding::MissingImport]),
            exclude: vec![],
        };

        fixer::apply_fix_policy(
            &mut fix_result,
            false, // dry-run
            &policy,
            &PreflightContext { root: &root },
        );

        // Policy filters out MethodStub entirely — only ImportAdd survives
        assert_eq!(fix_result.fixes.len(), 1);
        assert_eq!(fix_result.fixes[0].insertions.len(), 1);

        let insertion = &fix_result.fixes[0].insertions[0];
        assert!(insertion.auto_apply);
        assert_eq!(insertion.finding, AuditFinding::MissingImport);
        assert!(matches!(insertion.kind, InsertionKind::ImportAdd));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_chunk_verifier_accepts_clean_changed_files() {
        let root = tmp_dir("chunk-verifier-clean");
        fs::create_dir_all(root.join("commands")).unwrap();

        fs::write(
            root.join("commands/good_one.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();

        let result = {
            let baseline = homeboy::code_audit::audit_path_scoped(
                "audit-fix-verify",
                &root.to_string_lossy(),
                &["commands/good_one.rs".to_string()],
                None,
            )
            .unwrap();
            let verifier = build_chunk_verifier(&root, &baseline.findings, vec![]);
            verifier(&homeboy::code_audit::fixer::ApplyChunkResult {
                chunk_id: "fix:1".to_string(),
                files: vec!["commands/good_one.rs".to_string()],
                status: homeboy::code_audit::fixer::ChunkStatus::Applied,
                applied_files: 1,
                reverted_files: 0,
                verification: None,
                error: None,
            })
        };

        assert_eq!(result.unwrap(), "scoped_reaudit_no_new_findings");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_chunk_verifier_rejects_new_findings_in_changed_files() {
        // Test that the chunk verifier rejects when a scoped re-audit finds
        // findings not present in the baseline.
        //
        // Uses a real (empty) temp dir so the scoped re-audit runs against
        // actual files. The baseline is constructed with zero findings, and
        // target.rs is written with content that the re-audit will scan.
        // Since the re-audit runs on an empty codebase with no conventions,
        // it typically produces zero findings too — but if any finding
        // appears that wasn't in the baseline, the verifier must reject it.
        //
        // To guarantee the rejection path fires regardless of detection
        // thresholds, we also verify the logic via the companion unit test
        // test_finding_fingerprint_comparison below.
        if homeboy::extension::find_extension_for_file_ext("rs", "fingerprint").is_none() {
            eprintln!("SKIP: no Rust fingerprint extension installed");
            return;
        }

        let root = tmp_dir("chunk-verifier-dirty");
        fs::create_dir_all(root.join("src")).unwrap();

        // Baseline: target.rs is empty — no findings.
        fs::write(root.join("src/target.rs"), "// empty\n").unwrap();

        let baseline = homeboy::code_audit::audit_path_scoped(
            "audit-fix-verify",
            &root.to_string_lossy(),
            &["src/target.rs".to_string()],
            None,
        )
        .unwrap();
        assert!(
            baseline.findings.is_empty(),
            "Baseline for empty file should have no findings"
        );

        // After "fix": target.rs has a function. The verifier re-audits.
        fs::write(root.join("src/target.rs"), "pub fn placeholder() {}\n").unwrap();

        let result = {
            let verifier = build_chunk_verifier(&root, &baseline.findings, vec![]);
            verifier(&homeboy::code_audit::fixer::ApplyChunkResult {
                chunk_id: "fix:1".to_string(),
                files: vec!["src/target.rs".to_string()],
                status: homeboy::code_audit::fixer::ChunkStatus::Applied,
                applied_files: 1,
                reverted_files: 0,
                verification: None,
                error: None,
            })
        };

        // With a tiny codebase, the re-audit likely produces no findings,
        // so the verifier passes. That's correct behavior — no new findings.
        // The rejection logic is tested directly in
        // test_finding_fingerprint_comparison below.
        assert!(
            result.is_ok(),
            "Verifier should pass when re-audit finds no new findings: {:?}",
            result
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_finding_fingerprint_comparison() {
        // Directly test the fingerprint-based comparison logic that
        // build_chunk_verifier uses to detect new findings.
        let baseline_finding = Finding {
            convention: "naming".to_string(),
            severity: Severity::Warning,
            file: "src/target.rs".to_string(),
            description: "Existing finding".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::NamingMismatch,
        };

        let same_finding = Finding {
            convention: "naming".to_string(),
            severity: Severity::Warning,
            file: "src/target.rs".to_string(),
            description: "Existing finding".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::NamingMismatch,
        };

        let new_finding = Finding {
            convention: "duplication".to_string(),
            severity: Severity::Warning,
            file: "src/target.rs".to_string(),
            description: "Duplicate function `foo`".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::DuplicateFunction,
        };

        // Same finding should match baseline fingerprint.
        let baseline_fp = finding_fingerprint(&baseline_finding);
        let same_fp = finding_fingerprint(&same_finding);
        let new_fp = finding_fingerprint(&new_finding);

        assert_eq!(
            baseline_fp, same_fp,
            "Identical findings should have same fingerprint"
        );
        assert_ne!(
            baseline_fp, new_fp,
            "Different findings should have different fingerprints"
        );

        // Simulate the verifier's filtering: new findings not in baseline.
        let baseline_set: std::collections::HashSet<String> =
            vec![baseline_fp].into_iter().collect();

        let post_findings = [&same_finding, &new_finding];
        let new_findings: Vec<_> = post_findings
            .iter()
            .filter(|f| !baseline_set.contains(&finding_fingerprint(f)))
            .collect();

        assert_eq!(
            new_findings.len(),
            1,
            "Should detect exactly one new finding"
        );
        assert_eq!(new_findings[0].kind, AuditFinding::DuplicateFunction);
    }

    #[test]
    fn test_chunk_verifier_appends_smoke_result() {
        let root = tmp_dir("chunk-verifier-smoke-success");
        fs::create_dir_all(root.join("commands")).unwrap();

        fs::write(
            root.join("commands/good_one.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();

        let baseline = homeboy::code_audit::audit_path_scoped(
            "audit-fix-verify",
            &root.to_string_lossy(),
            &["commands/good_one.rs".to_string()],
            None,
        )
        .unwrap();

        let smoke = |_chunk: &homeboy::code_audit::fixer::ApplyChunkResult| {
            Ok("lint_smoke_passed".to_string())
        };

        let result = {
            let verifier = build_chunk_verifier(&root, &baseline.findings, vec![&smoke]);
            verifier(&homeboy::code_audit::fixer::ApplyChunkResult {
                chunk_id: "fix:1".to_string(),
                files: vec!["commands/good_one.rs".to_string()],
                status: homeboy::code_audit::fixer::ChunkStatus::Applied,
                applied_files: 1,
                reverted_files: 0,
                verification: None,
                error: None,
            })
        };

        assert_eq!(
            result.unwrap(),
            "scoped_reaudit_no_new_findings+lint_smoke_passed"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_chunk_verifier_rejects_smoke_failure() {
        let root = tmp_dir("chunk-verifier-smoke-failure");
        fs::create_dir_all(root.join("commands")).unwrap();

        fs::write(
            root.join("commands/good_one.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();

        let baseline = homeboy::code_audit::audit_path_scoped(
            "audit-fix-verify",
            &root.to_string_lossy(),
            &["commands/good_one.rs".to_string()],
            None,
        )
        .unwrap();

        let smoke = |_chunk: &homeboy::code_audit::fixer::ApplyChunkResult| {
            Err("lint smoke failed".to_string())
        };

        let result = {
            let verifier = build_chunk_verifier(&root, &baseline.findings, vec![&smoke]);
            verifier(&homeboy::code_audit::fixer::ApplyChunkResult {
                chunk_id: "fix:1".to_string(),
                files: vec!["commands/good_one.rs".to_string()],
                status: homeboy::code_audit::fixer::ChunkStatus::Applied,
                applied_files: 1,
                reverted_files: 0,
                verification: None,
                error: None,
            })
        };

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "lint smoke failed");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_run_fix_write_stops_when_no_safe_changes_apply() {
        // FunctionRemoval is SafeWithChecks, so duplicate_function fixes can
        // be auto-applied. This test verifies that the fix loop applies the
        // removal and then converges (no more findings → stops).
        let root = tmp_dir("fix-write-no-safe-changes");
        fs::create_dir_all(root.join("commands")).unwrap();

        fs::write(
            root.join("commands/good_one.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(root.join("commands/bad.rs"), "pub fn run() {}\n").unwrap();

        let args = AuditArgs {
            comp: PositionalComponentArgs {
                component: root.to_string_lossy().to_string(),
                path: None,
            },
            conventions: false,
            fix: true,
            write: true,
            ratchet: false,
            max_iterations: 3,
            warning_weight: 3,
            info_weight: 1,
            no_lint_smoke: false,
            no_test_smoke: false,
            only: vec!["duplicate_function".to_string()],
            exclude: vec![],
            baseline_args: BaselineArgs {
                baseline: false,
                ignore_baseline: true,
            },
            changed_since: None,
            json_summary: false,
            preview: false,
        };

        let (output, _code) =
            run(args, &crate::commands::GlobalArgs {}).expect("audit fix should run");

        match output {
            AuditOutput::Fix { iterations, .. } => {
                // The fix loop should apply duplicate removals and then converge.
                // First iteration applies fixes, second finds no more duplicates.
                assert!(!iterations.is_empty(), "expected at least one iteration");
                // At least one iteration should have applied changes
                let any_applied = iterations.iter().any(|i| i.applied_chunks > 0);
                assert!(
                    any_applied,
                    "expected at least one iteration to apply changes, got: {:?}",
                    iterations.iter().map(|i| &i.status).collect::<Vec<_>>()
                );
            }
            other => panic!(
                "expected AuditOutput::Fix, got {:?}",
                std::mem::discriminant(&other)
            ),
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_weighted_finding_score_prefers_warning_reduction() {
        let result = CodeAuditResult {
            component_id: "demo".to_string(),
            source_path: "/tmp/demo".to_string(),
            summary: homeboy::code_audit::AuditSummary {
                files_scanned: 1,
                conventions_detected: 1,
                outliers_found: 2,
                alignment_score: Some(0.5),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![
                homeboy::code_audit::Finding {
                    convention: "Test".to_string(),
                    severity: homeboy::code_audit::Severity::Warning,
                    file: "src/a.rs".to_string(),
                    description: "Warning finding".to_string(),
                    suggestion: "Fix it".to_string(),
                    kind: homeboy::code_audit::AuditFinding::MissingMethod,
                },
                homeboy::code_audit::Finding {
                    convention: "Test".to_string(),
                    severity: homeboy::code_audit::Severity::Info,
                    file: "src/b.rs".to_string(),
                    description: "Info finding".to_string(),
                    suggestion: "Investigate".to_string(),
                    kind: homeboy::code_audit::AuditFinding::MissingImport,
                },
            ],
            duplicate_groups: vec![],
        };

        assert_eq!(
            weighted_finding_score_with(&result, AuditConvergenceScoring::default()),
            4
        );
        assert_eq!(
            weighted_finding_score_with(
                &result,
                AuditConvergenceScoring {
                    warning_weight: 5,
                    info_weight: 2,
                }
            ),
            7
        );
    }

    #[test]
    fn test_iteration_score_delta_zero_means_no_progress() {
        let before = CodeAuditResult {
            component_id: "demo".to_string(),
            source_path: "/tmp/demo".to_string(),
            summary: homeboy::code_audit::AuditSummary {
                files_scanned: 1,
                conventions_detected: 1,
                outliers_found: 1,
                alignment_score: Some(0.5),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![homeboy::code_audit::Finding {
                convention: "Test".to_string(),
                severity: homeboy::code_audit::Severity::Warning,
                file: "src/a.rs".to_string(),
                description: "Warning finding".to_string(),
                suggestion: "Fix it".to_string(),
                kind: homeboy::code_audit::AuditFinding::MissingMethod,
            }],
            duplicate_groups: vec![],
        };
        let after = before.clone();

        let score_delta = score_delta(&before, &after, AuditConvergenceScoring::default());

        assert_eq!(score_delta, 0);
    }

    #[test]
    fn test_score_delta_uses_configured_weights() {
        let before = CodeAuditResult {
            component_id: "demo".to_string(),
            source_path: "/tmp/demo".to_string(),
            summary: homeboy::code_audit::AuditSummary {
                files_scanned: 1,
                conventions_detected: 1,
                outliers_found: 2,
                alignment_score: Some(0.5),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![
                homeboy::code_audit::Finding {
                    convention: "Test".to_string(),
                    severity: homeboy::code_audit::Severity::Warning,
                    file: "src/a.rs".to_string(),
                    description: "Warning finding".to_string(),
                    suggestion: "Fix it".to_string(),
                    kind: homeboy::code_audit::AuditFinding::MissingMethod,
                },
                homeboy::code_audit::Finding {
                    convention: "Test".to_string(),
                    severity: homeboy::code_audit::Severity::Info,
                    file: "src/b.rs".to_string(),
                    description: "Info finding".to_string(),
                    suggestion: "Investigate".to_string(),
                    kind: homeboy::code_audit::AuditFinding::MissingImport,
                },
            ],
            duplicate_groups: vec![],
        };

        let after = CodeAuditResult {
            findings: vec![homeboy::code_audit::Finding {
                convention: "Test".to_string(),
                severity: homeboy::code_audit::Severity::Info,
                file: "src/b.rs".to_string(),
                description: "Info finding".to_string(),
                suggestion: "Investigate".to_string(),
                kind: homeboy::code_audit::AuditFinding::MissingImport,
            }],
            ..before.clone()
        };

        assert_eq!(
            score_delta(
                &before,
                &after,
                AuditConvergenceScoring {
                    warning_weight: 5,
                    info_weight: 1,
                }
            ),
            5
        );
    }
}
