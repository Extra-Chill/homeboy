use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use homeboy::core::daemon::{self, DaemonStartResult, DaemonStatus, DaemonStopResult};
use homeboy::core::http_api::{AnalysisJobRunOutput, AnalysisJobRunner};

use super::CmdResult;

#[derive(Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the local daemon in the background
    Start {
        /// Local bind address. Defaults to an OS-selected loopback port.
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Run the local daemon in the foreground
    Serve {
        /// Local bind address. Defaults to an OS-selected loopback port.
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Stop the background daemon recorded in the state file
    Stop,
    /// Show daemon state and selected local address
    Status,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DaemonOutput {
    Start(DaemonStartResult),
    Serve(DaemonStartResult),
    Stop(DaemonStopResult),
    Status(DaemonStatus),
}

pub fn run(args: DaemonArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<DaemonOutput> {
    match args.command {
        DaemonCommand::Start { addr } => start(&addr),
        DaemonCommand::Serve { addr } => serve(&addr),
        DaemonCommand::Stop => Ok((DaemonOutput::Stop(daemon::stop()?), 0)),
        DaemonCommand::Status => Ok((DaemonOutput::Status(daemon::read_status()?), 0)),
    }
}

fn serve(addr: &str) -> CmdResult<DaemonOutput> {
    let parsed = daemon::parse_bind_addr(addr)?;
    let state = daemon::serve_with_analysis_runner(parsed, CommandAnalysisJobRunner)?;
    Ok((
        DaemonOutput::Serve(DaemonStartResult {
            pid: state.pid,
            address: state.address,
            state_path: state.state_path,
        }),
        0,
    ))
}

fn start(addr: &str) -> CmdResult<DaemonOutput> {
    daemon::parse_bind_addr(addr)?;

    let exe = std::env::current_exe().map_err(|e| {
        homeboy::core::Error::internal_io(
            e.to_string(),
            Some("resolve current executable".to_string()),
        )
    })?;
    let child = Command::new(exe)
        .args(["daemon", "serve", "--addr", addr])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            homeboy::core::Error::internal_io(e.to_string(), Some("spawn daemon".to_string()))
        })?;
    let pid = child.id();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = daemon::read_status()?;
        if let Some(state) = status.state {
            if state.pid == pid {
                return Ok((
                    DaemonOutput::Start(DaemonStartResult {
                        pid,
                        address: state.address,
                        state_path: state.state_path,
                    }),
                    0,
                ));
            }
        }

        if Instant::now() >= deadline {
            return Err(homeboy::core::Error::internal_unexpected(format!(
                "daemon process {} did not publish state before timeout",
                pid
            )));
        }

        thread::sleep(Duration::from_millis(50));
    }
}

#[derive(Debug, Clone, Copy)]
struct CommandAnalysisJobRunner;

impl AnalysisJobRunner for CommandAnalysisJobRunner {
    fn run_analysis_job(&self, argv: Vec<String>) -> homeboy::core::Result<AnalysisJobRunOutput> {
        let cli = crate::cli_surface::Cli::try_parse_from(argv).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "body",
                error.to_string(),
                None,
                Some(vec![
                    "Use the documented JSON request body contract for this endpoint".to_string(),
                ]),
            )
        })?;
        let global = crate::commands::GlobalArgs {};
        let (result, exit_code) = crate::commands::json_output::run(cli.command, &global);
        Ok(AnalysisJobRunOutput {
            exit_code,
            output: result?,
        })
    }
}
