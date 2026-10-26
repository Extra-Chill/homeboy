//! `homeboy review lint --fix`: run the extension's declared lint fixers in
//! place, keep only in-scope edits, and verify the result is lint-clean.
//!
//! The lifecycle is diagnose → route fixers → constrain changes → verify:
//!
//! 1. Run the linter without fixing to learn which files have findings.
//! 2. Invoke each extension-declared fixer route with `HOMEBOY_FIX_ONLY=1`.
//! 3. Keep only the changes inside the requested scope, restore release-owned
//!    files, and revert edits that change callable signatures.
//! 4. Re-run diagnostics; any remaining finding fails the fix.

use homeboy_core::component::Component;
use homeboy_core::engine::run_dir::{self, RunDir};
use homeboy_core::finding::HomeboyFinding;
use homeboy_core::git;
use homeboy_core::validation_progress::{write_command_artifact, ValidationProgressRecorder};
use homeboy_core::Error;
use homeboy_extension_contract::{FixResultsSummary, PrimitiveFixCount, RuleFixCount};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::fix_scope::{
    capture_dirty_file_snapshot, capture_release_owned_files, constrain_lint_fix_changes,
    lint_finding_scope_files, lint_scope_glob, reject_unsafe_lint_autofix_changes,
    restore_release_owned_files,
};
use super::{build_lint_runner, LintRunnerRequest, LintSniffFilters};
use crate::extension::invoke::ExtensionRunner;

/// Keep a mutating lint invocation below the client command timeout while
/// retaining enough time for large producer-specific fixers to finish.
const LINT_FIX_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Inputs for one `homeboy review lint --fix` run.
#[derive(Debug, Clone)]
pub struct LintFixRequest {
    pub component: Component,
    pub root: PathBuf,
    pub settings: Vec<(String, serde_json::Value)>,
    /// Explicit file selection (`--changed-only`). An empty selection is a
    /// clean no-op rather than a fall back to the whole workspace.
    pub selected_files: Option<Vec<String>>,
    pub summary: bool,
    pub file: Option<String>,
    pub glob: Option<String>,
    pub sniff_filters: LintSniffFilters,
    pub category: Option<String>,
    /// Restrict fixing to files changed since this git ref.
    pub changed_since: Option<String>,
    /// Allow writing to a dirty working tree or a checkout held by another
    /// live session.
    pub force: bool,
}

/// Result of a lint fix run, consumed by the lint report builder.
#[derive(Debug, Clone, Default)]
pub struct LintFixRun {
    pub applied: bool,
    pub files_modified: usize,
    pub changed_files: Vec<String>,
    pub hints: Vec<String>,
    pub warnings: Vec<String>,
    pub fix_summary: Option<FixResultsSummary>,
}

/// A single fix reported by an extension fixer through the fix-results sidecar.
#[derive(Debug, Clone, Deserialize)]
struct FixApplied {
    file: String,
    rule: String,
    #[serde(default)]
    primitive: Option<String>,
}

pub fn run_lint_fix(request: LintFixRequest) -> homeboy_core::Result<LintFixRun> {
    homeboy_core::worktree::enforce_write_ownership(
        &request.root,
        std::env::var(homeboy_core::observation::ACTIVE_RUN_ID_ENV)
            .ok()
            .as_deref(),
        request.force,
    )?;
    ensure_clean_enough_for_write(&request)?;

    let changed_since_files = match request.changed_since.as_deref() {
        Some(git_ref) => Some(git::get_files_changed_since(
            &request.root.to_string_lossy(),
            git_ref,
        )?),
        None => None,
    };

    let run_dir = RunDir::create()?;
    let outcome = fix_lint_findings(&request, changed_since_files.as_deref(), &run_dir)?;

    if !outcome.changed_files.is_empty() {
        let abs_changed: Vec<PathBuf> = outcome
            .changed_files
            .iter()
            .map(|file| request.root.join(file))
            .collect();
        require_successful_format(homeboy_core::engine::format_write::format_after_write(
            &request.component,
            &request.root,
            &abs_changed,
        )?)?;
    }

    let applied = !outcome.changed_files.is_empty();
    let hints = if applied {
        vec![format!(
            "Re-run checks: homeboy review lint {}",
            request.component.id
        )]
    } else {
        Vec::new()
    };

    run_dir.finish(true);
    Ok(LintFixRun {
        applied,
        files_modified: outcome.changed_files.len(),
        changed_files: outcome.changed_files,
        hints,
        warnings: outcome.warnings,
        fix_summary: summarize_fix_results(&outcome.fix_results),
    })
}

