use crate::component::{Component, VersionTarget};
use crate::local_files::{self, FileSystem};
use crate::json::{self, set_json_pointer};
use crate::module::{load_module, ModuleManifest};
use crate::error::{Error, Result};
use crate::changelog;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Parse version from content using regex pattern.
/// Pattern must contain a capture group for the version string.
pub fn parse_version(content: &str, pattern: &str) -> Option<String> {
    let re = Regex::new(pattern).ok()?;
    re.captures(content)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

pub fn parse_versions(content: &str, pattern: &str) -> Option<Vec<String>> {
    let re = Regex::new(pattern).ok()?;
    let mut versions = Vec::new();

    for caps in re.captures_iter(content) {
        if let Some(m) = caps.get(1) {
            versions.push(m.as_str().to_string());
        }
    }

    Some(versions)
}

pub fn replace_versions(
    content: &str,
    pattern: &str,
    new_version: &str,
) -> Option<(String, usize)> {
    let re = Regex::new(pattern).ok()?;
    let mut count = 0usize;

    let replaced = re
        .replace_all(content, |caps: &regex::Captures| {
            count += 1;
            let full_match = caps.get(0).map(|m| m.as_str()).unwrap_or("");
            let captured = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            full_match.replacen(captured, new_version, 1)
        })
        .to_string();

    Some((replaced, count))
}

/// Get version pattern from module configuration.
/// Returns None if no module defines a pattern for this file type.
pub fn default_pattern_for_file(filename: &str, modules: &[String]) -> Option<String> {
    for module_id in modules {
        if let Some(module) = load_module(module_id) {
            if let Some(pattern) = find_version_pattern_in_module(&module, filename) {
                return Some(pattern);
            }
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
    modules: &[String],
) -> Result<usize> {
    // JSON files with default pattern use structured update
    if Path::new(path)
        .extension()
        .is_some_and(|ext| ext == "json")
        && default_pattern_for_file(path, modules).as_deref() == Some(pattern)
    {
        let content = local_files::local().read(Path::new(path))?;
        let mut json: Value = json::from_str(&content)?;
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
        let output = json::to_string_pretty(&json)?;
        local_files::local().write(Path::new(path), &output)?;
        return Ok(1);
    }

    // Text files use regex replacement
    let content = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read version file".to_string())))?;

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

    fs::write(path, &new_content)
        .map_err(|e| Error::internal_io(e.to_string(), Some("write version file".to_string())))?;

    Ok(replaced_count)
}

/// Get version string from a component's first version target.
/// Returns None if no version targets configured or version can't be read.
/// Use this for simple version checks (e.g., deploy outdated detection).
pub fn get_component_version(component: &Component) -> Option<String> {
    let target = component.version_targets.as_ref()?.first()?;
    read_local_version(&component.local_path, target, &component.modules)
}

/// Read version from a local file for a component's version target.
/// Returns None if file doesn't exist or version can't be parsed.
pub fn read_local_version(
    local_path: &str,
    version_target: &VersionTarget,
    modules: &[String],
) -> Option<String> {
    let path = resolve_version_file_path(local_path, &version_target.file);
    let content = local_files::local().read(Path::new(&path)).ok()?;

    let pattern: String = version_target.pattern.clone()
        .or_else(|| default_pattern_for_file(&version_target.file, modules))?;

    parse_version(&content, &pattern)
}

/// Resolve version file path (absolute or relative to local_path)
fn resolve_version_file_path(local_path: &str, file: &str) -> String {
    if file.starts_with('/') {
        file.to_string()
    } else {
        format!("{}/{}", local_path, file)
    }
}

/// Information about a version target after reading
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionTargetInfo {
    pub file: String,
    pub pattern: String,
    pub full_path: String,
    pub match_count: usize,
}

/// Result of reading a component's version
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentVersionInfo {
    pub version: String,
    pub targets: Vec<VersionTargetInfo>,
}

/// Result of bumping a component's version
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BumpResult {
    pub old_version: String,
    pub new_version: String,
    pub targets: Vec<VersionTargetInfo>,
    pub changelog_path: String,
    pub changelog_finalized: bool,
    pub changelog_changed: bool,
}

/// Resolve pattern for a version target, using explicit pattern or module default.
fn resolve_target_pattern(target: &VersionTarget, modules: &[String]) -> Result<String> {
    target.pattern.clone()
        .or_else(|| default_pattern_for_file(&target.file, modules))
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

/// Build a detailed error for version parsing failures
fn build_version_parse_error(file: &str, pattern: &str, content: &str) -> Error {
    let preview: String = content.chars().take(500).collect();
    let escaped_pattern = pattern.replace('\\', "\\\\");

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
        file, escaped_pattern, hints_text, preview
    ))
}

/// Read the current version from a component's version targets.
pub fn read_component_version(component: &Component) -> Result<ComponentVersionInfo> {
    let targets = component.version_targets.as_ref().ok_or_else(|| {
        Error::config_missing_key("versionTargets", Some(component.id.clone()))
    })?;

    if targets.is_empty() {
        return Err(Error::config_invalid_value(
            "versionTargets",
            None,
            format!("Component '{}' has empty versionTargets", component.id),
        ));
    }

    let primary = &targets[0];
    let primary_pattern = resolve_target_pattern(primary, &component.modules)?;
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
        return Err(build_version_parse_error(&primary.file, &primary_pattern, &content));
    }

    let unique: BTreeSet<String> = versions.iter().cloned().collect();
    if unique.len() != 1 {
        return Err(Error::internal_unexpected(format!(
            "Multiple different versions found in {}: {}",
            primary.file,
            unique.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }

    let version = versions[0].clone();

    Ok(ComponentVersionInfo {
        version,
        targets: vec![VersionTargetInfo {
            file: primary.file.clone(),
            pattern: primary_pattern,
            full_path: primary_full_path,
            match_count: versions.len(),
        }],
    })
}

/// Bump a component's version and finalize changelog.
/// bump_type: "patch", "minor", or "major"
pub fn bump_component_version(
    component: &Component,
    bump_type: &str,
    dry_run: bool,
) -> Result<BumpResult> {
    let targets = component.version_targets.as_ref().ok_or_else(|| {
        Error::config_missing_key("versionTargets", Some(component.id.clone()))
    })?;

    if targets.is_empty() {
        return Err(Error::config_invalid_value(
            "versionTargets",
            None,
            format!("Component '{}' has empty versionTargets", component.id),
        ));
    }

    // Read current version from primary target
    let primary = &targets[0];
    let primary_pattern = resolve_target_pattern(primary, &component.modules)?;
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
        return Err(build_version_parse_error(&primary.file, &primary_pattern, &primary_content));
    }

    let unique_primary: BTreeSet<String> = primary_versions.iter().cloned().collect();
    if unique_primary.len() != 1 {
        return Err(Error::internal_unexpected(format!(
            "Multiple different versions found in {}: {}",
            primary.file,
            unique_primary.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }

    let old_version = primary_versions[0].clone();
    let new_version = increment_version(&old_version, bump_type).ok_or_else(|| {
        Error::validation_invalid_argument(
            "version",
            format!("Invalid version format: {}", old_version),
            None,
            Some(vec![old_version.clone()]),
        )
    })?;

    // Validate changelog is in sync
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
                    "Add at least one finalized version section like '## 0.1.0'".to_string(),
                ]),
            )
        })?;

    if latest_changelog_version != old_version {
        return Err(Error::validation_invalid_argument(
            "version",
            format!(
                "Version mismatch: changelog is at {} but files are at {}. Bumping would create a version gap.",
                latest_changelog_version, old_version
            ),
            None,
            Some(vec![
                "Ensure changelog and version files are in sync before bumping.".to_string(),
            ]),
        ));
    }

    // Finalize changelog
    let (finalized_changelog, changelog_changed) = changelog::finalize_next_section(
        &changelog_content,
        &settings.next_section_aliases,
        &new_version,
        false,
    )?;

    if changelog_changed && !dry_run {
        local_files::local().write(&changelog_path, &finalized_changelog)?;
    }

    // Update all version targets
    let mut target_infos = Vec::new();

    for target in targets {
        let version_pattern = resolve_target_pattern(target, &component.modules)?;
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
        let unique: BTreeSet<String> = versions.iter().cloned().collect();
        if unique.len() != 1 {
            return Err(Error::internal_unexpected(format!(
                "Multiple different versions found in {}: {}",
                target.file,
                unique.into_iter().collect::<Vec<_>>().join(", ")
            )));
        }

        let found = &versions[0];
        if found != &old_version {
            return Err(Error::internal_unexpected(format!(
                "Version mismatch in {}: found {}, expected {}",
                target.file, found, old_version
            )));
        }

        let match_count = versions.len();

        if !dry_run {
            let replaced_count = update_version_in_file(
                &full_path,
                &version_pattern,
                &old_version,
                &new_version,
                &component.modules,
            )?;

            if replaced_count != match_count {
                return Err(Error::internal_unexpected(format!(
                    "Unexpected replacement count in {}",
                    target.file
                )));
            }
        }

        target_infos.push(VersionTargetInfo {
            file: target.file.clone(),
            pattern: version_pattern,
            full_path,
            match_count,
        });
    }

    Ok(BumpResult {
        old_version,
        new_version,
        targets: target_infos,
        changelog_path: changelog_path.to_string_lossy().to_string(),
        changelog_finalized: true,
        changelog_changed,
    })
}
