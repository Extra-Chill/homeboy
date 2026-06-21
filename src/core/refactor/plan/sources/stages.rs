use super::cache::{
    try_load_cached_audit, try_load_cached_lint, try_load_cached_test, CachedLintResult,
    CachedTestResult, OUTPUT_DIR_ENV,
};
use super::extension_source::{read_optional_json, try_extension_refactor_source_stage};
use super::lint_scope::{
    capture_dirty_file_snapshot, capture_release_owned_files, constrain_lint_fix_changes,
    lint_finding_scope_files, lint_scope_glob, reject_unsafe_lint_autofix_changes,
    restore_release_owned_files,
};
use super::planning::{PlannedStage, SourceStageSummary};
use super::{audit_source::filtered_audit_source_result, LintSourceOptions, TestSourceOptions};
use crate::core::component::Component;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::engine::undo::UndoSnapshot;
use crate::core::extension;
use crate::core::git;
use crate::core::refactor::auto as fixer;
use crate::core::refactor::auto::{self, FixApplied};
use crate::core::refactor::plan::verify::AuditConvergenceScoring;
use crate::core::Error;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

pub(super) struct AuditStageRequest<'a> {
    pub(super) component: &'a Component,
    pub(super) root: &'a Path,
    pub(super) changed_files: Option<&'a [String]>,
    pub(super) only: &'a [crate::core::code_audit::AuditFinding],
    pub(super) exclude: &'a [crate::core::code_audit::AuditFinding],
    pub(super) write: bool,
    pub(super) settings: &'a [(String, String)],
}

/// Format modified files between refactor stages.
///
/// This ensures generated code (test files, refactored sources) is properly
/// formatted before subsequent stages run. Without this, the lint stage's
/// format-check fails on unformatted auto-generated code — blocking
/// the pipeline on problems it didn't create.
///
/// Uses the same `format_after_write` as the post-write step. Formatter
/// failures are fatal in write mode so the command never returns an
/// applied-success payload for a partially formatted worktree.
pub(super) fn format_changed_files(
    root: &Path,
    changed_files: &[String],
) -> crate::core::Result<()> {
    if changed_files.is_empty() {
        return Ok(());
    }

    let abs_changed: Vec<PathBuf> = changed_files.iter().map(|f| root.join(f)).collect();

    let fmt = crate::core::engine::format_write::format_after_write(root, &abs_changed)?;
    if let Some(cmd) = &fmt.command {
        if fmt.success {
            crate::log_status!(
                "format",
                "Formatted {} file(s) via {} (inter-stage)",
                abs_changed.len(),
                cmd
            );
        }
    }

    require_successful_format(fmt, "inter-stage formatter")
}

pub(super) fn require_successful_format(
    fmt: crate::core::engine::format_write::FormatResult,
    label: &str,
) -> crate::core::Result<()> {
    if fmt.success {
        return Ok(());
    }

    let command = fmt
        .command
        .unwrap_or_else(|| "unknown formatter".to_string());
    let output = fmt.output.unwrap_or_default();
    let problem = if output.trim().is_empty() {
        format!("{} ({}) exited non-zero", label, command)
    } else {
        format!("{} ({}) exited non-zero: {}", label, command, output.trim())
    };

    Err(Error::validation_invalid_argument(
        "write",
        problem,
        None,
        Some(vec![
            "The worktree may be partially formatted; inspect the formatter output before continuing".to_string(),
            "Fix the formatter failure and rerun the write command".to_string(),
        ]),
    ))
}

pub(super) fn reject_remaining_lint_fix_findings(
    findings: &[crate::core::finding::HomeboyFinding],
) -> crate::core::Result<()> {
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
            "Rerun homeboy lint without --fix to see the current diagnostics".to_string(),
        ]),
    ))
}

