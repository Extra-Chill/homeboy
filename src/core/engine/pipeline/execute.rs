//! execute — extracted from pipeline.rs.

use std::sync::Arc;
use crate::error::{Error, Result};
use std::collections::{HashMap, VecDeque};
use serde::{Deserialize, Serialize};
use super::PipelineStep;
use super::PipelineStepExecutor;
use super::PipelineStepResult;
use super::PipelineCapabilityResolver;
use super::PipelineRunStatus;


pub(crate) fn execute_batch(
    steps: &[PipelineStep],
    executor: Arc<dyn PipelineStepExecutor>,
    resolver: Arc<dyn PipelineCapabilityResolver>,
) -> Result<Vec<PipelineStepResult>> {
    if steps.len() <= 1 {
        if let Some(step) = steps.first() {
            return Ok(vec![execute_single_step(
                step.clone(),
                executor.as_ref(),
                resolver.as_ref(),
            )?]);
        }
        return Ok(Vec::new());
    }

    use std::thread;

    let handles: Vec<_> = steps
        .iter()
        .map(|step| {
            let step = step.clone();
            let executor = Arc::clone(&executor);
            let resolver = Arc::clone(&resolver);
            thread::spawn(move || execute_single_step(step, executor.as_ref(), resolver.as_ref()))
        })
        .collect();

    let mut results = Vec::with_capacity(steps.len());
    for handle in handles {
        results.push(handle.join().map_err(|_| {
            Error::validation_invalid_argument(
                "pipeline",
                "Step execution thread panicked".to_string(),
                None,
                None,
            )
        })??);
    }

    Ok(results)
}

pub(crate) fn execute_single_step(
    step: PipelineStep,
    executor: &dyn PipelineStepExecutor,
    resolver: &dyn PipelineCapabilityResolver,
) -> Result<PipelineStepResult> {
    if !resolver.is_supported(&step.step_type) {
        let step_type = step.step_type.clone();
        let missing = resolver.missing(&step.step_type);
        return Ok(PipelineStepResult {
            id: step.id,
            step_type,
            status: PipelineRunStatus::Missing,
            missing,
            warnings: Vec::new(),
            hints: Vec::new(),
            data: None,
            error: None,
        });
    }

    match executor.execute_step(&step) {
        Ok(mut result) => {
            if result.status == PipelineRunStatus::Success {
                result.missing = Vec::new();
                result.error = None;
            }
            Ok(result)
        }
        Err(err) => Ok(PipelineStepResult {
            id: step.id,
            step_type: step.step_type,
            status: PipelineRunStatus::Failed,
            missing: Vec::new(),
            warnings: Vec::new(),
            hints: err.hints.clone(),
            data: Some(serde_json::json!({ "error_details": err.details })),
            error: Some(err.message.clone()),
        }),
    }
}
