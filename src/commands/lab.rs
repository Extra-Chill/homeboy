use clap::{Args, Subcommand};
use serde::Serialize;

use super::{CmdResult, GlobalArgs};
use homeboy::core::runners::{
    self as runner, RunnerExecOptions, RunnerExecOutput, RunnerRequiredTool,
};
use homeboy::core::Error;

#[derive(Args)]
pub struct LabArgs {
    #[command(subcommand)]
    command: Option<LabCommand>,
}

#[derive(Subcommand)]
enum LabCommand {
    /// Show Lab routing status and benchmark commands
    Status,
    /// Print the runner-backed benchmark command for the provided bench args
    Bench {
        /// Arguments to pass after `homeboy bench`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Sync an extension install on a Lab runner by source and git ref
    ExtensionSync {
        /// Runner ID. Defaults to lab.preferred_runner or the only configured SSH Lab runner.
        #[arg(long)]
        runner: Option<String>,
        /// Git URL or runner-local path to the extension source
        #[arg(long)]
        source: String,
        /// Installed extension id to create or replace on the runner
        #[arg(long)]
        id: String,
        /// Git ref to check out for URL installs (branch, tag, or commit)
        #[arg(long = "ref")]
        revision: String,
        /// Install without replacing an existing runner extension
        #[arg(long)]
        no_replace: bool,
    },
}

#[derive(Serialize)]
pub struct LabOutput {
    command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    preferred_runner: Option<String>,
    config_key: &'static str,
    config_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_workspace: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    managed_followups: Vec<LabFollowup>,
    guidance: Vec<String>,
}

#[derive(Serialize)]
pub struct LabFollowup {
    label: &'static str,
    command: String,
    purpose: &'static str,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum LabCommandOutput {
    Status(LabOutput),
    ExtensionSync(LabExtensionSyncOutput),
}

#[derive(Serialize)]
pub struct LabExtensionSyncOutput {
    command: &'static str,
    runner_id: String,
    runner_homeboy_path: String,
    extension_id: String,
    source: String,
    source_revision: String,
    replace: bool,
    install_command: Vec<String>,
    execution: RunnerExecOutput,
}

pub fn run(args: LabArgs, _global: &GlobalArgs) -> CmdResult<LabCommandOutput> {
    let preferred_runner = homeboy::core::runners::resolve_default_lab_runner()?;
    let config_path = homeboy::core::defaults::config_path()?;
    let current_workspace = std::env::current_dir()
        .ok()
        .map(|path| path.display().to_string());
    let managed_followups =
        lab_followups(preferred_runner.as_deref(), current_workspace.as_deref());
    let command = match args.command.unwrap_or(LabCommand::Status) {
        LabCommand::Status => "lab.status",
        LabCommand::Bench { args } => {
            let mut bench_command = "homeboy bench".to_string();
            if !args.is_empty() {
                bench_command.push(' ');
                bench_command.push_str(&args.join(" "));
            }
            return Ok((
                LabCommandOutput::Status(LabOutput {
                    command: "lab.bench",
                    preferred_runner,
                    config_key: "/lab/preferred_runner",
                    config_path,
                    current_workspace,
                    managed_followups,
                    guidance: vec![
                        bench_command,
                        "Homeboy auto-routes portable benchmarks to `lab.preferred_runner`, or to the only configured SSH Lab runner when there is exactly one.".to_string(),
                        "Use `--runner <runner-id>` only to override an ambiguous or non-default Lab selection.".to_string(),
                    ],
                }),
                0,
            ));
        }
        LabCommand::ExtensionSync {
            runner,
            source,
            id,
            revision,
            no_replace,
        } => {
            return sync_lab_extension(runner, &source, &id, &revision, !no_replace);
        }
    };

    Ok((
        LabCommandOutput::Status(LabOutput {
            command,
            preferred_runner,
            config_key: "/lab/preferred_runner",
            config_path,
            current_workspace,
            managed_followups,
            guidance: vec![
                "Use `homeboy bench <component>` to run benchmarks on the default Lab runner."
                    .to_string(),
                "Use the `managed_followups` commands when a Lab run needs runner diagnostics, environment inspection, workspace materialization, or managed execution.".to_string(),
                "Use `homeboy config set /lab/preferred_runner '\"<runner-id>\"'` to set the default Lab runner.".to_string(),
                "Use `homeboy config set /bench/local_execution '\"denied\"'` to make local benchmark execution fail closed.".to_string(),
                "Use `--runner <runner-id>` only when multiple Lab runners are available and no default should be inferred.".to_string(),
            ],
        }),
        0,
    ))
}

fn sync_lab_extension(
    runner_id: Option<String>,
    source: &str,
    extension_id: &str,
    revision: &str,
    replace: bool,
) -> CmdResult<LabCommandOutput> {
    let runner_id = match runner_id {
        Some(runner_id) => runner_id,
        None => homeboy::core::runners::resolve_default_lab_runner()?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner",
                "No default Lab runner is configured or inferable",
                None,
                Some(vec![
                    "Pass --runner <runner-id>.".to_string(),
                    "Set one with: homeboy config set /lab/preferred_runner '\"<runner-id>\"'"
                        .to_string(),
                ]),
            )
        })?,
    };
    let runner_config = runner::load(&runner_id)?;
    let homeboy_path = runner_config
        .settings
        .homeboy_path
        .clone()
        .unwrap_or_else(|| "homeboy".to_string());
    let install_command =
        runner_extension_install_command(&homeboy_path, source, extension_id, revision, replace);
    let (execution, exit_code) = runner::exec(
        &runner_id,
        RunnerExecOptions {
            cwd: None,
            project_id: None,
            allow_diagnostic_ssh: true,
            command: install_command.clone(),
            env: runner_config.env.clone(),
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            capability_preflight: Some(homeboy::core::runners::RunnerCapabilityPreflight {
                command: "homeboy lab extension-sync".to_string(),
                required_tools: vec![RunnerRequiredTool::Homeboy],
                required_commands: Vec::new(),
                required_components: Vec::new(),
                required_env: Vec::new(),
            }),
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
        },
    )?;

    Ok((
        LabCommandOutput::ExtensionSync(LabExtensionSyncOutput {
            command: "lab.extension_sync",
            runner_id,
            runner_homeboy_path: homeboy_path,
            extension_id: extension_id.to_string(),
            source: source.to_string(),
            source_revision: revision.to_string(),
            replace,
            install_command,
            execution,
        }),
        exit_code,
    ))
}

