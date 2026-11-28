//! Pull-based dependency refresh shared by release planning and execution.
use homeboy_core::component::Component;
use homeboy_core::deps::{
    stack_refresh_nonbreaking_for_component, DependencyStackEdgeRefreshState,
    DependencyStackRefreshResult,
};
use homeboy_core::{Error, Result};
use std::path::Path;

/// Planning always evaluates without mutation. Execution rechecks the preview
/// before applying it so a moving upstream cannot silently change release scope.
pub(super) fn preview(component: &Component) -> Result<DependencyStackRefreshResult> {
    refresh(component, true)
}

fn refresh(component: &Component, dry_run: bool) -> Result<DependencyStackRefreshResult> {
    let result = stack_refresh_nonbreaking_for_component(
        component,
        Path::new(&component.local_path),
        dry_run,
    )?;
    if result.exit_code() != 0 {
        return Err(Error::validation_invalid_argument(
            "refresh_dependencies",
            result.gate_failure_message(),
            None,
            None,
        ));
    }
    Ok(result)
}

pub(super) fn apply(
    component: &Component,
    expected: &serde_json::Value,
) -> Result<serde_json::Value> {
    let current = preview(component)?;
    let actual =
        serde_json::to_value(&current).map_err(|e| Error::internal_unexpected(e.to_string()))?;
    if &actual != expected {
        return Err(refresh_changed());
    }
    let result = refresh(component, false)?;
    // The apply pass resolves upstreams again. Don't ship a changelog for a
    // different refresh if a range moved or became uninspectable after the recheck.
    if current.edges.len() != result.edges.len()
        || !current
            .edges
            .iter()
            .zip(&result.edges)
            .all(|(before, after)| {
                before.state == after.state && before.from == after.from && before.to == after.to
            })
    {
        return Err(refresh_changed());
    }
    serde_json::to_value(result).map_err(|e| Error::internal_unexpected(e.to_string()))
}

fn refresh_changed() -> Error {
    Error::validation_invalid_argument(
        "refresh_dependencies",
        "Dependency refresh changed since planning; rerun release",
        None,
        None,
    )
}

pub(super) fn skip_messages(
    result: &DependencyStackRefreshResult,
) -> impl Iterator<Item = String> + '_ {
    result
        .edges
        .iter()
        .filter(|edge| edge.state == DependencyStackEdgeRefreshState::SkippedUnsafeRange)
        .map(|edge| {
            format!(
                concat!(
                    "Skipped dependency refresh for {} ({} -> {}): breaking or unknown release range ({}). ",
                    "Keeping the pinned version; review the bump in the dependency-refresh PR."
                ),
                edge.package,
                edge.from.as_deref().unwrap_or_default(),
                edge.to.as_deref().unwrap_or_default(),
                edge.breaking_reasons.join("; ")
            )
        })
}
