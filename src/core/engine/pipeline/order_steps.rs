//! order_steps — extracted from pipeline.rs.

use std::collections::{HashMap, VecDeque};
use crate::error::{Error, Result};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use super::PipelineStep;
use super::PipelineRunPlan;
use super::PipelineStepStatus;
use super::PipelineCapabilityResolver;
use super::PipelinePlan;
use super::PipelinePlanStep;


pub fn plan(
    steps: &[PipelineStep],
    resolver: &dyn PipelineCapabilityResolver,
    enabled: bool,
    field: &str,
) -> Result<PipelinePlan> {
    let (ordered, warnings) = order_steps(steps, field)?;
    let planned_steps = ordered
        .into_iter()
        .map(|step| to_plan_step(step, resolver, enabled))
        .collect();

    Ok(PipelinePlan {
        steps: planned_steps,
        warnings,
    })
}

pub fn plan_run(steps: &[PipelineStep], field: &str) -> Result<PipelineRunPlan> {
    let (ordered, warnings) = order_steps(steps, field)?;
    Ok(PipelineRunPlan {
        steps: ordered,
        warnings,
    })
}

pub(crate) fn order_steps(steps: &[PipelineStep], field: &str) -> Result<(Vec<PipelineStep>, Vec<String>)> {
    if steps.len() <= 1 {
        return Ok((steps.to_vec(), Vec::new()));
    }

    let mut id_index = HashMap::new();
    for (idx, step) in steps.iter().enumerate() {
        if id_index.contains_key(&step.id) {
            return Err(Error::validation_invalid_argument(
                field,
                format!("Duplicate step id '{}'", step.id),
                None,
                None,
            ));
        }
        id_index.insert(step.id.clone(), idx);
    }

    let mut indegree = vec![0usize; steps.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); steps.len()];

    for (idx, step) in steps.iter().enumerate() {
        for need in &step.needs {
            if let Some(&parent_idx) = id_index.get(need) {
                indegree[idx] += 1;
                dependents[parent_idx].push(idx);
            } else {
                return Err(Error::validation_invalid_argument(
                    field,
                    format!("Step '{}' depends on unknown step '{}'", step.id, need),
                    None,
                    None,
                ));
            }
        }
    }

    let mut queue = VecDeque::new();
    for (idx, count) in indegree.iter().enumerate() {
        if *count == 0 {
            queue.push_back(idx);
        }
    }

    let mut ordered = Vec::with_capacity(steps.len());
    while let Some(idx) = queue.pop_front() {
        ordered.push(steps[idx].clone());
        for &child in &dependents[idx] {
            if indegree[child] > 0 {
                indegree[child] -= 1;
            }
            if indegree[child] == 0 {
                queue.push_back(child);
            }
        }
    }

    if ordered.len() != steps.len() {
        let pending: Vec<String> = steps
            .iter()
            .enumerate()
            .filter(|(idx, _)| indegree[*idx] > 0)
            .map(|(_, step)| step.id.clone())
            .collect();
        return Err(Error::validation_invalid_argument(
            field,
            "Steps contain a cycle".to_string(),
            None,
            Some(pending),
        ));
    }

    let mut warnings = Vec::new();
    if steps.iter().any(|step| !step.needs.is_empty()) {
        warnings.push("Steps reordered based on dependencies".to_string());
    }

    Ok((ordered, warnings))
}

pub(crate) fn to_plan_step(
    step: PipelineStep,
    resolver: &dyn PipelineCapabilityResolver,
    enabled: bool,
) -> PipelinePlanStep {
    let (status, missing) = if !enabled {
        (PipelineStepStatus::Disabled, Vec::new())
    } else if resolver.is_supported(&step.step_type) {
        (PipelineStepStatus::Ready, Vec::new())
    } else {
        (
            PipelineStepStatus::Missing,
            resolver.missing(&step.step_type),
        )
    };

    PipelinePlanStep {
        id: step.id,
        step_type: step.step_type,
        label: step.label,
        needs: step.needs,
        config: step.config,
        status,
        missing,
    }
}
