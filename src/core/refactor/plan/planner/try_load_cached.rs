//! try_load_cached — extracted from planner.rs.

use crate::code_audit::CodeAuditResult;
use crate::refactor::auto as fixer;
use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use super::super::verify::AuditConvergenceScoring;
use crate::component::Component;
use crate::engine::run_dir::{self, RunDir};
use crate::engine::undo::UndoSnapshot;
use crate::Error;
use serde::Serialize;
use super::PlanStageSummary;
use super::PlannedStage;


/// Try to load a cached audit result from a previous `homeboy audit` run.
///
/// Checks `HOMEBOY_OUTPUT_DIR/audit.json` for a `CliResponse<CodeAuditResult>`
/// envelope. If found and parseable, returns the `CodeAuditResult` without
/// re-running the audit. This avoids redundant full-codebase scans when the
/// refactor step runs after an audit gate that already produced the results.
///
/// Returns `None` if:
/// - `HOMEBOY_OUTPUT_DIR` is not set
/// - The file doesn't exist
/// - The file can't be parsed (e.g. the audit failed and wrote an error envelope)
pub(crate) fn try_load_cached_audit() -> Option<CodeAuditResult> {
    let output_dir = std::env::var(OUTPUT_DIR_ENV).ok()?;
    let audit_file = PathBuf::from(&output_dir).join("audit.json");

    let content = std::fs::read_to_string(&audit_file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Only use cached results from successful runs
    if json.get("success")?.as_bool()? != true {
        return None;
    }

    // The `--output` envelope wraps the audit in a `data` field
    let data = json.get("data")?;
    let result: CodeAuditResult = serde_json::from_value(data.clone()).ok()?;

    crate::log_status!(
        "refactor",
        "Using cached audit result ({} findings from {})",
        result.findings.len(),
        audit_file.display()
    );

    Some(result)
}

pub(crate) fn plan_audit_stage(
    component_id: &str,
    root: &Path,
    changed_files: Option<&[String]>,
    only: &[crate::code_audit::AuditFinding],
    exclude: &[crate::code_audit::AuditFinding],
    write: bool,
) -> crate::Result<PlannedStage> {
    let result = if let Some(cached) = try_load_cached_audit() {
        cached
    } else if let Some(changed) = changed_files {
        if changed.is_empty() {
            CodeAuditResult {
                component_id: component_id.to_string(),
                source_path: root.to_string_lossy().to_string(),
                summary: crate::code_audit::AuditSummary {
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
            crate::code_audit::audit_path_scoped(
                component_id,
                &root.to_string_lossy(),
                changed,
                None,
            )?
        }
    } else {
        crate::code_audit::audit_path_with_id(component_id, &root.to_string_lossy())?
    };

    let mut fix_result = super::generate::generate_audit_fixes(&result, root);
    let policy = fixer::FixPolicy {
        only: (!only.is_empty()).then_some(only.to_vec()),
        exclude: exclude.to_vec(),
    };
    let preflight_context = fixer::PreflightContext { root };
    let (fix_result, policy_summary, changed_files, stage_warnings): (
        fixer::FixResult,
        fixer::PolicySummary,
        Vec<String>,
        Vec<String>,
    ) = if write {
        // Single pass: generate fixes from the provided findings, apply, validate.
        // The audit already ran and provided findings in `result` — the refactor
        // command does not re-run the audit internally. The convergence loop
        // (audit → fix → merge → re-audit) belongs in the full orchestration
        // pipeline, not inside a single refactor invocation.
        let outcome = super::verify::run_audit_refactor(
            result.clone(),
            only,
            exclude,
            AuditConvergenceScoring::default(),
            1,
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
            .iterations
            .iter()
            .filter(|iteration| iteration.status != "continued")
            .map(|iteration| {
                format!(
                    "audit iteration {}: {}",
                    iteration.iteration, iteration.status
                )
            })
            .collect::<Vec<_>>();

        (
            outcome.fix_result,
            outcome.policy_summary,
            changed_files,
            warnings,
        )
    } else {
        let policy_summary =
            fixer::apply_fix_policy(&mut fix_result, false, &policy, &preflight_context);
        let changed_files = collect_audit_changed_files(&fix_result);
        (fix_result, policy_summary, changed_files, Vec::new())
    };

    let fix_results = summarize_audit_fix_result_entries(&fix_result);
    let fixes_proposed = fix_results.len();

    Ok(PlannedStage {
        source: "audit".to_string(),
        summary: PlanStageSummary {
            stage: "audit".to_string(),
            planned: true,
            applied: write && !changed_files.is_empty(),
            fixes_proposed,
            files_modified: changed_files.len(),
            detected_findings: Some(result.findings.len()),
            changed_files,
            fix_summary: if write {
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

pub(crate) fn collect_audit_changed_files(fix_result: &fixer::FixResult) -> Vec<String> {
    let mut files = BTreeSet::new();
    for fix in &fix_result.fixes {
        if !fix.insertions.is_empty() {
            files.insert(fix.file.clone());
        }
    }
    for file in &fix_result.new_files {
        files.insert(file.file.clone());
    }
    files.into_iter().collect()
}

pub(crate) fn summarize_audit_fix_result_entries(fix_result: &fixer::FixResult) -> Vec<FixApplied> {
    let mut entries = Vec::new();

    for fix in &fix_result.fixes {
        for insertion in &fix.insertions {
            if insertion.auto_apply {
                entries.push(FixApplied {
                    file: fix.file.clone(),
                    rule: format!("{:?}", insertion.finding).to_lowercase(),
                    action: Some("insert".to_string()),
                });
            }
        }
    }

    for new_file in &fix_result.new_files {
        entries.push(FixApplied {
            file: new_file.file.clone(),
            rule: format!("{:?}", new_file.finding).to_lowercase(),
            action: Some("create".to_string()),
        });
    }

    entries
}
