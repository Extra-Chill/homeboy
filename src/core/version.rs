use crate::changelog;
use crate::component::{self, Component, VersionTarget};
use crate::config::{from_str, set_json_pointer, to_string_pretty};
use crate::defaults;
use crate::error::{Error, Result};
use crate::local_files::{self, FileSystem};
use crate::module::{load_all_modules, ModuleManifest};
use crate::ssh::execute_local_command_in_dir;
use crate::utils::{io, parser, validation};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Execute pre-bump commands as part of the version bump pipeline.
///
/// These commands are expected to update and/or stage generated artifacts that may change
/// due to the version bump (e.g., `cargo build` updating Cargo.lock).
///
/// Important: this runs *after* the version/changelog files are updated so generated artifacts
/// can reflect the new version, but *before* the release commit is created.
pub fn run_pre_bump_commands(commands: &[String], working_dir: &str) -> Result<()> {
    if commands.is_empty() {
        return Ok(());
    }

    for command in commands {
        let output = execute_local_command_in_dir(command, Some(working_dir), None);
        if !output.success {
            let error_text = if output.stderr.trim().is_empty() {
                output.stdout
            } else {
                output.stderr
            };
            return Err(Error::internal_unexpected(format!(
                "Pre version bump command failed: {}\n{}",
                command, error_text
            )));
        }
    }

    Ok(())
}

fn run_post_bump_commands(commands: &[String], working_dir: &str) -> Result<()> {
    if commands.is_empty() {
        return Ok(());
    }

    for command in commands {
        let output = execute_local_command_in_dir(command, Some(working_dir), None);
        if !output.success {
            let error_text = if output.stderr.trim().is_empty() {
                output.stdout
            } else {
                output.stderr
            };
            return Err(Error::internal_unexpected(format!(
                "Post version bump command failed: {}\n{}",
                command, error_text
            )));
        }
    }

    Ok(())
}

/// Parse version from content using regex pattern.
/// Pattern must contain a capture group for the version string.
/// Content is trimmed to handle trailing newlines in VERSION files.
pub fn parse_version(content: &str, pattern: &str) -> Option<String> {
    parser::extract_first(content, pattern)
}

/// Parse all versions from content using regex pattern.
/// Content is trimmed to handle trailing newlines in VERSION files.
pub fn parse_versions(content: &str, pattern: &str) -> Option<Vec<String>> {
    parser::extract_all(content, pattern)
}

pub fn replace_versions(
    content: &str,
    pattern: &str,
    new_version: &str,
) -> Option<(String, usize)> {
    parser::replace_all(content, pattern, new_version)
}

/// Get version pattern from module configuration.
/// Returns None if no module defines a pattern for this file type.
pub fn default_pattern_for_file(filename: &str) -> Option<String> {
    for module in load_all_modules().unwrap_or_default() {
        if let Some(pattern) = find_version_pattern_in_module(&module, filename) {
            return Some(pattern);
        }
    }
    None
}

fn find_version_pattern_in_module(module: &ModuleManifest, filename: &str) -> Option<String> {
    for vp in &module.version_patterns {
        if filename.ends_with(&vp.extension) {
            return Some(vp.pattern.clone());
        }
    }
    None
}

/// Increment semver version.
/// bump_type: "patch", "minor", or "major"
pub fn increment_version(version: &str, bump_type: &str) -> Option<String> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    let major: u32 = parts[0].parse().ok()?;
    let minor: u32 = parts[1].parse().ok()?;
    let patch: u32 = parts[2].parse().ok()?;

    let (new_major, new_minor, new_patch) = match bump_type {
        "patch" => (major, minor, patch + 1),
        "minor" => (major, minor + 1, 0),
        "major" => (major + 1, 0, 0),
        _ => return None,
    };

    Some(format!("{}.{}.{}", new_major, new_minor, new_patch))
}

/// Update version in a file, handling both JSON and text-based version files.
/// Returns the number of replacements made.
pub fn update_version_in_file(
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
    let content = io::read_file(Path::new(path), "read version file")?;

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

    io::write_file(Path::new(path), &new_content, "write version file")?;

    Ok(replaced_count)
}

/// Get version string from a component's first version target.
/// Returns None if no version targets configured or version can't be read.
/// Use this for simple version checks (e.g., deploy outdated detection).
pub fn get_component_version(component: &Component) -> Option<String> {
    let target = component.version_targets.as_ref()?.first()?;
    read_local_version(&component.local_path, target)
}

