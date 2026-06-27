use crate::core::error::Result;

use super::registry::load_extension;
use super::version;

/// Validate that all extensions declared in a component's `extensions` field are installed.
///
/// If `component.extensions` contains linked extension IDs, those extensions
/// are implicitly required. Returns an actionable error with install commands
/// when any are missing.
pub fn validate_required_extensions(component: &crate::core::component::Component) -> Result<()> {
    let extensions = match &component.extensions {
        Some(m) if !m.is_empty() => m,
        _ => return Ok(()),
    };

    let mut missing: Vec<(&String, &crate::core::component::ScopedExtensionConfig)> = Vec::new();
    for (extension_id, ext_config) in extensions {
        if load_extension(extension_id).is_err() {
            missing.push((extension_id, ext_config));
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    missing.sort_by(|(left, _), (right, _)| left.cmp(right));

    let missing_ids: Vec<String> = missing.iter().map(|(id, _)| (*id).clone()).collect();
    let extension_list = missing_ids.join(", ");
    let install_hints: Vec<String> = missing
        .iter()
        .map(|(id, ext_config)| extension_install_hint(id, ext_config))
        .collect();

    let message = if missing.len() == 1 {
        format!(
            "Component '{}' requires extension '{}' which is not installed",
            component.id, missing_ids[0]
        )
    } else {
        format!(
            "Component '{}' requires extensions not installed: {}",
            component.id, extension_list
        )
    };

    let mut err = crate::core::error::Error::new(
        crate::core::error::ErrorCode::ExtensionNotFound,
        message,
        serde_json::json!({
            "component_id": component.id,
            "missing_extensions": missing_ids,
        }),
    );

    for hint in &install_hints {
        err = err.with_hint(hint.to_string());
    }

    err = err.with_hint(
        "Provide an extension source with `homeboy extension install <source> --id <extension-id>` or add `source`/`source_url` to the component extension settings."
            .to_string(),
    );

    Err(err)
}

/// Validate that all extensions declared in a component's `extensions` field are installed
/// and satisfy the declared version constraints.
///
/// Returns an actionable error listing every unsatisfied requirement with install/update hints.
pub fn validate_extension_requirements(
    component: &crate::core::component::Component,
) -> Result<()> {
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
            Ok(extension) => match extension.semver() {
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
            },
            Err(_) => {
                errors.push(format!("Extension '{}' is not installed", extension_id));
                hints.push(extension_install_hint(extension_id, ext_config));
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

    let mut err = crate::core::error::Error::new(
        crate::core::error::ErrorCode::ExtensionNotFound,
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

fn extension_install_hint(
    extension_id: &str,
    ext_config: &crate::core::component::ScopedExtensionConfig,
) -> String {
    match extension_source(ext_config) {
        Some(source) => format!("homeboy extension install {} --id {}", source, extension_id),
        None => format!(
            "homeboy extension install <source> --id {} (declare `source` or `source_url` in this component's extension settings to make this command exact)",
            extension_id
        ),
    }
}

fn extension_source(ext_config: &crate::core::component::ScopedExtensionConfig) -> Option<&str> {
    ["source", "source_url", "install_source"]
        .iter()
        .find_map(|key| {
            ext_config
                .settings
                .get(*key)
                .and_then(|value| value.as_str())
        })
        .filter(|value| !value.trim().is_empty())
}

/// Check if any of the component's linked extensions provide build configuration.
pub fn extension_provides_build(component: &crate::core::component::Component) -> bool {
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
    use crate::core::component::Component;

    #[test]
    fn test_validate_required_extensions() {
        let component = Component {
            id: "plain".to_string(),
            ..Default::default()
        };

        assert!(validate_required_extensions(&component).is_ok());
    }

    #[test]
    fn test_validate_extension_requirements() {
        let component = Component {
            id: "plain".to_string(),
            ..Default::default()
        };

        assert!(validate_extension_requirements(&component).is_ok());
    }

    #[test]
    fn test_extension_provides_build() {
        let component = Component {
            id: "plain".to_string(),
            ..Default::default()
        };

        assert!(!extension_provides_build(&component));
    }
}
