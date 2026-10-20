//! Pull-based dependency refresh shared by release planning and execution.
use homeboy_core::component::Component;
use homeboy_core::deps::{stack_refresh_for_component, DependencyStackRefreshResult};
use homeboy_core::{Error, Result};
use std::path::Path;

/// Planning always evaluates without mutation. Execution rechecks the preview
/// before applying it so a moving upstream cannot silently change release scope.
pub(super) fn preview(component: &Component) -> Result<DependencyStackRefreshResult> {
    let result =
        stack_refresh_for_component(component, Path::new(&component.local_path), true, true)?;
    validate(&result)?;
    Ok(result)
}

fn validate(result: &DependencyStackRefreshResult) -> Result<()> {
    if result.exit_code() != 0 {
        return Err(Error::validation_invalid_argument(
            "refresh_dependencies",
            result.gate_failure_message(),
            None,
            None,
        ));
    }
    if result.breaking {
        let reasons: Vec<String> = result
            .edges
            .iter()
            .filter(|edge| edge.breaking != Some(false))
            .map(|edge| format!("{}: {}", edge.package, edge.breaking_reasons.join("; ")))
            .collect();
        return Err(Error::validation_invalid_argument(
            "refresh_dependencies",
            format!("Dependency refresh blocked: breaking or unknown release range. Update and review dependencies separately before releasing. {}", reasons.join("; ")),
            None, None,
        ));
    }
    Ok(())
}

pub(super) fn apply(
    component: &Component,
    expected: &serde_json::Value,
) -> Result<serde_json::Value> {
    let current = preview(component)?;
    let actual =
        serde_json::to_value(&current).map_err(|e| Error::internal_unexpected(e.to_string()))?;
    if &actual != expected {
        return Err(Error::validation_invalid_argument(
            "refresh_dependencies",
            "Dependency refresh changed since planning; rerun release",
            None,
            None,
        ));
    }
    let result =
        stack_refresh_for_component(component, Path::new(&component.local_path), false, true)?;
    validate(&result)?;
    serde_json::to_value(result).map_err(|e| Error::internal_unexpected(e.to_string()))
}
