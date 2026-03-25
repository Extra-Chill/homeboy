//! build — extracted from pipeline.rs.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use crate::error::{Error, Result};
use super::PipelineStepResult;
use super::PipelineRunSummary;
use super::PipelineRunStatus;


pub(crate) fn build_step_summary_line(result: &PipelineStepResult) -> Option<String> {
    if !matches!(result.status, PipelineRunStatus::Success) {
        return None;
    }

    let data = result.data.as_ref();

    match result.step_type.as_str() {
        "version" => data
            .and_then(|d| d.get("new_version"))
            .and_then(|v| v.as_str())
            .map(|ver| format!("Version bumped to {}", ver)),
        "git.commit" => {
            let skipped = data
                .and_then(|d| d.get("skipped"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if skipped {
                Some("Working tree was clean".to_string())
            } else {
                Some("Committed release changes".to_string())
            }
        }
        "git.tag" => {
            let tag = data.and_then(|d| d.get("tag")).and_then(|v| v.as_str());
            let skipped = data
                .and_then(|d| d.get("skipped"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            match (tag, skipped) {
                (Some(t), true) => Some(format!("Tag {} already exists", t)),
                (Some(t), false) => Some(format!("Tagged {}", t)),
                (None, _) => Some("Tagged release".to_string()),
            }
        }
        "git.push" => Some("Pushed to origin (with tags)".to_string()),
        "package" => Some("Created release artifacts".to_string()),
        "cleanup" => None,
        "post_release" => {
            let all_succeeded = data
                .and_then(|d| d.get("all_succeeded"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if all_succeeded {
                Some("Post-release commands completed".to_string())
            } else {
                Some("Post-release commands completed (with warnings)".to_string())
            }
        }
        step if step.starts_with("publish.") => {
            let target = step.strip_prefix("publish.").unwrap_or("registry");
            Some(format!("Published to {}", target))
        }
        _ => None,
    }
}

pub(crate) fn build_summary(results: &[PipelineStepResult], status: &PipelineRunStatus) -> PipelineRunSummary {
    let succeeded = results
        .iter()
        .filter(|r| matches!(r.status, PipelineRunStatus::Success))
        .count();
    let failed = results
        .iter()
        .filter(|r| matches!(r.status, PipelineRunStatus::Failed))
        .count();
    let skipped = results
        .iter()
        .filter(|r| matches!(r.status, PipelineRunStatus::Skipped))
        .count();
    let missing = results
        .iter()
        .filter(|r| matches!(r.status, PipelineRunStatus::Missing))
        .count();

    let next_actions = match status {
        PipelineRunStatus::PartialSuccess | PipelineRunStatus::Failed => {
            vec![
                "Fix the issue and re-run (idempotent - completed steps will succeed again)"
                    .to_string(),
            ]
        }
        PipelineRunStatus::Missing => {
            vec!["Install missing extensions or actions to resolve missing steps".to_string()]
        }
        _ => Vec::new(),
    };

    let success_summary = if matches!(status, PipelineRunStatus::Success) {
        results.iter().filter_map(build_step_summary_line).collect()
    } else {
        Vec::new()
    };

    PipelineRunSummary {
        total_steps: results.len(),
        succeeded,
        failed,
        skipped,
        missing,
        next_actions,
        success_summary,
    }
}
