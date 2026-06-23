//! Component resolution plus build, extension, and stack pipeline steps.

use super::super::expand::expand_vars;
use super::super::spec::{ComponentSpec, RigSpec, StackOp};
use super::super::stack as rig_stack;
use crate::core::component::Component;
use crate::core::error::{Error, Result};

pub(super) fn resolve_component_path(
    rig: &RigSpec,
    component_id: &str,
) -> Result<(ComponentSpec, String)> {
    let component = rig.components.get(component_id).ok_or_else(|| {
        Error::rig_pipeline_failed(
            &rig.id,
            "build",
            format!(
                "component '{}' not declared in rig `components` map",
                component_id
            ),
        )
    })?;
    let path = expand_vars(rig, &component.path);
    Ok((component.clone(), path))
}

fn resolve_rig_component(rig: &RigSpec, component_id: &str) -> Result<Component> {
    let (component, path) = resolve_component_path(rig, component_id)?;
    let mut resolved = Component {
        id: component_id.to_string(),
        local_path: path,
        remote_url: component.remote_url,
        triage_remote_url: component.triage_remote_url,
        extensions: component.extensions,
        ..Component::default()
    };
    resolved.resolve_remote_path();
    Ok(resolved)
}

pub(super) fn run_build_step(rig: &RigSpec, component_id: &str) -> Result<()> {
    let component = resolve_rig_component(rig, component_id)?;
    let (result, exit_code) = crate::core::build::run_component(&component)?;

    if exit_code != 0 {
        let detail = match &result {
            crate::core::build::BuildResult::Single(output) => {
                let tail = output
                    .output
                    .stderr
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                if tail.trim().is_empty() {
                    format!("exit {}", exit_code)
                } else {
                    format!("exit {} — {}", exit_code, tail)
                }
            }
            crate::core::build::BuildResult::Bulk(_) => format!("exit {}", exit_code),
        };
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "build",
            format!("build {} failed: {}", component_id, detail),
        ));
    }
    Ok(())
}

pub(super) fn run_extension_step(rig: &RigSpec, component_id: &str, op: &str) -> Result<()> {
    match op {
        "build" => run_build_step(rig, component_id),
        other => Err(Error::rig_pipeline_failed(
            &rig.id,
            "extension",
            format!(
                "extension op '{}' is not supported for component '{}'; supported ops: build",
                other, component_id
            ),
        )),
    }
}

pub(super) fn run_stack_step(
    rig: &RigSpec,
    component_id: &str,
    op: StackOp,
    dry_run: bool,
) -> Result<()> {
    match op {
        StackOp::Sync => {
            rig_stack::run_component_sync(rig, component_id, dry_run)?;
            Ok(())
        }
    }
}
