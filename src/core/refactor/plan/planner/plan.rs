//! plan — extracted from planner.rs.

use crate::engine::run_dir::{self, RunDir};
use crate::engine::undo::UndoSnapshot;
use crate::extension::test::compute_changed_test_files;
use crate::git;
use crate::Error;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use crate::code_audit::CodeAuditResult;
use crate::component::Component;
use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use serde::Serialize;
use super::super::verify::AuditConvergenceScoring;
use super::FixAccumulator;
use super::RefactorPlan;
use super::RefactorPlanRequest;
use super::analyze_stage_overlaps;
use super::plan_audit_stage;
use super::run_test_stage;
use super::run_lint_stage;
use super::PlanStageSummary;
use super::PlanTotals;


pub fn build_refactor_plan(request: RefactorPlanRequest) -> crate::Result<RefactorPlan> {
    let sources = normalize_sources(&request.sources)?;
    let root_str = request.root.to_string_lossy().to_string();
    let original_changes = git::get_uncommitted_changes(&root_str).ok();

    // Refuse to write to a dirty working tree unless --force is set.
    // Refactoring operates directly on the working tree, so mixing auto-generated
    // fixes with uncommitted manual changes makes rollback difficult.
    // Dry runs (no --write) are always safe — they don't modify files.
    if request.write && !request.force {
        if let Some(ref changes) = original_changes {
            if changes.has_changes {
                return Err(crate::Error::validation_invalid_argument(
                    "write",
                    "Working tree has uncommitted changes",
                    None,
                    Some(vec![
                        "Commit or stash your changes first".to_string(),
                        "Or use --force to proceed anyway".to_string(),
                    ]),
                ));
            }
        }
    }

    let scoped_changed_files = if let Some(git_ref) = request.changed_since.as_deref() {
        Some(git::get_files_changed_since(&root_str, git_ref)?)
    } else {
        None
    };
    let scoped_test_files = if let Some(git_ref) = request.changed_since.as_deref() {
        Some(compute_changed_test_files(&request.component, git_ref)?)
    } else {
        None
    };

    let mut planned_stages = Vec::new();
    let merge_order = sources.join(" → ");
    let mut warnings = vec![format!("Deterministic merge order: {}", merge_order)];
    let mut accumulator = FixAccumulator::default();

    // Save undo snapshot before any modifications so we can roll back.
    if request.write {
        let mut snapshot_files: HashSet<String> = HashSet::new();
        if let Some(changes) = &original_changes {
            snapshot_files.extend(changes.staged.iter().cloned());
            snapshot_files.extend(changes.unstaged.iter().cloned());
            snapshot_files.extend(changes.untracked.iter().cloned());
        }
        if !snapshot_files.is_empty() {
            let mut snap = UndoSnapshot::new(&request.root, "refactor sources (pre)");
            for file in &snapshot_files {
                snap.capture_file(file);
            }
            if let Err(e) = snap.save() {
                crate::log_status!(
                    "undo",
                    "Warning: failed to save pre-refactor undo snapshot: {}",
                    e
                );
            }
        }
    }

    let run_dir = RunDir::create()?;

    for source in &sources {
        let stage = match source.as_str() {
            "audit" => plan_audit_stage(
                &request.component.id,
                &request.root,
                scoped_changed_files.as_deref(),
                &request.only,
                &request.exclude,
                request.write,
            )?,
            "lint" => run_lint_stage(
                &request.component,
                &request.root,
                &request.settings,
                &request.lint,
                scoped_changed_files.as_deref(),
                request.write,
                &run_dir,
            )?,
            "test" => run_test_stage(
                &request.component,
                &request.root,
                &request.settings,
                &request.test,
                scoped_test_files.as_deref(),
                request.write,
                &run_dir,
            )?,
            _ => unreachable!("sources are normalized before planning"),
        };

        // Format generated/modified files so subsequent stages (especially lint)
        // see properly formatted code.
        if stage.summary.files_modified > 0 {
            format_changed_files(&request.root, &stage.summary.changed_files, &mut warnings);
        }

        accumulator.extend(stage.fix_results.clone());
        planned_stages.push(stage);
    }

    let proposals = collect_fix_proposals(&planned_stages);
    let mut stage_summaries: Vec<PlanStageSummary> = planned_stages
        .into_iter()
        .map(|stage| stage.summary)
        .collect();
    let changed_files = collect_stage_changed_files(&stage_summaries);
    let overlaps = analyze_stage_overlaps(&stage_summaries);
    if !overlaps.is_empty() {
        warnings.push(format!(
            "{} staged file overlap(s) resolved by precedence order {}",
            overlaps.len(),
            merge_order
        ));
    }

    let plan_totals = summarize_plan_totals(&stage_summaries, changed_files.len());
    let files_modified = changed_files.len();
    let applied = request.write && files_modified > 0;

    if applied {
        let abs_changed: Vec<PathBuf> =
            changed_files.iter().map(|f| request.root.join(f)).collect();
        match crate::engine::format_write::format_after_write(&request.root, &abs_changed) {
            Ok(fmt) => {
                if let Some(cmd) = &fmt.command {
                    if !fmt.success {
                        warnings.push(format!("Formatter ({}) exited non-zero", cmd));
                    }
                }
            }
            Err(e) => {
                crate::log_status!("format", "Warning: post-write format failed: {}", e);
            }
        }
    }

    for stage in &mut stage_summaries {
        stage.applied = request.write && stage.files_modified > 0;
    }

    if files_modified == 0 {
        warnings.push("No automated fixes accumulated across audit/lint/test".to_string());
    }

    let hints = if applied {
        sources
            .iter()
            .map(|source| format!("Re-run checks: homeboy {} {}", source, request.component.id))
            .collect()
    } else if files_modified > 0 {
        vec!["Dry run. Re-run with --write to apply fixes to the working tree.".to_string()]
    } else {
        Vec::new()
    };

    Ok(RefactorPlan {
        component_id: request.component.id,
        source_path: root_str,
        sources,
        dry_run: !request.write,
        applied,
        merge_strategy: "sequential_source_merge".to_string(),
        proposals,
        stages: stage_summaries,
        plan_totals,
        overlaps,
        files_modified,
        changed_files,
        fix_summary: accumulator.summary(),
        warnings,
        hints,
    })
}

pub fn summarize_plan_totals(
    stages: &[PlanStageSummary],
    total_files_selected: usize,
) -> PlanTotals {
    PlanTotals {
        stages_with_proposals: stages
            .iter()
            .filter(|stage| stage.fixes_proposed > 0)
            .count(),
        total_fixes_proposed: stages.iter().map(|stage| stage.fixes_proposed).sum(),
        total_files_selected,
    }
}
