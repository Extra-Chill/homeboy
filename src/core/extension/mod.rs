mod execution;
mod lifecycle;
mod manifest;
mod runner;
mod scope;
pub mod version;

pub mod exec_context;

// Re-export runner types
pub use runner::{ExtensionRunner, RunnerOutput};

// Re-export manifest types
pub use manifest::{
    ActionConfig, ActionType, AuditCapability, BuildConfig, CliConfig, DatabaseCliConfig,
    DatabaseConfig, DeployCapability, DeployOverride, DeployVerification, DiscoveryConfig,
    DocTarget, ExecutableCapability, HttpMethod, InputConfig, LintConfig, ExtensionManifest,
    OutputConfig, OutputSchema, PlatformCapability, ProvidesConfig, RequirementsConfig,
    RuntimeConfig, ScriptsConfig, SelectOption, SettingConfig, SinceTagConfig, TestConfig,
    VersionPatternConfig,
};

// Re-export version types
pub use version::{VersionConstraint, parse_extension_version};

// Re-export execution types and functions
pub(crate) use execution::execute_action;
pub use execution::{
    is_extension_compatible, extension_ready_status, run_action, run_extension, run_setup,
    ExtensionExecutionMode, ExtensionReadyStatus, ExtensionRunResult, ExtensionSetupResult, ExtensionStepFilter,
};

// Re-export scope types
pub use scope::ExtensionScope;

// Re-export lifecycle types and functions
pub use lifecycle::{
    check_update_available, derive_id_from_url, install, is_git_url, slugify_id, uninstall, update,
    InstallResult, UpdateAvailable, UpdateResult,
};

// Extension loader functions

use crate::config;
use crate::error::Result;
use crate::output::MergeOutput;
use crate::paths;
use std::path::PathBuf;

pub fn load_extension(id: &str) -> Result<ExtensionManifest> {
    let mut manifest = config::load::<ExtensionManifest>(id)?;
    let extension_dir = paths::extension(id)?;
    manifest.extension_path = Some(extension_dir.to_string_lossy().to_string());
    Ok(manifest)
}

pub fn load_all_extensions() -> Result<Vec<ExtensionManifest>> {
    let extensions = config::list::<ExtensionManifest>()?;
    let mut extensions_with_paths = Vec::new();
    for mut extension in extensions {
        let extension_dir = paths::extension(&extension.id)?;
        extension.extension_path = Some(extension_dir.to_string_lossy().to_string());
        extensions_with_paths.push(extension);
    }
    Ok(extensions_with_paths)
}

pub fn find_extension_by_tool(tool: &str) -> Option<ExtensionManifest> {
    load_all_extensions().ok().and_then(|extensions| {
        extensions
            .into_iter()
            .find(|m| m.cli.as_ref().is_some_and(|c| c.tool == tool))
    })
}

/// Find a extension that handles a given file extension and has a specific capability script.
///
/// Looks through all installed extensions for one whose `provides.file_extensions` includes
/// the given extension and whose `scripts` has the requested capability configured.
///
/// Returns the extension manifest with `extension_path` populated.
pub fn find_extension_for_file_ext(ext: &str, capability: &str) -> Option<ExtensionManifest> {
    load_all_extensions().ok().and_then(|extensions| {
        extensions.into_iter().find(|m| {
            if !m.handles_file_extension(ext) {
                return false;
            }
            match capability {
                "fingerprint" => m.fingerprint_script().is_some(),
                "refactor" => m.refactor_script().is_some(),
                _ => false,
            }
        })
    })
}

