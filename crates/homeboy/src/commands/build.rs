use clap::Args;
use serde::Serialize;

use homeboy_core::config::ConfigManager;
use homeboy_core::ssh::{execute_local_command_in_dir, CommandOutput};

use crate::commands::CmdResult;

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_component_loader(
        component_id: &str,
    ) -> homeboy_core::Result<homeboy_core::config::ComponentConfiguration> {
        Ok(homeboy_core::config::ComponentConfiguration {
            id: component_id.to_string(),
            name: "Test Component".to_string(),
            local_path: "component".to_string(),
            remote_path: "/var/www/component".to_string(),
            build_artifact: "dist/plugin.zip".to_string(),
            modules: vec![],
            scoped_modules: None,
            version_targets: None,
            changelog_targets: None,
            changelog_next_section_label: None,
            changelog_next_section_aliases: None,
            build_command: Some("echo hello".to_string()),
            is_network: None,
        })
    }

    fn fake_executor(command: &str, current_dir: Option<&str>) -> CommandOutput {
        assert_eq!(command, "echo hello");
        assert_eq!(current_dir, Some("component"));
        CommandOutput {
            stdout: "ok".to_string(),
            stderr: "".to_string(),
            success: true,
            exit_code: 0,
        }
    }

    #[test]
    fn runs_configured_build_command_in_component_dir() {
        let args = BuildArgs {
            component_id: "test".to_string(),
        };

        let (out, exit_code) =
            run_with_loader_and_executor(args, fake_component_loader, fake_executor).unwrap();

        assert_eq!(exit_code, 0);
        assert_eq!(out.component_id, "test");
        assert_eq!(out.build_command, "echo hello");
        assert_eq!(out.stdout, "ok");
        assert_eq!(out.success, true);
    }
}

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
    run_with_loader_and_executor(
        args,
        ConfigManager::load_component,
        execute_local_command_in_dir,
    )
}

fn run_with_loader_and_executor(
    args: BuildArgs,
    component_loader: fn(
        &str,
    ) -> homeboy_core::Result<homeboy_core::config::ComponentConfiguration>,
    local_executor: fn(&str, Option<&str>) -> CommandOutput,
) -> CmdResult<BuildOutput> {
    let component = component_loader(&args.component_id)?;

    let build_cmd = component.build_command.clone().or_else(|| {
        homeboy_core::build::detect_build_command(
            &component.local_path,
            &component.build_artifact,
            &component.modules,
        )
        .map(|candidate| candidate.command)
    });

    let build_cmd = build_cmd.ok_or_else(|| {
        homeboy_core::Error::other(format!(
            "Component '{}' has no build_command configured and no build script was detected",
            args.component_id
        ))
    })?;

    let output = local_executor(&build_cmd, Some(&component.local_path));

    Ok((
        BuildOutput {
            command: "build".to_string(),
            component_id: args.component_id,
            build_command: build_cmd,
            stdout: output.stdout,
            stderr: output.stderr,
            success: output.success,
        },
        output.exit_code,
    ))
}
