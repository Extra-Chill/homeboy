use crate::core::component::{self, Component};
use crate::core::extension::build;
use crate::core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod dependency_graph;
#[path = "npm_deps_provider.rs"]
pub(crate) mod npm_provider;
#[path = "deps_provider.rs"]
pub(crate) mod provider;

pub use dependency_graph::{
    stack_apply, stack_apply_plan, stack_plan, stack_plan_from_components, stack_status,
    DependencyStackApplyResult, DependencyStackApplyStep, DependencyStackCommandResult,
    DependencyStackEdgeStatus, DependencyStackPlan, DependencyStackPlanStep, DependencyStackStatus,
};
pub use npm_provider::npm_command_args;
pub use provider::{composer_command_args, composer_install_command_args, ComposerAction};

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

pub fn status_value(
    component_id: Option<&str>,
    path_override: Option<&str>,
    package_filter: Option<&str>,
) -> Result<serde_json::Value> {
    serialize_dependency_output(
        status(component_id, path_override, package_filter)?,
        "serialize deps status",
    )
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
        if provider.handles_package(&component, &path, package)? {
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

pub fn update_value(
    component_id: Option<&str>,
    path_override: Option<&str>,
    package: &str,
    constraint: Option<&str>,
    install: bool,
    rebuild: bool,
) -> Result<serde_json::Value> {
    serialize_dependency_output(
        update(
            component_id,
            path_override,
            package,
            constraint,
            DependencyUpdateOptions { install, rebuild },
        )?,
        "serialize deps update",
    )
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

pub fn install_value(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> Result<serde_json::Value> {
    serialize_dependency_output(
        install(component_id, path_override)?,
        "serialize deps install",
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

/// A single provider's dependency-install command for a detected workspace,
/// without executing it. Produced by [`dependency_install_plan`] so callers
/// (e.g. Lab workspace hydration) can detect providers on the controller using
/// the existing machinery and run the same install command on a runner (#7366).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyInstallPlanStep {
    /// Dependency-provider id (e.g. `composer`, `npm`) reporting the manifest
    /// that triggered detection. Matches the `package_manager` reported by
    /// `deps status` for the same provider.
    pub provider_id: String,
    /// Full install argv (program + args) the provider would run, e.g.
    /// `["composer", "install", "--no-interaction", "--no-progress"]`.
    pub command: Vec<String>,
}

/// Detect dependency providers for a workspace path and return the install
/// command each would run, without executing any of them.
///
/// Reuses [`provider::resolve_dependency_providers_optional`] (the detection
/// behind `homeboy deps install`) so a lockfile/manifest detected by an existing
/// provider surfaces its install command here: `composer.json` →
/// `composer install`, `package.json`/`package-lock.json` → `npm ci`/`npm
/// install`, and so on for whatever providers `deps.rs` already implements.
/// Providers whose install cannot be expressed as a standalone shell command
/// (component-script/extension providers) are omitted. Returns an empty vector
/// when no provider detects the workspace.
///
/// The lockfile/manifest files are part of the synced snapshot (only built
/// dependency trees like `vendor/`/`node_modules/` are excluded), so detecting
/// against the controller-side source path yields the same providers the
/// materialized runner workspace exposes.
pub fn dependency_install_plan(path: &Path) -> Result<Vec<DependencyInstallPlanStep>> {
    let (component, resolved_path) =
        resolve_component_path(None, Some(&path.display().to_string()))?;
    let providers = provider::resolve_dependency_providers_optional(&component, &resolved_path)?;
    let mut steps = Vec::new();
    for provider in providers {
        let status = provider.status(&component, &resolved_path, None)?;
        if let Some(command) = provider.install_command(&component, &resolved_path)? {
            steps.push(DependencyInstallPlanStep {
                provider_id: status.package_manager,
                command: command.argv(),
            });
        }
    }
    Ok(steps)
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

pub fn stack_status_value() -> Result<serde_json::Value> {
    serialize_dependency_output(stack_status()?, "serialize deps stack status")
}

pub fn stack_plan_value(upstream: &str) -> Result<serde_json::Value> {
    serialize_dependency_output(stack_plan(upstream)?, "serialize deps stack plan")
}

pub fn stack_apply_value(
    upstream: &str,
    constraint: Option<&str>,
    dry_run: bool,
    install: bool,
    rebuild: bool,
) -> Result<serde_json::Value> {
    serialize_dependency_output(
        stack_apply(upstream, constraint, dry_run, install, rebuild)?,
        "serialize deps stack apply",
    )
}

fn serialize_dependency_output<T: Serialize>(value: T, context: &str) -> Result<serde_json::Value> {
    serde_json::to_value(value)
        .map_err(|e| Error::internal_json(e.to_string(), Some(context.to_string())))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_install_plan_detects_composer_lockfile() {
        let _guard = crate::test_support::home_env_guard();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("composer.json"), "{}").expect("composer json");

        let plan = dependency_install_plan(project.path()).expect("composer plan");

        assert!(plan.iter().any(|step| {
            step.provider_id == "composer"
                && step.command
                    == vec![
                        "composer".to_string(),
                        "install".to_string(),
                        "--no-interaction".to_string(),
                        "--no-progress".to_string(),
                    ]
        }));
    }

    #[test]
    fn dependency_install_plan_detects_npm_package_lock() {
        let _guard = crate::test_support::home_env_guard();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("package.json"), "{}").expect("package json");
        std::fs::write(project.path().join("package-lock.json"), "{}").expect("package lock");

        let plan = dependency_install_plan(project.path()).expect("npm plan");

        assert!(plan.iter().any(|step| {
            step.provider_id == "npm" && step.command == vec!["npm".to_string(), "ci".to_string()]
        }));
    }

    #[test]
    fn dependency_install_plan_skips_when_no_provider_detects() {
        let _guard = crate::test_support::home_env_guard();
        let project = tempfile::tempdir().expect("project tempdir");

        let plan = dependency_install_plan(project.path()).expect("empty plan");

        assert!(
            plan.is_empty(),
            "no lockfile/manifest should yield no steps: {plan:?}"
        );
    }
}
