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

pub fn run(args: BuildArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<BuildOutput> {
    let component = ConfigManager::load_component(&args.component_id)?;

    let build_cmd = component.build_command.clone().or_else(|| {
        homeboy_core::build::detect_build_command(&component.local_path, &component.build_artifact)
            .map(|candidate| candidate.command)
    });

    let build_cmd = build_cmd.ok_or_else(|| {
        homeboy_core::Error::other(format!(
            "Component '{}' has no build_command configured and no build script was detected",
            args.component_id
        ))
    })?;

    #[cfg(windows)]
    let output = Command::new("cmd")
        .args(["/C", &build_cmd])
        .current_dir(&component.local_path)
        .output()
        .map_err(|e| homeboy_core::Error::other(e.to_string()))?;

    #[cfg(not(windows))]
    let output = Command::new("sh")
        .args(["-c", &build_cmd])
        .current_dir(&component.local_path)
        .output()
        .map_err(|e| homeboy_core::Error::other(e.to_string()))?;

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
