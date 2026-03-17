mod default_pattern_for_file;
mod types;
mod version;

pub use default_pattern_for_file::*;
pub use types::*;
pub use version::*;

use crate::component::{self, Component, VersionTarget};
use crate::config::{from_str, set_json_pointer, to_string_pretty};
use crate::engine::hooks::{self, HookFailureMode};
use crate::engine::local_files::{self, FileSystem};
use crate::engine::text;
use crate::error::{Error, Result};
use crate::extension::ExtensionManifest;
use crate::release::changelog;
use serde_json::Value;
use std::path::Path;

pub(crate) fn replace_versions(
    content: &str,
    pattern: &str,
    new_version: &str,
) -> Option<(String, usize)> {
    text::replace_all(content, pattern, new_version)
}

fn find_version_pattern_in_extension(
    extension: &ExtensionManifest,
    filename: &str,
) -> Option<String> {
    for vp in extension.version_patterns() {
        if filename.ends_with(&vp.extension) {
            return Some(vp.pattern.clone());
        }
    }
    None
}

/// Update version in a file, handling both JSON and text-based version files.
/// Returns the number of replacements made.
pub(crate) fn update_version_in_file(
    path: &str,
    pattern: &str,
    old_version: &str,
    new_version: &str,
) -> Result<usize> {
    // JSON files with default pattern use structured update
    if Path::new(path).extension().is_some_and(|ext| ext == "json")
        && default_pattern_for_file(path).as_deref() == Some(pattern)
    {
        let content = local_files::local().read(Path::new(path))?;
        let mut json: Value = from_str(&content)?;
        let Some(current) = json.get("version").and_then(|v: &Value| v.as_str()) else {
            return Err(Error::config_missing_key("version", Some(path.to_string())));
        };

        if current != old_version {
            return Err(Error::internal_unexpected(format!(
                "Version mismatch in {}: found {}, expected {}",
                path, current, old_version
            )));
        }

        set_json_pointer(
            &mut json,
            "/version",
            serde_json::Value::String(new_version.to_string()),
        )?;
        let output = to_string_pretty(&json)?;
        local_files::local().write(Path::new(path), &output)?;
        return Ok(1);
    }

    // Text files use regex replacement
    let content = local_files::read_file(Path::new(path), "read version file")?;

    let versions = parse_versions(&content, pattern).ok_or_else(|| {
        Error::validation_invalid_argument(
            "versionPattern",
            format!("Invalid version regex pattern '{}'", pattern),
            None,
            Some(vec![pattern.to_string()]),
        )
    })?;

    if versions.is_empty() {
        return Err(Error::internal_unexpected(format!(
            "Could not find version in {}",
            path
        )));
    }

    // Validate all found versions match expected
    for v in &versions {
        if v != old_version {
            return Err(Error::internal_unexpected(format!(
                "Version mismatch in {}: found {}, expected {}",
                path, v, old_version
            )));
        }
    }

    let (new_content, replaced_count) = replace_versions(&content, pattern, new_version)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "versionPattern",
                format!("Invalid version regex pattern '{}'", pattern),
                None,
                Some(vec![pattern.to_string()]),
            )
        })?;

    local_files::write_file(Path::new(path), &new_content, "write version file")?;

    Ok(replaced_count)
}

/// Read version from a local file for a component's version target.
/// Returns None if file doesn't exist or version can't be parsed.
pub(crate) fn read_local_version(
    local_path: &str,
    version_target: &VersionTarget,
) -> Option<String> {
    let path = resolve_version_file_path(local_path, &version_target.file);
    let content = local_files::local().read(Path::new(&path)).ok()?;

    let pattern: String = version_target
        .pattern
        .clone()
        .or_else(|| default_pattern_for_file(&version_target.file))?;

    parse_version(&content, &pattern)
}

