use crate::core::component::{self, Component};
use crate::core::extension;
use crate::core::extension::build;
use crate::core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod dependency_graph;
#[path = "deps_provider.rs"]
pub(crate) mod provider;

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
    /// Dependency-provider id reporting the manifest that triggered detection.
    /// Matches the `package_manager` reported by `deps status` for the same
    /// provider.
    pub provider_id: String,
    /// Portable install invocation the runner can execute without receiving a
    /// controller-local extension path.
    pub invocation: DependencyInstallInvocation,
}

/// An install command suitable for crossing a controller/runner boundary.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DependencyInstallInvocation {
    /// A provider command that does not reference an installed extension path.
    Argv { argv: Vec<String> },
    /// An extension-owned entrypoint. The runner resolves `entrypoint` inside
    /// its own materialized copy of `extension_id` before executing `argv`.
    ExtensionEntrypoint {
        extension_id: String,
        entrypoint: String,
        /// The executable and argument list with the entrypoint removed.
        argv: Vec<String>,
        entrypoint_index: usize,
    },
}

/// Detect dependency providers for a workspace path and return the install
/// command each would run, without executing any of them.
///
/// Reuses [`provider::resolve_dependency_providers_optional`] (the detection
/// behind `homeboy deps install`) so a manifest detected by an existing provider
/// surfaces its install command here. A linked extension set that does not
/// provide dependency support is equivalent to no provider for this optional
/// planning surface; invalid explicit capability ownership still fails.
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
    let providers =
        match provider::resolve_dependency_providers_optional(&component, &resolved_path) {
            Ok(providers) => providers,
            Err(error) => {
                if !extension::has_linked_extension_for_capability(
                    &component,
                    crate::core::extension::ExtensionCapability::Deps,
                )? {
                    Vec::new()
                } else {
                    return Err(error);
                }
            }
        };
    let mut steps = Vec::new();
    for provider in providers {
        let status = provider.status(&component, &resolved_path, None)?;
        if let Some(command) = provider.install_command(&component, &resolved_path)? {
            steps.push(DependencyInstallPlanStep {
                provider_id: status.package_manager,
                invocation: dependency_install_invocation(command.argv())?,
            });
        }
    }
    Ok(steps)
}

fn dependency_install_invocation(argv: Vec<String>) -> Result<DependencyInstallInvocation> {
    for (entrypoint_index, value) in argv.iter().enumerate() {
        if let Some((extension_id, entrypoint)) = installed_extension_entrypoint(Path::new(value)) {
            return Ok(extension_install_invocation(
                argv,
                entrypoint_index,
                extension_id,
                entrypoint,
            ));
        }
    }
    for extension in extension::load_all_extensions()? {
        let Some(root) = extension.extension_path else {
            continue;
        };
        for (entrypoint_index, value) in argv.iter().enumerate() {
            let path = Path::new(value);
            if !path.is_absolute() {
                continue;
            }
            if let Ok(entrypoint) = path.strip_prefix(&root) {
                let entrypoint = entrypoint.to_string_lossy().to_string();
                return Ok(extension_install_invocation(
                    argv,
                    entrypoint_index,
                    extension.id,
                    entrypoint,
                ));
            }
        }
    }
    Ok(DependencyInstallInvocation::Argv { argv })
}

fn installed_extension_entrypoint(path: &Path) -> Option<(String, String)> {
    let components = path.components().collect::<Vec<_>>();
    let marker = [".config", "homeboy", "extensions"];
    let marker_index = components.windows(marker.len()).position(|window| {
        window
            .iter()
            .zip(marker)
            .all(|(component, expected)| component.as_os_str() == expected)
    })?;
    let extension_id = components.get(marker_index + marker.len())?;
    let entrypoint = components.get(marker_index + marker.len() + 1..)?;
    if entrypoint.is_empty() {
        return None;
    }
    Some((
        extension_id.as_os_str().to_string_lossy().to_string(),
        entrypoint
            .iter()
            .map(|component| component.as_os_str())
            .collect::<PathBuf>()
            .to_string_lossy()
            .to_string(),
    ))
}

fn extension_install_invocation(
    mut argv: Vec<String>,
    entrypoint_index: usize,
    extension_id: String,
    entrypoint: String,
) -> DependencyInstallInvocation {
    argv.remove(entrypoint_index);
    DependencyInstallInvocation::ExtensionEntrypoint {
        extension_id,
        entrypoint,
        argv,
        entrypoint_index,
    }
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
    fn extension_owned_install_path_becomes_portable_invocation() {
        crate::test_support::with_isolated_home(|home| {
            let extension_id = "fixture-runtime";
            let extension_root = home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id);
            std::fs::create_dir_all(extension_root.join("scripts")).expect("extension root");
            std::fs::write(
                extension_root.join(format!("{extension_id}.json")),
                r#"{"name":"Fixture runtime","version":"1.0.0"}"#,
            )
            .expect("extension manifest");
            let invocation = dependency_install_invocation(vec![
                "sh".to_string(),
                extension_root
                    .join("scripts/install.sh")
                    .display()
                    .to_string(),
                "install".to_string(),
            ])
            .expect("portable invocation");

            assert_eq!(
                invocation,
                DependencyInstallInvocation::ExtensionEntrypoint {
                    extension_id: extension_id.to_string(),
                    entrypoint: "scripts/install.sh".to_string(),
                    argv: vec!["sh".to_string(), "install".to_string()],
                    entrypoint_index: 1,
                }
            );
        });
    }

    #[test]
    fn configured_extension_asset_from_foreign_controller_home_becomes_portable() {
        crate::test_support::with_isolated_home(|_active_home| {
            let invocation = dependency_install_invocation(vec![
                "bash".to_string(),
                "/controller/home/.config/homeboy/extensions/fixture-runtime/scripts/install.sh"
                    .to_string(),
                "install".to_string(),
            ])
            .expect("portable invocation");

            assert_eq!(
                invocation,
                DependencyInstallInvocation::ExtensionEntrypoint {
                    extension_id: "fixture-runtime".to_string(),
                    entrypoint: "scripts/install.sh".to_string(),
                    argv: vec!["bash".to_string(), "install".to_string()],
                    entrypoint_index: 1,
                }
            );
        });
    }

    #[test]
    fn dependency_install_plan_skips_linked_extensions_without_deps_support() {
        crate::test_support::with_isolated_home(|home| {
            let project = tempfile::tempdir().expect("project tempdir");
            let extension_id = "fixture-non-deps";
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id);
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            std::fs::write(
                extension_dir.join(format!("{extension_id}.json")),
                r#"{"name":"Fixture non-deps","version":"1.0.0"}"#,
            )
            .expect("extension manifest");
            std::fs::write(
                project.path().join("homeboy.json"),
                format!(
                    r#"{{"id":"fixture","local_path":"{}","extensions":{{"{extension_id}":{{}}}}}}"#,
                    project.path().display()
                ),
            )
            .expect("component manifest");

            let plan = dependency_install_plan(project.path())
                .expect("unrelated linked extensions do not require dependency hydration");

            assert!(plan.is_empty());
        });
    }
}
