use crate::core::component::{self, Component};
use crate::core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod stack;

use crate::extensions::deps_provider as provider;
pub use crate::extensions::deps_provider::{composer_command_args, ComposerAction};
pub use stack::{
    stack_apply, stack_plan, stack_plan_from_components, stack_status, DependencyStackApplyResult,
    DependencyStackApplyStep, DependencyStackCommandResult, DependencyStackEdgeStatus,
    DependencyStackPlan, DependencyStackPlanStep, DependencyStackStatus,
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
) -> Result<DependencyUpdateResult> {
    let (component, path) = resolve_component_path(component_id, path_override)?;
    let providers = provider::resolve_dependency_providers(&component, &path)?;

    for provider in providers {
        if provider.handles_package(&path, package)? {
            return provider.update(&component, &path, package, constraint);
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