/// Pre-validate all version targets match the expected version.
/// This is a read-only operation that ensures all targets are in sync
/// BEFORE any file modifications (like changelog finalization) occur.
fn pre_validate_version_targets(
    targets: &[VersionTarget],
    local_path: &str,
    expected_version: &str,
) -> Result<Vec<VersionTargetInfo>> {
    let mut target_infos = Vec::new();

    for target in targets {
        let version_pattern = resolve_target_pattern(target)?;
        let full_path = resolve_version_file_path(local_path, &target.file);
        let content = local_files::local().read(Path::new(&full_path))?;

        let versions = parse_versions(&content, &version_pattern).ok_or_else(|| {
            Error::validation_invalid_argument(
                "versionPattern",
                format!("Invalid version regex pattern '{}'", version_pattern),
                None,
                Some(vec![version_pattern.clone()]),
            )
        })?;

        if versions.is_empty() {
            return Err(Error::internal_unexpected(format!(
                "Could not find version in {}",
                target.file
            )));
        }

        // Validate all versions in this file match expected
        let found = text::require_identical(&versions, &target.file)?;
        if found != expected_version {
            return Err(Error::internal_unexpected(format!(
                "Version mismatch in {}: found {}, expected {}",
                target.file, found, expected_version
            )));
        }

        target_infos.push(VersionTargetInfo {
            file: target.file.clone(),
            pattern: version_pattern,
            full_path,
            match_count: versions.len(),
            warning: None,
        });
    }

    Ok(target_infos)
}

/// Read-only changelog validation for version bump operations.
/// Validates that changelog can be finalized without making any changes.
/// Returns the same validation results as validate_and_finalize_changelog but without modifying files.
pub fn validate_changelog_for_bump(
    component: &Component,
    current_version: &str,
    new_version: &str,
) -> Result<ChangelogValidationResult> {
    let settings = changelog::resolve_effective_settings(Some(component));
    let changelog_path = changelog::resolve_changelog_path(component)?;

    let changelog_content = local_files::local().read(&changelog_path)?;

    let latest_changelog_version = changelog::get_latest_finalized_version(&changelog_content)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "changelog",
                "Changelog has no finalized versions".to_string(),
                None,
                Some(vec![
                    "Add at least one finalized version section like '## [0.1.0] - YYYY-MM-DD'"
                        .to_string(),
                ]),
            )
        })?;

    // Check if changelog is already finalized for the target version
    if latest_changelog_version == new_version {
        return Ok(ChangelogValidationResult {
            changelog_path: changelog_path.to_string_lossy().to_string(),
            changelog_finalized: true,
            changelog_changed: false, // Already finalized, no changes needed
        });
    }

    // Reject if changelog is ahead of files (version gap)
    let changelog_ver = semver::Version::parse(&latest_changelog_version);
    let file_ver = semver::Version::parse(current_version);
    match (changelog_ver, file_ver) {
        (Ok(clv), Ok(fv)) if clv > fv => {
            return Err(Error::validation_invalid_argument(
                "version",
                format!(
                    "Version mismatch: changelog is at {} but files are at {}. Setting version would create a version gap.",
                    latest_changelog_version, current_version
                ),
                None,
                Some(vec![
                    "Ensure changelog and version files are in sync before updating version.".to_string(),
                ]),
            ));
        }
        _ => {}
    }

    let (_, changelog_changed) = changelog::finalize_next_section(
        &changelog_content,
        &settings.next_section_aliases,
        new_version,
        false,
    )?;

    Ok(ChangelogValidationResult {
        changelog_path: changelog_path.to_string_lossy().to_string(),
        changelog_finalized: true,
        changelog_changed,
    })
}

