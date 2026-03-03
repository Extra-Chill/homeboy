//! Config health checks for components.
//!
//! Identifies configuration drift: broken paths, dead version targets,
//! unused extension links, and other config-level issues that accumulate
//! as projects evolve.

use std::path::Path;

use serde::Serialize;

use crate::component::Component;
use crate::extension;
use crate::version;

/// Severity of a config issue.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum IssueSeverity {
    /// Will cause command failures.
    Error,
    /// Something is off but not blocking.
    Warning,
    /// Informational — could be cleaned up.
    Info,
}

/// A single config health issue found on a component.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigIssue {
    pub severity: IssueSeverity,
    pub category: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_hint: Option<String>,
}

/// Run all config health checks on a component. Returns a list of issues found.
pub fn check_config(component: &Component) -> Vec<ConfigIssue> {
    let mut issues = Vec::new();

    check_local_path(component, &mut issues);
    check_remote_path(component, &mut issues);
    check_version_targets(component, &mut issues);
    check_extensions(component, &mut issues);

    issues
}

/// Check if local_path exists and is absolute.
fn check_local_path(component: &Component, issues: &mut Vec<ConfigIssue>) {
    if component.local_path.is_empty() {
        issues.push(ConfigIssue {
            severity: IssueSeverity::Error,
            category: "local_path".to_string(),
            message: "local_path is empty.".to_string(),
            fix_hint: Some(format!(
                "homeboy component set {} --local-path \"/path/to/component\"",
                component.id
            )),
        });
        return;
    }

    let expanded = shellexpand::tilde(&component.local_path);
    let path = Path::new(expanded.as_ref());

    if !path.is_absolute() {
        issues.push(ConfigIssue {
            severity: IssueSeverity::Error,
            category: "local_path".to_string(),
            message: format!(
                "local_path '{}' is relative. Must be absolute.",
                component.local_path
            ),
            fix_hint: Some(format!(
                "homeboy component set {} --local-path \"/absolute/path/to/{}\"",
                component.id, component.local_path
            )),
        });
        return;
    }

    if !path.exists() {
        issues.push(ConfigIssue {
            severity: IssueSeverity::Error,
            category: "local_path".to_string(),
            message: format!("local_path does not exist: {}", path.display()),
            fix_hint: Some(format!(
                "homeboy component set {} --local-path \"/correct/path\"",
                component.id
            )),
        });
    }
}

/// Check if remote_path looks configured.
fn check_remote_path(component: &Component, issues: &mut Vec<ConfigIssue>) {
    if component.remote_path.is_empty() {
        issues.push(ConfigIssue {
            severity: IssueSeverity::Info,
            category: "remote_path".to_string(),
            message: "remote_path is empty. Deploy will not work.".to_string(),
            fix_hint: Some(format!(
                "homeboy component set {} --remote-path \"server:/path/to/deploy\"",
                component.id
            )),
        });
    }
}

/// Check that version targets point to real files with parseable versions.
fn check_version_targets(component: &Component, issues: &mut Vec<ConfigIssue>) {
    let targets = match &component.version_targets {
        Some(t) if !t.is_empty() => t,
        _ => return, // No version targets configured — not an issue by itself
    };

    // Only check if local_path exists (avoid cascading errors)
    let expanded = shellexpand::tilde(&component.local_path);
    let base = Path::new(expanded.as_ref());
    if !base.exists() {
        return;
    }

    for target in targets {
        let file_path = base.join(&target.file);

        if !file_path.exists() {
            issues.push(ConfigIssue {
                severity: IssueSeverity::Error,
                category: "version_targets".to_string(),
                message: format!(
                    "Version target file '{}' does not exist at {}",
                    target.file,
                    file_path.display()
                ),
                fix_hint: Some(format!(
                    "homeboy component set {} --replace version_targets --version-target \"correct-file.php::pattern\"",
                    component.id
                )),
            });
            continue;
        }

        // Try to read version using configured or extension-default pattern
        let pattern = target
            .pattern
            .clone()
            .or_else(|| version::default_pattern_for_file(&target.file));

        if let Some(ref pat) = pattern {
            let content = match std::fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if version::parse_version(&content, pat).is_none() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    category: "version_targets".to_string(),
                    message: format!(
                        "Version target '{}' exists but pattern '{}' doesn't match any version.",
                        target.file, pat
                    ),
                    fix_hint: Some(format!(
                        "Check pattern syntax. Test with: homeboy version read {}",
                        component.id
                    )),
                });
            }
        } else {
            issues.push(ConfigIssue {
                severity: IssueSeverity::Warning,
                category: "version_targets".to_string(),
                message: format!(
                    "Version target '{}' has no pattern and no extension provides a default for this file type.",
                    target.file
                ),
                fix_hint: Some(format!(
                    "homeboy component set {} --replace version_targets --version-target \"{}::Version:\\\\s*(\\\\d+\\\\.\\\\d+\\\\.\\\\d+)\"",
                    component.id, target.file
                )),
            });
        }
    }
}

