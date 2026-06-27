use crate::core::component::{self, Component};
use crate::core::extension::build;
use crate::core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod dependency_graph;

use crate::extensions::deps_provider as provider;
pub use dependency_graph::{
    stack_apply, stack_apply_plan, stack_plan, stack_plan_from_components, stack_status,
    DependencyStackApplyResult, DependencyStackApplyStep, DependencyStackCommandResult,
    DependencyStackEdgeStatus, DependencyStackPlan, DependencyStackPlanStep, DependencyStackStatus,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyPackage {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_section: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constraint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyStatus {
    pub component_id: String,
    pub component_path: String,
    pub package_manager: String,
    pub packages: Vec<DependencyPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyUpdateResult {
    pub component_id: String,
    pub component_path: String,
    pub package_manager: String,
    pub package: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_constraint: Option<String>,
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<DependencyPackage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<DependencyPackage>,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install: Option<DependencyCommandResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebuild: Option<DependencyCommandResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyCommandResult {
    pub command: Vec<String>,
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyInstallResult {
    pub component_id: String,
    pub component_path: String,
    pub package_manager: String,
    /// One entry per dependency provider that ran an install. Providers that
    /// report nothing to install (e.g. no manifest detected) are omitted.
    pub installs: Vec<DependencyCommandResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyUpdateOptions {
    pub install: bool,
    pub rebuild: bool,
}

impl Default for DependencyUpdateOptions {
    fn default() -> Self {
        Self {
            install: true,
            rebuild: false,
        }
    }
}

pub fn status(
    component_id: Option<&str>,
    path_override: Option<&str>,
    package_filter: Option<&str>,
) -> Result<DependencyStatus> {
    let (component, path) = resolve_component_path(component_id, path_override)?;
    let providers = provider::resolve_dependency_providers(&component, &path)?;
    let mut statuses = Vec::new();

    for provider in providers {
        statuses.push(provider.status(&component, &path, package_filter)?);
    }

    Ok(combine_provider_statuses(&component, &path, statuses))
}

pub fn update(
    component_id: Option<&str>,
    path_override: Option<&str>,
    package: &str,
    constraint: Option<&str>,
    options: DependencyUpdateOptions,
) -> Result<DependencyUpdateResult> {
    let (component, path) = resolve_component_path(component_id, path_override)?;
    let providers = provider::resolve_dependency_providers(&component, &path)?;

    for provider in providers {
        if provider.handles_package(&path, package)? {
            let mut result = provider.update(&component, &path, package, constraint)?;
            if options.install {
                result.install = provider.install(&component, &path)?;
            }
            if options.rebuild {
                result.rebuild = Some(rebuild_component(&component, &path)?);
            }
            return Ok(result);
        }
    }

    Err(Error::validation_invalid_argument(
        "package",
        format!(
            "No dependency provider for component '{}' manages package '{}'",
            component.id, package
        ),
        Some(package.to_string()),
        None,
    ))
}

/// Install a component's dependencies through its resolved dependency providers.
///
/// This is the detection/config-driven replacement for hardcoded
/// per-ecosystem install CI policy: the package manager(s) are chosen by
/// [`provider::resolve_dependency_providers`] based on the manifest and lock
/// files present in the workspace and the component/extension manifest — not
/// by shell literals in the calling environment. CI (or any caller) runs
/// `homeboy component setup` or `homeboy deps install` and lets core own the
/// policy.
pub fn install(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> Result<DependencyInstallResult> {
    let (component, path) = resolve_component_path(component_id, path_override)?;
    // Reuse the command-facing resolver so a dependency-less component returns
    // the same actionable "no provider" error as `deps status`/`deps update`.
    provider::resolve_dependency_providers(&component, &path)?;
    Ok(
        install_for_resolved(&component, &path)?.unwrap_or_else(|| DependencyInstallResult {
            component_id: component.id.clone(),
            component_path: path.display().to_string(),
            package_manager: String::new(),
            installs: Vec::new(),
        }),
    )
}

/// Run dependency installs for an already-resolved component/workspace pair.
///
/// Shared by [`install`] and the higher-level `component setup` orchestrator so
/// the provider-resolution and best-effort install policy lives in exactly one
/// place.
///
/// Returns `None` when the workspace exposes no dependency provider (nothing to
/// install) so callers can treat a dependency-less component as a no-op.
pub fn install_for_resolved(
    component: &Component,
    path: &Path,
) -> Result<Option<DependencyInstallResult>> {
    let providers = provider::resolve_dependency_providers_optional(component, path)?;
    if providers.is_empty() {
        return Ok(None);
    }

    let mut installs = Vec::new();
    let mut package_managers = Vec::new();
    for provider in providers {
        let status = provider.status(component, path, None)?;
        if let Some(result) = provider.install(component, path)? {
            package_managers.push(status.package_manager);
            installs.push(result);
        }
    }

    let package_manager = match package_managers.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        many => many.join(","),
    };

    Ok(Some(DependencyInstallResult {
        component_id: component.id.clone(),
        component_path: path.display().to_string(),
        package_manager,
        installs,
    }))
}

fn rebuild_component(component: &Component, path: &Path) -> Result<DependencyCommandResult> {
    let mut build_component = component.clone();
    build_component.local_path = path.display().to_string();
    let (result, exit_code) = build::run_component(&build_component)?;
    let stdout = serde_json::to_string(&result).map_err(|e| {
        Error::internal_json(e.to_string(), Some("serialize deps rebuild".to_string()))
    })?;
    let command = vec![
        "homeboy".to_string(),
        "build".to_string(),
        component.id.clone(),
        "--path".to_string(),
        path.display().to_string(),
    ];

    if exit_code != 0 {
        return Err(Error::validation_invalid_argument(
            "rebuild",
            format!(
                "Dependency update rebuild failed for '{}' with status {}",
                component.id, exit_code
            ),
            Some(component.id.clone()),
            Some(vec![format!("Run manually: {}", command.join(" "))]),
        ));
    }

    Ok(DependencyCommandResult {
        command,
        skipped: false,
        status: Some(exit_code),
        stdout,
        stderr: String::new(),
    })
}

fn resolve_component_path(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> Result<(Component, PathBuf)> {
    let component = component::resolve_effective(component_id, path_override, None)?;
    let path = PathBuf::from(shellexpand::tilde(&component.local_path).as_ref());

    if !path.exists() {
        return Err(Error::validation_invalid_argument(
            "component_path",
            format!(
                "Component '{}' path does not exist: {}",
                component.id,
                path.display()
            ),
            Some(component.id.clone()),
            None,
        ));
    }

    Ok((component, path))
}

fn combine_provider_statuses(
    component: &Component,
    path: &Path,
    statuses: Vec<provider::ProviderDependencyStatus>,
) -> DependencyStatus {
    let package_manager = match statuses.as_slice() {
        [only] => only.package_manager.clone(),
        _ => statuses
            .iter()
            .map(|status| status.package_manager.as_str())
            .collect::<Vec<_>>()
            .join(","),
    };
    let packages = statuses
        .into_iter()
        .flat_map(|status| status.packages)
        .collect();

    DependencyStatus {
        component_id: component.id.clone(),
        component_path: path.display().to_string(),
        package_manager,
        packages,
    }
}
