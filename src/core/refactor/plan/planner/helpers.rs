//! helpers — extracted from planner.rs.

use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use crate::Error;
use std::path::{Path, PathBuf};
use super::PlanTotals;
use super::PlanStageSummary;
use super::KNOWN_PLAN_SOURCES;
use super::super::*;


pub(crate) fn normalize_sources(sources: &[String]) -> crate::Result<Vec<String>> {
    let lowered: Vec<String> = sources.iter().map(|source| source.to_lowercase()).collect();

    if lowered.iter().any(|source| source == "all") {
        return Ok(KNOWN_PLAN_SOURCES
            .iter()
            .map(|source| source.to_string())
            .collect());
    }

    let unknown: Vec<String> = lowered
        .iter()
        .filter(|source| !KNOWN_PLAN_SOURCES.contains(&source.as_str()))
        .cloned()
        .collect();

    if !unknown.is_empty() {
        return Err(Error::validation_invalid_argument(
            "from",
            format!("Unknown refactor source(s): {}", unknown.join(", ")),
            None,
            Some(vec![format!(
                "Known sources: {}",
                KNOWN_PLAN_SOURCES.join(", ")
            )]),
        ));
    }

    let mut ordered = Vec::new();
    for known in KNOWN_PLAN_SOURCES {
        if lowered.iter().any(|source| source == known) {
            ordered.push((*known).to_string());
        }
    }

    if ordered.is_empty() {
        return Err(Error::validation_missing_argument(vec!["from".to_string()]));
    }

    Ok(ordered)
}

/// Format modified files between refactor stages.
///
/// This ensures generated code (test files, refactored sources) is properly
/// formatted before subsequent stages run. Without this, the lint stage's
/// `cargo fmt --check` fails on unformatted auto-generated code — blocking
/// the pipeline on problems it didn't create.
///
/// Uses the same `format_after_write` as the post-write step. Non-fatal:
/// if formatting fails, it logs a warning and continues.
pub(crate) fn format_changed_files(root: &Path, changed_files: &[String], warnings: &mut Vec<String>) {
    if changed_files.is_empty() {
        return;
    }

    let abs_changed: Vec<PathBuf> = changed_files.iter().map(|f| root.join(f)).collect();

    match crate::engine::format_write::format_after_write(root, &abs_changed) {
        Ok(fmt) => {
            if let Some(cmd) = &fmt.command {
                if fmt.success {
                    crate::log_status!(
                        "format",
                        "Formatted {} file(s) via {}",
                        abs_changed.len(),
                        cmd
                    );
                } else {
                    warnings.push(format!(
                        "Formatter ({}) exited non-zero (continuing)",
                        cmd
                    ));
                }
            }
        }
        Err(e) => {
            crate::log_status!(
                "format",
                "Warning: inter-stage format failed (continuing): {}",
                e
            );
        }
    }
}

pub(crate) fn summarize_plan_totals(
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
