//! The single component-version read spine (#11144).
//!
//! Three call paths used to read a component's `versionTargets` config
//! independently — `read_component_version`, `validate_version_targets_at`, and
//! `bump_component_version_with_changelog` — each repeating the same
//! local-path validation, the same `config_missing_key("versionTargets", ..)`,
//! and the same empty-targets rejection, and two of them additionally
//! repeating the same "resolve pattern, resolve path, read file, parse
//! versions" block. Three copies meant three places for the contract to drift
//! silently.
//!
//! Everything that reads a declared version target now goes through
//! [`require_targets`] / [`declared_targets`] (the config read) and
//! [`read_target`] (the file read). Where the callers genuinely differ they
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
pub fn require_targets(component: &Component) -> Result<&[VersionTarget]> {
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
pub fn read_target(local_path: &str, target: &VersionTarget) -> Result<TargetRead> {
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

    #[test]
    fn require_targets_rejects_missing_version_targets() {
        let temp_dir = tempfile::tempdir().unwrap();
        let component = component_at(&temp_dir);

        let error = require_targets(&component).expect_err("missing versionTargets");
        assert_eq!(error.code, ErrorCode::ConfigMissingKey);
        assert_eq!(error.details["key"], "versionTargets");
        assert_eq!(error.details["path"], "test-component");
    }

    #[test]
    fn require_targets_rejects_empty_version_targets() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut component = component_at(&temp_dir);
        component.version_targets = Some(vec![]);

        let error = require_targets(&component).expect_err("empty versionTargets");
        assert_eq!(error.code, ErrorCode::ConfigInvalidValue);
        assert_eq!(error.details["key"], "versionTargets");
        assert_eq!(
            error.details["problem"],
            "Component 'test-component' has empty versionTargets"
        );
    }

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

    /// Every former call site must now produce the *same* missing-key error,
    /// because they share one reader. This is the regression that made the
    /// three copies worth collapsing.
    #[test]
    fn all_version_read_paths_share_one_missing_targets_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let component = component_at(&temp_dir);

        let from_read = read(&component).expect_err("read requires targets");
        let from_validate = validate_version_targets_at(&component, "1.0.0")
            .expect_err("validate requires targets");
        let from_bump = bump_component_version_with_changelog(&component, "patch", None, None)
            .expect_err("bump requires targets");

        for error in [&from_read, &from_validate, &from_bump] {
            assert_eq!(error.code, ErrorCode::ConfigMissingKey);
            assert_eq!(error.details["key"], "versionTargets");
            assert_eq!(error.details["path"], "test-component");
        }
    }

    /// Same for the empty-list rejection.
    #[test]
    fn all_version_read_paths_share_one_empty_targets_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut component = component_at(&temp_dir);
        component.version_targets = Some(vec![]);

        let from_read = read(&component).expect_err("read rejects empty");
        let from_validate =
            validate_version_targets_at(&component, "1.0.0").expect_err("validate rejects empty");
        let from_bump = bump_component_version_with_changelog(&component, "patch", None, None)
            .expect_err("bump rejects empty");
        let from_sweep =
            validate_component_versions(&[component.clone()]).expect_err("sweep rejects empty");

        for error in [&from_read, &from_validate, &from_bump, &from_sweep] {
            assert_eq!(error.code, ErrorCode::ConfigInvalidValue);
            assert_eq!(error.details["key"], "versionTargets");
            assert_eq!(
                error.details["problem"],
                "Component 'test-component' has empty versionTargets"
            );
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
    fn read_target_reports_resolved_pattern_path_and_matches() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_package_json(&temp_dir, "1.2.3");

        let target = package_json_target();
        let read = read_target(&temp_dir.path().to_string_lossy(), &target).expect("read target");

        assert_eq!(read.file, "package.json");
        assert_eq!(read.versions, vec!["1.2.3".to_string()]);
        assert_eq!(
            read.full_path,
            temp_dir.path().join("package.json").to_string_lossy()
        );
        assert!(read.content.contains("1.2.3"));
        assert!(!read.is_empty());
        assert_eq!(read.require_identical().unwrap(), "1.2.3");
    }

    /// A pattern that compiles but matches nothing is not a read failure — the
    /// callers decide. Keeping that decision out of the reader is what allows
    /// one reader to serve a hard-error caller and a warn-only caller.
    #[test]
    fn read_target_reports_no_matches_without_failing() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::write(temp_dir.path().join("VERSION"), "no version here\n").unwrap();

        let target = VersionTarget {
            file: "VERSION".to_string(),
            pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
            artifact_path: None,
        };
        let read = read_target(&temp_dir.path().to_string_lossy(), &target).expect("read target");

        assert!(read.is_empty());
        let error = read.parse_error();
        assert!(
            error
                .message
                .contains("Could not parse version from VERSION"),
            "expected the detailed parse error, got: {}",
            error.message
        );
        assert!(
            error.message.contains("no version here"),
            "parse error must include a file preview, got: {}",
            error.message
        );
    }

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

    /// The primary target is the one that hard-fails, with the detailed
    /// preview error rather than the terse one.
    #[test]
    fn read_fails_with_a_preview_when_the_primary_target_does_not_match() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::write(
            temp_dir.path().join("package.json"),
            "{\n  \"name\": \"x\"\n}\n",
        )
        .unwrap();

        let mut component = component_at(&temp_dir);
        component.version_targets = Some(vec![package_json_target()]);

        let error = read(&component).expect_err("primary target must match");
        assert!(
            error
                .message
                .contains("Could not parse version from package.json"),
            "got: {}",
            error.message
        );
    }

    /// `validate_version_targets_at` keeps its own terse diagnostic for a
    /// non-matching target — that divergence is deliberate, and the shared
    /// reader must not have flattened it into the preview error.
    #[test]
    fn validate_version_targets_at_keeps_its_terse_no_version_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::write(
            temp_dir.path().join("package.json"),
            "{\n  \"name\": \"x\"\n}\n",
        )
        .unwrap();

        let mut component = component_at(&temp_dir);
        component.version_targets = Some(vec![package_json_target()]);

        let error =
            validate_version_targets_at(&component, "1.2.3").expect_err("target must match");
        assert_eq!(
            error.message, "Could not find version in package.json",
            "the validate path keeps its terse diagnostic"
        );
    }

    #[test]
    fn validate_version_targets_at_reports_every_target_when_all_agree() {
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
    }

    #[test]
    fn validate_version_targets_at_rejects_a_target_at_a_different_version() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_package_json(&temp_dir, "1.2.3");

        let mut component = component_at(&temp_dir);
        component.version_targets = Some(vec![package_json_target()]);

        let error = validate_version_targets_at(&component, "9.9.9").expect_err("version mismatch");
        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert!(
            error.message.contains("Version mismatch in package.json"),
            "got: {}",
            error.message
        );
    }

    /// The bump path reads the primary target through the same reader, so the
    /// version it bumps from is the version `read` reports.
    #[test]
    fn bump_reads_the_same_primary_version_the_shared_reader_reports() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_package_json(&temp_dir, "0.1.0");
        fs::write(
            temp_dir.path().join("CHANGELOG.md"),
            "# Changelog\n\n## Unreleased\n\n",
        )
        .unwrap();

        let mut component = component_at(&temp_dir);
        component.version_targets = Some(vec![package_json_target()]);

        let observed = read(&component).expect("read version").version;

        let mut entries: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        entries.insert("fixed".to_string(), vec!["shared version read".to_string()]);

        let result =
            bump_component_version_with_changelog(&component, "patch", Some(&entries), None)
                .expect("bump from the shared read");

        assert_eq!(result.old_version, observed);
        assert_eq!(result.new_version, "0.1.1");
    }

    /// The bump path also keeps the detailed preview error on a non-matching
    /// primary target, and must not have picked up the validate path's terse
    /// one when the two were merged.
    #[test]
    fn bump_keeps_the_detailed_parse_error_on_a_non_matching_primary() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::write(
            temp_dir.path().join("package.json"),
            "{\n  \"name\": \"x\"\n}\n",
        )
        .unwrap();

        let mut component = component_at(&temp_dir);
        component.version_targets = Some(vec![package_json_target()]);

        let error = bump_component_version_with_changelog(&component, "patch", None, None)
            .expect_err("primary target must match");
        assert!(
            error
                .message
                .contains("Could not parse version from package.json"),
            "got: {}",
            error.message
        );
    }
}