/// Validate and finalize changelog for a version operation.
/// Ensures changelog is in sync with current version and has valid unreleased content.
///
/// When `generated_entries` is provided (from the release pipeline's commit analysis),
/// entries are generated and finalized into a versioned section in a single disk write —
/// no intermediate `## Unreleased` section is ever persisted.
///
/// When `generated_entries` is None (standalone `homeboy version bump`), falls back to
/// finalizing an existing `## Unreleased` section.
pub(crate) fn validate_and_finalize_changelog(
    component: &Component,
    current_version: &str,
    new_version: &str,
    generated_entries: Option<&std::collections::HashMap<String, Vec<String>>>,
) -> Result<ChangelogValidationResult> {
    let settings = changelog::resolve_effective_settings(Some(component));
    let changelog_path = changelog::resolve_changelog_path(component)?;

    let changelog_content = match local_files::local().read(&changelog_path) {
        Ok(content) => content,
        Err(e) => {
            // When the configured changelog_target doesn't exist, search for
            // the file in common locations and suggest a config fix.
            let error_str = e.to_string();
            if error_str.contains("File not found") || error_str.contains("No such file") {
                let mut hints = vec![format!(
                    "Configured changelog_target resolved to: {}",
                    changelog_path.display()
                )];

                let common_locations = [
                    "CHANGELOG.md",
                    "docs/CHANGELOG.md",
                    "changelog.md",
                    "docs/changelog.md",
                    "CHANGES.md",
                ];

                for location in &common_locations {
                    let candidate = std::path::Path::new(&component.local_path).join(location);
                    if candidate.exists() && candidate != changelog_path {
                        hints.push(format!(
                            "Found changelog at {}. Fix with:\n  homeboy component set {} --changelog-target \"{}\"",
                            location, component.id, location
                        ));
                        break;
                    }
                }

                if hints.len() == 1 {
                    // No existing file found — suggest creating one
                    hints.push(format!(
                        "Create a new changelog:\n  homeboy changelog init {} --configure",
                        component.id
                    ));
                }

                return Err(Error::validation_invalid_argument(
                    "changelog",
                    format!("Changelog file not found: {}", changelog_path.display()),
                    None,
                    Some(hints),
                ));
            }
            return Err(e);
        }
    };

    let latest_changelog_version = changelog::get_latest_finalized_version(&changelog_content)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "changelog",
                "Changelog has no finalized versions".to_string(),
                None,
                Some(vec![
                    "Add at least one finalized version section like '## [0.1.0] - YYYY-MM-DD'"
                        .to_string(),
                ]),
            )
        })?;

    // Reject if changelog is ahead of files (version gap)
    let changelog_ver = semver::Version::parse(&latest_changelog_version);
    let file_ver = semver::Version::parse(current_version);
    match (changelog_ver, file_ver) {
        (Ok(clv), Ok(fv)) if clv > fv => {
            return Err(Error::validation_invalid_argument(
                "version",
                format!(
                    "Version mismatch: changelog is at {} but files are at {}. Setting version would create a version gap.",
                    latest_changelog_version, current_version
                ),
                None,
                Some(vec![
                    "Ensure changelog and version files are in sync before updating version.".to_string(),
                ]),
            ));
        }
        _ => {}
    }

    let (finalized_changelog, changelog_changed) = if let Some(entries) = generated_entries {
        // Atomic path: generate entries and finalize into versioned section in one pass.
        // No ## Unreleased section is ever written to disk.
        let entries_ref: std::collections::HashMap<&str, Vec<String>> = entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        changelog::finalize_with_generated_entries(
            &changelog_content,
            &settings.next_section_aliases,
            &entries_ref,
            new_version,
        )?
    } else {
        // Legacy path: finalize an existing ## Unreleased section.
        changelog::finalize_next_section(
            &changelog_content,
            &settings.next_section_aliases,
            new_version,
            false,
        )?
    };

    if changelog_changed {
        local_files::local().write(&changelog_path, &finalized_changelog)?;
    }

    Ok(ChangelogValidationResult {
        changelog_path: changelog_path.to_string_lossy().to_string(),
        changelog_finalized: true,
        changelog_changed,
    })
}

pub fn validate_baseline_alignment(
    version: Option<&ComponentVersionSnapshot>,
    baseline_ref: Option<&str>,
) -> Option<String> {
    let version_snapshot = version?;
    let baseline = baseline_ref?;
    let baseline_version = baseline.strip_prefix('v').unwrap_or(baseline);

    if version_snapshot.version != baseline_version {
        Some(format!(
            "Version mismatch: source files show {} but git baseline is {}. Consider creating a tag or bumping the version.",
            version_snapshot.version, baseline
        ))
    } else {
        None
    }
}

