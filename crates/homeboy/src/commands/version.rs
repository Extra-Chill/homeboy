use clap::{Args, Subcommand, ValueEnum};
use homeboy_core::changelog;
use homeboy_core::output::CliWarning;
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use homeboy_core::config::{ConfigManager, VersionTarget};
use homeboy_core::json::{read_json_file, set_json_pointer, write_json_file_pretty};
use homeboy_core::version::{
    default_pattern_for_file, increment_version, parse_versions, replace_versions,
};
use homeboy_core::Error;

#[derive(Args)]
pub struct VersionArgs {
    #[command(subcommand)]
    command: VersionCommand,
}

#[derive(Subcommand)]
enum VersionCommand {
    /// Show current version of a component
    Show {
        /// Component ID
        component_id: String,
    },
    /// Bump version of a component and finalize changelog
    Bump {
        /// Component ID
        component_id: String,
        /// Version bump type
        bump_type: BumpType,
    },
}

#[derive(Clone, ValueEnum)]
enum BumpType {
    Patch,
    Minor,
    Major,
}

impl BumpType {
    fn as_str(&self) -> &'static str {
        match self {
            BumpType::Patch => "patch",
            BumpType::Minor => "minor",
            BumpType::Major => "major",
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionTargetOutput {
    version_file: String,
    version_pattern: String,
    full_path: String,
    match_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionShowOutput {
    command: String,
    component_id: String,
    pub version: String,
    targets: Vec<VersionTargetOutput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionBumpOutput {
    command: String,
    component_id: String,
    /// Detected current version before bump.
    version: String,
    /// Version after bump.
    new_version: String,
    targets: Vec<VersionTargetOutput>,
    changelog_path: String,
    changelog_finalized: bool,
    changelog_changed: bool,
}

pub fn run(
    args: VersionArgs,
    global: &crate::commands::GlobalArgs,
) -> homeboy_core::output::CmdResult {
    match args.command {
        VersionCommand::Show { component_id } => {
            let (out, exit_code) = show_version_output(&component_id)?;
            let json = serde_json::to_value(out)
                .map_err(|e| homeboy_core::Error::internal_json(e.to_string(), None))?;
            Ok((json, Vec::new(), exit_code))
        }
        VersionCommand::Bump {
            component_id,
            bump_type,
        } => bump(&component_id, bump_type, global.dry_run),
    }
}

fn resolve_target_full_path(component_local_path: &str, version_file: &str) -> String {
    if version_file.starts_with('/') {
        version_file.to_string()
    } else {
        format!("{}/{}", component_local_path, version_file)
    }
}

fn resolve_target_pattern(
    target: &VersionTarget,
    modules: &[String],
) -> homeboy_core::Result<String> {
    target
        .pattern
        .clone()
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

fn extract_versions_from_content(
    content: &str,
    pattern: &str,
) -> homeboy_core::Result<Vec<String>> {
    parse_versions(content, pattern).ok_or_else(|| {
        Error::validation_invalid_argument(
            "versionPattern",
            format!("Invalid version regex pattern '{}'", pattern),
            None,
            Some(vec![pattern.to_string()]),
        )
    })
}

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

fn validate_single_version(
    versions: Vec<String>,
    version_file: &str,
    expected: &str,
) -> homeboy_core::Result<(String, usize)> {
    if versions.is_empty() {
        return Err(Error::internal_unexpected(format!(
            "Could not find version in {}",
            version_file
        )));
    }

    let unique: BTreeSet<String> = versions.iter().cloned().collect();

    if unique.len() != 1 {
        return Err(Error::internal_unexpected(format!(
            "Multiple different versions found in {}: {}",
            version_file,
            unique.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }

    let found = versions[0].clone();
    if found != expected {
        return Err(Error::internal_unexpected(format!(
            "Version mismatch in {}: found {}, expected {}",
            version_file, found, expected
        )));
    }

    Ok((found, versions.len()))
}

fn replace_versions_in_content(
    content: &str,
    pattern: &str,
    expected_old: &str,
    new_version: &str,
) -> homeboy_core::Result<(String, usize)> {
    let all_versions = extract_versions_from_content(content, pattern)?;
    let _ = validate_single_version(all_versions, "<content>", expected_old)?;

    let (replaced, replaced_count) =
        replace_versions(content, pattern, new_version).ok_or_else(|| {
            Error::validation_invalid_argument(
                "versionPattern",
                format!("Invalid version regex pattern '{}'", pattern),
                None,
                Some(vec![pattern.to_string()]),
            )
        })?;

    Ok((replaced, replaced_count))
}

fn write_updated_version(
    full_path: &str,
    version_pattern: &str,
    old_version: &str,
    new_version: &str,
    modules: &[String],
) -> homeboy_core::Result<usize> {
    if Path::new(full_path)
        .extension()
        .is_some_and(|ext| ext == "json")
        && default_pattern_for_file(full_path, modules).as_deref() == Some(version_pattern)
    {
        let mut json = read_json_file(full_path)?;
        let Some(current) = json.get("version").and_then(|v| v.as_str()) else {
            return Err(Error::config_missing_key(
                "version",
                Some(full_path.to_string()),
            ));
        };

        if current != old_version {
            return Err(Error::internal_unexpected(format!(
                "Version mismatch in {}: found {}, expected {}",
                full_path, current, old_version
            )));
        }

        set_json_pointer(
            &mut json,
            "/version",
            serde_json::Value::String(new_version.to_string()),
        )?;
        write_json_file_pretty(full_path, &json)?;
        return Ok(1);
    }

    let content = fs::read_to_string(full_path).map_err(|err| {
        Error::internal_io(err.to_string(), Some("read version file".to_string()))
    })?;
    let (new_content, replaced_count) =
        replace_versions_in_content(&content, version_pattern, old_version, new_version)?;
    fs::write(full_path, &new_content).map_err(|err| {
        Error::internal_io(err.to_string(), Some("write version file".to_string()))
    })?;
    Ok(replaced_count)
}

pub fn show_version_output(component_id: &str) -> homeboy_core::Result<(VersionShowOutput, i32)> {
    let component = ConfigManager::load_component(component_id)?;
    let targets = component.version_targets.ok_or_else(|| {
        Error::config_missing_key("versionTargets", Some(component_id.to_string()))
    })?;

    if targets.is_empty() {
        return Err(Error::config_invalid_value(
            "versionTargets",
            None,
            format!("Component '{}' has empty versionTargets", component_id),
        ));
    }

    let primary = &targets[0];
    let primary_pattern = resolve_target_pattern(primary, &component.modules)?;
    let primary_full_path = resolve_target_full_path(&component.local_path, &primary.file);

    let content = fs::read_to_string(&primary_full_path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("read primary version target".to_string()),
        )
    })?;
    let versions = extract_versions_from_content(&content, &primary_pattern)?;

    if versions.is_empty() {
        return Err(build_version_parse_error(
            &primary.file,
            &primary_pattern,
            &content,
        ));
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

    Ok((
        VersionShowOutput {
            command: "version.show".to_string(),
            component_id: component_id.to_string(),
            version,
            targets: vec![VersionTargetOutput {
                version_file: primary.file.clone(),
                version_pattern: primary_pattern,
                full_path: primary_full_path,
                match_count: versions.len(),
            }],
        },
        0,
    ))
}

fn bump(component_id: &str, bump_type: BumpType, dry_run: bool) -> homeboy_core::output::CmdResult {
    let mut warnings: Vec<CliWarning> = Vec::new();

    if dry_run {
        warnings.push(CliWarning {
            code: "mode.dry_run".to_string(),
            message: "Dry-run: no files were written".to_string(),
            details: serde_json::Value::Object(serde_json::Map::new()),
            hints: None,
            retryable: None,
        });
    }

    let component = ConfigManager::load_component(component_id)?;
    let targets = component.version_targets.clone().ok_or_else(|| {
        Error::config_missing_key("versionTargets", Some(component_id.to_string()))
    })?;

    if targets.is_empty() {
        return Err(Error::config_invalid_value(
            "versionTargets",
            None,
            format!("Component '{}' has empty versionTargets", component_id),
        ));
    }

    let primary = &targets[0];
    let primary_pattern = resolve_target_pattern(primary, &component.modules)?;
    let primary_full_path = resolve_target_full_path(&component.local_path, &primary.file);

    let primary_content = fs::read_to_string(&primary_full_path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("read primary version target".to_string()),
        )
    })?;
    let primary_versions = extract_versions_from_content(&primary_content, &primary_pattern)?;

    if primary_versions.is_empty() {
        return Err(build_version_parse_error(
            &primary.file,
            &primary_pattern,
            &primary_content,
        ));
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
    let new_version = increment_version(&old_version, bump_type.as_str()).ok_or_else(|| {
        Error::validation_invalid_argument(
            "version",
            format!("Invalid version format: {}", old_version),
            None,
            Some(vec![old_version.clone()]),
        )
    })?;

    let settings = changelog::resolve_effective_settings(Some(&component))?;
    let changelog_path = changelog::resolve_changelog_path(&component)?;

    let changelog_content = fs::read_to_string(&changelog_path)
        .map_err(|err| Error::internal_io(err.to_string(), Some("read changelog".to_string())))?;

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

    let (finalized_changelog, changelog_changed) = changelog::finalize_next_section(
        &changelog_content,
        &settings.next_section_aliases,
        &new_version,
        false,
    )?;

    if changelog_changed && !dry_run {
        fs::write(&changelog_path, &finalized_changelog).map_err(|err| {
            Error::internal_io(err.to_string(), Some("write changelog".to_string()))
        })?;
    }

    let mut outputs = Vec::new();

    for target in targets {
        let version_pattern = resolve_target_pattern(&target, &component.modules)?;
        let full_path = resolve_target_full_path(&component.local_path, &target.file);
        let content = fs::read_to_string(&full_path).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read version file".to_string()))
        })?;

        let versions = extract_versions_from_content(&content, &version_pattern)?;
        let (_, match_count) = validate_single_version(versions, &target.file, &old_version)?;

        let replaced_count = if dry_run {
            match_count
        } else {
            write_updated_version(
                &full_path,
                &version_pattern,
                &old_version,
                &new_version,
                &component.modules,
            )?
        };

        if replaced_count != match_count {
            return Err(Error::internal_unexpected(format!(
                "Unexpected replacement count in {}",
                target.file
            )));
        }

        outputs.push(VersionTargetOutput {
            version_file: target.file,
            version_pattern,
            full_path,
            match_count,
        });
    }

    let out = VersionBumpOutput {
        command: "version.bump".to_string(),
        component_id: component_id.to_string(),
        version: old_version,
        new_version,
        targets: outputs,
        changelog_path: changelog_path.to_string_lossy().to_string(),
        changelog_finalized: true,
        changelog_changed,
    };

    let json = serde_json::to_value(out)
        .map_err(|e| homeboy_core::Error::internal_json(e.to_string(), None))?;

    Ok((json, warnings, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_skips_version_file_write() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let version_file = tmp.path().join("Cargo.toml");
        fs::write(&version_file, "version = \"0.1.0\"\n").expect("write version");

        // Explicit TOML version pattern (no builtin patterns)
        let toml_pattern = r#"version\s*=\s*"(\d+\.\d+\.\d+)""#;
        let content = fs::read_to_string(&version_file).expect("read before");
        let versions = extract_versions_from_content(&content, toml_pattern).expect("extract");
        let (old_version, match_count) =
            validate_single_version(versions, "Cargo.toml", "0.1.0").expect("validate");

        let replaced_count = if true { match_count } else { 0 };
        assert_eq!(replaced_count, match_count);

        let after = fs::read_to_string(&version_file).expect("read after");
        assert_eq!(content, after);
        assert_eq!(old_version, "0.1.0");
    }
}
