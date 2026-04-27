use clap::{Args, Subcommand};
use serde::Serialize;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use homeboy::daemon::{self, DaemonStartResult, DaemonStatus, DaemonStopResult};

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
    let state = daemon::serve(parsed)?;
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
        homeboy::Error::internal_io(
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
            homeboy::Error::internal_io(e.to_string(), Some("spawn daemon".to_string()))
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
            return Err(homeboy::Error::internal_unexpected(format!(
                "daemon process {} did not publish state before timeout",
                pid
            )));
        }

        thread::sleep(Duration::from_millis(50));
    }
}
