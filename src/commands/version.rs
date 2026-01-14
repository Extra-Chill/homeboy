use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::version::{
    bump_version, bump_version_cwd, read_version, read_version_cwd, set_version, VersionTargetInfo,
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
    #[command(visible_aliases = ["edit", "merge"])]
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
            let (info, component_id_str) = if cwd {
                (read_version_cwd()?, "cwd".to_string())
            } else {
                let info = read_version(component_id.as_deref())?;
                let id = component_id.unwrap_or_else(|| "cwd".to_string());
                (info, id)
            };

            let out = VersionShowOutput {
                command: "version.show".to_string(),
                component_id: component_id_str,
                version: info.version,
                targets: info.targets,
            };
            let json = serde_json::to_value(out)
                .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;
            Ok((json, 0))
        }
        VersionCommand::Bump {
            cwd,
            component_id,
            bump_type,
        } => {
            // Priority: --cwd > component_id
            let (result, component_id_str) = if cwd {
                (bump_version_cwd(bump_type.as_str(), global.dry_run)?, "cwd".to_string())
            } else {
                let result = bump_version(component_id.as_deref(), bump_type.as_str(), global.dry_run)?;
                let id = component_id.unwrap_or_else(|| "cwd".to_string());
                (result, id)
            };

            let out = VersionBumpOutput {
                command: "version.bump".to_string(),
                component_id: component_id_str,
                old_version: result.old_version,
                new_version: result.new_version,
                targets: result.targets,
                changelog_path: result.changelog_path,
                changelog_finalized: result.changelog_finalized,
                changelog_changed: result.changelog_changed,
                dry_run: global.dry_run,
            };
            let json = serde_json::to_value(out)
                .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;
            Ok((json, 0))
        }
        VersionCommand::Set {
            component_id,
            new_version,
        } => {
            // Core validates componentId
            let result = set_version(component_id.as_deref(), &new_version, global.dry_run)?;
            let component_id_str = component_id.unwrap_or_else(|| "unknown".to_string());

            let out = VersionSetOutput {
                command: "version.set".to_string(),
                component_id: component_id_str,
                old_version: result.old_version,
                new_version: result.new_version,
                targets: result.targets,
                dry_run: global.dry_run,
            };
            let json = serde_json::to_value(out)
                .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;
            Ok((json, 0))
        }
    }
}

pub fn show_version_output(component_id: &str) -> homeboy::Result<(VersionShowOutput, i32)> {
    let info = read_version(Some(component_id))?;

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