/// Run a extension's fingerprint script on file content.
///
/// The script receives a JSON object on stdin:
/// ```json
/// {"file_path": "src/core/foo.rs", "content": "...file content..."}
/// ```
///
/// The script must output a JSON object on stdout matching the FileFingerprint schema:
/// ```json
/// {
///   "methods": ["foo", "bar"],
///   "type_name": "MyStruct",
///   "implements": ["SomeTrait"],
///   "registrations": [],
///   "namespace": null,
///   "imports": ["crate::error::Result"]
/// }
/// ```
pub fn run_fingerprint_script(
    extension: &ExtensionManifest,
    file_path: &str,
    content: &str,
) -> Option<FingerprintOutput> {
    let extension_path = extension.extension_path.as_deref()?;
    let script_rel = extension.fingerprint_script()?;
    let script_path = std::path::Path::new(extension_path).join(script_rel);

    if !script_path.exists() {
        return None;
    }

    let input = serde_json::json!({
        "file_path": file_path,
        "content": content,
    });

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(script_path.to_string_lossy().as_ref())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(input.to_string().as_bytes());
            }
            child.wait_with_output().ok()
        })?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).ok()
}

/// Output from a fingerprint extension script.
/// Matches the structural data extracted from a source file.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FingerprintOutput {
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub type_name: Option<String>,
    #[serde(default)]
    pub implements: Vec<String>,
    #[serde(default)]
    pub registrations: Vec<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub imports: Vec<String>,
}

pub fn extension_path(id: &str) -> PathBuf {
    paths::extension(id).unwrap_or_else(|_| PathBuf::from(id))
}

pub fn available_extension_ids() -> Vec<String> {
    config::list_ids::<ExtensionManifest>().unwrap_or_default()
}

pub fn save_manifest(manifest: &ExtensionManifest) -> Result<()> {
    config::save(manifest)
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    config::merge::<ExtensionManifest>(id, json_spec, replace_fields)
}

/// Check if a extension is a symlink (linked, not installed).
pub fn is_extension_linked(extension_id: &str) -> bool {
    paths::extension(extension_id)
        .map(|p| p.is_symlink())
        .unwrap_or(false)
}

