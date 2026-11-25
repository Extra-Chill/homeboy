//! The single component-version read spine (#11144).
//!
//! `read_component_version`, `validate_version_targets_at`, and
//! `bump_component_version_with_changelog` all read a component's
//! `versionTargets` config. Everything that reads a declared version target goes through
//! `require_targets` / [`declared_targets`] (the config read) and
//! `read_target` (the file read). Where the callers genuinely differ they
//! differ *after* the read, on the [`TargetRead`] they get back:
//!
//! - `read` and the bump path treat a non-matching primary target as a hard
//!   error with a file preview ([`TargetRead::parse_error`]);
//! - `validate_version_targets` treats it as a terse "Could not find version
//!   in <file>";
//! - `read`'s non-primary targets treat it as a warning, not an error.
//!
//! That divergence is a property of the caller, not of the read, so it stays
//! at the call site instead of being encoded as three readers.

use homeboy_core::component::{self, Component, VersionTarget};
use homeboy_core::engine::local_files;
use homeboy_core::engine::text;
use homeboy_core::error::{Error, Result};
use std::path::Path;

use super::default_pattern_for_file::{
    build_version_parse_error, parse_versions, resolve_target_pattern, resolve_version_file_path,
};
use super::types::{ComponentVersionInfo, ComponentVersionSnapshot, VersionTargetInfo};

/// One version target, read from disk.
///
/// `versions` may be empty: the pattern compiled and the file was readable but
/// nothing matched. That is an error for some callers and a warning for
/// others, so this type reports it rather than deciding it.
#[derive(Debug, Clone)]
pub struct TargetRead {
    /// The target's configured file, as written in `versionTargets`.
    pub file: String,
    /// The resolved (and normalized) pattern actually used to parse.
    pub pattern: String,
    /// The absolute path the pattern was applied to.
    pub full_path: String,
    /// File contents, retained so a parse failure can render a preview.
    pub content: String,
    /// Every version the pattern matched, in file order.
    pub versions: Vec<String>,
}

impl TargetRead {
    /// The pattern compiled and the file was read, but nothing matched.
    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// The detailed "could not parse version" error, with hints and a preview.
    pub fn parse_error(&self) -> Error {
        build_version_parse_error(&self.file, &self.pattern, &self.content)
    }

    /// The single version this target declares, rejecting a file that declares
    /// more than one distinct version.
    pub fn require_identical(&self) -> Result<String> {
        text::require_identical(&self.versions, &self.file)
    }

    /// Project into the reportable target info callers return to operators.
    pub fn into_target_info(self, warning: Option<String>) -> VersionTargetInfo {
        VersionTargetInfo {
            match_count: self.versions.len(),
            file: self.file,
            pattern: self.pattern,
            full_path: self.full_path,
            warning,
        }
    }
}

/// Read a component's declared version targets without requiring them.
///
/// - no `versionTargets` key → `Ok(None)` (the component is unversioned)
/// - `versionTargets: []` → hard error; an explicitly empty list is a
///   misconfiguration, not "unversioned"
///
/// Deliberately does **not** validate `local_path`: callers that tolerate an
/// absent config (`validate_component_versions` sweeps every component in a
/// deploy) must not fail on a component they were going to skip anyway.
pub fn declared_targets(component: &Component) -> Result<Option<&[VersionTarget]>> {
    let Some(targets) = component.version_targets.as_deref() else {
        return Ok(None);
    };

    if targets.is_empty() {
        return Err(Error::config_invalid_value(
            "versionTargets",
            None,
            format!("Component '{}' has empty versionTargets", component.id),
        ));
    }

    Ok(Some(targets))
}

/// Read a component's version targets, requiring them.
///
/// The canonical entry point for every path that is about to read or write a
/// version: validates `local_path` before any file operation, then requires a
/// non-empty `versionTargets`. The returned slice is guaranteed non-empty, so
/// callers may index `[0]` for the primary target.
pub(crate) fn require_targets(component: &Component) -> Result<&[VersionTarget]> {
    // Validate local_path is absolute and exists before any file operations.
    component::validate_local_path(component)?;

    declared_targets(component)?
        .ok_or_else(|| Error::config_missing_key("versionTargets", Some(component.id.clone())))
}

/// Read one version target from disk.
///
/// Resolves the pattern (explicit, else the extension default) and the file
/// path, reads the file, and parses every match. An unusable regex is an
/// error; zero matches is not — see [`TargetRead::is_empty`].
pub(crate) fn read_target(local_path: &str, target: &VersionTarget) -> Result<TargetRead> {
    let pattern = resolve_target_pattern(target)?;
    let full_path = resolve_version_file_path(local_path, &target.file);
    let content = local_files::local().read(Path::new(&full_path))?;

    let versions = parse_versions(&content, &pattern).ok_or_else(|| {
        Error::validation_invalid_argument(
            "versionPattern",
            format!("Invalid version regex pattern '{}'", pattern),
            None,
            Some(vec![pattern.clone()]),
        )
    })?;

    Ok(TargetRead {
        file: target.file.clone(),
        pattern,
        full_path,
        content,
        versions,
    })
}