struct FixOutcome {
    changed_files: Vec<String>,
    fix_results: Vec<FixApplied>,
    warnings: Vec<String>,
}

impl FixOutcome {
    fn empty() -> Self {
        Self {
            changed_files: Vec::new(),
            fix_results: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

fn fix_lint_findings(
    request: &LintFixRequest,
    changed_since_files: Option<&[String]>,
    run_dir: &RunDir,
) -> homeboy_core::Result<FixOutcome> {
    let component = &request.component;
    let root = request.root.as_path();
    let root_str = root.to_string_lossy().to_string();
    let findings_file = run_dir.step_file(run_dir::files::LINT_FINDINGS);
    let fix_results_file = run_dir.step_file(run_dir::files::FIX_RESULTS);

    let requested_scope_files = request.selected_files.as_deref().or(changed_since_files);
    if requested_scope_files.is_some_and(|files| files.is_empty()) {
        return Ok(FixOutcome::empty());
    }
    let diagnostic_glob = match requested_scope_files {
        Some(files) => lint_scope_glob(&root_str, files),
        None => request.glob.clone(),
    };

    let mut progress = ValidationProgressRecorder::new(
        run_dir,
        None,
        vec![(
            "lint diagnostics".to_string(),
            "diagnose lint findings".to_string(),
        )],
    )?;
    let started = Instant::now();

    let lint_runner = |effective_glob: Option<&str>, step: Option<&str>| {
        build_lint_runner(LintRunnerRequest {
            component,
            path_override: None,
            settings: &request.settings,
            summary: request.summary,
            file: request.file.as_deref(),
            glob: effective_glob,
            errors_only: request.sniff_filters.errors_only,
            sniffs: request.sniff_filters.sniffs.as_deref(),
            exclude_sniffs: request.sniff_filters.exclude_sniffs.as_deref(),
            category: request.category.as_deref(),
            step,
            changed_files: None,
            run_dir,
        })
    };

    // Phase 1: diagnose without fixing so the engine owns the lifecycle.
    run_fix_phase(
        &mut progress,
        0,
        "lint diagnostics",
        lint_runner(diagnostic_glob.as_deref(), None)?,
        run_dir,
        remaining_budget(started, "lint diagnostics")?,
    )?;
    let lint_findings =
        homeboy_core::extension::lint::baseline::parse_findings_file(&findings_file)
            .unwrap_or_default();
    if lint_findings.is_empty() {
        return Ok(FixOutcome::empty());
    }

    // Phase 2: route each fixer. Diagnostics are the fallback scope; explicit
    // selected files still win.
    let finding_scope_files = lint_finding_scope_files(root, &lint_findings);
    let fix_files = requested_scope_files
        .or((!finding_scope_files.is_empty()).then_some(finding_scope_files.as_slice()))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "fix",
                "Lint findings did not identify files for an extension-declared fixer route"
                    .to_string(),
                None,
                Some(vec![
                    "Run homeboy review lint without --fix to inspect diagnostics".to_string(),
                ]),
            )
        })?;
    let fix_routes = super::resolve_lint_fix_routes(component, fix_files);
    if fix_routes.is_empty() {
        return Err(Error::validation_invalid_argument(
            "fix",
            format!(
                "No extension-declared lint fixer applies to the {} file(s) with findings",
                fix_files.len()
            ),
            None,
            Some(vec![
                "Run homeboy review lint without --fix to inspect unresolved diagnostics"
                    .to_string(),
            ]),
        ));
    }

    let before_dirty = git::get_dirty_files(&root_str).unwrap_or_default();
    let before_dirty_snapshot = capture_dirty_file_snapshot(root, &before_dirty);
    let release_owned = capture_release_owned_files(component, root)?;