/// Bump a component's version and finalize changelog.
/// bump_type: "patch", "minor", or "major"
pub(crate) fn bump_component_version(
    component: &Component,
    bump_type: &str,
    changelog_entries: Option<&std::collections::HashMap<String, Vec<String>>>,
) -> Result<BumpResult> {
    // Validate local_path is absolute and exists before any file operations
    component::validate_local_path(component)?;

    let targets = component
        .version_targets
        .as_ref()
        .ok_or_else(|| Error::config_missing_key("versionTargets", Some(component.id.clone())))?;

    if targets.is_empty() {
        return Err(Error::config_invalid_value(
            "versionTargets",
            None,
            format!("Component '{}' has empty versionTargets", component.id),
        ));
    }

    // Read current version from primary target
    let primary = &targets[0];
    let primary_pattern = resolve_target_pattern(primary)?;
    let primary_full_path = resolve_version_file_path(&component.local_path, &primary.file);

    let primary_content = local_files::local().read(Path::new(&primary_full_path))?;
    let primary_versions = parse_versions(&primary_content, &primary_pattern).ok_or_else(|| {
        Error::validation_invalid_argument(
            "versionPattern",
            format!("Invalid version regex pattern '{}'", primary_pattern),
            None,
            Some(vec![primary_pattern.clone()]),
        )
    })?;

    if primary_versions.is_empty() {
        return Err(build_version_parse_error(
            &primary.file,
            &primary_pattern,
            &primary_content,
        ));
    }

    let old_version = text::require_identical(&primary_versions, &primary.file)?;
    let new_version = increment_version(&old_version, bump_type).ok_or_else(|| {
        Error::validation_invalid_argument(
            "version",
            format!("Invalid version format: {}", old_version),
            None,
            Some(vec![old_version.clone()]),
        )
    })?;

    // Pre-validate ALL version targets BEFORE any file modifications.
    // This prevents changelog finalization when version files are out of sync.
    let target_infos = pre_validate_version_targets(targets, &component.local_path, &old_version)?;

    // Now safe to finalize changelog - all targets validated
    let changelog_validation =
        validate_and_finalize_changelog(component, &old_version, &new_version, changelog_entries)?;

    // Update all version files (validation already done, just write new versions)
    for info in &target_infos {
        let replaced_count =
            update_version_in_file(&info.full_path, &info.pattern, &old_version, &new_version)?;

        if replaced_count != info.match_count {
            return Err(Error::internal_unexpected(format!(
                "Unexpected replacement count in {}",
                info.file
            )));
        }
    }

    // If any version target is Cargo.toml, regenerate Cargo.lock so it stays in sync.
    // Without this, the release commit only includes Cargo.toml and post-release hooks
    // like `cargo publish` fail because Cargo.lock is dirty.
    let has_cargo_target = target_infos.iter().any(|t| t.file.ends_with("Cargo.toml"));
    if has_cargo_target {
        log_status!(
            "version",
            "Regenerating Cargo.lock after Cargo.toml version bump"
        );
        let lockfile_result = std::process::Command::new("cargo")
            .args(["generate-lockfile"])
            .current_dir(&component.local_path)
            .output();
        if let Err(e) = lockfile_result {
            log_status!("warning", "Failed to regenerate Cargo.lock: {}", e);
        }
    }

    // Replace @since placeholder tags with the new version (extension-driven).
    let since_tags_replaced = replace_since_tag_placeholders(component, &new_version)?;

    // Run lifecycle hooks that may update/stage generated artifacts impacted by the bump.
    // This must happen AFTER version targets are updated so artifacts match the new version.
    hooks::run_hooks(
        component,
        hooks::events::PRE_VERSION_BUMP,
        HookFailureMode::Fatal,
    )?;
    hooks::run_hooks(
        component,
        hooks::events::POST_VERSION_BUMP,
        HookFailureMode::Fatal,
    )?;

    Ok(BumpResult {
        old_version,
        new_version,
        targets: target_infos,
        since_tags_replaced,
        changelog_path: changelog_validation.changelog_path,
        changelog_finalized: changelog_validation.changelog_finalized,
        changelog_changed: changelog_validation.changelog_changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[test]
    fn since_tag_regex_matches_placeholders() {
        let pattern_str = format!(r"@since\s+({})", DEFAULT_SINCE_PLACEHOLDER);
        let regex = Regex::new(&pattern_str).unwrap();

        // Should match placeholders
        assert!(regex.is_match("@since 0.0.0"));
        assert!(regex.is_match("@since NEXT"));
        assert!(regex.is_match("@since TBD"));
        assert!(regex.is_match("@since TODO"));
        assert!(regex.is_match("@since UNRELEASED"));
        assert!(regex.is_match("@since x.x.x"));
        assert!(regex.is_match(" * @since TBD"));
        assert!(regex.is_match(" * @since  NEXT")); // extra space

        // Should NOT match real versions
        assert!(!regex.is_match("@since 1.2.3"));
        assert!(!regex.is_match("@since 0.1.0"));
    }

    #[test]
    fn since_tag_replacement_preserves_context() {
        let pattern_str = format!(r"@since\s+({})", DEFAULT_SINCE_PLACEHOLDER);
        let regex = Regex::new(&pattern_str).unwrap();

        let input = " * @since TBD\n * @since 1.0.0\n * @since NEXT\n";
        let result = regex.replace_all(input, |caps: &regex::Captures| {
            let full = caps.get(0).unwrap().as_str();
            let placeholder = caps.get(1).unwrap().as_str();
            full.replacen(placeholder, "2.0.0", 1)
        });

        assert_eq!(
            result,
            " * @since 2.0.0\n * @since 1.0.0\n * @since 2.0.0\n"
        );
    }
}