fn runner_extension_install_command(
    homeboy_path: &str,
    source: &str,
    extension_id: &str,
    revision: &str,
    replace: bool,
) -> Vec<String> {
    let mut command = vec![
        homeboy_path.to_string(),
        "extension".to_string(),
        "install".to_string(),
        source.to_string(),
        "--id".to_string(),
        extension_id.to_string(),
        "--ref".to_string(),
        revision.to_string(),
    ];
    if replace {
        command.push("--replace".to_string());
    }
    command
}

fn lab_followups(runner_id: Option<&str>, current_workspace: Option<&str>) -> Vec<LabFollowup> {
    let Some(runner_id) = runner_id else {
        return Vec::new();
    };
    let runner_arg = shell_arg(runner_id);
    let mut followups = vec![
        LabFollowup {
            label: "doctor",
            command: format!("homeboy runner doctor {runner_arg}"),
            purpose: "Probe runner tools, workspace writability, artifact storage, and browser readiness without raw SSH.",
        },
        LabFollowup {
            label: "env",
            command: format!("homeboy runner env {runner_arg}"),
            purpose: "Show the redacted environment Homeboy injects into runner jobs.",
        },
        LabFollowup {
            label: "exec",
            command: format!("homeboy runner exec {runner_arg} -- <command>"),
            purpose: "Run a managed follow-up command through Homeboy instead of opening an ad-hoc shell.",
        },
    ];

    if let Some(path) = current_workspace {
        followups.push(LabFollowup {
            label: "workspace_sync",
            command: format!(
                "homeboy runner workspace sync {runner_arg} --path {} --mode snapshot",
                shell_arg(path)
            ),
            purpose: "Materialize the current checkout into the Lab runner workspace before a replay or follow-up run.",
        });
    }

    followups
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::{lab_followups, runner_extension_install_command};

    #[test]
    fn lab_extension_sync_command_replaces_by_default() {
        assert_eq!(
            runner_extension_install_command(
                "/home/chubes/.cargo/bin/homeboy",
                "https://github.com/Extra-Chill/homeboy-extensions.git",
                "wordpress",
                "5842f0e",
                true,
            ),
            vec![
                "/home/chubes/.cargo/bin/homeboy",
                "extension",
                "install",
                "https://github.com/Extra-Chill/homeboy-extensions.git",
                "--id",
                "wordpress",
                "--ref",
                "5842f0e",
                "--replace",
            ]
        );
    }

    #[test]
    fn lab_extension_sync_command_can_install_without_replace() {
        assert_eq!(
            runner_extension_install_command(
                "homeboy",
                "/home/chubes/Developer/homeboy-extensions/wordpress",
                "wordpress",
                "main",
                false,
            ),
            vec![
                "homeboy",
                "extension",
                "install",
                "/home/chubes/Developer/homeboy-extensions/wordpress",
                "--id",
                "wordpress",
                "--ref",
                "main",
            ]
        );
    }

    #[test]
    fn lab_followups_name_managed_runner_commands() {
        let followups = lab_followups(Some("homeboy-lab"), Some("/tmp/example workspace"));
        let commands: Vec<_> = followups.iter().map(|step| step.command.as_str()).collect();

        assert!(commands.contains(&"homeboy runner doctor homeboy-lab"));
        assert!(commands.contains(&"homeboy runner env homeboy-lab"));
        assert!(commands.contains(&"homeboy runner exec homeboy-lab -- <command>"));
        assert!(commands.contains(
            &"homeboy runner workspace sync homeboy-lab --path '/tmp/example workspace' --mode snapshot"
        ));
    }

    #[test]
    fn lab_followups_are_empty_without_a_selected_runner() {
        assert!(lab_followups(None, Some("/tmp/workspace")).is_empty());
    }
}