/// Read the current version from a component's version targets.
///
/// The primary (first) target decides the version and must match. Remaining
/// targets are reported with a warning when they do not match, which is what
/// makes this usable for status output on a partially configured component.
pub fn read(component: &Component) -> Result<ComponentVersionInfo> {
    let targets = require_targets(component)?;

    let primary = read_target(&component.local_path, &targets[0])?;
    if primary.is_empty() {
        return Err(primary.parse_error());
    }

    let version = primary.require_identical()?;
    let mut target_infos = vec![primary.into_target_info(None)];

    // Add info for all remaining targets
    for target in targets.iter().skip(1) {
        let read = read_target(&component.local_path, target)?;

        let warning = if read.is_empty() {
            homeboy_core::log_status!(
                "warning",
                "Version target {}: pattern '{}' did not match any content",
                read.file,
                read.pattern
            );
            Some(format!(
                "Pattern did not match any content in {}",
                read.file
            ))
        } else {
            None
        };

        target_infos.push(read.into_target_info(warning));
    }

    Ok(ComponentVersionInfo {
        version,
        targets: target_infos,
    })
}

/// Read version by component ID.
/// If component_id is None, returns homeboy binary's own version.
pub fn read_by_id(component_id: Option<&str>) -> Result<ComponentVersionInfo> {
    // If no component_id, return homeboy binary's own version
    let id = match component_id {
        None => {
            let version = homeboy_product_identity::product_version().to_string();
            return Ok(ComponentVersionInfo {
                version,
                targets: vec![],
            });
        }
        Some(id) => id,
    };

    let component = component::load(id)?;
    read(&component)
}

