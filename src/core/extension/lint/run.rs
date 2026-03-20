//! Lint workflow orchestration — runs lint, resolves changed-file scoping,
//! drives autofix, processes baseline lifecycle, and assembles results.
//!
//! Mirrors `core/extension/test/run.rs` — the command layer provides CLI args,
//! this module owns all business logic and returns a structured result.

use crate::component::Component;
use crate::engine::temp;
use crate::extension::lint::baseline::{self as lint_baseline, LintFinding};
use crate::extension::lint::build_lint_runner;
use crate::git;
use crate::refactor::AppliedRefactor;
use serde::Serialize;
use std::path::PathBuf;

/// Arguments for the main lint workflow — populated by the command layer from CLI flags.
#[derive(Debug, Clone)]
pub struct LintRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub summary: bool,
    pub file: Option<String>,
    pub glob: Option<String>,
    pub changed_only: bool,
    pub changed_since: Option<String>,
    pub errors_only: bool,
    pub sniffs: Option<String>,
    pub exclude_sniffs: Option<String>,
    pub category: Option<String>,
    pub baseline: bool,
    pub ignore_baseline: bool,
}

/// Result of the main lint workflow — ready for report assembly.
#[derive(Debug, Clone, Serialize)]
pub struct LintRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub autofix: Option<AppliedRefactor>,
    pub hints: Option<Vec<String>>,
    pub baseline_comparison: Option<lint_baseline::BaselineComparison>,
    pub lint_findings: Option<Vec<LintFinding>>,
}

/// Run the main lint workflow.
///
/// Handles changed-file scoping, autofix planning, lint runner execution,
/// baseline lifecycle, hint assembly, and result construction.
pub fn run_main_lint_workflow(
    component: &Component,
    source_path: &PathBuf,
    args: LintRunWorkflowArgs,
) -> crate::Result<LintRunWorkflowResult> {
    // Resolve effective glob from --changed-only or --changed-since flags
    let effective_glob = resolve_effective_glob(component, &args)?;

    // Early exit if changed-file mode produced no files
    if let Some(ref glob_val) = effective_glob {
        if glob_val.is_empty() {
            return Ok(LintRunWorkflowResult {
                status: "passed".to_string(),
                component: args.component_label,
                exit_code: 0,
                autofix: None,
                hints: None,
                baseline_comparison: None,
                lint_findings: None,
            });
        }
    }

    // Run lint
    let lint_findings_file = temp::runtime_temp_file("homeboy-lint-findings", ".json")?;
    let findings_file_str = lint_findings_file.to_string_lossy().to_string();

    let output = build_lint_runner(
        component,
        args.path_override.clone(),
        &args.settings,
        args.summary,
        args.file.as_deref(),
        effective_glob.as_deref(),
        args.errors_only,
        args.sniffs.as_deref(),
        args.exclude_sniffs.as_deref(),
        args.category.as_deref(),
        &findings_file_str,
    )?
    .run()?;

    let lint_findings = lint_baseline::parse_findings_file(&lint_findings_file)?;
    let _ = std::fs::remove_file(&lint_findings_file);

    // Status computation — check findings first, exit code as fallback.
    // The extension runner uses passthrough mode (stdout goes to terminal),
    // so `output.success` only reflects the shell exit code. PHPCS/PHPStan
    // wrappers may exit 0 even when findings exist, so the sidecar findings
    // file is the canonical source of truth (mirrors test command pattern).
    let mut status = if !lint_findings.is_empty() {
        "failed"
    } else if output.success {
        "passed"
    } else {
        "failed"
    }
    .to_string();

    let mut hints = Vec::new();

    let lint_clean = lint_findings.is_empty() && output.success;

    // Baseline lifecycle
    let (baseline_comparison, baseline_exit_override) =
        process_baseline(source_path, &args, &lint_findings)?;

    // Hint assembly — point to refactor for fixes
    if !lint_clean {
        hints.push(format!(
            "Auto-fix: homeboy refactor {} --from lint --write",
            args.component_label
        ));
        hints.push("Some issues may require manual fixes".to_string());
    }

    if args.file.is_none()
        && args.glob.is_none()
        && !args.changed_only
        && args.changed_since.is_none()
    {
        hints.push(
            "For targeted linting: --file <path>, --glob <pattern>, --changed-only, or --changed-since <ref>".to_string(),
        );
    }

    hints.push("Full options: homeboy docs commands/lint".to_string());

    if !args.baseline && baseline_comparison.is_none() {
        hints.push(format!(
            "Save lint baseline: homeboy lint {} --baseline",
            args.component_label
        ));
    }

    let hints = if hints.is_empty() { None } else { Some(hints) };
    let exit_code = baseline_exit_override.unwrap_or(output.exit_code);
    if exit_code != output.exit_code {
        status = "failed".to_string();
    }

    Ok(LintRunWorkflowResult {
        status,
        component: args.component_label,
        exit_code,
        autofix: None,
        hints,
        baseline_comparison,
        lint_findings: Some(lint_findings),
    })
}

