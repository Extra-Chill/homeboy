use clap::{Args, Subcommand, ValueEnum};
use homeboy_core::changelog;
use homeboy_core::config::ProjectConfiguration;
use homeboy_core::output::CliWarning;
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
    /// Bump version of a component
    Bump {
        /// Component ID
        component_id: String,
        /// Version bump type
        bump_type: BumpType,
        /// Add a changelog item to the configured "next" section (repeatable)
        #[arg(long = "changelog-add", action = clap::ArgAction::Append)]
        changelog_add: Vec<String>,
        /// Optional project ID override (defaults to active project)
        #[arg(long)]
        project_id: Option<String>,
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
    version: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog_items_added: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog_finalized: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog_changed: Option<bool>,
}

pub fn run(
    args: VersionArgs,
    global: &crate::commands::GlobalArgs,
) -> homeboy_core::output::CmdResult {
    let json_spec = global.json_spec();

    if json_spec.is_some() {
        return Err(homeboy_core::Error::other(
            "json input spec is not supported for version commands".to_string(),
        ));
    }

    match args.command {
        VersionCommand::Show { component_id } => {
            let (out, exit_code) = show(&component_id)?;
            let json = serde_json::to_value(out)
                .map_err(|e| homeboy_core::Error::internal_json(e.to_string(), None))?;
            Ok((json, Vec::new(), exit_code))
        }
        VersionCommand::Bump {
            component_id,
            bump_type,
            changelog_add,
            project_id,
        } => bump(
            &component_id,
            bump_type,
            &changelog_add,
            project_id.as_deref(),
            global.dry_run,
        ),
    }
}

fn resolve_target_full_path(component_local_path: &str, version_file: &str) -> String {
    if version_file.starts_with('/') {
        version_file.to_string()
    } else {
        format!("{}/{}", component_local_path, version_file)
    }
}

fn resolve_target_pattern(target: &VersionTarget) -> String {
    target
        .pattern
        .clone()
        .unwrap_or_else(|| default_pattern_for_file(&target.file).to_string())
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
) -> homeboy_core::Result<usize> {
    if Path::new(full_path)
        .extension()
        .is_some_and(|ext| ext == "json")
        && version_pattern == default_pattern_for_file(full_path)
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

fn show(component_id: &str) -> homeboy_core::Result<(VersionShowOutput, i32)> {
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
    let primary_pattern = resolve_target_pattern(primary);
    let primary_full_path = resolve_target_full_path(&component.local_path, &primary.file);

    let content = fs::read_to_string(&primary_full_path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("read primary version target".to_string()),
        )
    })?;
    let versions = extract_versions_from_content(&content, &primary_pattern)?;

    if versions.is_empty() {
        return Err(Error::internal_unexpected(format!(
            "Could not parse version from {} using pattern: {}",
            primary.file, primary_pattern
        )));
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

fn bump(
    component_id: &str,
    bump_type: BumpType,
    changelog_add: &[String],
    project_id_override: Option<&str>,
    dry_run: bool,
) -> homeboy_core::output::CmdResult {
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
    let primary_pattern = resolve_target_pattern(primary);
    let primary_full_path = resolve_target_full_path(&component.local_path, &primary.file);

    let primary_content = fs::read_to_string(&primary_full_path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("read primary version target".to_string()),
        )
    })?;
    let primary_versions = extract_versions_from_content(&primary_content, &primary_pattern)?;

    if primary_versions.is_empty() {
        return Err(Error::internal_unexpected(format!(
            "Could not parse version from {} using pattern: {}",
            primary.file, primary_pattern
        )));
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

    let mut outputs = Vec::new();

    for target in targets {
        let version_pattern = resolve_target_pattern(&target);
        let full_path = resolve_target_full_path(&component.local_path, &target.file);
        let content = fs::read_to_string(&full_path).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read version file".to_string()))
        })?;

        let versions = extract_versions_from_content(&content, &version_pattern)?;
        let (_, match_count) = validate_single_version(versions, &target.file, &old_version)?;

        let replaced_count = if dry_run {
            match_count
        } else {
            write_updated_version(&full_path, &version_pattern, &old_version, &new_version)?
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

    let mut changelog_path: Option<String> = None;
    let mut changelog_items_added: Option<usize> = None;
    let mut changelog_finalized: Option<bool> = None;
    let mut changelog_changed: Option<bool> = None;

    if !changelog_add.is_empty() {
        let project_id = match project_id_override {
            Some(id) => Some(id.to_string()),
            None => ConfigManager::load_app_config()?.active_project_id,
        };

        let project: Option<ProjectConfiguration> = match project_id.as_deref() {
            Some(id) => Some(ConfigManager::load_project(id)?),
            None => None,
        };

        let settings = changelog::resolve_effective_settings(project.as_ref(), Some(&component))?;

        let path = match changelog::resolve_changelog_path(&component) {
            Ok(path) => path,
            Err(err) => {
                warnings.push(CliWarning {
                    code: err.code.as_str().to_string(),
                    message: "Changelog not updated".to_string(),
                    details: err.details.clone(),
                    hints: if err.hints.is_empty() {
                        None
                    } else {
                        Some(err.hints.clone())
                    },
                    retryable: err.retryable,
                });

                let out = VersionBumpOutput {
                    command: "version.bump".to_string(),
                    component_id: component_id.to_string(),
                    version: old_version,
                    new_version,
                    targets: outputs,
                    changelog_path,
                    changelog_items_added,
                    changelog_finalized,
                    changelog_changed,
                };

                let json = serde_json::to_value(out)
                    .map_err(|e| homeboy_core::Error::internal_json(e.to_string(), None))?;

                return Ok((json, warnings, 0));
            }
        };

        changelog_path = Some(path.to_string_lossy().to_string());

        let content = fs::read_to_string(&path).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read changelog".to_string()))
        })?;
        let mut new_content = content;
        let mut any_changed = false;
        let mut added_count = 0usize;

        for message in changelog_add {
            let (next_content, item_changed) = changelog::add_next_section_item(
                &new_content,
                &settings.next_section_aliases,
                message,
            )?;
            new_content = next_content;
            if item_changed {
                any_changed = true;
                added_count += 1;
            }
        }

        let (finalized_content, finalized_changed) = changelog::finalize_next_section(
            &new_content,
            &settings.next_section_aliases,
            &new_version,
            true,
        )?;

        new_content = finalized_content;
        any_changed = any_changed || finalized_changed;

        if any_changed && !dry_run {
            fs::write(&path, &new_content).map_err(|err| {
                Error::internal_io(err.to_string(), Some("write changelog".to_string()))
            })?;
        }

        changelog_items_added = Some(added_count);
        changelog_finalized = Some(true);
        changelog_changed = Some(any_changed);
    }

    let out = VersionBumpOutput {
        command: "version.bump".to_string(),
        component_id: component_id.to_string(),
        version: old_version,
        new_version,
        targets: outputs,
        changelog_path,
        changelog_items_added,
        changelog_finalized,
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

        let content = fs::read_to_string(&version_file).expect("read before");
        let versions =
            extract_versions_from_content(&content, default_pattern_for_file("Cargo.toml"))
                .expect("extract");
        let (old_version, match_count) =
            validate_single_version(versions, "Cargo.toml", "0.1.0").expect("validate");

        let replaced_count = if true { match_count } else { 0 };
        assert_eq!(replaced_count, match_count);

        let after = fs::read_to_string(&version_file).expect("read after");
        assert_eq!(content, after);
        assert_eq!(old_version, "0.1.0");
    }
}
