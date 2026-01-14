use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::component;
use homeboy::version::{
    bump_component_version, bump_version_cwd, read_component_version, read_version_cwd,
    set_component_version, VersionTargetInfo,
};

use super::CmdResult;

#[derive(Args)]
pub struct VersionArgs {
    #[command(subcommand)]
    command: VersionCommand,
}

#[derive(Subcommand)]
enum VersionCommand {
    /// Show current version of a component
    Show {
        /// Use current working directory (ad-hoc mode with auto-detection)
        #[arg(long)]
        cwd: bool,

        /// Component ID
        component_id: Option<String>,
    },
    /// Bump version of a component and finalize changelog
    Bump {
        /// Use current working directory (ad-hoc mode with auto-detection)
        #[arg(long)]
        cwd: bool,

        /// Component ID
        component_id: Option<String>,

        /// Version bump type
        bump_type: BumpType,
    },
    /// Set version directly (without incrementing or changelog finalization)
    Set {
        /// Component ID
        component_id: Option<String>,

        /// New version (e.g., 1.2.3)
        new_version: String,
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
    dry_run: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionSetOutput {
    command: String,
    component_id: String,
    old_version: String,
    new_version: String,
    targets: Vec<VersionTargetInfo>,
    dry_run: bool,
}

pub fn run(
    args: VersionArgs,
    global: &crate::commands::GlobalArgs,
) -> CmdResult<serde_json::Value> {
    match args.command {
        VersionCommand::Show { cwd, component_id } => {
            // Priority: --cwd > component_id
            if cwd {
                let info = read_version_cwd()?;
                let out = VersionShowOutput {
                    command: "version.show".to_string(),
                    component_id: "cwd".to_string(),
                    version: info.version,
                    targets: info.targets,
                };
                let json = serde_json::to_value(out)
                    .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;
                return Ok((json, 0));
            }

            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId (or use --cwd)",
                    None,
                    None,
                )
            })?;
            let (out, exit_code) = show_version_output(&id)?;
            let json = serde_json::to_value(out)
                .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;
            Ok((json, exit_code))
        }
        VersionCommand::Bump {
            cwd,
            component_id,
            bump_type,
        } => {
            // Priority: --cwd > component_id
            if cwd {
                return bump_cwd(bump_type, global.dry_run);
            }

            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId (or use --cwd)",
                    None,
                    None,
                )
            })?;
            bump(&id, bump_type, global.dry_run)
        }
        VersionCommand::Set {
            component_id,
            new_version,
        } => {
            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId",
                    None,
                    None,
                )
            })?;
            set(&id, &new_version, global.dry_run)
        }
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

fn bump(
    component_id: &str,
    bump_type: BumpType,
    dry_run: bool,
) -> CmdResult<serde_json::Value> {
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
        dry_run,
    };

    let json = serde_json::to_value(out)
        .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;

    Ok((json, 0))
}

fn bump_cwd(bump_type: BumpType, dry_run: bool) -> CmdResult<serde_json::Value> {
    let result = bump_version_cwd(bump_type.as_str(), dry_run)?;

    let out = VersionBumpOutput {
        command: "version.bump".to_string(),
        component_id: "cwd".to_string(),
        old_version: result.old_version,
        new_version: result.new_version,
        targets: result.targets,
        changelog_path: result.changelog_path,
        changelog_finalized: result.changelog_finalized,
        changelog_changed: result.changelog_changed,
        dry_run,
    };

    let json = serde_json::to_value(out)
        .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;

    Ok((json, 0))
}

fn set(
    component_id: &str,
    new_version: &str,
    dry_run: bool,
) -> CmdResult<serde_json::Value> {
    let component = component::load(component_id)?;
    let result = set_component_version(&component, new_version, dry_run)?;

    let out = VersionSetOutput {
        command: "version.set".to_string(),
        component_id: component_id.to_string(),
        old_version: result.old_version,
        new_version: result.new_version,
        targets: result.targets,
        dry_run,
    };

    let json = serde_json::to_value(out)
        .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;

    Ok((json, 0))
}
