use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub config: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelinePlan {
    pub steps: Vec<PipelinePlanStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelinePlanStep {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub config: HashMap<String, serde_json::Value>,
    pub status: PipelineStepStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStepStatus {
    Ready,
    Missing,
    Disabled,
}

pub trait PipelineCapabilityResolver: Send + Sync {
    fn is_supported(&self, step_type: &str) -> bool;
    fn missing(&self, step_type: &str) -> Vec<String>;
}

pub trait PipelineStepExecutor: Send + Sync {
    fn execute_step(&self, step: &PipelineStep) -> Result<PipelineStepResult>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRunPlan {
    pub steps: Vec<PipelineStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStepResult {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String,
    pub status: PipelineRunStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<crate::error::Hint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRunResult {
    pub steps: Vec<PipelineStepResult>,
    pub status: PipelineRunStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<PipelineRunSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRunSummary {
    pub total_steps: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub missing: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineRunStatus {
    Success,
    PartialSuccess,
    Failed,
    Skipped,
    Missing,
}

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

fn order_steps(steps: &[PipelineStep], field: &str) -> Result<(Vec<PipelineStep>, Vec<String>)> {
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

fn to_plan_step(
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

fn split_ready_steps(
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

fn derive_overall_status(
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

fn build_summary(results: &[PipelineStepResult], status: &PipelineRunStatus) -> PipelineRunSummary {
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
            vec!["Install missing modules or actions to resolve missing steps".to_string()]
        }
        _ => Vec::new(),
    };

    PipelineRunSummary {
        total_steps: results.len(),
        succeeded,
        failed,
        skipped,
        missing,
        next_actions,
    }
}

fn execute_batch(
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

fn execute_single_step(
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
            data: None,
            error: Some(err.message.clone()),
        }),
    }
}
