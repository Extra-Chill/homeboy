use clap::Args;
use serde::Serialize;

use crate::cli_surface::{current_command_safety_manifest, CommandSafetyManifest};

use super::{CmdResult, GlobalArgs};

#[derive(Args, Debug, Clone)]
pub struct ManifestArgs {}

#[derive(Serialize)]
pub struct ManifestOutput {
    pub command: String,
    #[serde(flatten)]
    pub manifest: CommandSafetyManifest,
}

pub fn run(_args: ManifestArgs, _global: &GlobalArgs) -> CmdResult<ManifestOutput> {
    Ok((
        ManifestOutput {
            command: "manifest".to_string(),
            manifest: current_command_safety_manifest(),
        },
        0,
    ))
}
