use crate::changelog;
use crate::component::{self, Component, VersionTarget};
use crate::config::{from_str, set_json_pointer, to_string_pretty};
use crate::error::{Error, Result};
use crate::extension::{load_all_extensions, ExtensionManifest};
use crate::hooks::{self, HookFailureMode};
use crate::local_files::{self, FileSystem};
use crate::utils::{self, io, parser};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::Path;

/// Parse version from content using regex pattern.
/// Pattern must contain a capture group for the version string.
/// Content is trimmed to handle trailing newlines in VERSION files.
pub fn parse_version(content: &str, pattern: &str) -> Option<String> {
    parser::extract_first(content, pattern)
}

/// Parse all versions from content using regex pattern.
/// Content is trimmed to handle trailing newlines in VERSION files.
pub(crate) fn parse_versions(content: &str, pattern: &str) -> Option<Vec<String>> {
    parser::extract_all(content, pattern)
}

pub(crate) fn replace_versions(
    content: &str,
    pattern: &str,
    new_version: &str,
) -> Option<(String, usize)> {
    parser::replace_all(content, pattern, new_version)
}

/// Get version pattern from extension configuration.
/// Returns None if no extension defines a pattern for this file type.
pub fn default_pattern_for_file(filename: &str) -> Option<String> {
    for extension in load_all_extensions().unwrap_or_default() {
        if let Some(pattern) = find_version_pattern_in_extension(&extension, filename) {
            return Some(pattern);
        }
    }
    None
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
    /// Number of `@since` placeholder tags replaced with the new version.
    #[serde(skip_serializing_if = "utils::is_zero")]
    pub since_tags_replaced: usize,
}

/// Resolve pattern for a version target, using explicit pattern or extension default.
fn resolve_target_pattern(target: &VersionTarget) -> Result<String> {
    let pattern = target
        .pattern
        .clone()
        .or_else(|| default_pattern_for_file(&target.file))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "versionTargets[].pattern",
                format!(
                    "No version pattern configured for '{}' and no extension provides one",
                    target.file
                ),
                None,
                None,
            )
        })?;

    // Normalize the pattern to fix double-escaped backslashes
    Ok(component::normalize_version_pattern(&pattern))
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

/// Build a detailed error for version parsing failures
fn build_version_parse_error(file: &str, pattern: &str, content: &str) -> Error {
    let preview: String = content.chars().take(500).collect();

    let mut hints = Vec::new();

    if pattern.contains("\\\\s") || pattern.contains("\\\\d") {
        hints.push("Pattern appears double-escaped. Use \\s for whitespace, \\d for digits.");
    }

    if content.contains("Version:")
        && !Regex::new(&crate::utils::parser::ensure_multiline(pattern))
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

/// Detect version targets in a directory by checking for well-known version files.
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
pub fn detect_unconfigured_patterns(component: &Component) -> Vec<UnconfiguredPattern> {
    let mut unconfigured = Vec::new();
    let base_path = &component.local_path;

    // Get configured file/pattern combinations as compiled regexes for fuzzy matching.
    // We can't use exact string comparison because the user's configured pattern
    // may differ textually from the detected pattern while matching the same content.
    let configured_patterns: Vec<(String, Option<Regex>)> = component
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
                    let compiled = Regex::new(&pattern).ok();
                    Some((t.file.clone(), compiled))
                })
                .collect()
        })
        .unwrap_or_default();

    // Patterns to scan for in PHP files (beyond plugin headers)
    let php_constant_patterns = [(
        r#"define\s*\(\s*['"]([A-Z_]+VERSION)['"]\s*,\s*['"](\d+\.\d+\.\d+)['"]\s*\)"#,
        "PHP constant",
    )];

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

                                    // Check if already configured by testing whether
                                    // any configured pattern for this file matches the
                                    // define() line. This avoids false positives from
                                    // textual pattern differences (e.g. ['\"] vs ['\"]).
                                    let full_match = caps.get(0).map(|m| m.as_str()).unwrap_or("");
                                    let already_configured =
                                        configured_patterns.iter().any(|(f, re)| {
                                            f == &filename
                                                && re
                                                    .as_ref()
                                                    .is_some_and(|r| r.is_match(full_match))
                                        });
                                    if !already_configured {
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

/// Default placeholder pattern for `@since` tags.
const DEFAULT_SINCE_PLACEHOLDER: &str = r"0\.0\.0|NEXT|TBD|TODO|UNRELEASED|x\.x\.x";

/// Replace `@since` placeholder tags in source files with the actual version.
/// Returns the total number of replacements made across all files.
///
/// This is extension-driven: the component's extension must define `since_tag` config
/// specifying which file extensions to scan and optionally a custom placeholder pattern.
fn replace_since_tag_placeholders(component: &Component, new_version: &str) -> Result<usize> {
    use crate::extension::load_extension;

    // Find the extension's since_tag config
    let since_tag = component.extensions.as_ref().and_then(|extensions| {
        extensions.keys().find_map(|extension_id| {
            load_extension(extension_id)
                .ok()
                .and_then(|m| m.since_tag().cloned())
        })
    });

    let config = match since_tag {
        Some(c) => c,
        None => return Ok(0),
    };

    if config.extensions.is_empty() {
        return Ok(0);
    }

    let placeholder = config
        .placeholder_pattern
        .as_deref()
        .unwrap_or(DEFAULT_SINCE_PLACEHOLDER);

    // Build regex: @since\s+(<placeholder>)
    let pattern_str = format!(r"@since\s+({})", placeholder);
    let regex = Regex::new(&pattern_str).map_err(|e| {
        Error::validation_invalid_argument(
            "since_tag.placeholder_pattern",
            format!("Invalid regex: {}", e),
            None,
            None,
        )
    })?;

    let base_path = Path::new(&component.local_path);
    let mut total_replaced = 0;

    walk_source_files(base_path, &config.extensions, &mut |path| {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return,
        };

        if !regex.is_match(&content) {
            return;
        }

        let replaced = regex.replace_all(&content, |caps: &regex::Captures| {
            // Replace only the placeholder group, keep `@since ` prefix
            let full = caps.get(0).unwrap().as_str();
            let placeholder_match = caps.get(1).unwrap().as_str();
            full.replacen(placeholder_match, new_version, 1)
        });

        if replaced != content {
            let count = regex.find_iter(&content).count();
            total_replaced += count;
            let _ = fs::write(path, replaced.as_ref());
        }
    });

    Ok(total_replaced)
}

/// Recursively walk source files matching given extensions, skipping common non-source dirs.
fn walk_source_files(dir: &Path, extensions: &[String], callback: &mut impl FnMut(&Path)) {
    let skip_dirs = ["vendor", "node_modules", "build", "dist", ".git", "tests"];

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
            if !skip_dirs.contains(&dir_name.as_ref()) {
                walk_source_files(&path, extensions, callback);
            }
        } else if path.is_file() {
            let file_name = path.to_string_lossy();
            if extensions
                .iter()
                .any(|ext| file_name.ends_with(ext.as_str()))
            {
                callback(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
