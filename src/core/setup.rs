//! Component setup orchestration.
//!
//! Core-owned entry point that CI (and humans) call instead of hardcoding the
//! extension install/refresh + dependency install sequence in shell. The
//! sequence — install every extension a component declares, then install the
//! component's dependencies through its detected providers — is policy that
//! belongs in core, not in GitHub Action scripts.
//!
//! All package-manager selection stays agnostic: dependency installs are
//! resolved through [`crate::core::deps`] providers (composer/npm/component
//! script/extension), chosen by workspace detection and manifest config rather
//! than literals here.

use std::path::PathBuf;

use serde::Serialize;

use crate::core::component;
use crate::core::deps::{self, DependencyInstallResult};
use crate::core::extension::{self, is_extension_linked};
use crate::core::{Error, Result};

#[derive(Debug, Clone, Serialize)]
pub struct ComponentSetupResult {
    pub component_id: String,
    pub component_path: String,
    /// Extensions installed for the component. `None` when no `--source` was
    /// provided (extension install is skipped; dependency install still runs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<ExtensionSetupSummary>,
    /// Dependency install results, when the component has any resolvable
    /// dependency provider. `None` when no provider is detected (nothing to do).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<DependencyInstallResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionSetupSummary {
    pub source: String,
    pub installed: Vec<InstalledExtensionSummary>,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledExtensionSummary {
    pub extension_id: String,
    pub path: String,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ComponentSetupOptions<'a> {
    /// Source (git URL or local path) to install the component's configured
    /// extensions from. When `None`, extension install is skipped — useful when
    /// extensions are already installed and only dependency setup is needed.
    pub extension_source: Option<&'a str>,
    /// Skip the dependency install step entirely.
    pub skip_dependencies: bool,
}

/// Run setup for a component: install its declared extensions (optional) and
/// install its dependencies through resolved providers.
///
/// This replaces the action-side `install-extension.sh` +
/// `auto-setup-environment.sh` (composer/npm/pnpm/yarn) policy with a single
/// core call. The package manager is chosen by detection/config, never by
/// hardcoded literals.
pub fn component_setup(
    component_id: Option<&str>,
    path_override: Option<&str>,
    options: &ComponentSetupOptions<'_>,
) -> Result<ComponentSetupResult> {
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

    let extensions = match options.extension_source {
        Some(source) => {
            let result = extension::install_for_component(&component, source)?;
            Some(ExtensionSetupSummary {
                source: result.source,
                installed: result
                    .installed
                    .into_iter()
                    .map(|entry| InstalledExtensionSummary {
                        linked: is_extension_linked(&entry.extension_id),
                        extension_id: entry.extension_id,
                        path: entry.path.to_string_lossy().to_string(),
                        source_revision: entry.source_revision,
                    })
                    .collect(),
                skipped: result.skipped,
            })
        }
        None => None,
    };

    // Dependency install is best-effort: a component without a resolvable
    // provider (no composer.json/package.json/deps script) is a no-op, not an
    // error — `install_for_resolved` returns `None` in that case.
    let dependencies = if options.skip_dependencies {
        None
    } else {
        deps::install_for_resolved(&component, &path)?
    };

    Ok(ComponentSetupResult {
        component_id: component.id.clone(),
        component_path: path.display().to_string(),
        extensions,
        dependencies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use std::fs;

    fn write_extension_fixture(root: &std::path::Path, id: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).expect("extension dir");
        fs::write(
            dir.join(format!("{}.json", id)),
            format!(r#"{{"name":"{} extension","version":"1.0.0"}}"#, id),
        )
        .expect("extension manifest");
    }

    fn write_component_fixture(root: &std::path::Path, extensions: &[&str]) {
        let extension_json = extensions
            .iter()
            .map(|id| format!(r#"    "{}": {{}}"#, id))
            .collect::<Vec<_>>()
            .join(",\n");
        fs::write(
            root.join("homeboy.json"),
            format!(
                r#"{{
  "id": "setup-component",
  "local_path": "{}",
  "extensions": {{
{}
  }}
}}"#,
                root.display(),
                extension_json
            ),
        )
        .expect("component config");
    }

    #[test]
    fn setup_installs_configured_extensions_from_source() {
        with_isolated_home(|home| {
            let home = home.path();
            let source = home.join("source");
            write_extension_fixture(&source, "alpha");
            write_extension_fixture(&source, "beta");

            let component_dir = home.join("component");
            fs::create_dir_all(&component_dir).expect("component dir");
            write_component_fixture(&component_dir, &["alpha", "beta"]);

            let result = component_setup(
                None,
                Some(&component_dir.to_string_lossy()),
                &ComponentSetupOptions {
                    extension_source: Some(&source.to_string_lossy()),
                    skip_dependencies: true,
                },
            )
            .expect("setup should succeed");

            let extensions = result.extensions.expect("extensions installed");
            let installed_ids: Vec<&str> = extensions
                .installed
                .iter()
                .map(|entry| entry.extension_id.as_str())
                .collect();
            assert_eq!(installed_ids, vec!["alpha", "beta"]);
            assert!(result.dependencies.is_none(), "deps were skipped");
        });
    }

    #[test]
    fn setup_dependency_step_is_noop_without_providers() {
        with_isolated_home(|home| {
            let home = home.path();
            let component_dir = home.join("component");
            fs::create_dir_all(&component_dir).expect("component dir");
            // Component with no extensions, composer.json, package.json, or deps
            // script — there is nothing to install.
            fs::write(
                component_dir.join("homeboy.json"),
                format!(
                    r#"{{"id":"setup-component","local_path":"{}"}}"#,
                    component_dir.display()
                ),
            )
            .expect("component config");

            let result = component_setup(
                None,
                Some(&component_dir.to_string_lossy()),
                &ComponentSetupOptions::default(),
            )
            .expect("setup should succeed even with nothing to install");

            assert!(result.extensions.is_none());
            assert!(
                result.dependencies.is_none(),
                "no dependency provider should be a no-op, not an error"
            );
        });
    }
}
