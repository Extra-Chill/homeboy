//! Topological ordering of pipeline steps via `depends_on` edges.

use std::collections::{BTreeSet, HashMap, VecDeque};

use super::super::spec::{PipelineStep, RigSpec};
use crate::core::error::{Error, Result};

pub(super) fn order_pipeline_steps(
    rig: &RigSpec,
    pipeline_name: &str,
    steps: &[PipelineStep],
) -> Result<Vec<usize>> {
    if steps.is_empty() {
        return Ok(Vec::new());
    }

    let mut id_to_index = HashMap::new();
    for (idx, step) in steps.iter().enumerate() {
        if let Some(id) = step_id(step) {
            if let Some(previous_idx) = id_to_index.insert(id, idx) {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    pipeline_name,
                    format!(
                        "duplicate pipeline step id '{}' at positions {} and {}",
                        id, previous_idx, idx
                    ),
                ));
            }
        }
    }

    let mut indegree = vec![0usize; steps.len()];
    let mut dependents = vec![Vec::<usize>::new(); steps.len()];

    for (idx, step) in steps.iter().enumerate() {
        for dependency_id in step_dependencies(step) {
            let Some(&dependency_idx) = id_to_index.get(dependency_id.as_str()) else {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    pipeline_name,
                    format!(
                        "pipeline step {} depends on missing step id '{}'",
                        step_node_label(step, idx),
                        dependency_id
                    ),
                ));
            };
            indegree[idx] += 1;
            dependents[dependency_idx].push(idx);
        }
    }

    for child_indices in &mut dependents {
        child_indices.sort_unstable();
    }

    let mut ready = VecDeque::new();
    for (idx, count) in indegree.iter().enumerate() {
        if *count == 0 {
            ready.push_back(idx);
        }
    }

    let mut ordered = Vec::with_capacity(steps.len());
    while let Some(idx) = ready.pop_front() {
        ordered.push(idx);
        for dependent_idx in dependents[idx].iter().copied() {
            indegree[dependent_idx] -= 1;
            if indegree[dependent_idx] == 0 {
                ready.push_back(dependent_idx);
            }
        }
    }

    if ordered.len() != steps.len() {
        let cycle_members = steps
            .iter()
            .enumerate()
            .filter(|&(idx, _step)| indegree[idx] > 0)
            .map(|(idx, step)| step_node_label(step, idx))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            pipeline_name,
            format!(
                "pipeline dependency cycle detected involving {}",
                cycle_members
            ),
        ));
    }

    Ok(ordered)
}

pub(super) fn step_matches_groups(step: &PipelineStep, wanted: &BTreeSet<&str>) -> bool {
    match step {
        PipelineStep::Check { groups, .. } => {
            groups.iter().any(|group| wanted.contains(group.as_str()))
        }
        _ => false,
    }
}

fn step_id(step: &PipelineStep) -> Option<&str> {
    match step {
        PipelineStep::Service { step_id, .. }
        | PipelineStep::Build { step_id, .. }
        | PipelineStep::Extension { step_id, .. }
        | PipelineStep::Git { step_id, .. }
        | PipelineStep::Stack { step_id, .. }
        | PipelineStep::Command { step_id, .. }
        | PipelineStep::CommandIfMissing { step_id, .. }
        | PipelineStep::Requirement { step_id, .. }
        | PipelineStep::Symlink { step_id, .. }
        | PipelineStep::SharedPath { step_id, .. }
        | PipelineStep::Patch { step_id, .. }
        | PipelineStep::Check { step_id, .. } => step_id.as_deref(),
    }
}

fn step_dependencies(step: &PipelineStep) -> &[String] {
    match step {
        PipelineStep::Service { depends_on, .. }
        | PipelineStep::Build { depends_on, .. }
        | PipelineStep::Extension { depends_on, .. }
        | PipelineStep::Git { depends_on, .. }
        | PipelineStep::Stack { depends_on, .. }
        | PipelineStep::Command { depends_on, .. }
        | PipelineStep::CommandIfMissing { depends_on, .. }
        | PipelineStep::Requirement { depends_on, .. }
        | PipelineStep::Symlink { depends_on, .. }
        | PipelineStep::SharedPath { depends_on, .. }
        | PipelineStep::Patch { depends_on, .. }
        | PipelineStep::Check { depends_on, .. } => depends_on,
    }
}

fn step_node_label(step: &PipelineStep, idx: usize) -> String {
    step_id(step)
        .map(|id| format!("'{}'", id))
        .unwrap_or_else(|| format!("at position {}", idx))
}
