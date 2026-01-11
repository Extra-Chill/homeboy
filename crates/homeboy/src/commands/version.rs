use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::fs;

use homeboy_core::config::ConfigManager;
use homeboy_core::version::{default_pattern_for_file, increment_version, parse_version};
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
pub struct VersionOutput {
    command: String,
    component_id: String,
    version_file: String,
    version: Option<String>,
    old_version: Option<String>,
    new_version: Option<String>,
    version_pattern: String,
    full_path: String,
}

pub fn run(args: VersionArgs) -> homeboy_core::Result<(VersionOutput, i32)> {
    match args.command {
        VersionCommand::Show { component_id } => show(&component_id),
        VersionCommand::Bump {
            component_id,
            bump_type,
        } => bump(&component_id, bump_type),
    }
}

fn get_version_config(
    component_id: &str,
) -> homeboy_core::Result<(String, String, String, String)> {
    let component = ConfigManager::load_component(component_id)?;

    let version_file = component.version_file.ok_or_else(|| {
        Error::Config(format!(
            "Component '{}' has no version_file configured",
            component_id
        ))
    })?;

    let full_path = if version_file.starts_with('/') {
        version_file.clone()
    } else {
        format!("{}/{}", component.local_path, version_file)
    };

    let version_pattern = component
        .version_pattern
        .unwrap_or_else(|| default_pattern_for_file(&version_file).to_string());

    Ok((
        full_path,
        version_file,
        version_pattern,
        component.local_path,
    ))
}

fn show(component_id: &str) -> homeboy_core::Result<(VersionOutput, i32)> {
    let (full_path, version_file, version_pattern, _local_path) = get_version_config(component_id)?;

    let content = fs::read_to_string(&full_path)?;

    let version = parse_version(&content, &version_pattern).ok_or_else(|| {
        Error::Other(format!(
            "Could not parse version from {} using pattern: {}",
            version_file, version_pattern
        ))
    })?;

    Ok((
        VersionOutput {
            command: "version.show".to_string(),
            component_id: component_id.to_string(),
            version_file,
            version: Some(version),
            old_version: None,
            new_version: None,
            version_pattern,
            full_path,
        },
        0,
    ))
}

fn bump(component_id: &str, bump_type: BumpType) -> homeboy_core::Result<(VersionOutput, i32)> {
    let (full_path, version_file, version_pattern, _local_path) = get_version_config(component_id)?;

    let content = fs::read_to_string(&full_path)?;

    let old_version = parse_version(&content, &version_pattern).ok_or_else(|| {
        Error::Other(format!(
            "Could not parse version from {} using pattern: {}",
            version_file, version_pattern
        ))
    })?;

    let new_version = increment_version(&old_version, bump_type.as_str())
        .ok_or_else(|| Error::Other(format!("Invalid version format: {}", old_version)))?;

    let new_content = content.replace(&old_version, &new_version);
    fs::write(&full_path, &new_content)?;

    Ok((
        VersionOutput {
            command: "version.bump".to_string(),
            component_id: component_id.to_string(),
            version_file,
            version: None,
            old_version: Some(old_version),
            new_version: Some(new_version),
            version_pattern,
            full_path,
        },
        0,
    ))
}
