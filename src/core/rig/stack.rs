//! Rig-to-stack integration.
//!
//! Rigs own local lifecycle orchestration. Stacks own combined-fixes branch
//! upkeep. This module is the narrow bridge: discover stack IDs declared on
//! rig components, then delegate to the existing stack primitive explicitly.

use serde::Serialize;
use std::path::{Path, PathBuf};

use super::spec::RigSpec;
use crate::core::error::{ErrorCode, Result};
use crate::core::plan::HomeboyPlan;
use crate::core::stack::{self, StackSpec, SyncOutput};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RigStackPlanEntry {
    pub component_id: String,
    pub stack_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RigStackSyncReport {
    pub rig_id: String,
    pub stacks: Vec<RigStackSyncEntry>,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RigStackSyncEntry {
    pub component_id: String,
    pub stack_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Stack sync plan reused from the underlying stack sync contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<HomeboyPlan>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub picked_count: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped_count: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

pub fn plan_stack_sync(rig: &RigSpec) -> Vec<RigStackPlanEntry> {
    let mut entries = rig
        .components
        .iter()
        .filter_map(|(component_id, component)| {
            component.stack.as_ref().map(|stack_id| RigStackPlanEntry {
                component_id: component_id.clone(),
                stack_id: stack_id.clone(),
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.component_id.cmp(&b.component_id));
    entries
}

pub fn run_sync(rig: &RigSpec, dry_run: bool) -> Result<RigStackSyncReport> {
    Ok(run_sync_with(
        rig,
        dry_run,
        |component_id, stack_id, dry_run| sync_checked(rig, component_id, stack_id, dry_run),
    ))
}

pub fn run_component_sync(
    rig: &RigSpec,
    component_id: &str,
    dry_run: bool,
) -> Result<RigStackSyncEntry> {
    let component = rig.components.get(component_id).ok_or_else(|| {
        crate::core::Error::rig_pipeline_failed(
            &rig.id,
            "stack",
            format!(
                "component '{}' not declared in rig `components` map",
                component_id
            ),
        )
    })?;
    let stack_id = component.stack.as_ref().ok_or_else(|| {
        crate::core::Error::rig_pipeline_failed(
            &rig.id,
            "stack",
            format!("component '{}' does not declare a stack", component_id),
        )
    })?;

    let entry = run_one(
        component_id,
        stack_id,
        dry_run,
        |component_id, stack_id, dry_run| sync_checked(rig, component_id, stack_id, dry_run),
    );

    if entry.status == "changed" || entry.status == "no-op" {
        return Ok(entry);
    }

    Err(crate::core::Error::rig_pipeline_failed(
        &rig.id,
        "stack",
        format!(
            "stack '{}' for component '{}' {}{}",
            entry.stack_id,
            entry.component_id,
            entry.status,
            entry
                .error
                .as_ref()
                .map(|e| format!(": {}", e))
                .unwrap_or_default()
        ),
    ))
}

pub(crate) fn run_sync_with<F>(
    rig: &RigSpec,
    dry_run: bool,
    mut sync_stack: F,
) -> RigStackSyncReport
where
    F: FnMut(&str, &str, bool) -> Result<SyncOutput>,
{
    let mut stacks = Vec::new();
    let mut success = true;

    for entry in plan_stack_sync(rig) {
        let result = run_one(
            &entry.component_id,
            &entry.stack_id,
            dry_run,
            &mut sync_stack,
        );
        if result.status == "conflict" || result.status == "failed" {
            success = false;
            stacks.push(result);
            break;
        }
        stacks.push(result);
    }

    RigStackSyncReport {
        rig_id: rig.id.clone(),
        stacks,
        success,
    }
}

fn run_one<F>(
    component_id: &str,
    stack_id: &str,
    dry_run: bool,
    mut sync_stack: F,
) -> RigStackSyncEntry
where
    F: FnMut(&str, &str, bool) -> Result<SyncOutput>,
{
    match sync_stack(component_id, stack_id, dry_run) {
        Ok(output) => entry_from_output(component_id, output),
        Err(error) => {
            let status = if error.code == ErrorCode::StackApplyConflict {
                "conflict"
            } else {
                "failed"
            };
            RigStackSyncEntry {
                component_id: component_id.to_string(),
                stack_id: stack_id.to_string(),
                status: status.to_string(),
                branch: None,
                base: None,
                target: None,
                plan: None,
                picked_count: 0,
                skipped_count: 0,
                dropped_count: 0,
                error: Some(error.to_string()),
            }
        }
    }
}

fn sync_checked(
    rig: &RigSpec,
    component_id: &str,
    stack_id: &str,
    dry_run: bool,
) -> Result<SyncOutput> {
    let mut spec = stack::load(stack_id)?;
    validate_component_stack_path(rig, component_id, &spec)?;
    stack::sync(&mut spec, dry_run)
}

pub(crate) fn validate_component_stack_path(
    rig: &RigSpec,
    component_id: &str,
    stack_spec: &StackSpec,
) -> Result<()> {
    rig.components.get(component_id).ok_or_else(|| {
        crate::core::Error::rig_pipeline_failed(
            &rig.id,
            "stack",
            format!(
                "component '{}' not declared in rig `components` map",
                component_id
            ),
        )
    })?;

    let rig_path = super::resolve_component_path(rig, component_id)?;
    let stack_path = stack::expand_path(&stack_spec.component_path);
    if comparable_path(&rig_path) == comparable_path(&stack_path) {
        return Ok(());
    }

    Err(crate::core::Error::rig_pipeline_failed(
        &rig.id,
        "stack",
        format!(
            "component '{}' declares stack '{}' but paths differ: rig component path '{}' vs stack component_path '{}'. Align one spec before running stack sync.",
            component_id, stack_spec.id, rig_path, stack_path
        ),
    ))
}

fn comparable_path(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if let Ok(canonical) = std::fs::canonicalize(&candidate) {
        return canonical;
    }

    if candidate.is_absolute() {
        return candidate;
    }

    std::env::current_dir()
        .unwrap_or_else(|_| Path::new(".").to_path_buf())
        .join(candidate)
}

fn entry_from_output(component_id: &str, output: SyncOutput) -> RigStackSyncEntry {
    let SyncOutput {
        preview,
        picked_count,
        skipped_count,
        ..
    } = output;
    let changed = stack::sync::sync_plan_would_mutate(&preview.plan);
    let dropped_count = preview.dropped_count;
    RigStackSyncEntry {
        component_id: component_id.to_string(),
        stack_id: preview.stack_id,
        status: if changed { "changed" } else { "no-op" }.to_string(),
        branch: Some(preview.branch),
        base: Some(preview.base),
        target: Some(preview.target),
        plan: Some(preview.plan),
        picked_count,
        skipped_count,
        dropped_count,
        error: None,
    }
}

#[cfg(test)]
#[path = "../../../tests/core/rig/stack_test.rs"]
mod stack_test;