    // HOMEBOY_FIX_ONLY=1 is the single contract: the extension runs its fixers
    // and skips its own validation pass; the engine verifies separately.
    let fixers = (|| -> homeboy_core::Result<()> {
        for route in &fix_routes {
            let route_glob = lint_scope_glob(&root_str, &route.files);
            let runner = lint_runner(route_glob.as_deref(), route.step.as_deref())?
                .env("HOMEBOY_FIX_ONLY", "1");
            let phase = format!("lint fixer {}", route.step.as_deref().unwrap_or("default"));
            let index = progress.append(phase.clone(), route_glob.clone().unwrap_or_default())?;
            run_fix_phase(
                &mut progress,
                index,
                &phase,
                runner,
                run_dir,
                remaining_budget(started, &phase)?,
            )?;
        }
        Ok(())
    })();
    restore_release_owned_files(root, &release_owned)?;
    fixers?;

    // Phase 3: keep only in-scope edits that preserve callable signatures.
    let after_dirty = git::get_dirty_files(&root_str).unwrap_or_default();
    let scope_outcome = constrain_lint_fix_changes(
        root,
        Some(fix_files),
        &before_dirty,
        &before_dirty_snapshot,
        after_dirty,
        &release_owned,
    )?;
    reject_unsafe_lint_autofix_changes(root, &scope_outcome.changed_files)?;

    // Phase 4: verify.
    let phase = "post-fix verification";
    let index = progress.append(
        phase.to_string(),
        "verify lint findings after fixes".to_string(),
    )?;
    run_fix_phase(
        &mut progress,
        index,
        phase,
        lint_runner(diagnostic_glob.as_deref(), None)?,
        run_dir,
        remaining_budget(started, phase)?,
    )?;
    let remaining = homeboy_core::extension::lint::baseline::parse_findings_file(&findings_file)?;
    reject_remaining_findings(&remaining)?;

    // A run that advertised safe fixes but changed nothing would make callers
    // retry the same fixer indefinitely (#10935).
    reject_unapplied_fixable_findings(&lint_findings, &scope_outcome.changed_files)?;

    Ok(FixOutcome {
        changed_files: scope_outcome.changed_files,
        fix_results: parse_fix_results_file(&fix_results_file),
        warnings: scope_outcome.warnings,
    })
}

/// Refuse to write to a dirty working tree unless `--force` is set or the run
/// is bounded to explicitly selected files. Mixing automated fixes with
/// uncommitted manual edits makes them hard to tell apart and roll back.
fn ensure_clean_enough_for_write(request: &LintFixRequest) -> homeboy_core::Result<()> {
    if request.force
        || request
            .selected_files
            .as_ref()
            .is_some_and(|files| !files.is_empty())
    {
        return Ok(());
    }

    let has_changes = git::get_uncommitted_changes(&request.root.to_string_lossy())
        .map(|changes| changes.has_changes)
        .unwrap_or(false);
    if !has_changes {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "fix",
        "Working tree has uncommitted changes",
        None,
        Some(vec![
            "Commit or stash your changes first".to_string(),
            "Rerun with --force to allow the fixer to edit the current dirty working tree"
                .to_string(),
        ]),
    ))
}

fn remaining_budget(started: Instant, phase: &str) -> homeboy_core::Result<Duration> {
    LINT_FIX_LIFECYCLE_TIMEOUT
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "fix",
                format!(
                    "Lint fix lifecycle exceeded its {}s budget before {phase}",
                    LINT_FIX_LIFECYCLE_TIMEOUT.as_secs()
                ),
                None,
                Some(vec![
                    "Inspect validation-progress.json to identify the last completed phase"
                        .to_string(),
                ]),
            )
        })
}

fn run_fix_phase(
    progress: &mut ValidationProgressRecorder<'_>,
    index: usize,
    phase: &str,
    runner: ExtensionRunner,
    run_dir: &RunDir,
    timeout: Duration,
) -> homeboy_core::Result<()> {
    progress.start(index)?;
    let output = match runner.timeout(Some(timeout)).run() {
        Ok(output) => output,
        Err(error) => {
            progress.finish(index, -1, None, None)?;
            return Err(error);
        }
    };
    let stdout_artifact = write_command_artifact(run_dir, index, "stdout", &output.stdout)?;
    let stderr_artifact = write_command_artifact(run_dir, index, "stderr", &output.stderr)?;
    progress.finish(index, output.exit_code, stdout_artifact, stderr_artifact)?;
    if output.timed_out {
        return Err(Error::validation_invalid_argument(
            "fix",
            format!("Lint fix {phase} timed out after {}ms", timeout.as_millis()),
            None,
            Some(vec![
                "Inspect validation-progress.json and child-supervision.json for the stalled phase"
                    .to_string(),
                "Rerun homeboy review lint without --fix to inspect the current diagnostics"
                    .to_string(),
            ]),
        ));
    }
    Ok(())
}

