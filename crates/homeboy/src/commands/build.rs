use clap::Args;
use serde::Serialize;
use std::process::Command;

use homeboy_core::config::ConfigManager;

use crate::commands::CmdResult;

#[derive(Args)]
pub struct BuildArgs {
    /// Component ID
    pub component_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildOutput {
    pub command: String,
    pub component_id: String,
    pub build_command: String,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

pub fn run(args: BuildArgs) -> CmdResult<BuildOutput> {
    let component = ConfigManager::load_component(&args.component_id)?;

    let build_cmd = component.build_command.ok_or_else(|| {
        homeboy_core::Error::Other(format!(
            "Component '{}' has no build_command configured",
            args.component_id
        ))
    })?;

    let output = Command::new("sh")
        .args(["-c", &build_cmd])
        .current_dir(&component.local_path)
        .output()
        .map_err(|e| homeboy_core::Error::Other(e.to_string()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(1);
    let success = output.status.success();

    Ok((
        BuildOutput {
            command: "build".to_string(),
            component_id: args.component_id,
            build_command: build_cmd,
            stdout,
            stderr,
            success,
        },
        code,
    ))
}
