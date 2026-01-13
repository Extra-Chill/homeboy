use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::component;
use homeboy::version::{
    bump_component_version, read_component_version, VersionTargetInfo,
};

use crate::output::{CliWarning, CmdResult};

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
pub struct VersionShowOutput {
    command: String,
    component_id: String,
    pub version: String,
    targets: Vec<VersionTargetInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionBumpOutput {
    command: String,
    component_id: String,
    old_version: String,
    new_version: String,
    targets: Vec<VersionTargetInfo>,
    changelog_path: String,
    changelog_finalized: bool,
    changelog_changed: bool,
}

pub fn run(
    args: VersionArgs,
    global: &crate::commands::GlobalArgs,
) -> CmdResult {
    match args.command {
        VersionCommand::Show { component_id } => {
            let (out, exit_code) = show_version_output(&component_id)?;
            let json = serde_json::to_value(out)
                .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;
            Ok((json, Vec::new(), exit_code))
        }
        VersionCommand::Bump {
            component_id,
            bump_type,
        } => bump(&component_id, bump_type, global.dry_run),
    }
}

pub fn show_version_output(component_id: &str) -> homeboy::Result<(VersionShowOutput, i32)> {
    let component = component::load(component_id)?;
    let info = read_component_version(&component)?;

    Ok((
        VersionShowOutput {
            command: "version.show".to_string(),
            component_id: component_id.to_string(),
            version: info.version,
            targets: info.targets,
        },
        0,
    ))
}

fn bump(component_id: &str, bump_type: BumpType, dry_run: bool) -> CmdResult {
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

    let component = component::load(component_id)?;
    let result = bump_component_version(&component, bump_type.as_str(), dry_run)?;

    let out = VersionBumpOutput {
        command: "version.bump".to_string(),
        component_id: component_id.to_string(),
        old_version: result.old_version,
        new_version: result.new_version,
        targets: result.targets,
        changelog_path: result.changelog_path,
        changelog_finalized: result.changelog_finalized,
        changelog_changed: result.changelog_changed,
    };

    let json = serde_json::to_value(out)
        .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;

    Ok((json, warnings, 0))
}
