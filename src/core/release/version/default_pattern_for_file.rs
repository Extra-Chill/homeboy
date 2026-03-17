//! default_pattern_for_file — extracted from version.rs.

use crate::component::{self, Component, VersionTarget};
use crate::engine::codebase_scan;
use crate::engine::local_files::{self, FileSystem};
use crate::engine::text;
use crate::error::{Error, Result};
use crate::extension::load_all_extensions;
use crate::paths::resolve_path_string;
use regex::Regex;
use std::fs;
use std::path::Path;

use super::find_version_pattern_in_extension;
use super::types::{
    ComponentVersionInfo, ComponentVersionSnapshot, UnconfiguredPattern, VersionTargetInfo,
    DEFAULT_SINCE_PLACEHOLDER,
};

/// Parse all versions from content using regex pattern.
/// Content is trimmed to handle trailing newlines in VERSION files.
pub(crate) fn parse_versions(content: &str, pattern: &str) -> Option<Vec<String>> {
    text::extract_all(content, pattern)
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

/// Resolve version file path (absolute or relative to local_path)
pub(crate) fn resolve_version_file_path(local_path: &str, file: &str) -> String {
    resolve_path_string(local_path, file)
}

/// Resolve pattern for a version target, using explicit pattern or extension default.
pub(crate) fn resolve_target_pattern(target: &VersionTarget) -> Result<String> {
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

/// Build a detailed error for version parsing failures
pub(crate) fn build_version_parse_error(file: &str, pattern: &str, content: &str) -> Error {
    let preview: String = content.chars().take(500).collect();

    let mut hints = Vec::new();

    if pattern.contains("\\\\s") || pattern.contains("\\\\d") {
        hints.push("Pattern appears double-escaped. Use \\s for whitespace, \\d for digits.");
    }

    if content.contains("Version:")
        && !Regex::new(&crate::engine::text::ensure_multiline(pattern))
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

    let version = text::require_identical(&versions, &primary.file)?;

    // Build target info for primary
    let mut target_infos = vec![VersionTargetInfo {
        file: primary.file.clone(),
        pattern: primary_pattern,
        full_path: primary_full_path,
        match_count: versions.len(),
        warning: None,
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

        let warning = if target_versions.is_empty() {
            log_status!(
                "warning",
                "Version target {}: pattern '{}' did not match any content",
                target.file,
                pattern
            );
            Some(format!(
                "Pattern did not match any content in {}",
                target.file
            ))
        } else {
            None
        };

        target_infos.push(VersionTargetInfo {
            file: target.file.clone(),
            pattern,
            full_path,
            match_count: target_versions.len(),
            warning,
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

pub(crate) fn read_component_snapshot(component: &Component) -> Result<ComponentVersionSnapshot> {
    let info = read_component_version(component)?;
    Ok(ComponentVersionSnapshot {
        component_id: component.id.clone(),
        version: info.version,
        targets: info.targets,
    })
}

pub(crate) fn build_init_warnings(component: &Component) -> Vec<String> {
    let mut warnings = Vec::new();

    if let Some(targets) = &component.version_targets {
        for target in targets {
            if target.pattern.is_none() && default_pattern_for_file(&target.file).is_none() {
                warnings.push(format!(
                    "Component '{}' has version target '{}' with no pattern and no extension default. Run: homeboy component set {} --version-targets @file.json",
                    component.id, target.file, component.id
                ));
            }
        }
    }

    let unconfigured = detect_unconfigured_patterns(component);
    for pattern in &unconfigured {
        warnings.push(format!(
            "Unconfigured version pattern in '{}': {} found in {} (v{}). Add with: homeboy component add-version-target {} '{}' '{}'",
            component.id,
            pattern.description,
            pattern.file,
            pattern.found_version,
            component.id,
            pattern.file,
            pattern.pattern
        ));
    }

    warnings
}

/// Detect additional version patterns that exist in PHP files but aren't configured.
/// Returns patterns that are found in the file but NOT in the configured version targets.
pub(crate) fn detect_unconfigured_patterns(component: &Component) -> Vec<UnconfiguredPattern> {
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

/// Replace `@since` placeholder tags in source files with the actual version.
/// Returns the total number of replacements made across all files.
///
/// This is extension-driven: the component's extension must define `since_tag` config
/// specifying which file extensions to scan and optionally a custom placeholder pattern.
pub(crate) fn replace_since_tag_placeholders(
    component: &Component,
    new_version: &str,
) -> Result<usize> {
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

    let scan_config = codebase_scan::ScanConfig {
        extensions: codebase_scan::ExtensionFilter::Only(config.extensions.clone()),
        extra_skip_dirs: vec!["tests".to_string()],
        ..Default::default()
    };

    codebase_scan::walk_files_with(base_path, &scan_config, &mut |path| {
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
