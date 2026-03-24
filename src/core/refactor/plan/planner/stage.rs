//! stage — extracted from planner.rs.

use crate::component::Component;
use crate::engine::temp;
use crate::extension;
use crate::git;
use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use crate::component::Component;
use std::fs;
use super::LintSourceOptions;
use super::PlanStageSummary;
use super::PlannedStage;
use super::summary;
use super::TestSourceOptions;
use super::PlanOverlap;
use super::super::*;


pub(crate) fn run_lint_stage(
    component: &Component,
    root: &Path,
    settings: &[(String, String)],
    options: &LintSourceOptions,
    changed_files: Option<&[String]>,
    write: bool,
) -> crate::Result<PlannedStage> {
    let root_str = root.to_string_lossy().to_string();
    let findings_file = temp::runtime_temp_file("homeboy-lint-findings", ".json")?;
    let fix_sidecars = auto::AutofixSidecarFiles::for_plan();

    // Capture dirty files before the lint script runs so we can detect what it changed.
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

    let findings_file_str = findings_file.to_string_lossy().to_string();
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
        &findings_file_str,
    )?
    .env_if(
        write,
        "HOMEBOY_FIX_PLAN_FILE",
        &fix_sidecars
            .plan_file
            .as_ref()
            .expect("plan sidecar initialized")
            .to_string_lossy(),
    )
    .env_if(
        write,
        "HOMEBOY_FIX_RESULTS_FILE",
        &fix_sidecars.results_file.to_string_lossy(),
    )
    .env_if(write, "HOMEBOY_AUTO_FIX", "1");

    runner.run()?;

    // Detect files changed by the lint script using git.
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
    let _ = std::fs::remove_file(&findings_file);

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
) -> crate::Result<PlannedStage> {
    let root_str = root.to_string_lossy().to_string();
    let results_file = temp::runtime_temp_file("homeboy-test-results", ".json")?;
    let fix_sidecars = auto::AutofixSidecarFiles::for_plan();

    // Capture dirty files before the test script runs.
    let before_dirty = if write {
        git::get_dirty_files(&root_str).unwrap_or_default()
    } else {
        Vec::new()
    };

    let results_file_str = results_file.to_string_lossy().to_string();
    let selected_test_files = options.selected_files.as_deref().or(changed_test_files);

    let mut runner = extension::test::build_test_runner(
        component,
        None,
        settings,
        options.skip_lint,
        false,
        &results_file_str,
        None,
        None,
        None,
        selected_test_files,
    )?
    .env_if(
        write,
        "HOMEBOY_FIX_PLAN_FILE",
        &fix_sidecars
            .plan_file
            .as_ref()
            .expect("plan sidecar initialized")
            .to_string_lossy(),
    )
    .env_if(
        write,
        "HOMEBOY_FIX_RESULTS_FILE",
        &fix_sidecars.results_file.to_string_lossy(),
    )
    .env_if(write, "HOMEBOY_AUTO_FIX", "1");

    if !options.script_args.is_empty() {
        runner = runner.script_args(&options.script_args);
    }

    runner.run()?;

    // Detect files changed by the test script using git.
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
    let _ = std::fs::remove_file(&results_file);

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

pub(crate) fn analyze_stage_overlaps(stages: &[PlanStageSummary]) -> Vec<PlanOverlap> {
    let mut overlaps = Vec::new();

    for (later_index, later_stage) in stages.iter().enumerate() {
        if later_stage.changed_files.is_empty() {
            continue;
        }

        let later_files: BTreeSet<&str> = later_stage
            .changed_files
            .iter()
            .map(String::as_str)
            .collect();

        for earlier_stage in stages.iter().take(later_index) {
            if earlier_stage.changed_files.is_empty() {
                continue;
            }

            for file in earlier_stage.changed_files.iter().map(String::as_str) {
                if later_files.contains(file) {
                    overlaps.push(PlanOverlap {
                        file: file.to_string(),
                        earlier_stage: earlier_stage.stage.clone(),
                        later_stage: later_stage.stage.clone(),
                        resolution: format!(
                            "{} pass ran after {} in sandbox sequence",
                            later_stage.stage, earlier_stage.stage
                        ),
                    });
                }
            }
        }
    }

    overlaps.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.earlier_stage.cmp(&b.earlier_stage))
            .then(a.later_stage.cmp(&b.later_stage))
    });

    overlaps
}