/// Check that linked extensions exist and have capabilities configured.
fn check_extensions(component: &Component, issues: &mut Vec<ConfigIssue>) {
    let extensions = match &component.extensions {
        Some(m) => m,
        None => return,
    };

    for extension_id in extensions.keys() {
        match extension::load_extension(extension_id) {
            Ok(manifest) => {
                // Extension exists — check if it provides any useful capabilities
                let has_build = manifest.has_build();
                let has_lint = manifest.has_lint();
                let has_test = manifest.has_test();
                let has_cli = manifest.has_cli();

                if !has_build && !has_lint && !has_test && !has_cli {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Info,
                        category: "extensions".to_string(),
                        message: format!(
                            "Extension '{}' is linked but has no build, lint, test, or CLI capabilities.",
                            extension_id
                        ),
                        fix_hint: None,
                    });
                }
            }
            Err(_) => {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    category: "extensions".to_string(),
                    message: format!(
                        "Extension '{}' is linked but could not be loaded. Extension may be missing or malformed.",
                        extension_id
                    ),
                    fix_hint: Some(format!(
                        "Check installed extensions: homeboy extension list\nRemove dead link: homeboy component set {} --json '{{\"extensions\": null}}'",
                        component.id
                    )),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Component;

    fn make_component(id: &str, local_path: &str) -> Component {
        Component::new(id.to_string(), local_path.to_string(), String::new(), None)
    }

    #[test]
    fn empty_local_path_is_error() {
        let comp = make_component("test", "");
        let issues = check_config(&comp);
        assert!(issues.iter().any(|i| i.category == "local_path"
            && i.severity == IssueSeverity::Error
            && i.message.contains("empty")));
    }

    #[test]
    fn relative_local_path_is_error() {
        let comp = make_component("test", "relative/path");
        let issues = check_config(&comp);
        assert!(issues.iter().any(|i| i.category == "local_path"
            && i.severity == IssueSeverity::Error
            && i.message.contains("relative")));
    }

    #[test]
    fn nonexistent_local_path_is_error() {
        let comp = make_component("test", "/definitely/does/not/exist/abc123");
        let issues = check_config(&comp);
        assert!(issues.iter().any(|i| i.category == "local_path"
            && i.severity == IssueSeverity::Error
            && i.message.contains("does not exist")));
    }

    #[test]
    fn empty_remote_path_is_info() {
        let comp = make_component("test", "/tmp");
        let issues = check_config(&comp);
        assert!(issues
            .iter()
            .any(|i| i.category == "remote_path" && i.severity == IssueSeverity::Info));
    }

    #[test]
    fn missing_version_target_file_is_error() {
        use crate::component::VersionTarget;
        let mut comp = make_component("test", "/tmp");
        comp.version_targets = Some(vec![VersionTarget {
            file: "nonexistent-file.php".to_string(),
            pattern: Some(r"Version:\s*(\d+\.\d+\.\d+)".to_string()),
        }]);
        let issues = check_config(&comp);
        assert!(issues.iter().any(|i| i.category == "version_targets"
            && i.severity == IssueSeverity::Error
            && i.message.contains("does not exist")));
    }

    #[test]
    fn missing_extension_is_error() {
        use crate::component::ScopedExtensionConfig;
        use std::collections::HashMap;
        let mut comp = make_component("test", "/tmp");
        let mut extensions = HashMap::new();
        extensions.insert(
            "nonexistent-extension-xyz".to_string(),
            ScopedExtensionConfig::default(),
        );
        comp.extensions = Some(extensions);
        let issues = check_config(&comp);
        assert!(issues.iter().any(|i| i.category == "extensions"
            && i.severity == IssueSeverity::Error
            && i.message.contains("could not be loaded")));
    }

    #[test]
    fn valid_component_has_minimal_issues() {
        // /tmp exists and is absolute, so local_path checks pass.
        // Only remote_path (empty) should be flagged as info.
        let comp = make_component("test", "/tmp");
        let issues = check_config(&comp);
        // Should only have the empty remote_path info
        assert!(issues.iter().all(|i| i.severity == IssueSeverity::Info));
    }
}