/// Resolve effective glob from --changed-only or --changed-since flags.
///
/// Returns `Some("")` (empty string) when changed-file mode is active but no files
/// were found — the caller should treat this as an early "passed" exit.
/// Returns `None` when no changed-file scoping is active (use args.glob directly).
fn resolve_effective_glob(
    component: &Component,
    args: &LintRunWorkflowArgs,
) -> crate::Result<Option<String>> {
    if args.changed_only {
        let uncommitted = git::get_uncommitted_changes(&component.local_path)?;
        let mut changed_files: Vec<String> = Vec::new();
        changed_files.extend(uncommitted.staged);
        changed_files.extend(uncommitted.unstaged);
        changed_files.extend(uncommitted.untracked);

        if changed_files.is_empty() {
            println!("No files in working tree changes");
            return Ok(Some(String::new()));
        }

        let abs_files: Vec<String> = changed_files
            .iter()
            .map(|f| format!("{}/{}", component.local_path, f))
            .collect();

        if abs_files.len() == 1 {
            Ok(Some(abs_files[0].clone()))
        } else {
            Ok(Some(format!("{{{}}}", abs_files.join(","))))
        }
    } else if let Some(ref git_ref) = args.changed_since {
        let changed_files = git::get_files_changed_since(&component.local_path, git_ref)?;

        if changed_files.is_empty() {
            println!("No files changed since {}", git_ref);
            return Ok(Some(String::new()));
        }

        let abs_files: Vec<String> = changed_files
            .iter()
            .map(|f| format!("{}/{}", component.local_path, f))
            .collect();

        if abs_files.len() == 1 {
            Ok(Some(abs_files[0].clone()))
        } else {
            Ok(Some(format!("{{{}}}", abs_files.join(","))))
        }
    } else {
        Ok(args.glob.clone())
    }
}

/// Process baseline lifecycle — save, load, compare.
fn process_baseline(
    source_path: &PathBuf,
    args: &LintRunWorkflowArgs,
    lint_findings: &[LintFinding],
) -> crate::Result<(Option<lint_baseline::BaselineComparison>, Option<i32>)> {
    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if args.baseline {
        let saved = lint_baseline::save_baseline(source_path, &args.component_id, lint_findings)?;
        eprintln!(
            "[lint] Baseline saved to {} ({} findings)",
            saved.display(),
            lint_findings.len()
        );
    }

    if !args.baseline && !args.ignore_baseline {
        if let Some(existing) = lint_baseline::load_baseline(source_path) {
            let comparison = lint_baseline::compare(lint_findings, &existing);

            if comparison.drift_increased {
                eprintln!(
                    "[lint] DRIFT INCREASED: {} new finding(s) since baseline",
                    comparison.new_items.len()
                );
                baseline_exit_override = Some(1);
            } else if !comparison.resolved_fingerprints.is_empty() {
                eprintln!(
                    "[lint] Drift reduced: {} finding(s) resolved since baseline",
                    comparison.resolved_fingerprints.len()
                );
            } else {
                eprintln!("[lint] No change from baseline");
            }

            baseline_comparison = Some(comparison);
        }
    }

    Ok((baseline_comparison, baseline_exit_override))
}
