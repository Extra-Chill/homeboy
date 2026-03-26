//! stage — extracted from planner.rs.

use crate::component::Component;
use crate::engine::run_dir::{self, RunDir};
use crate::extension;
use crate::git;
use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use crate::code_audit::CodeAuditResult;
use crate::engine::undo::UndoSnapshot;
use crate::Error;
use serde::Serialize;
use super::super::verify::AuditConvergenceScoring;
use super::LintSourceOptions;
use super::TestSourceOptions;
use super::PlannedStage;
use super::PlanStageSummary;


pub(crate) fn run_lint_stage(
    component: &Component,
    root: &Path,
    settings: &[(String, String)],
    options: &LintSourceOptions,
    changed_files: Option<&[String]>,
    write: bool,
    run_dir: &RunDir,
) -> crate::Result<PlannedStage> {
    let root_str = root.to_string_lossy().to_string();
    let findings_file = run_dir.step_file(run_dir::files::LINT_FINDINGS);
    let fix_sidecars = auto::AutofixSidecarFiles::for_run_dir(run_dir);
    let before_dirty = if write {
        git::get_dirty_files(&root_str).unwrap_or_default()
    } else {
        Vec::new()
    };

    let selected_files = options.selected_files.as_deref().or(changed_files);
    let effective_glob = if let Some(changed_files) = selected_files {
        if changed_files.is_empty() {
            None
        } else {
            let abs_files: Vec<String> = changed_files
                .iter()
                .map(|f| format!("{}/{}", root_str, f))
                .collect();
            if abs_files.len() == 1 {
                Some(abs_files[0].clone())
            } else {
                Some(format!("{{{}}}", abs_files.join(",")))
            }
        }
    } else {
        options.glob.clone()
    };

    let runner = extension::lint::build_lint_runner(
        component,
        None,
        settings,
        false,
        options.file.as_deref(),
        effective_glob.as_deref(),
        options.errors_only,
        options.sniffs.as_deref(),
        options.exclude_sniffs.as_deref(),
        options.category.as_deref(),
        run_dir,
    )?
    .env_if(write, "HOMEBOY_AUTO_FIX", "1");

    runner.run()?;

    let stage_changed_files = if write {
        let after_dirty = git::get_dirty_files(&root_str).unwrap_or_default();
        let before_set: HashSet<&str> = before_dirty.iter().map(|s| s.as_str()).collect();
        after_dirty
            .into_iter()
            .filter(|f| !before_set.contains(f.as_str()))
            .collect()
    } else {
        Vec::new()
    };

    let fix_results = fix_sidecars.consume_fix_results();
    let fixes_proposed = fix_results.len();
    let lint_findings =
        crate::extension::lint::baseline::parse_findings_file(&findings_file).unwrap_or_default();

    Ok(PlannedStage {
        source: "lint".to_string(),
        summary: PlanStageSummary {
            stage: "lint".to_string(),
            planned: true,
            applied: write && !stage_changed_files.is_empty(),
            fixes_proposed,
            files_modified: stage_changed_files.len(),
            detected_findings: Some(lint_findings.len()),
            changed_files: stage_changed_files,
            fix_summary: auto::summarize_optional_fix_results(&fix_results),
            warnings: Vec::new(),
        },
        fix_results,
    })
}

pub(crate) fn run_test_stage(
    component: &Component,
    root: &Path,
    settings: &[(String, String)],
    options: &TestSourceOptions,
    changed_test_files: Option<&[String]>,
    write: bool,
    run_dir: &RunDir,
) -> crate::Result<PlannedStage> {
    let root_str = root.to_string_lossy().to_string();
    let fix_sidecars = auto::AutofixSidecarFiles::for_run_dir(run_dir);
    let before_dirty = if write {
        git::get_dirty_files(&root_str).unwrap_or_default()
    } else {
        Vec::new()
    };

    let selected_test_files = options.selected_files.as_deref().or(changed_test_files);

    let mut runner = extension::test::build_test_runner(
        component,
        None,
        settings,
        options.skip_lint,
        false,
        None,
        selected_test_files,
        run_dir,
    )?
    .env_if(write, "HOMEBOY_AUTO_FIX", "1");

    if !options.script_args.is_empty() {
        runner = runner.script_args(&options.script_args);
    }

    runner.run()?;

    let stage_changed_files = if write {
        let after_dirty = git::get_dirty_files(&root_str).unwrap_or_default();
        let before_set: HashSet<&str> = before_dirty.iter().map(|s| s.as_str()).collect();
        after_dirty
            .into_iter()
            .filter(|f| !before_set.contains(f.as_str()))
            .collect()
    } else {
        Vec::new()
    };

    let fix_results = fix_sidecars.consume_fix_results();
    let fixes_proposed = fix_results.len();
    Ok(PlannedStage {
        source: "test".to_string(),
        summary: PlanStageSummary {
            stage: "test".to_string(),
            planned: true,
            applied: write && !stage_changed_files.is_empty(),
            fixes_proposed,
            files_modified: stage_changed_files.len(),
            detected_findings: None,
            changed_files: stage_changed_files,
            fix_summary: auto::summarize_optional_fix_results(&fix_results),
            warnings: Vec::new(),
        },
        fix_results,
    })
}
