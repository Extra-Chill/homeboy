use clap::{Args, Subcommand};
use serde::Serialize;

use super::{CmdResult, GlobalArgs};

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
}

#[derive(Serialize)]
pub struct LabOutput {
    command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    preferred_runner: Option<String>,
    config_key: &'static str,
    config_path: String,
    guidance: Vec<String>,
}

pub fn run(args: LabArgs, _global: &GlobalArgs) -> CmdResult<LabOutput> {
    let preferred_runner = homeboy::core::runner::resolve_default_lab_runner()?;
    let config_path = homeboy::core::defaults::config_path()?;
    let command = match args.command.unwrap_or(LabCommand::Status) {
        LabCommand::Status => "lab.status",
        LabCommand::Bench { args } => {
            let mut bench_command = "homeboy bench".to_string();
            if !args.is_empty() {
                bench_command.push(' ');
                bench_command.push_str(&args.join(" "));
            }
            return Ok((
                LabOutput {
                    command: "lab.bench",
                    preferred_runner,
                    config_key: "/lab/preferred_runner",
                    config_path,
                    guidance: vec![
                        bench_command,
                        "Homeboy auto-routes portable benchmarks to `lab.preferred_runner`, or to the only configured SSH Lab runner when there is exactly one.".to_string(),
                        "Use `--runner <runner-id>` only to override an ambiguous or non-default Lab selection.".to_string(),
                    ],
                },
                0,
            ));
        }
    };

    Ok((
        LabOutput {
            command,
            preferred_runner,
            config_key: "/lab/preferred_runner",
            config_path,
            guidance: vec![
                "Use `homeboy bench <component>` to run benchmarks on the default Lab runner.".to_string(),
                "Use `homeboy config set /lab/preferred_runner '\"<runner-id>\"'` to set the default Lab runner.".to_string(),
                "Use `homeboy config set /bench/local_execution '\"denied\"'` to make local benchmark execution fail closed.".to_string(),
                "Use `--runner <runner-id>` only when multiple Lab runners are available and no default should be inferred.".to_string(),
            ],
        },
        0,
    ))
}