fn reject_remaining_findings(findings: &[HomeboyFinding]) -> homeboy_core::Result<()> {
    if findings.is_empty() {
        return Ok(());
    }

    let examples = findings
        .iter()
        .take(5)
        .map(|finding| {
            let location = finding
                .location
                .file
                .clone()
                .unwrap_or_else(|| "unknown file".to_string());
            let rule = finding.rule.clone().unwrap_or_else(|| finding.tool.clone());
            format!("{}: {} ({})", location, finding.message, rule)
        })
        .collect::<Vec<_>>();

    Err(Error::validation_invalid_argument(
        "fix",
        format!(
            "Lint fix left {} finding(s) after applying fixes: {}",
            findings.len(),
            examples.join("; ")
        ),
        None,
        Some(vec![
            "The worktree may be partially fixed; inspect the remaining lint findings before continuing".to_string(),
            "Rerun homeboy review lint without --fix to see the current diagnostics".to_string(),
        ]),
    ))
}

fn reject_unapplied_fixable_findings(
    findings: &[HomeboyFinding],
    changed_files: &[String],
) -> homeboy_core::Result<()> {
    if !changed_files.is_empty() {
        return Ok(());
    }
    let advertised_fixable = findings
        .iter()
        .filter(|finding| finding.fix.fixable == Some(true))
        .count();
    if advertised_fixable == 0 {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "fix",
        format!(
            "Lint advertised {advertised_fixable} auto-fixable finding(s), but the fixer did not modify the local worktree"
        ),
        None,
        Some(vec![
            "Inspect the extension fixer output and local worktree permissions".to_string(),
            "Run homeboy review lint <component> to inspect the remaining findings".to_string(),
        ]),
    ))
}

fn require_successful_format(
    fmt: homeboy_core::engine::format_write::FormatResult,
) -> homeboy_core::Result<()> {
    if fmt.success {
        return Ok(());
    }

    let command = fmt
        .command
        .unwrap_or_else(|| "unknown formatter".to_string());
    let output = fmt.output.unwrap_or_default();
    let problem = if output.trim().is_empty() {
        format!("post-write formatter ({command}) exited non-zero")
    } else {
        format!(
            "post-write formatter ({command}) exited non-zero: {}",
            output.trim()
        )
    };

    Err(Error::validation_invalid_argument(
        "fix",
        problem,
        None,
        Some(vec![
            "The worktree may be partially formatted; inspect the formatter output before continuing".to_string(),
            "Fix the formatter failure and rerun homeboy review lint --fix".to_string(),
        ]),
    ))
}

fn parse_fix_results_file(path: &Path) -> Vec<FixApplied> {
    std::fs::read_to_string(path)
        .ok()
        .filter(|content| !content.trim().is_empty())
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn summarize_fix_results(fixes: &[FixApplied]) -> Option<FixResultsSummary> {
    if fixes.is_empty() {
        return None;
    }

    let mut files = std::collections::HashSet::new();
    let mut rule_counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut primitive_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for fix in fixes {
        files.insert(fix.file.as_str());
        *rule_counts.entry(fix.rule.as_str()).or_insert(0) += 1;
        if let Some(primitive) = fix.primitive.as_deref() {
            *primitive_counts.entry(primitive).or_insert(0) += 1;
        }
    }

    Some(FixResultsSummary {
        fixes_applied: fixes.len(),
        files_modified: files.len(),
        rules: rule_counts
            .into_iter()
            .map(|(rule, count)| RuleFixCount {
                rule: rule.to_string(),
                count,
            })
            .collect(),
        primitives: primitive_counts
            .into_iter()
            .map(|(primitive, count)| PrimitiveFixCount {
                primitive: primitive.to_string(),
                count,
            })
            .collect(),
    })
}

#[cfg(test)]
#[path = "fix_tests.rs"]
mod tests;
