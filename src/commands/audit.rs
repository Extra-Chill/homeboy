use clap::Args;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

use homeboy::code_audit::{self, baseline, fixer, CodeAuditResult};
use homeboy::component;
use homeboy::extension::ExtensionRunner;
use homeboy::git;
use homeboy::utils::autofix::{self, AutofixMode};

use super::args::BaselineArgs;
use super::test_scope::{build_phpunit_filter_regex, compute_changed_test_scope};
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

    /// Maximum recursive autofix iterations when writing
    #[arg(long, requires = "fix", default_value_t = 1)]
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
        policy_summary: AuditFixPolicySummary,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        iterations: Vec<AuditFixIterationSummary>,
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

#[derive(Debug, Serialize)]
pub struct AuditFixIterationSummary {
    iteration: usize,
    findings_before: usize,
    findings_after: usize,
    weighted_score_before: usize,
    weighted_score_after: usize,
    score_delta: isize,
    applied_chunks: usize,
    reverted_chunks: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    changed_files: Vec<String>,
    status: String,
}

#[derive(Debug, Clone, Copy)]
struct ConvergenceScoring {
    warning_weight: usize,
    info_weight: usize,
}

#[derive(Debug, Clone, Copy)]
struct VerificationToggles {
    lint_smoke: bool,
    test_smoke: bool,
}

impl Default for ConvergenceScoring {
    fn default() -> Self {
        Self {
            warning_weight: 3,
            info_weight: 1,
        }
    }
}

impl ConvergenceScoring {
    fn severity_weight(&self, severity: &homeboy::code_audit::Severity) -> usize {
        match severity {
            homeboy::code_audit::Severity::Warning => self.warning_weight,
            homeboy::code_audit::Severity::Info => self.info_weight,
        }
    }

    fn weighted_finding_score(&self, result: &CodeAuditResult) -> usize {
        result
            .findings
            .iter()
            .map(|finding| self.severity_weight(&finding.severity))
            .sum()
    }
}

fn weighted_finding_score_with(result: &CodeAuditResult, scoring: ConvergenceScoring) -> usize {
    scoring.weighted_finding_score(result)
}

fn score_delta(
    before: &CodeAuditResult,
    after: &CodeAuditResult,
    scoring: ConvergenceScoring,
) -> isize {
    weighted_finding_score_with(before, scoring) as isize
        - weighted_finding_score_with(after, scoring) as isize
}