pub(super) fn plan_audit_stage(
    request: AuditStageRequest<'_>,
) -> crate::core::Result<PlannedStage> {
    let component_id = &request.component.id;
    let root = request.root;
    let result = if let Some(cached) = try_load_cached_audit() {
        cached
    } else if let Some(changed) = request.changed_files {
        if changed.is_empty() {
            crate::core::code_audit::CodeAuditResult {
                component_id: component_id.to_string(),
                source_path: root.to_string_lossy().to_string(),
                summary: crate::core::code_audit::AuditSummary {
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
            }
        } else {
            crate::core::code_audit::audit_path_scoped(
                component_id,
                &root.to_string_lossy(),
                changed,
                None,
            )?
        }
    } else {
        crate::core::code_audit::audit_path_with_id(component_id, &root.to_string_lossy())?
    };

    let policy = fixer::FixPolicy {
        only: (!request.only.is_empty()).then_some(request.only.to_vec()),
        exclude: request.exclude.to_vec(),
    };
    let extension_result = filtered_audit_source_result(&result, &policy);
    if let Some(stage) = try_extension_refactor_source_stage(
        "audit",
        request.component,
        root,
        &extension_result,
        request.write,
        request.settings,
    )? {
        return Ok(stage);
    }
    let mut fix_result =
        crate::core::refactor::plan::generate::generate_audit_fixes(&result, root, &policy);
    let (fix_result, policy_summary, changed_files, stage_warnings): (
        fixer::FixResult,
        fixer::PolicySummary,
        Vec<String>,
        Vec<String>,
    ) = if request.write {
        // Single pass: generate fixes from the provided findings, apply, validate.
        // The audit already ran and provided findings in `result` — the refactor
        // command does not re-run the audit internally. The convergence loop
        // (audit → fix → merge → re-audit) belongs in the full orchestration
        // pipeline, not inside a single refactor invocation.
        let outcome = crate::core::refactor::plan::verify::run_audit_refactor(
            result.clone(),
            request.only,
            request.exclude,
            AuditConvergenceScoring::default(),
            true,
        )?;

        let changed_files = outcome
            .fix_result
            .chunk_results
            .iter()
            .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Applied))
            .flat_map(|chunk| chunk.files.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        let warnings = outcome
            .iteration_summary
            .iter()
            .filter(|summary| summary.status != "continued")
            .map(|summary| format!("audit refactor: {}", summary.status))
            .collect::<Vec<_>>();

        (
            outcome.fix_result,
            outcome.policy_summary,
            changed_files,
            warnings,
        )
    } else {
        let policy_summary = fixer::apply_fix_policy(&mut fix_result, false, &policy);
        let changed_files = collect_audit_changed_files(&fix_result);

        // Surface findings whose collected edits are all manual-only — these
        // are visible in preview but `--write` will decline to apply them.
        // Without this hint the divergence between dry-run and --write is
        // invisible (homeboy#1159).
        let manual_only_count = count_manual_only_fixes(&fix_result);
        let stage_warnings = if manual_only_count > 0 {
            vec![format!(
                "{} finding(s) produced manual-only edits — visible in preview but will NOT be applied by --write. \
                 Resolve manually or acknowledge via baseline.",
                manual_only_count
            )]
        } else {
            Vec::new()
        };

        (fix_result, policy_summary, changed_files, stage_warnings)
    };

    let fix_results = summarize_audit_fix_result_entries(&fix_result);
    let edit_count = fix_results.len();

    Ok(PlannedStage {
        source: "audit".to_string(),
        summary: SourceStageSummary {
            stage: "audit".to_string(),
            collected: true,
            applied: request.write && !changed_files.is_empty(),
            edit_count,
            files_modified: changed_files.len(),
            detected_findings: Some(result.findings.len()),
            changed_files,
            fix_summary: if request.write {
                if fix_result.files_modified > 0 {
                    Some(auto::summarize_audit_fix_result(&fix_result))
                } else {
                    None
                }
            } else if policy_summary.visible_insertions + policy_summary.visible_new_files > 0 {
                Some(auto::summarize_audit_fix_result(&fix_result))
            } else {
                None
            },
            warnings: stage_warnings,
        },
        fix_results,
    })
}

