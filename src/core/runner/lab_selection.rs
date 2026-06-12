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
                    homeboy_build_identity: session.homeboy_build_identity,
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
            homeboy_build_identity: None,
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
        if let Some(reason) = connected_runner_not_ready_reason(&selection.runner_id, &status) {
            return automatic_fallback_or_explicit_error(
                selection,
                reason,
                format!(
                    "Lab offload runner `{}` is connected but is not ready for remote execution",
                    selection.runner_id
                ),
                format!(
                    "Run `homeboy runner connect {}` to refresh the runner daemon session.",
                    selection.runner_id
                ),
            );
        }
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
        return automatic_fallback_or_explicit_error(
            selection,
            reason,
            format!(
                "Lab offload requires reverse runner `{}` to have an active reverse session",
                selection.runner_id
            ),
            "Start the reverse runner session on the Lab machine before using --runner."
                .to_string(),
        );
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

    automatic_fallback_or_explicit_error(
        selection,
        reason,
        format!(
            "Lab offload could not connect runner `{}` before execution",
            selection.runner_id
        ),
        format!(
            "Run `homeboy runner connect {}` for full diagnostics.",
            selection.runner_id
        ),
    )
}

fn automatic_fallback_or_explicit_error(
    selection: &LabRunnerSelection,
    reason: String,
    explicit_message: String,
    remediation: String,
) -> Result<LabRunnerPreparation> {
    match selection.source {
        LabRunnerSelectionSource::Default => Ok(LabRunnerPreparation::FallBackLocal { reason }),
        LabRunnerSelectionSource::Explicit => Err(Error::validation_invalid_argument(
            "runner",
            format!("{explicit_message}: {reason}"),
            Some(selection.runner_id.clone()),
            Some(vec![
                remediation,
                "Use --force-hot to run the command locally instead of offloading.".to_string(),
            ]),
        )),
    }
}

fn connected_runner_not_ready_reason(
    runner_id: &str,
    status: &RunnerStatusReport,
) -> Option<String> {
    if let Some(warning) = status.stale_daemon.as_ref() {
        let restart = warning.recovery_commands.join(" && ");
        let restart = if restart.is_empty() {
            format!("homeboy runner disconnect {runner_id} && homeboy runner connect {runner_id}")
        } else {
            restart
        };
        return Some(format!(
            "connected runner `{runner_id}` daemon is stale: connected daemon reports {}, but the configured runner executable reports {}; restart the active daemon with `{restart}`",
            warning.session_homeboy_version, warning.current_homeboy_version
        ));
    }

    let session = status.session.as_ref()?;
    match session.mode {
        RunnerTunnelMode::DirectSsh if session.local_url.as_deref().unwrap_or("").is_empty() => {
            Some(format!(
                "direct SSH runner `{runner_id}` has no local daemon URL; reconnect it with `homeboy runner connect {runner_id}`"
            ))
        }
        RunnerTunnelMode::Reverse if session.broker_url.as_deref().unwrap_or("").is_empty() => {
            Some(format!(
                "reverse-connected runner `{runner_id}` has no broker URL; restart the reverse runner session before retrying"
            ))
        }
        _ => None,
    }
}

pub(super) fn resolve_lab_runner_selection(
    command: &LabOffloadCommand,
    explicit_runner: Option<&str>,
    force_hot: bool,
    allow_local_hot: bool,
) -> Result<Option<LabRunnerSelection>> {
    let deny_local_bench = crate::core::defaults::load_config()
        .bench
        .local_execution
        .is_denied();
    let default_runner = if explicit_runner.is_none() && command.portable {
        super::resolve_default_lab_runner()?
    } else {
        None
    };

    resolve_lab_runner_selection_from_default(
        command,
        explicit_runner,
        force_hot,
        allow_local_hot,
        deny_local_bench,
        default_runner,
    )
}

pub(super) fn resolve_lab_runner_selection_from_default(
    command: &LabOffloadCommand,
    explicit_runner: Option<&str>,
    force_hot: bool,
    allow_local_hot: bool,
    deny_local_bench: bool,
    default_runner: Option<String>,
) -> Result<Option<LabRunnerSelection>> {
    if let Some(runner_id) = explicit_runner {
        if !command.portable {
            let message = command.unsupported_reason.map_or_else(
                || "--runner is only supported for hot Lab-offload commands: agent-task dispatch/cook/loop/run-plan/status/logs/artifacts, lint, test, audit, bench, trace, and refactor source runs".to_string(),
                |reason| format!("--runner is unavailable for this hot command. {reason}"),
            );
            return Err(Error::validation_invalid_argument(
                "runner",
                message,
                Some(runner_id.to_string()),
                Some(vec!["Current Lab offload support: agent-task dispatch/cook/loop/run-plan/status/logs/artifacts, audit, bench run, full lint, full test, trace, and refactor source runs.".to_string()]),
            ));
        }

        return Ok(Some(LabRunnerSelection {
            runner_id: runner_id.to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: runner_status_tunnel_mode(runner_id),
        }));
    }

    if force_hot && command.portable && default_runner.is_some() && !allow_local_hot {
        let runner_id = default_runner.expect("checked above");
        return Err(Error::validation_invalid_argument(
            "force_hot",
            format!(
                "--force-hot would run portable hot command `{}` locally, but default Lab runner `{runner_id}` is available",
                command.hot_label
            ),
            Some(runner_id),
            Some(vec![
                "Omit --force-hot or pass --runner to offload the command to Lab.".to_string(),
                "Pass --force-hot --allow-local-hot only when you intentionally want this portable hot command to run on the controller machine.".to_string(),
            ]),
        ));
    }

    if force_hot || !command.portable {
        fail_if_local_bench_denied(command, deny_local_bench)?;
        return Ok(None);
    }

    if default_runner.is_none() {
        fail_if_local_bench_denied(command, deny_local_bench)?;
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

fn fail_if_local_bench_denied(command: &LabOffloadCommand, denied: bool) -> Result<()> {
    if !denied || command.hot_label != "bench" {
        return Ok(());
    }

    let config_path = crate::core::defaults::config_path()
        .unwrap_or_else(|_| "the global Homeboy config".to_string());
    Err(Error::validation_invalid_argument(
        "bench.local_execution",
        "Refusing to run `homeboy bench` locally because global config `/bench/local_execution` is `denied`",
        Some("denied".to_string()),
        Some(vec![
            "Configure `lab.preferred_runner`, or keep exactly one SSH Lab runner configured, then run `homeboy bench <component>` so Homeboy auto-routes the benchmark to Lab.".to_string(),
            "Use `--runner <runner-id>` only to override an ambiguous or non-default Lab selection.".to_string(),
            format!("Change `/bench/local_execution` in {config_path} to `allowed` before intentionally re-enabling local benchmark execution."),
        ]),
    ))
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
