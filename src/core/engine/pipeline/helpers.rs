//! helpers — extracted from pipeline.rs.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use super::plan_run;
use super::PipelineRunResult;
use super::PipelineCapabilityResolver;
use super::PipelineStepResult;
use super::PipelineRunStatus;
use super::PipelineStepExecutor;
use super::plan;
use super::PipelineStep;


pub fn run(
    steps: &[PipelineStep],
    executor: Arc<dyn PipelineStepExecutor>,
    resolver: Arc<dyn PipelineCapabilityResolver>,
    enabled: bool,
    field: &str,
) -> Result<PipelineRunResult> {
    if !enabled {
        let results: Vec<PipelineStepResult> = steps
            .iter()
            .map(|step| PipelineStepResult {
                id: step.id.clone(),
                step_type: step.step_type.clone(),
                status: PipelineRunStatus::Skipped,
                missing: Vec::new(),
                warnings: Vec::new(),
                hints: Vec::new(),
                data: None,
                error: None,
            })
            .collect();
        let summary = build_summary(&results, &PipelineRunStatus::Skipped);
        return Ok(PipelineRunResult {
            steps: results,
            status: PipelineRunStatus::Skipped,
            warnings: Vec::new(),
            summary: Some(summary),
        });
    }

    let plan = plan_run(steps, field)?;
    let mut results = Vec::with_capacity(plan.steps.len());
    let mut overall_status = PipelineRunStatus::Success;
    let mut pending_steps: Vec<PipelineStep> = Vec::new();

    for step in plan.steps {
        if resolver.is_supported(&step.step_type) {
            pending_steps.push(step);
        } else {
            results.push(PipelineStepResult {
                id: step.id.clone(),
                step_type: step.step_type.clone(),
                status: PipelineRunStatus::Missing,
                missing: resolver.missing(&step.step_type),
                warnings: Vec::new(),
                hints: Vec::new(),
                data: None,
                error: None,
            });
            overall_status = PipelineRunStatus::Missing;
        }
    }

    while !pending_steps.is_empty() {
        let (ready, blocked, skipped) = split_ready_steps(&pending_steps, &results);
        results.extend(skipped);

        if ready.is_empty() {
            if blocked.is_empty() {
                break;
            }
            return Err(Error::validation_invalid_argument(
                field,
                "Steps blocked by missing dependencies".to_string(),
                None,
                None,
            ));
        }

        let batch_results = execute_batch(&ready, Arc::clone(&executor), Arc::clone(&resolver))?;
        for result in batch_results {
            if matches!(result.status, PipelineRunStatus::Failed) {
                overall_status = PipelineRunStatus::Failed;
            }
            results.push(result);
        }

        pending_steps = blocked;
    }

    let final_status = derive_overall_status(&results, overall_status);
    let summary = build_summary(&results, &final_status);

    Ok(PipelineRunResult {
        steps: results,
        status: final_status,
        warnings: plan.warnings,
        summary: Some(summary),
    })
}

pub(crate) fn split_ready_steps(
    pending: &[PipelineStep],
    results: &[PipelineStepResult],
) -> (
    Vec<PipelineStep>,
    Vec<PipelineStep>,
    Vec<PipelineStepResult>,
) {
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    let mut skipped = Vec::new();

    let mut status_map: HashMap<String, PipelineRunStatus> = results
        .iter()
        .map(|result| (result.id.clone(), result.status.clone()))
        .collect();

    for step in pending {
        let mut unmet = false;
        let mut failed_dependency: Option<String> = None;

        for need in &step.needs {
            match status_map.get(need) {
                Some(PipelineRunStatus::Success) | Some(PipelineRunStatus::PartialSuccess) => {}
                Some(PipelineRunStatus::Failed)
                | Some(PipelineRunStatus::Missing)
                | Some(PipelineRunStatus::Skipped) => {
                    failed_dependency = Some(need.clone());
                    break;
                }
                None => {
                    unmet = true;
                }
            }
        }

        if let Some(dep) = failed_dependency {
            let result = PipelineStepResult {
                id: step.id.clone(),
                step_type: step.step_type.clone(),
                status: PipelineRunStatus::Skipped,
                missing: Vec::new(),
                warnings: vec![format!("Skipped because '{}' did not succeed", dep)],
                hints: Vec::new(),
                data: None,
                error: None,
            };
            status_map.insert(step.id.clone(), PipelineRunStatus::Skipped);
            skipped.push(result);
            continue;
        }

        if unmet {
            blocked.push(step.clone());
        } else {
            ready.push(step.clone());
        }
    }

    (ready, blocked, skipped)
}

pub(crate) fn derive_overall_status(
    results: &[PipelineStepResult],
    current: PipelineRunStatus,
) -> PipelineRunStatus {
    let has_success = results
        .iter()
        .any(|result| matches!(result.status, PipelineRunStatus::Success));
    let has_failed = results
        .iter()
        .any(|result| matches!(result.status, PipelineRunStatus::Failed));
    let has_missing = results
        .iter()
        .any(|result| matches!(result.status, PipelineRunStatus::Missing));

    if has_failed && has_success {
        return PipelineRunStatus::PartialSuccess;
    }
    if has_failed {
        return PipelineRunStatus::Failed;
    }
    if has_missing && has_success {
        return PipelineRunStatus::PartialSuccess;
    }
    if has_missing {
        return PipelineRunStatus::Missing;
    }
    if matches!(current, PipelineRunStatus::Skipped) {
        return PipelineRunStatus::Skipped;
    }
    PipelineRunStatus::Success
}
