mod default_pattern_for_file;
mod types;

pub(crate) use default_pattern_for_file::{
    build_init_warnings, read_component_snapshot, resolve_version_file_path,
};
use default_pattern_for_file::{
    build_version_parse_error, parse_versions, replace_since_tag_placeholders,
    resolve_target_pattern,
};
pub use default_pattern_for_file::{
    default_pattern_for_file, read_component_version, read_version,
};
#[cfg(test)]
use types::DEFAULT_SINCE_PLACEHOLDER;
pub use types::{
    BumpResult, ChangelogValidationResult, ComponentVersionInfo, ComponentVersionSnapshot,
    UnconfiguredPattern, VersionTargetInfo,
};

use crate::core::component::{self, Component, VersionTarget};
use crate::core::config::{from_str, set_json_pointer, to_string_pretty};
use crate::core::engine::hooks::{self, HookFailureMode};
use crate::core::engine::local_files::{self, FileSystem};
use crate::core::engine::text;
use crate::core::error::{Error, Result};
use crate::core::extension::ExtensionManifest;
use crate::core::release::changelog;
use serde_json::Value;
use std::path::Path;

/// Parse version from content using regex pattern.
/// Pattern must contain a capture group for the version string.
/// Content is trimmed to handle trailing newlines in VERSION files.
pub fn parse_version(content: &str, pattern: &str) -> Option<String> {
    text::extract_first(content, pattern)
}

/// Increment semver version or set an explicit version.
///
/// bump_type can be:
/// - "patch", "minor", or "major" — increments the corresponding semver component
/// - An explicit version string like "2.0.0" — returned as-is after validation
pub fn increment_version(version: &str, bump_type: &str) -> Option<String> {
    if bump_type.contains('.') {
        let parts: Vec<&str> = bump_type.split('.').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.parse::<u32>().is_err()) {
            return None;
        }
        return Some(bump_type.to_string());
    }

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

/// Get version string from a component's first version target.
/// Returns None if no version targets configured or version can't be read.
/// Use this for simple version checks (e.g., deploy outdated detection).
pub fn get_component_version(component: &Component) -> Option<String> {
    let target = component.version_targets.as_ref()?.first()?;
    read_local_version(&component.local_path, target)
}

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
pub(crate) fn validate_version_targets_at(
    component: &Component,
    expected_version: &str,
) -> Result<Vec<VersionTargetInfo>> {
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

    validate_version_targets(targets, &component.local_path, expected_version)
}

