use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::component;
use homeboy::version::{read_component_version, read_version, VersionTargetInfo};

use super::CmdResult;

#[derive(Serialize)]
#[serde(untagged)]
pub enum VersionOutput {
    Show(VersionShowOutput),
}

#[derive(Args)]
pub struct VersionArgs {
    #[command(subcommand)]
    command: VersionCommand,
}

#[derive(Subcommand)]
enum VersionCommand {
    /// Show current version (default: discovered component, fallback: homeboy binary)
    Show {
        /// Component ID (optional - shows discovered component version, or homeboy binary version)
        component_id: Option<String>,

        /// Override local_path for version file lookup
        #[arg(long)]
        path: Option<String>,
    },
}

struct VersionShowArgs {
    /// Component ID (optional - shows discovered component version, or homeboy binary version)
    component_id: Option<String>,

    /// Override local_path for version file lookup
    path: Option<String>,
}

#[derive(Serialize)]
pub struct VersionShowOutput {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    component_id: Option<String>,
    pub version: String,
    targets: Vec<VersionTargetInfo>,
}

pub fn run(args: VersionArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<VersionOutput> {
    let VersionCommand::Show { component_id, path } = args.command;
    show(VersionShowArgs { component_id, path })
}

fn show(args: VersionShowArgs) -> CmdResult<VersionOutput> {
    let component_id = args.component_id;
    let path = args.path;

    let info = if let Some(ref p) = path {
        let comp = component::resolve_effective(component_id.as_deref(), Some(p), None)?;
        read_component_version(&comp)?
    } else if component_id.is_some() {
        // Explicit component ID or CWD discovery
        let comp = component::resolve_effective(component_id.as_deref(), None, None)?;
        read_component_version(&comp)?
    } else {
        // No component ID and no --path: try CWD discovery first,
        // fall back to showing homeboy binary version
        match component::resolve_effective(None, None, None) {
            Ok(comp) => read_component_version(&comp)?,
            Err(_) => read_version(None)?,
        }
    };

    let display_id = component_id.or_else(|| {
        // Include discovered component ID in output
        if info.targets.is_empty() {
            None
        } else {
            component::resolve_effective(None, None, None)
                .ok()
                .map(|c| c.id)
        }
    });

    Ok((
        VersionOutput::Show(VersionShowOutput {
            command: "version.show".to_string(),
            component_id: display_id,
            version: info.version,
            targets: info.targets,
        }),
        0,
    ))
}

pub fn show_version_output(component_id: &str) -> CmdResult<VersionShowOutput> {
    let info = read_version(Some(component_id))?;

    Ok((
        VersionShowOutput {
            command: "version.show".to_string(),
            component_id: Some(component_id.to_string()),
            version: info.version,
            targets: info.targets,
        },
        0,
    ))
}