/// Read a component's version as a snapshot carrying the component id.
pub fn read_snapshot(component: &Component) -> Result<ComponentVersionSnapshot> {
    let info = read(component)?;
    Ok(ComponentVersionSnapshot {
        component_id: component.id.clone(),
        version: info.version,
        targets: info.targets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::{
        bump_component_version_with_changelog, validate_component_versions,
        validate_version_targets_at,
    };
    use homeboy_core::error::ErrorCode;
    use std::fs;

    fn component_at(temp_dir: &tempfile::TempDir) -> Component {
        Component {
            id: "test-component".to_string(),
            local_path: temp_dir.path().to_string_lossy().to_string(),
            remote_path: String::new(),
            changelog_target: Some("CHANGELOG.md".to_string()),
            ..Default::default()
        }
    }

    fn package_json_target() -> VersionTarget {
        VersionTarget {
            file: "package.json".to_string(),
            pattern: Some(r#""version"\s*:\s*"([^"]+)""#.to_string()),
            artifact_path: None,
        }
    }

    fn write_package_json(temp_dir: &tempfile::TempDir, version: &str) {
        fs::write(
            temp_dir.path().join("package.json"),
            format!("{{\n  \"version\": \"{version}\"\n}}\n"),
        )
        .unwrap();
    }

    // ========================================================================
    // The config read: one reader, three former call sites
    // ========================================================================

    /// `local_path` is validated *before* the config is inspected, so a
    /// component pointing at a path that does not exist reports that rather
    /// than a confusing missing-key error. All three former readers called
    /// `validate_local_path` first; the shared reader must keep that order.
    #[test]
    fn require_targets_validates_local_path_before_reading_config() {
        let component = Component {
            id: "test-component".to_string(),
            local_path: "/nonexistent/homeboy/version/spine".to_string(),
            version_targets: None,
            ..Default::default()
        };

        let error = require_targets(&component).expect_err("missing local_path");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert!(
            error.message.contains("local_path"),
            "expected a local_path diagnostic, got: {}",
            error.message
        );
    }

    /// Every version read path must produce the *same* missing-key and
    /// empty-list errors, because they share one reader. This is the
    /// regression that made the three copies worth collapsing.
    #[test]
    fn all_version_read_paths_share_missing_and_empty_targets_errors() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cases = [
            (None, ErrorCode::ConfigMissingKey),
            (Some(vec![]), ErrorCode::ConfigInvalidValue),
        ];

        for (version_targets, code) in cases {
            let mut component = component_at(&temp_dir);
            component.version_targets = version_targets;
            let empty = component.version_targets.is_some();

            let mut errors = vec![
                read(&component).expect_err("read requires targets"),
                validate_version_targets_at(&component, "1.0.0")
                    .expect_err("validate requires targets"),
                bump_component_version_with_changelog(&component, "patch", None, None)
                    .expect_err("bump requires targets"),
            ];
            if empty {
                // The deploy-wide sweep skips absent config but rejects `[]`.
                errors.push(
                    validate_component_versions(&[component.clone()])
                        .expect_err("sweep rejects empty"),
                );
            }

            for error in &errors {
                assert_eq!(error.code, code);
                assert_eq!(error.details["key"], "versionTargets");
                if empty {
                    assert_eq!(
                        error.details["problem"],
                        "Component 'test-component' has empty versionTargets"
                    );
                } else {
                    assert_eq!(error.details["path"], "test-component");
                }
            }
        }
    }

    /// The lenient reader is what lets the deploy-wide sweep skip unversioned
    /// components. It must not borrow the strict reader's missing-key error,
    /// and must not touch `local_path`.
    #[test]
    fn declared_targets_treats_absent_config_as_unversioned() {
        let component = Component {
            id: "test-component".to_string(),
            local_path: "/nonexistent/homeboy/version/spine".to_string(),
            ..Default::default()
        };

        assert!(declared_targets(&component)
            .expect("absent config is not an error")
            .is_none());
        // The sweep over the same component is a no-op, not a failure.
        validate_component_versions(&[component]).expect("unversioned components are skipped");
    }

    // ========================================================================
    // The file read
    // ========================================================================

    #[test]
    fn read_target_rejects_an_uncompilable_pattern() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_package_json(&temp_dir, "1.2.3");

        let target = VersionTarget {
            file: "package.json".to_string(),
            pattern: Some("([0-9".to_string()),
            artifact_path: None,
        };
        let error =
            read_target(&temp_dir.path().to_string_lossy(), &target).expect_err("bad pattern");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert!(error.message.contains("Invalid version regex pattern"));
    }

    // ========================================================================
    // Caller-specific behavior preserved on top of the shared read
    // ========================================================================

    #[test]
    fn read_reports_primary_version_and_warns_on_non_matching_secondary() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_package_json(&temp_dir, "1.2.3");
        fs::write(temp_dir.path().join("plugin.php"), "<?php\n// nothing\n").unwrap();

        let mut component = component_at(&temp_dir);
        component.version_targets = Some(vec![
            package_json_target(),
            VersionTarget {
                file: "plugin.php".to_string(),
                pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
                artifact_path: None,
            },
        ]);

        let info = read(&component).expect("primary target decides the version");
        assert_eq!(info.version, "1.2.3");
        assert_eq!(info.targets.len(), 2);
        assert_eq!(info.targets[0].match_count, 1);
        assert!(info.targets[0].warning.is_none());
        assert_eq!(info.targets[1].match_count, 0);
        assert_eq!(
            info.targets[1].warning.as_deref(),
            Some("Pattern did not match any content in plugin.php")
        );
    }

    /// A non-matching primary target hard-fails on every path, but `read` and
    /// the bump path keep the detailed preview error while
    /// `validate_version_targets_at` keeps its terse diagnostic. That
    /// divergence is deliberate; the shared reader must not flatten it.
    #[test]
    fn non_matching_primary_target_keeps_each_callers_diagnostic() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::write(
            temp_dir.path().join("package.json"),
            "{\n  \"name\": \"no-version-here\"\n}\n",
        )
        .unwrap();

        let mut component = component_at(&temp_dir);
        component.version_targets = Some(vec![package_json_target()]);

        for error in [
            read(&component).expect_err("read: primary target must match"),
            bump_component_version_with_changelog(&component, "patch", None, None)
                .expect_err("bump: primary target must match"),
        ] {
            assert!(
                error
                    .message
                    .contains("Could not parse version from package.json"),
                "got: {}",
                error.message
            );
            assert!(
                error.message.contains("no-version-here"),
                "parse error must include a file preview, got: {}",
                error.message
            );
        }

        let error =
            validate_version_targets_at(&component, "1.2.3").expect_err("target must match");
        assert_eq!(error.message, "Could not find version in package.json");
    }

    #[test]
    fn validate_version_targets_at_requires_every_target_at_the_expected_version() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_package_json(&temp_dir, "1.2.3");
        fs::write(
            temp_dir.path().join("plugin.php"),
            "<?php\n/* Version: 1.2.3 */\n",
        )
        .unwrap();

        let mut component = component_at(&temp_dir);
        component.version_targets = Some(vec![
            package_json_target(),
            VersionTarget {
                file: "plugin.php".to_string(),
                pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
                artifact_path: None,
            },
        ]);

        let infos = validate_version_targets_at(&component, "1.2.3").expect("targets agree");
        assert_eq!(infos.len(), 2);
        assert!(infos.iter().all(|info| info.match_count == 1));
        assert!(infos.iter().all(|info| info.warning.is_none()));

        let error = validate_version_targets_at(&component, "9.9.9").expect_err("version mismatch");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert!(
            error.message.contains("Version mismatch in package.json"),
            "got: {}",
            error.message
        );
    }
}