fn validate_version_targets(
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
            return Err(Error::validation_invalid_argument(
                "version",
                format!(
                    "Version mismatch in {}: found {}, expected {}",
                    target.file, found, expected_version
                ),
                None,
                Some(vec![
                    format!(
                        "All version targets must be at {} before release proceeds.",
                        expected_version
                    ),
                    format!(
                        "Update {} from {} to {}, then re-run `homeboy release`.",
                        target.file, found, expected_version
                    ),
                ]),
            ));
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

/// Validate and finalize changelog for a version operation.
/// Ensures changelog is in sync with current version and has valid unreleased content.
///
/// When `generated_entries` is provided (from the release pipeline's commit analysis),
/// entries are generated and finalized into a versioned section in a single disk write —
/// no intermediate `## Unreleased` section is ever persisted.
///
/// When `generated_entries` is None, falls back to finalizing an existing
/// `## Unreleased` section for internal recovery paths.
pub(crate) fn validate_and_finalize_changelog(
    component: &Component,
    current_version: &str,
    new_version: &str,
    generated_entries: Option<&std::collections::HashMap<String, Vec<String>>>,
) -> Result<ChangelogValidationResult> {
    let settings = changelog::resolve_effective_settings(Some(component));
    let changelog_path = changelog::resolve_changelog_path(component)?;

    // resolve_changelog_path() already handles fallback discovery when the
    // configured path doesn't exist, so the path here should be valid.
    let changelog_content = match local_files::local().read(&changelog_path) {
        Ok(content) => content,
        Err(e) => {
            let error_str = e.to_string();
            if error_str.contains("File not found") || error_str.contains("No such file") {
                return Err(Error::validation_invalid_argument(
                    "changelog",
                    format!("Changelog file not found: {}", changelog_path.display()),
                    None,
                    Some(vec![format!(
                        "Configure the component's changelog target, then re-run release:\n  homeboy component set {} --changelog-target \"CHANGELOG.md\"",
                        component.id
                    )]),
                ));
            }
            return Err(e);
        }
    };

    // Bootstrap case: a fresh changelog with no finalized versions is a
    // legitimate first-release state, not an error. The finalize step below
    // writes the first version section. See #1172.
    let latest_changelog_version = changelog::get_latest_finalized_version(&changelog_content);

    // Reject if changelog is ahead of files (version gap). Skipped on the
    // bootstrap case (no prior finalized version).
    if let Some(ref prev) = latest_changelog_version {
        let changelog_ver = semver::Version::parse(prev);
        let file_ver = semver::Version::parse(current_version);
        if let (Ok(clv), Ok(fv)) = (changelog_ver, file_ver) {
            if clv > fv {
                return Err(Error::validation_invalid_argument(
                    "version",
                    format!(
                        "Version mismatch: changelog is at {} but files are at {}. Setting version would create a version gap.",
                        prev, current_version
                    ),
                    None,
                    Some(vec![
                        format!("The changelog has a finalized section for {} but the version files are still at {}.", prev, current_version),
                        "This usually means a previous release was partially prepared.".to_string(),
                        String::new(),
                        "To resolve:".to_string(),
                        format!("  1. Update all version_targets to {} (to match the changelog), commit, and re-run", prev),
                        format!("  2. Or revert the changelog {} section and re-run to let homeboy regenerate it", prev),
                    ]),
                ));
            }
        }
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
    } else if component.changelog_target.is_some() {
        let changelog_file = component
            .changelog_target
            .as_deref()
            .unwrap_or("CHANGELOG.md");
        let version_targets_list: Vec<String> = component
            .version_targets
            .as_ref()
            .map(|targets| targets.iter().map(|t| format!("  - {}", t.file)).collect())
            .unwrap_or_default();

        return Err(Error::validation_invalid_argument(
            "changelog",
            format!(
                "Configured changelog target '{}' was not updated for release {}",
                changelog_file, new_version
            ),
            None,
            Some(vec![
                format!(
                    "Planned release: {} → {} (bump: {})",
                    current_version, new_version,
                    if current_version == new_version { "none" } else { "auto" }
                ),
                "Pre-flight contract — before running `homeboy release`, the target component must have:".to_string(),
                format!("  1. A `## [{}]` section at the top of {}", new_version, changelog_file),
                "  2. Every version_targets file updated to match".to_string(),
                "  3. All changes committed".to_string(),
                String::new(),
                "To preview the target version without running the pipeline:".to_string(),
                format!("  homeboy release {} --dry-run", component.id),
                String::new(),
                "Or let homeboy manage the changelog automatically:".to_string(),
                format!("  homeboy release {} (homeboy generates entries from conventional commits)", component.id),
                if !version_targets_list.is_empty() {
                    format!("Version target files:\n{}", version_targets_list.join("\n"))
                } else {
                    String::new()
                },
            ].into_iter().filter(|s| !s.is_empty()).collect()),
        ));
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

pub(crate) fn bump_component_version_with_changelog(
    component: &Component,
    bump_type: &str,
    changelog_entries: Option<&std::collections::HashMap<String, Vec<String>>>,
    finalized_changelog: Option<&ChangelogValidationResult>,
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
    let target_infos = validate_version_targets(targets, &component.local_path, &old_version)?;

    // Now safe to finalize changelog - all targets validated. Release execution
    // can provide an already-finalized changelog result from the executable
    // changelog.finalize step.
    let changelog_validation = match finalized_changelog {
        Some(result) => result.clone(),
        None => validate_and_finalize_changelog(
            component,
            &old_version,
            &new_version,
            changelog_entries,
        )?,
    };

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
    use crate::core::component::Component;
    use regex::Regex;
    use std::fs;

    fn make_test_component(temp_dir: &tempfile::TempDir) -> Component {
        Component {
            id: "test-component".to_string(),
            local_path: temp_dir.path().to_string_lossy().to_string(),
            remote_path: String::new(),
            changelog_target: Some("CHANGELOG.md".to_string()),
            ..Default::default()
        }
    }

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

    #[test]
    fn increment_version_patch() {
        assert_eq!(
            increment_version("1.2.3", "patch"),
            Some("1.2.4".to_string())
        );
    }

    #[test]
    fn increment_version_minor() {
        assert_eq!(
            increment_version("1.2.3", "minor"),
            Some("1.3.0".to_string())
        );
    }

    #[test]
    fn increment_version_major() {
        assert_eq!(
            increment_version("1.2.3", "major"),
            Some("2.0.0".to_string())
        );
    }

    #[test]
    fn increment_version_explicit_version() {
        assert_eq!(
            increment_version("1.25.5", "2.0.0"),
            Some("2.0.0".to_string())
        );
        assert_eq!(
            increment_version("0.5.0", "1.0.0"),
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn increment_version_explicit_invalid() {
        assert_eq!(increment_version("1.0.0", "2.0"), None);
        assert_eq!(increment_version("1.0.0", "abc.def.ghi"), None);
    }

    #[test]
    fn increment_version_unknown_bump_type() {
        assert_eq!(increment_version("1.0.0", "huge"), None);
    }

    #[test]
    fn validate_and_finalize_changelog_fails_when_configured_target_stays_stale() {
        let temp_dir = tempfile::tempdir().unwrap();
        let changelog_path = temp_dir.path().join("CHANGELOG.md");
        fs::write(
            &changelog_path,
            "# Changelog\n\n## [0.4.16] - 2026-03-01\n\n### Fixed\n- old entry\n",
        )
        .unwrap();

        let component = make_test_component(&temp_dir);
        validate_and_finalize_changelog(&component, "0.4.16", "0.4.17", None)
            .expect_err("configured stale changelog should block release");

        let unchanged = fs::read_to_string(&changelog_path).unwrap();
        assert!(unchanged.contains("## [0.4.16] - 2026-03-01"));
        assert!(!unchanged.contains("0.4.17"));
    }

    /// Bootstrap with generated entries (the release pipeline's common path):
    /// no prior finalized version on disk, entries coming from conventional
    /// commits. `validate_and_finalize_changelog` should write the first
    /// versioned section without complaining about the missing baseline.
    #[test]
    fn validate_and_finalize_changelog_bootstraps_first_release_with_generated_entries() {
        let temp_dir = tempfile::tempdir().unwrap();
        let changelog_path = temp_dir.path().join("CHANGELOG.md");
        // Minimal changelog produced by the pipeline's auto-init preflight.
        fs::write(&changelog_path, "# Changelog\n\n## Unreleased\n\n").unwrap();

        let component = make_test_component(&temp_dir);

        let mut entries: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        entries.insert(
            "added".to_string(),
            vec!["initial public release".to_string()],
        );

        let result = validate_and_finalize_changelog(&component, "0.1.0", "0.1.0", Some(&entries))
            .expect("bootstrap finalize should succeed");

        assert!(result.changelog_finalized);
        assert!(result.changelog_changed);

        let finalized = fs::read_to_string(&changelog_path).unwrap();
        assert!(
            finalized.contains("## [0.1.0]"),
            "expected first version section, got: {}",
            finalized
        );
    }

    #[test]
    fn bump_component_version_runs_post_version_hook_after_version_file_update() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::write(
            temp_dir.path().join("package.json"),
            "{\n  \"version\": \"0.1.0\"\n}\n",
        )
        .unwrap();
        fs::write(
            temp_dir.path().join("CHANGELOG.md"),
            "# Changelog\n\n## Unreleased\n\n",
        )
        .unwrap();

        let mut component = make_test_component(&temp_dir);
        component.version_targets = Some(vec![VersionTarget {
            file: "package.json".to_string(),
            pattern: Some(r#""version"\s*:\s*"([^"]+)""#.to_string()),
            artifact_path: None,
        }]);
        component.hooks.insert(
            hooks::events::POST_VERSION_BUMP.to_string(),
            vec!["sh -c 'grep 0.1.1 package.json > generated.lock'".to_string()],
        );

        let mut entries: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        entries.insert(
            "fixed".to_string(),
            vec!["release hook contract".to_string()],
        );

        let result =
            bump_component_version_with_changelog(&component, "patch", Some(&entries), None)
                .expect("post-version hook should run after version write");

        assert_eq!(result.new_version, "0.1.1");
        let generated = fs::read_to_string(temp_dir.path().join("generated.lock")).unwrap();
        assert!(generated.contains("0.1.1"));
    }
}