fn parse_fix_kinds(values: &[String], flag: &str) -> homeboy::Result<Vec<fixer::FixKind>> {
    values
        .iter()
        .map(|value| {
            fixer::FixKind::from_str(value).map_err(|_| {
                homeboy::Error::validation_invalid_argument(
                    flag,
                    format!(
                        "Unknown fix kind '{}'. Valid kinds: method_stub, registration_stub, constructor_with_registration, import_add, function_removal, trait_use, missing_test_file, missing_test_method, shared_extraction",
                        value
                    ),
                    None,
                    None,
                )
            })
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
    let scoring = ConvergenceScoring {
        warning_weight: args.warning_weight,
        info_weight: args.info_weight,
    };
    let verification = VerificationToggles {
        lint_smoke: !args.no_lint_smoke,
        test_smoke: !args.no_test_smoke,
    };
    let only_kinds = parse_fix_kinds(&args.only, "only")?;
    let exclude_kinds = parse_fix_kinds(&args.exclude, "exclude")?;

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
        let mut current_result = result;
        let mut iterations = Vec::new();
        let mut seen_fingerprints = HashSet::new();
        let mut final_fix_result = fixer::FixResult {
            fixes: vec![],
            new_files: vec![],
            skipped: vec![],
            chunk_results: vec![],
            total_insertions: 0,
            files_modified: 0,
        };
        let mut final_policy_summary = fixer::PolicySummary::default();
        let written = args.write;

        if written {
            for iteration_index in 0..args.max_iterations.max(1) {
                let before_fingerprint = findings_fingerprint(&current_result);
                if !seen_fingerprints.insert(before_fingerprint) {
                    iterations.push(AuditFixIterationSummary {
                        iteration: iteration_index + 1,
                        findings_before: current_result.findings.len(),
                        findings_after: current_result.findings.len(),
                        weighted_score_before: weighted_finding_score_with(
                            &current_result,
                            scoring,
                        ),
                        weighted_score_after: weighted_finding_score_with(&current_result, scoring),
                        score_delta: 0,
                        applied_chunks: 0,
                        reverted_chunks: 0,
                        changed_files: vec![],
                        status: "stopped_cycle_detected".to_string(),
                    });
                    break;
                }

                let (fix_result, policy_summary, mut iteration_summary) = run_fix_iteration(
                    &current_result,
                    &only_kinds,
                    &exclude_kinds,
                    scoring,
                    verification,
                )?;

                let changed_files = iteration_summary.changed_files.clone();
                final_fix_result = fix_result.clone();
                final_policy_summary = policy_summary;

                if changed_files.is_empty() {
                    iteration_summary.iteration = iteration_index + 1;
                    iteration_summary.findings_after = current_result.findings.len();
                    iteration_summary.weighted_score_after =
                        weighted_finding_score_with(&current_result, scoring);
                    iteration_summary.score_delta =
                        score_delta(&current_result, &current_result, scoring);
                    iteration_summary.status = "stopped_no_safe_changes".to_string();
                    iterations.push(iteration_summary);
                    break;
                }

                let next_result = homeboy::code_audit::audit_path_with_id(
                    &current_result.component_id,
                    &current_result.source_path,
                )?;

                iteration_summary.iteration = iteration_index + 1;
                iteration_summary.findings_after = next_result.findings.len();
                iteration_summary.weighted_score_after =
                    weighted_finding_score_with(&next_result, scoring);
                iteration_summary.score_delta = score_delta(&current_result, &next_result, scoring);
                iteration_summary.status = if next_result.findings.is_empty() {
                    "stopped_clean".to_string()
                } else if iteration_summary.score_delta <= 0 {
                    "stopped_no_progress".to_string()
                } else {
                    "continued".to_string()
                };
                let should_stop =
                    next_result.findings.is_empty() || iteration_summary.score_delta <= 0;
                iterations.push(iteration_summary);

                if should_stop {
                    current_result = next_result;
                    break;
                }

                current_result = next_result;
            }
        } else {
            let root = Path::new(&current_result.source_path);
            let mut fix_result = fixer::generate_fixes(&current_result, root);
            let policy = fixer::FixPolicy {
                only: (!only_kinds.is_empty()).then_some(only_kinds.clone()),
                exclude: exclude_kinds.clone(),
            };
            let preflight_context = fixer::PreflightContext { root };
            final_policy_summary =
                fixer::apply_fix_policy(&mut fix_result, args.write, &policy, &preflight_context);
            final_fix_result = fix_result;
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

        return Ok((
            AuditOutput::Fix {
                component_id: current_result.component_id,
                source_path: current_result.source_path,
                status: outcome.status,
                fix_result: final_fix_result,
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

fn finding_fingerprint(finding: &homeboy::code_audit::Finding) -> String {
    format!(
        "{}::{:?}::{}::{}",
        finding.file, finding.kind, finding.convention, finding.description
    )
}

fn findings_fingerprint(result: &CodeAuditResult) -> Vec<String> {
    let mut fingerprints: Vec<String> = result.findings.iter().map(finding_fingerprint).collect();
    fingerprints.sort();
    fingerprints
}

fn build_smoke_verifier<'a>(
    component_id: &'a str,
    source_path: &'a str,
    changed_files: &'a [String],
) -> Option<impl Fn(&fixer::ApplyChunkResult) -> Result<String, String> + 'a> {
    let component = component::load(component_id).ok()?;
    let script_path = super::lint::resolve_lint_script(&component).ok()?;
    let root = PathBuf::from(source_path);
    Some(move |chunk: &fixer::ApplyChunkResult| {
        if changed_files.is_empty() {
            return Ok("lint_smoke_skipped_no_files".to_string());
        }

        if chunk.files.is_empty() {
            return Ok("lint_smoke_skipped_no_chunk_files".to_string());
        }

        let target_files: Vec<String> = changed_files
            .iter()
            .filter(|file| chunk.files.contains(file))
            .cloned()
            .collect();

        if target_files.is_empty() {
            return Ok("lint_smoke_skipped_no_overlap".to_string());
        }

        let glob = if target_files.len() == 1 {
            root.join(&target_files[0]).to_string_lossy().to_string()
        } else {
            let joined = target_files
                .iter()
                .map(|file| root.join(file).to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{}}}", joined)
        };

        let output = ExtensionRunner::new(component_id, &script_path)
            .path_override(Some(source_path.to_string()))
            .env("HOMEBOY_LINT_GLOB", &glob)
            .run()
            .map_err(|error| format!("lint smoke run failed: {}", error))?;

        if output.success {
            Ok("lint_smoke_passed".to_string())
        } else {
            Err("lint smoke failed".to_string())
        }
    })
}

fn build_test_smoke_verifier<'a>(
    component_id: &'a str,
    source_path: &'a str,
    changed_files: &'a [String],
) -> Option<impl Fn(&fixer::ApplyChunkResult) -> Result<String, String> + 'a> {
    let component = component::load(component_id).ok()?;
    let script_path = super::test::resolve_test_script(&component).ok()?;
    let changed_scope = compute_changed_test_scope(&component, "HEAD~1").ok();

    Some(move |chunk: &fixer::ApplyChunkResult| {
        if chunk.files.is_empty() || changed_files.is_empty() {
            return Ok("test_smoke_skipped_no_files".to_string());
        }

        let overlapping_files: Vec<String> = changed_files
            .iter()
            .filter(|file| chunk.files.contains(file))
            .cloned()
            .collect();

        if overlapping_files.is_empty() {
            return Ok("test_smoke_skipped_no_overlap".to_string());
        }

        let results_file = std::env::temp_dir().join(format!(
            "homeboy-audit-test-smoke-{}-{}.json",
            std::process::id(),
            chunk.chunk_id.replace(':', "-")
        ));

        let runner = ExtensionRunner::new(component_id, &script_path)
            .path_override(Some(source_path.to_string()))
            .env("HOMEBOY_SKIP_LINT", "1")
            .env("HOMEBOY_TEST_RESULTS_FILE", &results_file.to_string_lossy());

        let mut args = Vec::new();
        if let Some(scope) = &changed_scope {
            if !scope.selected_files.is_empty() {
                args.push(format!(
                    "--filter={}",
                    build_phpunit_filter_regex(&scope.selected_files)
                ));
            }
        }

        let output = runner
            .script_args(&args)
            .run()
            .map_err(|error| format!("test smoke run failed: {}", error))?;

        let _ = std::fs::remove_file(&results_file);

        if output.success {
            Ok("test_smoke_passed".to_string())
        } else {
            Err("test smoke failed".to_string())
        }
    })
}

fn run_fix_iteration(
    audit_result: &CodeAuditResult,
    only_kinds: &[fixer::FixKind],
    exclude_kinds: &[fixer::FixKind],
    scoring: ConvergenceScoring,
    verification: VerificationToggles,
) -> homeboy::Result<(
    fixer::FixResult,
    fixer::PolicySummary,
    AuditFixIterationSummary,
)> {
    let root = Path::new(&audit_result.source_path);
    let mut fix_result = fixer::generate_fixes(audit_result, root);
    let policy = fixer::FixPolicy {
        only: (!only_kinds.is_empty()).then_some(only_kinds.to_vec()),
        exclude: exclude_kinds.to_vec(),
    };
    let preflight_context = fixer::PreflightContext { root };
    let policy_summary =
        fixer::apply_fix_policy(&mut fix_result, true, &policy, &preflight_context);

    let mut applied_chunks = 0;
    let mut reverted_chunks = 0;
    let mut total_modified = 0;
    let mut auto_apply_result = fixer::auto_apply_subset(&fix_result);
    let changed_files: Vec<String> = auto_apply_result
        .fixes
        .iter()
        .map(|fix| fix.file.clone())
        .chain(
            auto_apply_result
                .new_files
                .iter()
                .map(|file| file.file.clone()),
        )
        .collect();
    let smoke_verifier = build_smoke_verifier(
        &audit_result.component_id,
        &audit_result.source_path,
        &changed_files,
    )
    .filter(|_| verification.lint_smoke);
    let test_smoke_verifier = build_test_smoke_verifier(
        &audit_result.component_id,
        &audit_result.source_path,
        &changed_files,
    )
    .filter(|_| verification.test_smoke);
    let mut extra_smokes: Vec<&dyn Fn(&fixer::ApplyChunkResult) -> Result<String, String>> =
        Vec::new();
    if let Some(verifier) = smoke_verifier.as_ref() {
        extra_smokes.push(verifier);
    }
    if let Some(verifier) = test_smoke_verifier.as_ref() {
        extra_smokes.push(verifier);
    }
    let verifier = build_chunk_verifier(root, &audit_result.findings, extra_smokes);

    if !auto_apply_result.fixes.is_empty() {
        let chunk_results = fixer::apply_fixes_chunked(
            &mut auto_apply_result.fixes,
            root,
            fixer::ApplyOptions {
                verifier: Some(&verifier),
            },
        );
        applied_chunks += chunk_results
            .iter()
            .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Applied))
            .count();
        reverted_chunks += chunk_results
            .iter()
            .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Reverted))
            .count();
        total_modified += chunk_results
            .iter()
            .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Applied))
            .map(|chunk| chunk.applied_files)
            .sum::<usize>();
        fix_result.chunk_results.extend(chunk_results);
    }

    if !auto_apply_result.new_files.is_empty() {
        let chunk_results = fixer::apply_new_files_chunked(
            &mut auto_apply_result.new_files,
            root,
            fixer::ApplyOptions {
                verifier: Some(&verifier),
            },
        );
        applied_chunks += chunk_results
            .iter()
            .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Applied))
            .count();
        reverted_chunks += chunk_results
            .iter()
            .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Reverted))
            .count();
        total_modified += chunk_results
            .iter()
            .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Applied))
            .map(|chunk| chunk.applied_files)
            .sum::<usize>();
        fix_result.chunk_results.extend(chunk_results);
    }

    for applied_fix in auto_apply_result.fixes {
        if let Some(original) = fix_result
            .fixes
            .iter_mut()
            .find(|candidate| candidate.file == applied_fix.file)
        {
            original.applied = applied_fix.applied;
        }
    }

    for written_file in auto_apply_result.new_files {
        if let Some(original) = fix_result
            .new_files
            .iter_mut()
            .find(|candidate| candidate.file == written_file.file)
        {
            original.written = written_file.written;
        }
    }

    fix_result.files_modified = total_modified;

    let changed_files: Vec<String> = fix_result
        .chunk_results
        .iter()
        .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Applied))
        .flat_map(|chunk| chunk.files.clone())
        .collect();

    Ok((
        fix_result,
        policy_summary,
        AuditFixIterationSummary {
            iteration: 0,
            findings_before: audit_result.findings.len(),
            findings_after: 0,
            weighted_score_before: weighted_finding_score_with(audit_result, scoring),
            weighted_score_after: 0,
            score_delta: 0,
            applied_chunks,
            reverted_chunks,
            changed_files,
            status: String::new(),
        },
    ))
}