/// Validate that all extensions declared in a component's `extensions` field are installed.
///
/// If `component.extensions` contains keys like `{"wordpress": {}}`, those extensions
/// are implicitly required. Returns an actionable error with install commands
/// when any are missing.
pub fn validate_required_extensions(component: &crate::component::Component) -> Result<()> {
    let extensions = match &component.extensions {
        Some(m) if !m.is_empty() => m,
        _ => return Ok(()),
    };

    let mut missing: Vec<String> = Vec::new();
    for extension_id in extensions.keys() {
        if load_extension(extension_id).is_err() {
            missing.push(extension_id.clone());
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    missing.sort();

    let extension_list = missing.join(", ");
    let install_hints: Vec<String> = missing
        .iter()
        .map(|id| {
            format!(
                "homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id {}",
                id
            )
        })
        .collect();

    let message = if missing.len() == 1 {
        format!(
            "Component '{}' requires extension '{}' which is not installed",
            component.id, missing[0]
        )
    } else {
        format!(
            "Component '{}' requires extensions not installed: {}",
            component.id, extension_list
        )
    };

    let mut err = crate::error::Error::new(
        crate::error::ErrorCode::ExtensionNotFound,
        message,
        serde_json::json!({
            "component_id": component.id,
            "missing_extensions": missing,
        }),
    );

    for hint in &install_hints {
        err = err.with_hint(hint.to_string());
    }

    err = err.with_hint("Browse available extensions: https://github.com/Extra-Chill/homeboy-extensions".to_string());

    Err(err)
}

/// Validate that all extensions declared in a component's `extensions` field are installed
/// and satisfy the declared version constraints.
///
/// Returns an actionable error listing every unsatisfied requirement with install/update hints.
pub fn validate_extension_requirements(component: &crate::component::Component) -> Result<()> {
    let extensions = match &component.extensions {
        Some(e) if !e.is_empty() => e,
        _ => return Ok(()),
    };

    let mut errors: Vec<String> = Vec::new();
    let mut hints: Vec<String> = Vec::new();

    for (extension_id, ext_config) in extensions {
        let constraint_str = match &ext_config.version {
            Some(v) => v.as_str(),
            None => continue, // No version constraint, skip validation
        };

        let constraint = match version::VersionConstraint::parse(constraint_str) {
            Ok(c) => c,
            Err(_) => {
                errors.push(format!(
                    "Invalid version constraint '{}' for extension '{}'",
                    constraint_str, extension_id
                ));
                continue;
            }
        };

        match load_extension(extension_id) {
            Ok(extension) => {
                match extension.semver() {
                    Ok(installed_version) => {
                        if !constraint.matches(&installed_version) {
                            errors.push(format!(
                                "'{}' requires {}, but {} is installed",
                                extension_id, constraint, installed_version
                            ));
                            hints.push(format!(
                                "Run `homeboy extension update {}` to get the latest version",
                                extension_id
                            ));
                        }
                    }
                    Err(_) => {
                        errors.push(format!(
                            "Extension '{}' has invalid version '{}'",
                            extension_id, extension.version
                        ));
                    }
                }
            }
            Err(_) => {
                errors.push(format!("Extension '{}' is not installed", extension_id));
                hints.push(format!(
                    "homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id {}",
                    extension_id
                ));
            }
        }
    }

    if errors.is_empty() {
        return Ok(());
    }

    let message = if errors.len() == 1 {
        format!(
            "Component '{}' has an unsatisfied extension requirement: {}",
            component.id, errors[0]
        )
    } else {
        format!(
            "Component '{}' has {} unsatisfied extension requirements:\n  - {}",
            component.id,
            errors.len(),
            errors.join("\n  - ")
        )
    };

    let mut err = crate::error::Error::new(
        crate::error::ErrorCode::ExtensionNotFound,
        message,
        serde_json::json!({
            "component_id": component.id,
            "unsatisfied": errors,
        }),
    );

    for hint in &hints {
        err = err.with_hint(hint.to_string());
    }

    Err(err)
}

/// Check if any of the component's linked extensions provide build configuration.
/// When true, the component's explicit build_command becomes optional.
pub fn extension_provides_build(component: &crate::component::Component) -> bool {
    let extensions = match &component.extensions {
        Some(m) => m,
        None => return false,
    };

    for extension_id in extensions.keys() {
        if let Ok(extension) = load_extension(extension_id) {
            if extension.has_build() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Component, ScopedExtensionConfig};
    use std::collections::HashMap;

    #[test]
    fn validate_required_extensions_passes_with_no_modules() {
        let comp = Component {
            id: "test-component".to_string(),
            ..Default::default()
        };
        assert!(validate_required_extensions(&comp).is_ok());
    }

    #[test]
    fn validate_required_extensions_passes_with_empty_modules() {
        let comp = Component {
            id: "test-component".to_string(),
            extensions: Some(HashMap::new()),
            ..Default::default()
        };
        assert!(validate_required_extensions(&comp).is_ok());
    }

    #[test]
    fn validate_required_extensions_fails_with_missing_module() {
        let mut extensions = HashMap::new();
        extensions.insert(
            "nonexistent-extension-abc123".to_string(),
            ScopedExtensionConfig::default(),
        );
        let comp = Component {
            id: "test-component".to_string(),
            extensions: Some(extensions),
            ..Default::default()
        };
        let err = validate_required_extensions(&comp).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ExtensionNotFound);
        assert!(err.message.contains("nonexistent-extension-abc123"));
        assert!(err.message.contains("test-component"));
        // Should have install hint + browse hint
        assert!(err.hints.len() >= 2);
        assert!(err.hints.iter().any(|h| h.message.contains("homeboy extension install")));
        assert!(err
            .hints
            .iter()
            .any(|h| h.message.contains("homeboy-extensions")));
    }

    #[test]
    fn validate_required_extensions_reports_all_missing() {
        let mut extensions = HashMap::new();
        extensions.insert(
            "missing-mod-a".to_string(),
            ScopedExtensionConfig::default(),
        );
        extensions.insert(
            "missing-mod-b".to_string(),
            ScopedExtensionConfig::default(),
        );
        let comp = Component {
            id: "multi-dep".to_string(),
            extensions: Some(extensions),
            ..Default::default()
        };
        let err = validate_required_extensions(&comp).unwrap_err();
        // Error should mention both missing extensions
        assert!(err.message.contains("missing-mod-a"));
        assert!(err.message.contains("missing-mod-b"));
        // Should have install hint for each + browse hint
        assert!(err.hints.len() >= 3);
    }
}