pub(super) fn run_lint_stage(
    component: &Component,
    root: &Path,
    settings: &[(String, String)],
    options: &LintSourceOptions,
    changed_files: Option<&[String]>,
    write: bool,
    run_dir: &RunDir,
) -> crate::core::Result<PlannedStage> {
    // Check for cached lint results from the quality gate.
    let cached = try_load_cached_lint();

    // If clean, nothing to fix — skip entirely.
    if let Some(CachedLintResult::Clean) = cached {
        return Ok(empty_lint_stage());
    }

    let root_str = root.to_string_lossy().to_string();
    let findings_file = run_dir.step_file(run_dir::files::LINT_FINDINGS);
    let fix_sidecars = auto::AutofixSidecarFiles::for_run_dir(run_dir);

    let requested_scope_files = options.selected_files.as_deref().or(changed_files);
    if requested_scope_files.is_some_and(|files| files.is_empty()) {
        return Ok(empty_lint_stage());
    }
    let diagnostic_glob = if let Some(files) = requested_scope_files {
        lint_scope_glob(&root_str, files)
    } else {
        options.glob.clone()
    };

    // Helper: build the lint runner with the current stage options.
    // Used by both the diagnostic pass and the fix-only pass.
    let typed_settings = extension::lint::settings_from_legacy_strings(settings);
    let build_lint_runner = |effective_glob: Option<&str>| {
        extension::lint::build_lint_runner(
            component,
            None,
            &typed_settings,
            false,
            options.file.as_deref(),
            effective_glob,
            options.errors_only,
            options.sniffs.as_deref(),
            options.exclude_sniffs.as_deref(),
            options.category.as_deref(),
            None,
            run_dir,
        )
    };

    // ── Phase 1: Diagnose ──────────────────────────────────────────────
    // Run the linter WITHOUT auto-fix. The extension reports findings but
    // does NOT modify files. This separates diagnosis from fix application
    // so the engine controls the full lifecycle.
    //
    // When cached findings exist from the quality gate, skip the diagnostic
    // pass entirely — the engine already knows what needs fixing.
    let lint_findings = if let Some(CachedLintResult::HasFindings(count)) = cached {
        crate::log_status!(
            "refactor",
            "Using {} cached lint findings — skipping diagnostic pass",
            count
        );
        // Parse cached findings for reporting
        let output_dir = std::env::var(OUTPUT_DIR_ENV).ok();
        let cached_findings = output_dir
            .and_then(|dir| {
                let file = PathBuf::from(&dir).join("lint.json");
                let content = std::fs::read_to_string(&file).ok()?;
                let json: serde_json::Value = serde_json::from_str(&content).ok()?;
                let data = json.get("data")?;
                let findings: Vec<crate::core::finding::HomeboyFinding> =
                    serde_json::from_value(data.get("findings")?.clone()).ok()?;
                Some(findings)
            })
            .unwrap_or_default();
        cached_findings
    } else {
        // No cached findings — run the diagnostic pass.
        build_lint_runner(diagnostic_glob.as_deref())?.run()?;

        crate::core::extension::lint::baseline::parse_findings_file(&findings_file)
            .unwrap_or_default()
    };

    let lint_source_result = serde_json::json!({
        "findings": &lint_findings,
    });
    if let Some(stage) = try_extension_refactor_source_stage(
        "lint",
        component,
        root,
        &lint_source_result,
        write,
        settings,
    )? {
        return Ok(stage);
    }

    // ── Phase 2: Apply fixes (only when --write) ───────────────────────
    // The engine controls fix application. The extension's fix-mode
    // invocation runs ONLY the fixers, not the diagnostic pass. The engine
    // tracks what changed via undo snapshots and git diff.
    let finding_scope_files = if requested_scope_files.is_none() && options.glob.is_none() {
        lint_finding_scope_files(&lint_findings)
    } else {
        Vec::new()
    };
    let fix_scope_files = requested_scope_files.or({
        if finding_scope_files.is_empty() {
            None
        } else {
            Some(finding_scope_files.as_slice())
        }
    });
    let fix_glob = if let Some(files) = fix_scope_files {
        lint_scope_glob(&root_str, files)
    } else {
        options.glob.clone()
    };
    let (stage_changed_files, fix_results, stage_warnings) = if write && !lint_findings.is_empty() {
        let before_dirty = git::get_dirty_files(&root_str).unwrap_or_default();
        let before_dirty_snapshot = capture_dirty_file_snapshot(root, &before_dirty);

        // Save undo snapshot before applying lint fixes.
        let mut snap = UndoSnapshot::new(root, "lint fix");
        for file in &before_dirty {
            snap.capture_file(file);
        }
        if let Err(error) = snap.save() {
            crate::log_status!("undo", "Warning: failed to save undo snapshot: {}", error);
        }

        let release_owned = capture_release_owned_files(component, root)?;

        // Invoke the extension in fix-only mode.
        //
        // HOMEBOY_FIX_ONLY=1 is the single contract: the extension runs its
        // fixers and skips its own validation pass (the engine validates
        // separately via the diagnose phase). Auto-fixing lives exclusively
        // under `homeboy refactor` — there is no other entry point.
        let fix_output = build_lint_runner(fix_glob.as_deref())?
            .env("HOMEBOY_FIX_ONLY", "1")
            .run();
        restore_release_owned_files(root, &release_owned)?;
        fix_output?;

        let after_dirty = git::get_dirty_files(&root_str).unwrap_or_default();
        let scope_outcome = constrain_lint_fix_changes(
            root,
            fix_scope_files,
            &before_dirty,
            &before_dirty_snapshot,
            after_dirty,
            &release_owned,
        )?;
        reject_unsafe_lint_autofix_changes(root, &scope_outcome.changed_files)?;

        if !scope_outcome.changed_files.is_empty() {
            build_lint_runner(fix_glob.as_deref())?.run()?;
            let remaining_findings =
                crate::core::extension::lint::baseline::parse_findings_file(&findings_file)?;
            reject_remaining_lint_fix_findings(&remaining_findings)?;
        }

        let fix_results = fix_sidecars.consume_fix_results();
        (
            scope_outcome.changed_files,
            fix_results,
            scope_outcome.warnings,
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    let edit_count = fix_results.len();

    Ok(PlannedStage {
        source: "lint".to_string(),
        summary: SourceStageSummary {
            stage: "lint".to_string(),
            collected: true,
            applied: write && !stage_changed_files.is_empty(),
            edit_count,
            files_modified: stage_changed_files.len(),
            detected_findings: Some(lint_findings.len()),
            changed_files: stage_changed_files,
            fix_summary: auto::summarize_optional_fix_results(&fix_results),
            warnings: stage_warnings,
        },
        fix_results,
    })
}

pub(super) fn empty_lint_stage() -> PlannedStage {
    PlannedStage {
        source: "lint".to_string(),
        summary: SourceStageSummary {
            stage: "lint".to_string(),
            collected: true,
            applied: false,
            edit_count: 0,
            files_modified: 0,
            detected_findings: Some(0),
            changed_files: Vec::new(),
            fix_summary: None,
            warnings: Vec::new(),
        },
        fix_results: Vec::new(),
    }
}

pub(super) fn run_test_stage(
    component: &Component,
    root: &Path,
    settings: &[(String, String)],
    options: &TestSourceOptions,
    changed_test_files: Option<&[String]>,
    write: bool,
    run_dir: &RunDir,
) -> crate::core::Result<PlannedStage> {
    // Check for cached test results — if the quality gate already passed,
    // there's nothing to fix and we can skip re-running the test suite entirely.
    if let Some(CachedTestResult::Clean) = try_load_cached_test() {
        return Ok(PlannedStage {
            source: "test".to_string(),
            summary: SourceStageSummary {
                stage: "test".to_string(),
                collected: true,
                applied: false,
                edit_count: 0,
                files_modified: 0,
                detected_findings: None,
                changed_files: Vec::new(),
                fix_summary: None,
                warnings: Vec::new(),
            },
            fix_results: Vec::new(),
        });
    }

    let root_str = root.to_string_lossy().to_string();
    let fix_sidecars = auto::AutofixSidecarFiles::for_run_dir(run_dir);
    let selected_test_files = options.selected_files.as_deref().or(changed_test_files);

    // ── Phase 1: Diagnose ──────────────────────────────────────────────
    // Run tests WITHOUT auto-fix. The extension reports failures but does
    // NOT modify files. This separates diagnosis from fix application.
    //
    // Helper: build the test runner with the current stage options.
    let build_runner = || {
        extension::test::build_test_runner(
            component,
            None,
            settings,
            options.skip_lint,
            false,
            None,
            selected_test_files,
            run_dir,
        )
    };

    let mut runner = build_runner()?;
    if !options.script_args.is_empty() {
        runner = runner.script_args(&options.script_args);
    }

    runner.run()?;

    let test_source_result = serde_json::json!({
        "test_results": read_optional_json(&run_dir.step_file(run_dir::files::TEST_RESULTS)),
        "test_failures": read_optional_json(&run_dir.step_file(run_dir::files::TEST_FAILURES)),
    });
    if let Some(stage) = try_extension_refactor_source_stage(
        "test",
        component,
        root,
        &test_source_result,
        write,
        settings,
    )? {
        return Ok(stage);
    }

    // ── Phase 2: Apply fixes (only when --write) ───────────────────────
    // The engine controls fix application. The extension's fix-mode
    // invocation runs ONLY the fixers, not the diagnostic pass.
    let (stage_changed_files, fix_results) = if write {
        let before_dirty = git::get_dirty_files(&root_str).unwrap_or_default();

        // Save undo snapshot before applying test fixes.
        let mut snap = UndoSnapshot::new(root, "test fix");
        for file in &before_dirty {
            snap.capture_file(file);
        }
        if let Err(error) = snap.save() {
            crate::log_status!("undo", "Warning: failed to save undo snapshot: {}", error);
        }

        // Invoke the extension in fix-only mode. Reuses the same builder
        // as the diagnostic pass with HOMEBOY_FIX_ONLY=1 — the single env-var
        // contract for running fixers. See `run_lint_stage` for context.
        let mut fix_runner = build_runner()?.env("HOMEBOY_FIX_ONLY", "1");
        if !options.script_args.is_empty() {
            fix_runner = fix_runner.script_args(&options.script_args);
        }

        fix_runner.run()?;

        let after_dirty = git::get_dirty_files(&root_str).unwrap_or_default();
        let before_set: HashSet<&str> = before_dirty.iter().map(|s| s.as_str()).collect();
        let changed: Vec<String> = after_dirty
            .into_iter()
            .filter(|f| !before_set.contains(f.as_str()))
            .collect();

        let fix_results = fix_sidecars.consume_fix_results();
        (changed, fix_results)
    } else {
        (Vec::new(), Vec::new())
    };

    let edit_count = fix_results.len();
    Ok(PlannedStage {
        source: "test".to_string(),
        summary: SourceStageSummary {
            stage: "test".to_string(),
            collected: true,
            applied: write && !stage_changed_files.is_empty(),
            edit_count,
            files_modified: stage_changed_files.len(),
            detected_findings: None,
            changed_files: stage_changed_files,
            fix_summary: auto::summarize_optional_fix_results(&fix_results),
            warnings: Vec::new(),
        },
        fix_results,
    })
}

/// Count fixes whose edits are ALL blocked from auto-apply — i.e., they'd be
/// dropped entirely by `apply_fix_policy(write=true)`. Used to surface a warning
/// when dry-run previews edits that `--write` will silently decline.
/// (homeboy#1159, homeboy#1478)
pub(super) fn count_manual_only_fixes(fix_result: &fixer::FixResult) -> usize {
    let manual_only_fixes = fix_result
        .fixes
        .iter()
        .filter(|fix| {
            !fix.insertions.is_empty() && fix.insertions.iter().all(|ins| !ins.auto_apply)
        })
        .count();
    let manual_only_new_files = fix_result
        .new_files
        .iter()
        .filter(|nf| !nf.auto_apply)
        .count();
    manual_only_fixes + manual_only_new_files
}

/// Count only files that `--write` mode would actually modify.
///
/// `apply_fix_policy` keeps all policy-selected edits visible in dry-run, but
/// `auto_apply` marks whether each edit would survive a subsequent `--write`.
/// Counting a file here as "would be modified" when `--write` would silently
/// decline it produces the CI deadlock described in homeboy#1159: dry-run exits
/// 1 (triggering autofix), autofix re-runs with `--write` and applies nothing,
/// the skipped-bot-loop guard fires, and the PR is stuck.
///
/// Filter to fixes that have at least one auto-apply insertion — the same
/// predicate `apply_fix_policy(write=true)` uses to keep a fix. This
/// aligns dry-run `files_modified` with what a subsequent `--write` run
/// would actually produce. New files follow the same rule.
pub(super) fn collect_audit_changed_files(fix_result: &fixer::FixResult) -> Vec<String> {
    let mut files = BTreeSet::new();
    for fix in &fix_result.fixes {
        if fix.insertions.iter().any(|ins| ins.auto_apply) {
            files.insert(fix.file.clone());
        }
    }
    for new_file in &fix_result.new_files {
        if new_file.auto_apply {
            files.insert(new_file.file.clone());
        }
    }
    files.into_iter().collect()
}

pub(super) fn summarize_audit_fix_result_entries(fix_result: &fixer::FixResult) -> Vec<FixApplied> {
    let mut entries = Vec::new();

    for fix in &fix_result.fixes {
        for insertion in &fix.insertions {
            if insertion.auto_apply {
                entries.push(FixApplied {
                    file: fix.file.clone(),
                    rule: format!("{:?}", insertion.finding).to_lowercase(),
                    action: Some("insert".to_string()),
                    primitive: insertion.primitive.as_ref().map(auto::primitive_name),
                });
            }
        }
    }

    for new_file in &fix_result.new_files {
        if new_file.auto_apply {
            entries.push(FixApplied {
                file: new_file.file.clone(),
                rule: format!("{:?}", new_file.finding).to_lowercase(),
                action: Some("create".to_string()),
                primitive: new_file.primitive.as_ref().map(auto::primitive_name),
            });
        }
    }

    entries
}