fn build_chunk_verifier<'a>(
    root: &'a Path,
    baseline_findings: &'a [homeboy::code_audit::Finding],
    extra_smokes: Vec<&'a dyn Fn(&fixer::ApplyChunkResult) -> Result<String, String>>,
) -> impl Fn(&fixer::ApplyChunkResult) -> Result<String, String> + 'a {
    move |chunk| {
        let changed_files = chunk.files.clone();
        if changed_files.is_empty() {
            return Ok("no_files".to_string());
        }

        let baseline: HashSet<String> = baseline_findings
            .iter()
            .filter(|finding| changed_files.contains(&finding.file))
            .map(finding_fingerprint)
            .collect();

        let audit_result = code_audit::audit_path_scoped(
            "audit-fix-verify",
            &root.to_string_lossy(),
            &changed_files,
        )
        .map_err(|error| format!("verification audit failed: {}", error))?;

        let new_findings: Vec<String> = audit_result
            .findings
            .iter()
            .filter(|finding| changed_files.contains(&finding.file))
            .filter(|finding| !baseline.contains(&finding_fingerprint(finding)))
            .map(|finding| format!("{}: {:?}", finding.file, finding.kind))
            .collect();

        if new_findings.is_empty() {
            if extra_smokes.is_empty() {
                Ok("scoped_reaudit_no_new_findings".to_string())
            } else {
                let mut verification = "scoped_reaudit_no_new_findings".to_string();
                for smoke in &extra_smokes {
                    let smoke_result = smoke(chunk)?;
                    verification.push('+');
                    verification.push_str(&smoke_result);
                }
                Ok(verification)
            }
        } else {
            Err(format!(
                "scoped re-audit introduced new findings in changed files: {}",
                new_findings.join(", ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::default_audit_exit_code;
    use super::{run, AuditArgs, AuditOutput};
    use crate::commands::args::BaselineArgs;
    use homeboy::code_audit::fixer::{FixKind, FixSafetyTier, InsertionKind};
    use homeboy::code_audit::DeviationKind;
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

    #[test]
    fn test_run_fix_write_applies_preflight_checked_method_stub() {
        let root = tmp_dir("fix-write-applies-preflight-checked-method-stub");
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
            component_id: root.to_string_lossy().to_string(),
            conventions: false,
            fix: true,
            write: true,
            max_iterations: 1,
            warning_weight: 3,
            info_weight: 1,
            no_lint_smoke: false,
            no_test_smoke: false,
            only: vec![],
            exclude: vec![],
            baseline_args: BaselineArgs {
                baseline: false,
                ignore_baseline: true,
            },
            path: None,
            changed_since: None,
            json_summary: false,
        };

        let (output, _code) =
            run(args, &crate::commands::GlobalArgs {}).expect("audit fix should run");

        match output {
            AuditOutput::Fix {
                fix_result,
                written,
                ..
            } => {
                assert!(written);
                assert_eq!(fix_result.fixes.len(), 1);
                let insertion = &fix_result.fixes[0].insertions[0];
                assert_eq!(insertion.fix_kind, FixKind::MethodStub);
                assert!(matches!(insertion.kind, InsertionKind::MethodStub));
                assert_eq!(insertion.safety_tier, FixSafetyTier::SafeWithChecks);
                assert!(insertion.auto_apply);
                assert!(fix_result.fixes[0].applied);
                assert!(matches!(
                    insertion.preflight.as_ref().map(|report| report.status),
                    Some(homeboy::code_audit::fixer::PreflightStatus::Passed)
                ));

                let content = fs::read_to_string(root.join("commands/bad.rs")).unwrap();
                assert!(content.contains("pub fn helper()"));
                assert!(content.contains("todo!(\"helper\")"));
            }
            other => panic!(
                "expected AuditOutput::Fix, got {:?}",
                std::mem::discriminant(&other)
            ),
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_run_fix_only_import_add_filters_method_stub() {
        let root = tmp_dir("fix-only-import-add");
        fs::create_dir_all(root.join("commands")).unwrap();

        fs::write(
            root.join("commands/good_one.rs"),
            "use super::CmdResult;\npub fn run() -> CmdResult<()> {\n    Ok(())\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "use super::CmdResult;\npub fn run() -> CmdResult<()> {\n    Ok(())\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/bad.rs"),
            "pub fn run() -> CmdResult<()> {\n    Ok(())\n}\n",
        )
        .unwrap();

        let args = AuditArgs {
            component_id: root.to_string_lossy().to_string(),
            conventions: false,
            fix: true,
            write: false,
            max_iterations: 1,
            warning_weight: 3,
            info_weight: 1,
            no_lint_smoke: false,
            no_test_smoke: false,
            only: vec!["import_add".to_string()],
            exclude: vec![],
            baseline_args: BaselineArgs {
                baseline: false,
                ignore_baseline: true,
            },
            path: None,
            changed_since: None,
            json_summary: false,
        };

        let (output, _code) =
            run(args, &crate::commands::GlobalArgs {}).expect("audit fix should run");

        match output {
            AuditOutput::Fix { fix_result, .. } => {
                assert_eq!(fix_result.fixes.len(), 1);
                let insertion = &fix_result.fixes[0].insertions[0];
                assert!(matches!(insertion.kind, InsertionKind::ImportAdd));
                assert_eq!(insertion.fix_kind, FixKind::ImportAdd);
                assert!(insertion.auto_apply);
            }
            other => panic!(
                "expected AuditOutput::Fix, got {:?}",
                std::mem::discriminant(&other)
            ),
        }

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
            )
            .unwrap();
            let verifier = super::build_chunk_verifier(&root, &baseline.findings, vec![]);
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
        let root = tmp_dir("chunk-verifier-dirty");
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
        fs::write(
            root.join("commands/target.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();

        let baseline = homeboy::code_audit::audit_path_scoped(
            "audit-fix-verify",
            &root.to_string_lossy(),
            &["commands/target.rs".to_string()],
        )
        .unwrap();

        fs::write(root.join("commands/target.rs"), "pub fn run() {}\n").unwrap();

        let result = {
            let verifier = super::build_chunk_verifier(&root, &baseline.findings, vec![]);
            verifier(&homeboy::code_audit::fixer::ApplyChunkResult {
                chunk_id: "fix:1".to_string(),
                files: vec!["commands/target.rs".to_string()],
                status: homeboy::code_audit::fixer::ChunkStatus::Applied,
                applied_files: 1,
                reverted_files: 0,
                verification: None,
                error: None,
            })
        };

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("scoped re-audit introduced new findings"));

        let _ = fs::remove_dir_all(root);
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
        )
        .unwrap();

        let smoke = |_chunk: &homeboy::code_audit::fixer::ApplyChunkResult| {
            Ok("lint_smoke_passed".to_string())
        };

        let result = {
            let verifier = super::build_chunk_verifier(&root, &baseline.findings, vec![&smoke]);
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
        )
        .unwrap();

        let smoke = |_chunk: &homeboy::code_audit::fixer::ApplyChunkResult| {
            Err("lint smoke failed".to_string())
        };

        let result = {
            let verifier = super::build_chunk_verifier(&root, &baseline.findings, vec![&smoke]);
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
            component_id: root.to_string_lossy().to_string(),
            conventions: false,
            fix: true,
            write: true,
            max_iterations: 3,
            warning_weight: 3,
            info_weight: 1,
            no_lint_smoke: false,
            no_test_smoke: false,
            only: vec!["function_removal".to_string()],
            exclude: vec![],
            baseline_args: BaselineArgs {
                baseline: false,
                ignore_baseline: true,
            },
            path: None,
            changed_since: None,
            json_summary: false,
        };

        let (output, _code) =
            run(args, &crate::commands::GlobalArgs {}).expect("audit fix should run");

        match output {
            AuditOutput::Fix { iterations, .. } => {
                assert_eq!(iterations.len(), 1);
                assert_eq!(iterations[0].status, "stopped_no_safe_changes");
                assert_eq!(iterations[0].applied_chunks, 0);
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
                    kind: homeboy::code_audit::DeviationKind::MissingMethod,
                },
                homeboy::code_audit::Finding {
                    convention: "Test".to_string(),
                    severity: homeboy::code_audit::Severity::Info,
                    file: "src/b.rs".to_string(),
                    description: "Info finding".to_string(),
                    suggestion: "Investigate".to_string(),
                    kind: homeboy::code_audit::DeviationKind::MissingImport,
                },
            ],
            duplicate_groups: vec![],
        };

        assert_eq!(
            super::weighted_finding_score_with(&result, super::ConvergenceScoring::default()),
            4
        );
        assert_eq!(
            super::weighted_finding_score_with(
                &result,
                super::ConvergenceScoring {
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
                kind: homeboy::code_audit::DeviationKind::MissingMethod,
            }],
            duplicate_groups: vec![],
        };
        let after = before.clone();

        let score_delta = super::score_delta(&before, &after, super::ConvergenceScoring::default());

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
                    kind: homeboy::code_audit::DeviationKind::MissingMethod,
                },
                homeboy::code_audit::Finding {
                    convention: "Test".to_string(),
                    severity: homeboy::code_audit::Severity::Info,
                    file: "src/b.rs".to_string(),
                    description: "Info finding".to_string(),
                    suggestion: "Investigate".to_string(),
                    kind: homeboy::code_audit::DeviationKind::MissingImport,
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
                kind: homeboy::code_audit::DeviationKind::MissingImport,
            }],
            ..before.clone()
        };

        assert_eq!(
            super::score_delta(
                &before,
                &after,
                super::ConvergenceScoring {
                    warning_weight: 5,
                    info_weight: 1,
                }
            ),
            5
        );
    }
}
