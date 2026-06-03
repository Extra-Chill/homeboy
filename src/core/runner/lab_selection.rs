use std::io::Read;
use std::process::Stdio;
use std::time::Duration;

use crate::core::{Error, Result};

use super::{
    load, status, LabOffloadCommand, LabRunnerGateMode, RunnerConnectReport, RunnerStatusReport,
    RunnerTunnelMode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabRunnerSelectionSource {
    Explicit,
    Default,
}

impl LabRunnerSelectionSource {
    pub(super) fn metadata_value(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Default => "automatic",
        }
    }

    pub(super) fn gate_mode(self) -> LabRunnerGateMode {
        match self {
            Self::Explicit => LabRunnerGateMode::Explicit,
            Self::Default => LabRunnerGateMode::Automatic,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LabRunnerSelection {
    pub(super) runner_id: String,
    pub(super) source: LabRunnerSelectionSource,
    pub(super) mode: RunnerTunnelMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LabRunnerPreparation {
    Ready,
    FallBackLocal { reason: String },
}

pub(super) fn prepare_lab_runner_for_offload(
    selection: &LabRunnerSelection,
) -> Result<LabRunnerPreparation> {
    let runner = load(&selection.runner_id)?;
    if runner.kind != super::RunnerKind::Ssh {
        return Err(Error::validation_invalid_argument(
            "runner",
            "Lab offload requires a remote direct SSH or reverse-connected runner; local runners would execute on this machine",
            Some(runner.id),
            Some(vec![
                "Register a direct SSH runner or configure a reverse-connected runner before using Lab offload.".to_string(),
            ]),
        ));
    }

    prepare_lab_runner_for_offload_with(selection, status, |runner_id| {
        connect_runner_for_offload(runner_id, selection.source)
    })
}

fn connect_runner_for_offload(
    runner_id: &str,
    source: LabRunnerSelectionSource,
) -> Result<(RunnerConnectReport, i32)> {
    let timeout = lab_connect_timeout(source);
    let (stdout, stderr, exit_code, timed_out) = run_runner_connect_command(runner_id, timeout)?;
    let status = status(runner_id)?;

    if status.connected {
        if let Some(session) = status.session {
            return Ok((
                RunnerConnectReport {
                    runner_id: runner_id.to_string(),
                    mode: Some(session.mode),
                    role: Some(session.role),
                    connected: true,
                    recorded: None,
                    local_url: session.local_url,
                    broker_url: session.broker_url,
                    controller_id: session.controller_id,
                    remote_daemon_address: session.remote_daemon_address,
                    tunnel_pid: session.tunnel_pid,
                    remote_daemon_pid: session.remote_daemon_pid,
                    homeboy_version: Some(session.homeboy_version),
                    session_path: Some(status.session_path),
                    failure_kind: None,
                    failure_message: None,
                },
                0,
            ));
        }
    }

    let reason = if timed_out {
        format!("runner connect timed out after {}s", timeout.as_secs())
    } else {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        if detail.is_empty() {
            format!("runner connect exited with code {exit_code}")
        } else {
            format!("runner connect exited with code {exit_code}: {detail}")
        }
    };

    Ok((
        RunnerConnectReport {
            runner_id: runner_id.to_string(),
            mode: None,
            role: None,
            connected: false,
            recorded: None,
            local_url: None,
            broker_url: None,
            controller_id: None,
            remote_daemon_address: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            homeboy_version: None,
            session_path: Some(status.session_path),
            failure_kind: Some(super::RunnerFailureKind::SshFailure),
            failure_message: Some(reason),
        },
        exit_code,
    ))
}

fn lab_connect_timeout(source: LabRunnerSelectionSource) -> Duration {
    match source {
        LabRunnerSelectionSource::Explicit => Duration::from_secs(30),
        LabRunnerSelectionSource::Default => Duration::from_secs(3),
    }
}

fn run_runner_connect_command(
    runner_id: &str,
    timeout: Duration,
) -> Result<(String, String, i32, bool)> {
    let exe = std::env::current_exe().map_err(|err| {
        Error::internal_io(err.to_string(), Some("resolve homeboy executable".into()))
    })?;
    let mut child = std::process::Command::new(exe)
        .args(["runner", "connect", runner_id])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| Error::internal_io(err.to_string(), Some("start runner connect".into())))?;
    let deadline = std::time::Instant::now() + timeout;

    loop {
        if let Some(status) = child.try_wait().map_err(|err| {
            Error::internal_io(err.to_string(), Some("wait runner connect".into()))
        })? {
            let mut stdout = String::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_string(&mut stdout);
            }
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            return Ok((stdout, stderr, status.code().unwrap_or(-1), false));
        }

        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok((String::new(), String::new(), 124, true));
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(super) fn prepare_lab_runner_for_offload_with(
    selection: &LabRunnerSelection,
    status_fn: impl Fn(&str) -> Result<RunnerStatusReport>,
    connect_fn: impl Fn(&str) -> Result<(RunnerConnectReport, i32)>,
) -> Result<LabRunnerPreparation> {
    let status = status_fn(&selection.runner_id)?;
    if status.connected {
        eprintln!(
            "Lab offload: runner `{}` is connected via {} mode.",
            selection.runner_id,
            status_tunnel_mode(&status).label()
        );
        return Ok(LabRunnerPreparation::Ready);
    }

    if status_tunnel_mode(&status) == RunnerTunnelMode::Reverse {
        let reason = format!(
            "reverse-connected runner `{}` is not currently connected",
            selection.runner_id
        );
        return match selection.source {
            LabRunnerSelectionSource::Default => Ok(LabRunnerPreparation::FallBackLocal { reason }),
            LabRunnerSelectionSource::Explicit => Err(Error::validation_invalid_argument(
                "runner",
                format!(
                    "Lab offload requires reverse runner `{}` to have an active reverse session",
                    selection.runner_id
                ),
                Some(selection.runner_id.clone()),
                Some(vec![
                    "Start the reverse runner session on the Lab machine before using --runner."
                        .to_string(),
                    "Use --force-hot to run the command locally instead of offloading.".to_string(),
                ]),
            )),
        };
    }

    eprintln!(
        "Lab offload: direct SSH runner `{}` is not connected; attempting connection.",
        selection.runner_id
    );
    let (report, _) = connect_fn(&selection.runner_id)?;
    if report.connected {
        return Ok(LabRunnerPreparation::Ready);
    }

    let reason = report
        .failure_message
        .unwrap_or_else(|| "runner connection did not become ready".to_string());

    match selection.source {
        LabRunnerSelectionSource::Default => Ok(LabRunnerPreparation::FallBackLocal { reason }),
        LabRunnerSelectionSource::Explicit => Err(Error::validation_invalid_argument(
            "runner",
            format!(
                "Lab offload could not connect runner `{}` before execution: {reason}",
                selection.runner_id
            ),
            Some(selection.runner_id.clone()),
            Some(vec![
                format!(
                    "Run `homeboy runner connect {}` for full diagnostics.",
                    selection.runner_id
                ),
                "Use --force-hot to run the command locally instead of offloading.".to_string(),
            ]),
        )),
    }
}

pub(super) fn resolve_lab_runner_selection(
    command: &LabOffloadCommand,
    explicit_runner: Option<&str>,
    force_hot: bool,
) -> Result<Option<LabRunnerSelection>> {
    let default_runner = if explicit_runner.is_none() && !force_hot && command.portable {
        super::resolve_default_lab_runner()?
    } else {
        None
    };

    resolve_lab_runner_selection_from_default(command, explicit_runner, force_hot, default_runner)
}

pub(super) fn resolve_lab_runner_selection_from_default(
    command: &LabOffloadCommand,
    explicit_runner: Option<&str>,
    force_hot: bool,
    default_runner: Option<String>,
) -> Result<Option<LabRunnerSelection>> {
    if let Some(runner_id) = explicit_runner {
        if !command.portable {
            let message = command.unsupported_reason.map_or_else(
                || "--runner is only supported for hot Lab-offload commands: lint, test, audit, bench, trace, and refactor source runs".to_string(),
                |reason| format!("--runner is unavailable for this hot command. {reason}"),
            );
            return Err(Error::validation_invalid_argument(
                "runner",
                message,
                Some(runner_id.to_string()),
                Some(vec!["Current Lab offload support: audit, bench run, full lint, full test, trace, and refactor source runs.".to_string()]),
            ));
        }

        return Ok(Some(LabRunnerSelection {
            runner_id: runner_id.to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: runner_status_tunnel_mode(runner_id),
        }));
    }

    if force_hot || !command.portable {
        return Ok(None);
    }

    default_runner
        .map(|runner_id| {
            Ok(LabRunnerSelection {
                mode: runner_status_tunnel_mode(&runner_id),
                runner_id,
                source: LabRunnerSelectionSource::Default,
            })
        })
        .transpose()
}

fn runner_status_tunnel_mode(runner_id: &str) -> RunnerTunnelMode {
    status(runner_id).map_or(RunnerTunnelMode::DirectSsh, |status| {
        status_tunnel_mode(&status)
    })
}

pub(super) fn status_tunnel_mode(status: &RunnerStatusReport) -> RunnerTunnelMode {
    status
        .session
        .as_ref()
        .map_or(RunnerTunnelMode::DirectSsh, |session| session.mode.clone())
}