/// Read version from a local file for a component's version target.
/// Returns None if file doesn't exist or version can't be parsed.
pub fn read_local_version(local_path: &str, version_target: &VersionTarget) -> Option<String> {
    let path = resolve_version_file_path(local_path, &version_target.file);
    let content = local_files::local().read(Path::new(&path)).ok()?;

    let pattern: String = version_target
        .pattern
        .clone()
        .or_else(|| default_pattern_for_file(&version_target.file))?;

    parse_version(&content, &pattern)
}

/// Resolve version file path (absolute or relative to local_path)
fn resolve_version_file_path(local_path: &str, file: &str) -> String {
    parser::resolve_path_string(local_path, file)
}

/// Information about a version target after reading
#[derive(Debug, Clone, Serialize)]

pub struct VersionTargetInfo {
    pub file: String,
    pub pattern: String,
    pub full_path: String,
    pub match_count: usize,
}

/// Result of reading a component's version
#[derive(Debug, Clone, Serialize)]

pub struct ComponentVersionInfo {
    pub version: String,
    pub targets: Vec<VersionTargetInfo>,
}

/// Result of bumping a component's version
#[derive(Debug, Clone, Serialize)]

pub struct BumpResult {
    pub old_version: String,
    pub new_version: String,
    pub targets: Vec<VersionTargetInfo>,
    pub changelog_path: String,
    pub changelog_finalized: bool,
    pub changelog_changed: bool,
}

/// Resolve pattern for a version target, using explicit pattern or module default.
fn resolve_target_pattern(target: &VersionTarget) -> Result<String> {
    target
        .pattern
        .clone()
        .or_else(|| default_pattern_for_file(&target.file))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "versionTargets[].pattern",
                format!(
                    "No version pattern configured for '{}' and no module provides one",
                    target.file
                ),
                None,
                None,
            )
        })
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
        let found = parser::require_identical(&versions, &target.file)?;
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
        });
    }

    Ok(target_infos)
}

