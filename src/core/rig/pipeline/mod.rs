//! Rig pipeline executor.
//!
//! Optional step IDs plus `depends_on` edges are topologically ordered before
//! sequential execution. Caching and parallelism are later #1464 phases.
//!
//! Every step emits a `PipelineStepOutcome`. The runner aggregates them into
//! a `PipelineOutcome` with overall success/failure.

mod command_step;
mod component;
mod fs_step;
mod git_step;
mod labels;
mod ordering;
mod outcome;
mod patch_step;
mod requirement_step;
mod service_step;

use std::collections::BTreeSet;

use super::check;
use super::spec::{PipelineStep, RigSpec};
use crate::core::error::Result;

pub use outcome::{PipelineOutcome, PipelineStepOutcome};

pub fn run_pipeline(rig: &RigSpec, name: &str, fail_fast: bool) -> Result<PipelineOutcome> {
    run_pipeline_with_settings(rig, name, fail_fast, &[])
}

pub fn run_pipeline_with_settings(
    rig: &RigSpec,
    name: &str,
    fail_fast: bool,
    settings: &[(String, String)],
) -> Result<PipelineOutcome> {
    let steps = rig.pipeline.get(name).cloned().unwrap_or_default();
    let ordered_indices = ordering::order_pipeline_steps(rig, name, &steps)?;
    run_ordered_steps(rig, name, &steps, ordered_indices, fail_fast, settings)
}

pub fn run_pipeline_check_groups(
    rig: &RigSpec,
    groups: &[String],
    fail_fast: bool,
) -> Result<PipelineOutcome> {
    let wanted: BTreeSet<&str> = groups.iter().map(String::as_str).collect();
    let steps = rig
        .pipeline
        .get("check")
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|step| ordering::step_matches_groups(step, &wanted))
        .collect::<Vec<_>>();
    let ordered_indices = ordering::order_pipeline_steps(rig, "check", &steps)?;
    run_ordered_steps(rig, "check", &steps, ordered_indices, fail_fast, &[])
}

pub fn cleanup_shared_paths(rig: &RigSpec) -> Result<()> {
    fs_step::cleanup_shared_paths(rig)
}

fn run_ordered_steps(
    rig: &RigSpec,
    name: &str,
    steps: &[PipelineStep],
    ordered_indices: Vec<usize>,
    fail_fast: bool,
    settings: &[(String, String)],
) -> Result<PipelineOutcome> {
    let mut outcomes = Vec::with_capacity(ordered_indices.len());
    let mut failed = 0;
    let mut passed = 0;
    let mut aborted = false;

    for idx in ordered_indices {
        let step = &steps[idx];
        if aborted {
            outcomes.push(PipelineStepOutcome {
                kind: labels::step_kind(step).to_string(),
                label: labels::step_label(rig, step, idx),
                status: "skip".to_string(),
                error: None,
            });
            continue;
        }

        let label = labels::step_label(rig, step, idx);
        crate::log_status!("rig", "{}: {}", name, label);

        let result = run_step(rig, name, step, settings);

        let outcome = match &result {
            Ok(()) => PipelineStepOutcome {
                kind: labels::step_kind(step).to_string(),
                label: label.clone(),
                status: "pass".to_string(),
                error: None,
            },
            Err(e) => PipelineStepOutcome {
                kind: labels::step_kind(step).to_string(),
                label: label.clone(),
                status: "fail".to_string(),
                error: Some(e.to_string()),
            },
        };

        match &result {
            Ok(()) => passed += 1,
            Err(_) => {
                failed += 1;
                if fail_fast {
                    aborted = true;
                }
            }
        }

        outcomes.push(outcome);
    }

    Ok(PipelineOutcome {
        name: name.to_string(),
        steps: outcomes,
        passed,
        failed,
    })
}

fn run_step(
    rig: &RigSpec,
    pipeline_name: &str,
    step: &PipelineStep,
    settings: &[(String, String)],
) -> Result<()> {
    match step {
        PipelineStep::Service { id, op, .. } => service_step::run_service_step(rig, id, *op),
        PipelineStep::Build { component, .. } => component::run_build_step(rig, component),
        PipelineStep::Extension { component, op, .. } => {
            component::run_extension_step(rig, component, op)
        }
        PipelineStep::Git {
            component,
            op,
            args,
            ..
        } => git_step::run_git_step(rig, component, *op, args),
        PipelineStep::Stack {
            component,
            op,
            dry_run,
            ..
        } => component::run_stack_step(rig, component, *op, *dry_run),
        PipelineStep::Command { .. } | PipelineStep::CommandIfMissing { .. } => {
            command_step::run_command_pipeline_step(rig, step, settings)
        }
        PipelineStep::Requirement { .. } => {
            requirement_step::run_requirement_step(rig, pipeline_name, step, settings)
        }
        PipelineStep::Symlink { op, .. } => fs_step::run_symlink_step(rig, *op),
        PipelineStep::SharedPath { op, .. } => fs_step::run_shared_path_step(rig, *op),
        PipelineStep::Patch {
            component,
            file,
            marker,
            after,
            content,
            op,
            ..
        } => {
            patch_step::run_patch_step(rig, component, file, marker, after.as_deref(), content, *op)
        }
        PipelineStep::Check { spec, .. } => check::evaluate(rig, spec),
    }
}

#[cfg(test)]
#[path = "../../../../tests/core/rig/pipeline_test.rs"]
mod pipeline_test;