/// Result of validating and finalizing changelog for a version operation.
#[derive(Debug, Clone, Serialize)]
pub struct ChangelogValidationResult {
    pub changelog_path: String,
    pub changelog_finalized: bool,
    pub changelog_changed: bool,
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
/// Finalizes the next section to the new version.
pub fn validate_and_finalize_changelog(
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

    let (finalized_changelog, changelog_changed) = changelog::finalize_next_section(
        &changelog_content,
        &settings.next_section_aliases,
        new_version,
        false,
    )?;

    if changelog_changed {
        local_files::local().write(&changelog_path, &finalized_changelog)?;
    }

    Ok(ChangelogValidationResult {
        changelog_path: changelog_path.to_string_lossy().to_string(),
        changelog_finalized: true,
        changelog_changed,
    })
}

/// Build a detailed error for version parsing failures
fn build_version_parse_error(file: &str, pattern: &str, content: &str) -> Error {
    let preview: String = content.chars().take(500).collect();

    let mut hints = Vec::new();

    if pattern.contains("\\\\s") || pattern.contains("\\\\d") {
        hints.push("Pattern appears double-escaped. Use \\s for whitespace, \\d for digits.");
    }

    if content.contains("Version:")
        && !Regex::new(pattern)
            .map(|r| r.is_match(content))
            .unwrap_or(false)
    {
        hints.push("File contains 'Version:' but pattern doesn't match. Check spacing and format.");
    }

    let hints_text = if hints.is_empty() {
        String::new()
    } else {
        format!("\nHints:\n  - {}", hints.join("\n  - "))
    };

    Error::internal_unexpected(format!(
        "Could not parse version from {} using pattern: {}{}\n\nFile preview (first 500 chars):\n{}",
        file, pattern, hints_text, preview
    ))
}

/// Read the current version from a component's version targets.
pub fn read_component_version(component: &Component) -> Result<ComponentVersionInfo> {
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

    let primary = &targets[0];
    let primary_pattern = resolve_target_pattern(primary)?;
    let primary_full_path = resolve_version_file_path(&component.local_path, &primary.file);

    let content = local_files::local().read(Path::new(&primary_full_path))?;
    let versions = parse_versions(&content, &primary_pattern).ok_or_else(|| {
        Error::validation_invalid_argument(
            "versionPattern",
            format!("Invalid version regex pattern '{}'", primary_pattern),
            None,
            Some(vec![primary_pattern.clone()]),
        )
    })?;

    if versions.is_empty() {
        return Err(build_version_parse_error(
            &primary.file,
            &primary_pattern,
            &content,
        ));
    }

    let version = parser::require_identical(&versions, &primary.file)?;

    // Build target info for primary
    let mut target_infos = vec![VersionTargetInfo {
        file: primary.file.clone(),
        pattern: primary_pattern,
        full_path: primary_full_path,
        match_count: versions.len(),
    }];

    // Add info for all remaining targets
    for target in targets.iter().skip(1) {
        let pattern = resolve_target_pattern(target)?;
        let full_path = resolve_version_file_path(&component.local_path, &target.file);
        let content = local_files::local().read(Path::new(&full_path))?;
        let target_versions = parse_versions(&content, &pattern).ok_or_else(|| {
            Error::validation_invalid_argument(
                "versionPattern",
                format!("Invalid version regex pattern '{}'", pattern),
                None,
                Some(vec![pattern.clone()]),
            )
        })?;

        target_infos.push(VersionTargetInfo {
            file: target.file.clone(),
            pattern,
            full_path,
            match_count: target_versions.len(),
        });
    }

    Ok(ComponentVersionInfo {
        version,
        targets: target_infos,
    })
}

/// Read version by component ID.
/// If component_id is None, returns homeboy binary's own version.
pub fn read_version(component_id: Option<&str>) -> Result<ComponentVersionInfo> {
    // If no component_id, return homeboy binary's own version
    let id = match component_id {
        None => {
            let version = crate::upgrade::current_version().to_string();
            return Ok(ComponentVersionInfo {
                version,
                targets: vec![],
            });
        }
        Some(id) => id,
    };

    let component = component::load(id)?;
    read_component_version(&component)
}

/// Result of directly setting a component's version
#[derive(Debug, Clone, Serialize)]

pub struct SetResult {
    pub old_version: String,
    pub new_version: String,
    pub targets: Vec<VersionTargetInfo>,
    pub changelog_path: String,
    pub changelog_finalized: bool,
    pub changelog_changed: bool,
}

/// Set a component's version directly (without incrementing).
pub fn set_component_version(component: &Component, new_version: &str) -> Result<SetResult> {
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

    let old_version = parser::require_identical(&primary_versions, &primary.file)?;

    // Update all version targets (no changelog validation - `set` is version-only)
    let mut target_infos = Vec::new();

    for target in targets {
        let version_pattern = resolve_target_pattern(target)?;
        let full_path = resolve_version_file_path(&component.local_path, &target.file);
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

        // Validate all versions match expected
        let found = parser::require_identical(&versions, &target.file)?;
        if found != old_version {
            return Err(Error::internal_unexpected(format!(
                "Version mismatch in {}: found {}, expected {}",
                target.file, found, old_version
            )));
        }

        let match_count = versions.len();

        let replaced_count =
            update_version_in_file(&full_path, &version_pattern, &old_version, new_version)?;

        if replaced_count != match_count {
            return Err(Error::internal_unexpected(format!(
                "Unexpected replacement count in {}",
                target.file
            )));
        }

        target_infos.push(VersionTargetInfo {
            file: target.file.clone(),
            pattern: version_pattern,
            full_path,
            match_count,
        });
    }

    Ok(SetResult {
        old_version,
        new_version: new_version.to_string(),
        targets: target_infos,
        changelog_path: String::new(),
        changelog_finalized: false,
        changelog_changed: false,
    })
}

/// Set version by component ID.
pub fn set_version(component_id: Option<&str>, new_version: &str) -> Result<SetResult> {
    let id = validation::require(component_id, "componentId", "Missing componentId")?;
    let component = component::load(id)?;
    set_component_version(&component, new_version)
}

/// Bump a component's version and finalize changelog.
/// bump_type: "patch", "minor", or "major"
pub fn bump_component_version(component: &Component, bump_type: &str) -> Result<BumpResult> {
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

    let old_version = parser::require_identical(&primary_versions, &primary.file)?;
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
        validate_and_finalize_changelog(component, &old_version, &new_version)?;

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

    // Run commands that may update/stage generated artifacts impacted by the bump (e.g. Cargo.lock).
    // This must happen AFTER version targets are updated so artifacts match the new version.
    run_pre_bump_commands(&component.pre_version_bump_commands, &component.local_path)?;

    run_post_bump_commands(&component.post_version_bump_commands, &component.local_path)?;

    Ok(BumpResult {
        old_version,
        new_version,
        targets: target_infos,
        changelog_path: changelog_validation.changelog_path,
        changelog_finalized: changelog_validation.changelog_finalized,
        changelog_changed: changelog_validation.changelog_changed,
    })
}

/// Bump version by component ID.
pub fn bump_version(component_id: Option<&str>, bump_type: &str) -> Result<BumpResult> {
    let mut hints = Vec::new();

    // Check if we can detect component from current directory
    if component_id.is_none() {
        if let Some(detected_id) = component::detect_from_cwd() {
            hints.push(format!(
                "Did you mean: homeboy version bump {} {}",
                detected_id, bump_type
            ));
        }
    }

    hints.push("Provide a component ID: homeboy version bump <component-id> <bump-type>".to_string());
    hints.push("List available components: homeboy component list".to_string());

    let id = validation::require_with_hints(
        component_id,
        "componentId",
        "Missing componentId",
        hints,
    )?;
    let component = component::load(id)?;
    bump_component_version(&component, bump_type)
}

/// Detect version targets in a directory by checking for well-known version files.
pub fn detect_version_targets(base_path: &str) -> Result<Vec<(String, String, String)>> {
    let mut found = Vec::new();

    // Load version candidates from configurable defaults
    let version_candidates = defaults::load_defaults().version_candidates;

    // Check well-known files first
    for candidate in &version_candidates {
        let full_path = format!("{}/{}", base_path, candidate.file);
        if Path::new(&full_path).exists() {
            let content = fs::read_to_string(&full_path).ok();
            if let Some(content) = content {
                if parse_version(&content, &candidate.pattern).is_some() {
                    found.push((candidate.file.clone(), candidate.pattern.clone(), full_path));
                }
            }
        }
    }

    // Check for PHP plugin files (*.php with Version: header)
    if let Ok(entries) = fs::read_dir(base_path) {
        let php_pattern = r"Version:\s*(\d+\.\d+\.\d+)";
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "php") {
                if let Ok(content) = fs::read_to_string(&path) {
                    // Only match if it looks like a WordPress plugin header
                    if (content.contains("Plugin Name:") || content.contains("Theme Name:"))
                        && parse_version(&content, php_pattern).is_some()
                    {
                        let filename = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown.php");
                        found.push((
                            filename.to_string(),
                            php_pattern.to_string(),
                            path.to_string_lossy().to_string(),
                        ));
                    }
                }
            }
        }
    }

    Ok(found)
}

/// Information about a version pattern found but not configured
#[derive(Debug, Clone, Serialize)]
pub struct UnconfiguredPattern {
    pub file: String,
    pub pattern: String,
    pub description: String,
    pub found_version: String,
    pub full_path: String,
}

/// Detect additional version patterns that exist in PHP files but aren't configured.
/// Returns patterns that are found in the file but NOT in the configured version targets.
pub fn detect_unconfigured_patterns(
    component: &Component,
) -> Vec<UnconfiguredPattern> {
    let mut unconfigured = Vec::new();
    let base_path = &component.local_path;

    // Get configured file/pattern combinations
    let configured: HashSet<(String, String)> = component
        .version_targets
        .as_ref()
        .map(|targets| {
            targets
                .iter()
                .filter_map(|t| {
                    let pattern = t
                        .pattern
                        .clone()
                        .or_else(|| default_pattern_for_file(&t.file))?;
                    Some((t.file.clone(), pattern))
                })
                .collect()
        })
        .unwrap_or_default();

    // Patterns to scan for in PHP files (beyond plugin headers)
    let php_constant_patterns = [
        (
            r#"define\s*\(\s*['"]([A-Z_]+VERSION)['"]\s*,\s*['"](\d+\.\d+\.\d+)['"]\s*\)"#,
            "PHP constant",
        ),
    ];

    // Scan PHP files in root directory
    if let Ok(entries) = fs::read_dir(base_path) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "php") {
                if let Ok(content) = fs::read_to_string(&path) {
                    let filename = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown.php")
                        .to_string();

                    // Check for PHP constant patterns
                    for (pattern, description) in &php_constant_patterns {
                        if let Ok(re) = Regex::new(pattern) {
                            for caps in re.captures_iter(&content) {
                                if let (Some(const_name), Some(version)) =
                                    (caps.get(1), caps.get(2))
                                {
                                    // Build the specific pattern for this constant
                                    let specific_pattern = format!(
                                        r#"define\s*\(\s*['"]{}['"]\s*,\s*['"](\d+\.\d+\.\d+)['"]\s*\)"#,
                                        regex::escape(const_name.as_str())
                                    );

                                    // Check if already configured
                                    if !configured.contains(&(filename.clone(), specific_pattern.clone())) {
                                        unconfigured.push(UnconfiguredPattern {
                                            file: filename.clone(),
                                            pattern: specific_pattern,
                                            description: format!(
                                                "{}: {}",
                                                description,
                                                const_name.as_str()
                                            ),
                                            found_version: version.as_str().to_string(),
                                            full_path: path.to_string_lossy().to_string(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    unconfigured
}
