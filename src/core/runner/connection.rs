use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::core::api_jobs::{ActiveRunnerJobSummary, JobClaimMetadata, JobStatus, RunnerJobSource};
use crate::core::daemon::DaemonStaleReasonCode;
use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::http_api::RunSummary;
use crate::core::paths;
use crate::core::server::{self, Server, SshClient};

use super::session::{
    ReverseRunnerConnectOptions, RunnerActiveJobError, RunnerActiveJobSource, RunnerActiveJobState,
    RunnerChangedRuntimePath, RunnerConnectReport, RunnerDisconnectReport, RunnerFailureKind,
    RunnerSession, RunnerSessionRole, RunnerSessionState, RunnerStaleDaemonWarning,
    RunnerStatusReport, RunnerTunnelMode,
};
use super::{broker_auth, broker_http};
use super::{load, remote_runner_homeboy_path, Runner, RunnerKind};

const REVERSE_RUNNER_HEARTBEAT_TTL: Duration = Duration::from_secs(90);

#[path = "connection_daemon.rs"]
mod connection_daemon;
use connection_daemon::{
    connect_remote_daemon, daemon_http_freshness, daemon_http_version, versions_match,
};
use connection_daemon::{daemon_http_identity, normalize_homeboy_version_owned};
use connection_daemon::{daemon_http_runtime_loaded_paths, daemon_http_runtime_stale_paths};

use super::daemon_http_get::daemon_get;

#[derive(Debug, Clone, Deserialize)]
struct CliEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
}

pub fn connect(runner_id: &str) -> Result<(RunnerConnectReport, i32)> {
    let runner = load(runner_id)?;
    let session_path = session_path(runner_id)?;
    let homeboy = remote_runner_homeboy_path(&runner, "runner connect")?;

    let Some((server_id, server, client)) = resolve_ssh_runner(&runner)? else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::SshFailure,
            "only SSH runners are supported by direct runner connect in this wave".to_string(),
        ));
    };

    let ssh_probe = client.execute("true");
    if !ssh_probe.success {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::SshFailure,
            command_failure_message("SSH connectivity check failed", &ssh_probe),
        ));
    }

    let identity = remote_homeboy_identity(&client, homeboy);
    let Ok(identity) = identity else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::MissingRemoteHomeboy,
            identity.err().unwrap(),
        ));
    };
    let version = identity.version.clone();

    let previous_session = read_session(runner_id)?;
    let daemon = ensure_remote_daemon(&client, homeboy, previous_session.as_ref());
    let Ok(daemon) = daemon else {
        return Ok(failed_connect(
            runner_id,
            session_path,
            RunnerFailureKind::DaemonStartupFailure,
            daemon.err().unwrap(),
        ));
    };

    let (local_port, tunnel_pid, local_url, daemon) = match connect_remote_daemon(
        &server,
        &client,
        homeboy,
        daemon,
        &version,
        &identity.display,
        runner_id,
        &session_path,
    ) {
        Ok(connection) => connection,
        Err(report) => return Ok(report),
    };

    let remote_daemon_lease_id = daemon.lease_id.clone();
    let session = RunnerSession {
        runner_id: runner.id.clone(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some(server_id),
        controller_id: None,
        broker_url: None,
        remote_daemon_address: Some(daemon.address),
        local_port: Some(local_port),
        local_url: Some(local_url),
        tunnel_pid,
        remote_daemon_pid: daemon.pid,
        remote_daemon_lease_id,
        homeboy_version: version,
        homeboy_build_identity: Some(identity.display),
        connected_at: Utc::now().to_rfc3339(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
    };
    write_session(&session)?;

    Ok((
        RunnerConnectReport {
            runner_id: runner.id,
            mode: Some(session.mode.clone()),
            role: Some(session.role.clone()),
            connected: true,
            recorded: None,
            local_url: session.local_url.clone(),
            broker_url: None,
            controller_id: None,
            remote_daemon_address: session.remote_daemon_address.clone(),
            tunnel_pid: session.tunnel_pid,
            remote_daemon_pid: session.remote_daemon_pid,
            homeboy_version: Some(session.homeboy_version.clone()),
            homeboy_build_identity: session.homeboy_build_identity.clone(),
            session_path: Some(session_path.display().to_string()),
            failure_kind: None,
            failure_message: None,
        },
        0,
    ))
}

pub fn connect_reverse(options: ReverseRunnerConnectOptions) -> Result<(RunnerConnectReport, i32)> {
    if options.runner_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "runner",
            "Reverse runner connect requires --reverse-runner <runner-id>",
            None,
            None,
        ));
    }
    if options.controller_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "controller",
            "Reverse runner connect requires a controller or broker ID",
            None,
            None,
        ));
    }

    let runner = load(&options.runner_id)?;
    let session_path = session_path(&runner.id)?;
    let homeboy_identity = crate::core::build_identity::current();
    let homeboy_version = homeboy_identity.version.clone();
    let now = Utc::now().to_rfc3339();
    let session = RunnerSession {
        runner_id: runner.id.clone(),
        mode: RunnerTunnelMode::Reverse,
        role: RunnerSessionRole::Runner,
        server_id: runner.server_id.clone(),
        controller_id: Some(options.controller_id.clone()),
        broker_url: options.broker_url.clone(),
        remote_daemon_address: None,
        local_port: None,
        local_url: None,
        tunnel_pid: None,
        remote_daemon_pid: None,
        remote_daemon_lease_id: None,
        homeboy_version,
        homeboy_build_identity: Some(homeboy_identity.display),
        connected_at: now.clone(),
        worker_identity: Some(format!("{}@{}", std::process::id(), hostname_fallback())),
        worker_pid: Some(std::process::id()),
        last_seen_at: Some(now),
    };
    write_session(&session)?;
    let broker_registered = match session.broker_url.as_deref() {
        Some(broker_url) => register_reverse_session_with_broker(broker_url, &session)?,
        None => false,
    };

    Ok((
        RunnerConnectReport {
            runner_id: runner.id,
            mode: Some(RunnerTunnelMode::Reverse),
            role: Some(RunnerSessionRole::Runner),
            connected: broker_registered,
            recorded: Some(true),
            local_url: None,
            broker_url: options.broker_url,
            controller_id: Some(options.controller_id),
            remote_daemon_address: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            homeboy_version: Some(session.homeboy_version),
            homeboy_build_identity: session.homeboy_build_identity,
            session_path: Some(session_path.display().to_string()),
            failure_kind: None,
            failure_message: None,
        },
        0,
    ))
}

fn register_reverse_session_with_broker(broker_url: &str, session: &RunnerSession) -> Result<bool> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build broker HTTP client: {err}")))?;
    let response = client
        .post(format!(
            "{}/runner/sessions",
            broker_url.trim_end_matches('/')
        ))
        .json(&serde_json::json!({
            "runner_id": session.runner_id,
            "controller_id": session.controller_id,
            "broker_url": session.broker_url,
            "homeboy_version": session.homeboy_version,
            "homeboy_build_identity": session.homeboy_build_identity,
            "worker_identity": session.worker_identity,
            "worker_pid": session.worker_pid,
            "last_seen_at": session.last_seen_at,
        }))
        .send()
        .map_err(|err| {
            Error::internal_unexpected(format!("register reverse runner session: {err}"))
        })?;
    let status_code = response.status().as_u16();
    let envelope: CliEnvelope = response.json().map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse runner session registration response".to_string()),
        )
    })?;
    if status_code >= 400 || !envelope.success {
        return Err(Error::internal_unexpected(format!(
            "reverse runner session registration failed: {}",
            envelope.error.unwrap_or(Value::Null)
        )));
    }
    Ok(true)
}

pub fn status(runner_id: &str) -> Result<RunnerStatusReport> {
    let runner = load(runner_id)?;
    let session_path = session_path(runner_id)?;
    let session = read_session(runner_id)?;
    let state = session_state(session.as_ref());
    let connected = state == RunnerSessionState::Connected;
    let stale_daemon = stale_daemon_warning(&runner, session.as_ref(), connected)?;
    let daemon_freshness = runner_daemon_freshness(&runner, session.as_ref(), connected)?;
    let active_job_source = session.as_ref().and_then(active_runner_job_source);
    let (active_jobs, stale_jobs, active_job_state, active_job_error) = if connected {
        match session.as_ref() {
            Some(session) => match runner_jobs(runner_id, session) {
                Ok((active_jobs, mut stale_jobs)) => {
                    stale_jobs.extend(orphaned_child_run_jobs(runner_id, session, &active_jobs));
                    (
                        active_jobs,
                        stale_jobs,
                        RunnerActiveJobState::Available,
                        None,
                    )
                }
                Err(err) => (
                    Vec::new(),
                    Vec::new(),
                    RunnerActiveJobState::Unavailable,
                    Some(RunnerActiveJobError {
                        code: err.code.as_str().to_string(),
                        message: err.message,
                    }),
                ),
            },
            None => (
                Vec::new(),
                Vec::new(),
                RunnerActiveJobState::NotQueried,
                None,
            ),
        }
    } else {
        (
            Vec::new(),
            Vec::new(),
            RunnerActiveJobState::NotQueried,
            None,
        )
    };
    let active_job_count = active_jobs.len();
    let stale_runner_job_count = stale_jobs.len();
    let active_runner_jobs = active_jobs.iter().map(Into::into).collect();
    let stale_runner_jobs = stale_jobs.iter().map(Into::into).collect();
    Ok(RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected,
        state,
        session,
        stale_daemon,
        daemon_freshness,
        active_jobs,
        active_runner_jobs,
        stale_runner_jobs,
        active_job_count,
        stale_runner_job_count,
        active_job_state,
        active_job_source,
        active_job_error,
        session_path: session_path.display().to_string(),
    })
}

fn runner_daemon_freshness(
    runner: &Runner,
    session: Option<&RunnerSession>,
    connected: bool,
) -> Result<Option<crate::core::daemon::DaemonFreshnessReport>> {
    if !connected || runner.kind != RunnerKind::Ssh {
        return Ok(None);
    }
    let Some(session) = session else {
        return Ok(None);
    };
    let Some(local_url) = session.local_url.as_deref() else {
        return Ok(None);
    };
    Ok(daemon_http_freshness(
        local_url,
        &session.homeboy_version,
        session.homeboy_build_identity.as_deref().unwrap_or(""),
    )
    .ok())
}

fn active_runner_job_source(session: &RunnerSession) -> Option<RunnerActiveJobSource> {
    if session.local_url.is_some() {
        Some(RunnerActiveJobSource::DirectDaemon)
    } else if session.mode == RunnerTunnelMode::Reverse && session.broker_url.is_some() {
        Some(RunnerActiveJobSource::ReverseBroker)
    } else {
        None
    }
}

fn runner_jobs(
    runner_id: &str,
    session: &RunnerSession,
) -> Result<(Vec<ActiveRunnerJobSummary>, Vec<ActiveRunnerJobSummary>)> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build active job client: {err}")))?;
    let (body, source) = if let Some(local_url) = session.local_url.as_deref() {
        let data = daemon_get(&client, local_url, "/jobs")?;
        (
            data.get("body").cloned().ok_or_else(|| {
                Error::internal_unexpected("daemon jobs response missing data.body")
            })?,
            RunnerJobSource::Daemon,
        )
    } else if session.mode == RunnerTunnelMode::Reverse {
        let Some(broker_url) = session.broker_url.as_deref() else {
            return Err(Error::internal_unexpected(format!(
                "reverse runner `{runner_id}` is connected but has no broker URL for active-job status"
            )));
        };
        (
            broker_http::get_json(
                &client,
                broker_url,
                "/jobs",
                "list reverse runner broker jobs",
                None,
            )?,
            RunnerJobSource::Broker,
        )
    } else {
        return Err(Error::internal_unexpected(format!(
            "runner `{runner_id}` is connected but has no active-job status endpoint"
        )));
    };
    let active_jobs = parse_runner_jobs(
        &body,
        "active_runner_jobs",
        "parse active runner jobs",
        runner_id,
        source,
    )?;
    let stale_jobs = parse_runner_jobs(
        &body,
        "stale_runner_jobs",
        "parse stale runner jobs",
        runner_id,
        source,
    )?;
    Ok((active_jobs, stale_jobs))
}

fn orphaned_child_run_jobs(
    runner_id: &str,
    session: &RunnerSession,
    active_jobs: &[ActiveRunnerJobSummary],
) -> Vec<ActiveRunnerJobSummary> {
    let Ok(runs) = runner_running_runs(session) else {
        return Vec::new();
    };
    runs.into_iter()
        .filter(|run| !child_run_has_active_job(run, active_jobs))
        .map(|run| orphaned_child_run_job(runner_id, run))
        .collect()
}

fn child_run_has_active_job(run: &RunSummary, active_jobs: &[ActiveRunnerJobSummary]) -> bool {
    active_jobs.iter().any(|job| {
        job.durable_run_id.as_deref() == Some(run.id.as_str())
            || job.command.contains(run.id.as_str())
    })
}

fn runner_running_runs(session: &RunnerSession) -> Result<Vec<RunSummary>> {
    let Some(local_url) = session.local_url.as_deref() else {
        return Ok(Vec::new());
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build runner runs client: {err}")))?;
    let data = daemon_get(&client, local_url, "/runs?status=running&limit=1000")?;
    let runs: Vec<RunSummary> =
        serde_json::from_value(data["body"]["runs"].clone()).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse runner daemon running runs".to_string()),
            )
        })?;
    Ok(runs
        .into_iter()
        .filter(|run| !is_synthetic_active_job_run_summary(run))
        .collect())
}

fn is_synthetic_active_job_run_summary(run: &RunSummary) -> bool {
    run.status_note
        .as_deref()
        .is_some_and(|note| note.starts_with("active runner job:"))
}

fn orphaned_child_run_job(runner_id: &str, run: RunSummary) -> ActiveRunnerJobSummary {
    ActiveRunnerJobSummary {
        runner_id: runner_id.to_string(),
        job_id: format!("orphaned-child-run-{}", run.id),
        operation: "child-run".to_string(),
        source: "runner-observation".to_string(),
        kind: run.kind,
        status: JobStatus::Failed,
        command: run
            .command
            .unwrap_or_else(|| format!("homeboy runs show {}", run.id)),
        cwd: run.cwd,
        started_at_ms: rfc3339_to_ms(&run.started_at).unwrap_or(0),
        updated_at_ms: 0,
        elapsed_ms: 0,
        heartbeat_age_ms: 0,
        claim: JobClaimMetadata {
            claim_id: None,
            claimed_by_runner_id: Some(runner_id.to_string()),
            claimed_at_ms: None,
            claim_expires_at_ms: None,
        },
        claim_expires_in_ms: None,
        lifecycle: None,
        durable_run_id: Some(run.id),
        stale_reason: Some("child_run_running_without_active_runner_job".to_string()),
        lifecycle_state: Some("recoverable_orphan".to_string()),
        retryable: Some(true),
        active_child_count: None,
        active_cell_count: None,
    }
}

fn rfc3339_to_ms(value: &str) -> Option<u64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|dt| u64::try_from(dt.timestamp_millis()).ok())
}

/// Parse a runner-jobs array (`key`) out of `body`, keep only jobs for
/// `runner_id`, and tag each with its originating `source`.
fn parse_runner_jobs(
    body: &Value,
    key: &str,
    error_context: &str,
    runner_id: &str,
    source: RunnerJobSource,
) -> Result<Vec<ActiveRunnerJobSummary>> {
    let jobs: Vec<ActiveRunnerJobSummary> = serde_json::from_value(
        body.get(key)
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .map_err(|err| Error::internal_json(err.to_string(), Some(error_context.to_string())))?;
    Ok(jobs
        .into_iter()
        .filter(|job| job.runner_id == runner_id)
        .map(|mut job| {
            job.source = source.to_string();
            job
        })
        .collect())
}

pub fn reverse_broker_reconcile(runner_id: &str) -> Result<Value> {
    let broker_url = reverse_broker_url(runner_id)?;
    let client = broker_client("build broker reconcile client")?;
    broker_http::post_json(
        &client,
        &broker_url,
        "/runner/jobs/reconcile",
        Value::Null,
        "reconcile reverse runner broker jobs",
        broker_auth::broker_submit_token_for_runner(runner_id)?.as_deref(),
    )
}

pub fn reverse_broker_artifact(runner_id: &str, job_id: &str, artifact_id: &str) -> Result<Value> {
    let broker_url = reverse_broker_url(runner_id)?;
    let artifact_id = crate::core::execution_contract::encode_uri_component(artifact_id);
    let client = broker_client("build broker artifact client")?;
    broker_http::get_json(
        &client,
        &broker_url,
        &format!("/runner/jobs/{job_id}/artifacts/{artifact_id}"),
        "lookup reverse runner broker artifact",
        broker_auth::broker_submit_token_for_runner(runner_id)?.as_deref(),
    )
}

fn reverse_broker_url(runner_id: &str) -> Result<String> {
    let report = status(runner_id)?;
    let Some(session) = report.session.filter(|_| report.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            format!("runner `{runner_id}` is not connected"),
            Some(runner_id.to_string()),
            None,
        ));
    };
    if session.mode != RunnerTunnelMode::Reverse {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            format!("runner `{runner_id}` is not reverse-connected"),
            Some(runner_id.to_string()),
            Some(vec![
                "Broker job wrappers require a reverse runner session.".to_string(),
            ]),
        ));
    }
    session.broker_url.ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner_id",
            format!("reverse runner `{runner_id}` has no broker URL"),
            Some(runner_id.to_string()),
            Some(vec![
                "Reconnect with `homeboy runner connect <controller-id> --reverse --reverse-runner <runner-id> --broker-url <url>`.".to_string(),
            ]),
        )
    })
}

fn broker_client(action: &str) -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("{action}: {err}")))
}

fn stale_daemon_warning(
    runner: &Runner,
    session: Option<&RunnerSession>,
    connected: bool,
) -> Result<Option<RunnerStaleDaemonWarning>> {
    if !connected || runner.kind != RunnerKind::Ssh {
        return Ok(None);
    }
    let Some(session) = session else {
        return Ok(None);
    };
    if session.mode != RunnerTunnelMode::DirectSsh {
        return Ok(None);
    }
    let homeboy = remote_runner_homeboy_path(runner, "runner status stale-daemon diagnostics")?;
    let Some((_server_id, _server, client)) = resolve_ssh_runner(runner)? else {
        return Ok(None);
    };
    let current_identity = match remote_homeboy_identity(&client, homeboy) {
        Ok(identity) => identity,
        Err(_) => return Ok(None),
    };
    let current_version = current_identity.version.clone();
    let observed_session_version = session
        .local_url
        .as_deref()
        .and_then(|local_url| daemon_http_version(local_url).ok())
        .unwrap_or_else(|| session.homeboy_version.clone());
    let daemon_identity = session
        .local_url
        .as_deref()
        .and_then(|local_url| daemon_http_identity(local_url).ok())
        .filter(|identity| !identity.trim().is_empty());
    let session_identity = daemon_identity.or_else(|| session.homeboy_build_identity.clone());
    let stale_runtime_paths = session
        .local_url
        .as_deref()
        .and_then(|local_url| daemon_http_runtime_stale_paths(local_url).ok())
        .unwrap_or_default();
    let changed_runtime_paths = session
        .local_url
        .as_deref()
        .and_then(|local_url| daemon_http_runtime_loaded_paths(local_url).ok())
        .map(|loaded| changed_runtime_paths(&runner.env, &loaded))
        .unwrap_or_default();
    if versions_match(&observed_session_version, &current_version)
        && versions_match(&session.homeboy_version, &current_version)
        && identities_match(session_identity.as_deref(), Some(&current_identity.display))
        && stale_runtime_paths.is_empty()
        && changed_runtime_paths.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(
        RunnerStaleDaemonWarning::new(
            &runner.id,
            observed_session_version,
            current_version,
            session_identity,
            Some(current_identity.display),
        )
        .with_runtime_paths(&runner.id, stale_runtime_paths, changed_runtime_paths),
    ))
}

fn changed_runtime_paths(
    runner_env: &std::collections::HashMap<String, String>,
    loaded_paths: &BTreeMap<String, String>,
) -> Vec<RunnerChangedRuntimePath> {
    let current_paths: BTreeMap<String, String> = runner_env
        .iter()
        .filter(|(name, value)| is_runtime_path_env(name) && !value.trim().is_empty())
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut names: std::collections::BTreeSet<String> = loaded_paths.keys().cloned().collect();
    names.extend(current_paths.keys().cloned());
    names
        .into_iter()
        .filter_map(|env| {
            let loaded_path = loaded_paths.get(&env).cloned();
            let configured_path = current_paths.get(&env).cloned();
            if loaded_path == configured_path {
                None
            } else {
                Some(RunnerChangedRuntimePath {
                    env,
                    loaded_path,
                    configured_path,
                })
            }
        })
        .collect()
}

fn is_runtime_path_env(name: &str) -> bool {
    name.starts_with("HOMEBOY_")
        && [
            "_COMPONENT_PATH",
            "_PLUGIN_PATH",
            "_PROVIDER_PATH",
            "_RUNTIME_PATH",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

pub fn statuses() -> Result<Vec<RunnerStatusReport>> {
    let mut reports = Vec::new();
    for runner in super::list()? {
        reports.push(status(&runner.id)?);
    }
    Ok(reports)
}

pub fn disconnect(runner_id: &str) -> Result<RunnerDisconnectReport> {
    let runner = load(runner_id)?;
    let session_path = session_path(runner_id)?;
    let session = read_session(runner_id)?;
    if let Some(session) = &session {
        if session.mode == RunnerTunnelMode::DirectSsh {
            if let Err(err) = disconnect_remote_daemon(&runner, session) {
                log_status!(
                    "runner",
                    "Could not stop remote daemon for runner `{runner_id}` during disconnect: {err}"
                );
            }
        }
        if let Some(pid) = session.tunnel_pid {
            terminate_pid(pid);
        }
    }
    if session_path.exists() {
        std::fs::remove_file(&session_path).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("delete {}", session_path.display())),
            )
        })?;
    }
    Ok(RunnerDisconnectReport {
        runner_id: runner_id.to_string(),
        disconnected: session.is_some(),
        session,
        session_path: session_path.display().to_string(),
    })
}

fn disconnect_remote_daemon(
    runner: &Runner,
    session: &RunnerSession,
) -> std::result::Result<(), String> {
    let Some((_, _, client)) =
        remote_daemon::resolve_ssh_runner(runner).map_err(|err| err.message)?
    else {
        return Ok(());
    };
    let homeboy =
        remote_runner_homeboy_path(runner, "runner disconnect").map_err(|err| err.message)?;
    let output = client.execute(&remote_daemon_disconnect_command(
        homeboy,
        session.remote_daemon_pid,
    ));
    if !output.success {
        return Err(command_failure_message(
            "remote daemon stop failed while disconnecting runner",
            &output,
        ));
    }
    Ok(())
}

fn remote_daemon_disconnect_command(homeboy: &str, remote_daemon_pid: Option<u32>) -> String {
    let mut command = format!(
        "{} daemon stop >/dev/null 2>&1 || true",
        shell::quote_arg(homeboy)
    );
    if let Some(pid) = remote_daemon_pid {
        command.push_str("\n");
        command.push_str(&format!("pid={pid}\n"));
        command.push_str("if [ -r \"/proc/$pid/cmdline\" ]; then\n");
        command
            .push_str("  cmdline=$(tr '\\0' ' ' < \"/proc/$pid/cmdline\" 2>/dev/null || true)\n");
        command.push_str("  case \"$cmdline\" in\n");
        command.push_str("    *\" daemon serve \"*)\n");
        command.push_str("      kill -TERM \"$pid\" 2>/dev/null || true\n");
        command.push_str("      i=0\n");
        command.push_str("      while kill -0 \"$pid\" 2>/dev/null && [ \"$i\" -lt 20 ]; do\n");
        command.push_str("        i=$((i + 1))\n");
        command.push_str("        sleep 0.1\n");
        command.push_str("      done\n");
        command.push_str("      if kill -0 \"$pid\" 2>/dev/null; then\n");
        command.push_str("        kill -KILL \"$pid\" 2>/dev/null || true\n");
        command.push_str("      fi\n");
        command.push_str("      ;;\n");
        command.push_str("  esac\n");
        command.push_str("fi");
    }
    command
}

mod remote_daemon {
    use super::*;

    pub(super) fn resolve_ssh_runner(
        runner: &Runner,
    ) -> Result<Option<(String, Server, SshClient)>> {
        if runner.kind != RunnerKind::Ssh {
            return Ok(None);
        }
        let server_id = runner.server_id.clone().ok_or_else(|| {
            Error::validation_invalid_argument(
                "server_id",
                "SSH runner requires server_id",
                Some(runner.id.clone()),
                None,
            )
        })?;
        let server = server::load(&server_id)?;
        let mut client = SshClient::from_server(&server, &server_id)?;
        client.env.extend(runner.env.clone());
        Ok(Some((server_id, server, client)))
    }

    pub(super) fn remote_homeboy_version(
        client: &SshClient,
        homeboy: &str,
    ) -> std::result::Result<String, String> {
        let command = format!("{} --version", shell::quote_arg(homeboy));
        let output = client.execute(&command);
        if !output.success {
            return Err(command_failure_message(
                "remote Homeboy version check failed",
                &output,
            ));
        }
        let version = output.stdout.trim().to_string();
        if version.is_empty() {
            return Err("remote Homeboy version check returned empty output".to_string());
        }
        Ok(version)
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct RemoteHomeboyIdentity {
        pub(super) version: String,
        pub(super) display: String,
    }

    pub(super) fn remote_homeboy_identity(
        client: &SshClient,
        homeboy: &str,
    ) -> std::result::Result<RemoteHomeboyIdentity, String> {
        let command = format!("{} self identity", shell::quote_arg(homeboy));
        let output = client.execute(&command);
        if output.success {
            if let Some(identity) = parse_self_identity_output(&output.stdout) {
                return Ok(identity);
            }
        }

        let version = remote_homeboy_version(client, homeboy)?;
        Ok(RemoteHomeboyIdentity {
            version: normalize_homeboy_version_owned(&version),
            display: version,
        })
    }

    pub(super) fn parse_self_identity_output(output: &str) -> Option<RemoteHomeboyIdentity> {
        let body: Value = serde_json::from_str(output.trim()).ok()?;
        let data = body.get("data").unwrap_or(&body);
        let version = data.get("version")?.as_str()?.trim();
        let display = data
            .get("display")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(version);
        if version.is_empty() {
            return None;
        }
        Some(RemoteHomeboyIdentity {
            version: version.to_string(),
            display: display.to_string(),
        })
    }

    pub(super) fn identities_match(left: Option<&str>, right: Option<&str>) -> bool {
        match (left, right) {
            (Some(left), Some(right)) => versions_match(left, right),
            _ => false,
        }
    }

    pub(super) struct SshTunnelOutput {
        pub(super) pid: Option<u32>,
        pub(super) stderr: String,
        pub(super) success: bool,
    }

    pub(super) fn open_loopback_tunnel(
        server: &Server,
        local_port: u16,
        remote_host: &str,
        remote_port: u16,
    ) -> SshTunnelOutput {
        if is_loopback_host(&server.host) {
            return SshTunnelOutput {
                pid: None,
                stderr: String::new(),
                success: true,
            };
        }

        let mut args = crate::core::server::ssh_args::server_option_args(
            server,
            crate::core::server::ssh_args::SshArgOptions {
                batch_mode: true,
                connect_timeout: true,
                exit_on_forward_failure: true,
                port_flag: Some(crate::core::server::ssh_args::SshPortFlag::Lowercase),
                ..crate::core::server::ssh_args::SshArgOptions::default()
            },
        );
        args.extend([
            "-N".to_string(),
            "-L".to_string(),
            format!("127.0.0.1:{}:{}:{}", local_port, remote_host, remote_port),
            format!("{}@{}", server.user, server.host),
        ]);

        let child = std::process::Command::new("ssh")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match child {
            Ok(child) => SshTunnelOutput {
                pid: Some(child.id()),
                stderr: String::new(),
                success: true,
            },
            Err(err) => SshTunnelOutput {
                pid: None,
                stderr: format!("SSH tunnel error: {}", err),
                success: false,
            },
        }
    }

    #[derive(Debug, Clone)]
    pub(super) struct RemoteDaemon {
        pub(super) address: String,
        pub(super) pid: Option<u32>,
        pub(super) lease_id: Option<String>,
    }

    #[derive(Debug, Clone)]
    pub(super) struct RemoteDaemonStatus {
        pub(super) daemon: Option<RemoteDaemon>,
        pub(super) stale_reason: Option<String>,
        pub(super) stale_reason_code: Option<DaemonStaleReasonCode>,
        pub(super) fresh: bool,
        pub(super) reachable: bool,
        pub(super) active_jobs: usize,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum RemoteDaemonConnectAction {
        Reattach,
        Start,
    }

    pub(super) fn ensure_remote_daemon(
        client: &SshClient,
        homeboy: &str,
        previous_session: Option<&RunnerSession>,
    ) -> std::result::Result<RemoteDaemon, String> {
        let status = remote_daemon_status(client, homeboy)?;
        match remote_daemon_connect_action(previous_session, &status)? {
            RemoteDaemonConnectAction::Reattach => {
                return status.daemon.ok_or_else(|| {
                    "remote daemon reattach selected without a daemon lease".to_string()
                });
            }
            RemoteDaemonConnectAction::Start => {
                return remote_daemon_ensure_running(client, homeboy)
            }
        }
    }

    pub(super) fn remote_daemon_connect_action(
        previous_session: Option<&RunnerSession>,
        status: &RemoteDaemonStatus,
    ) -> std::result::Result<RemoteDaemonConnectAction, String> {
        let healthy = status.fresh && status.reachable;
        if status.active_jobs > 0
            && !healthy
            && !remote_daemon_pid_dead_with_owned_lease(previous_session, status)
        {
            return Err(format!(
                "remote daemon has {} active job(s) but cannot be safely reattached (fresh={}, reachable={}, reason={:?}); refusing implicit replacement",
                status.active_jobs, status.fresh, status.reachable, status.stale_reason
            ));
        }

        let Some(daemon) = status.daemon.as_ref() else {
            return Ok(RemoteDaemonConnectAction::Start);
        };

        if healthy {
            if let Some(session) = previous_session.filter(|session| {
                session.mode == RunnerTunnelMode::DirectSsh
                    && session.role == RunnerSessionRole::Controller
            }) {
                if session.remote_daemon_lease_id.is_none() {
                    if session.remote_daemon_pid == daemon.pid
                        && session.remote_daemon_address.as_deref() == Some(daemon.address.as_str())
                    {
                        return Ok(RemoteDaemonConnectAction::Reattach);
                    }
                    return Err("persisted direct-SSH runner session has no daemon lease and does not match the live daemon PID/address; refusing replacement".to_string());
                }
                let expected_lease = session.remote_daemon_lease_id.as_deref().expect("checked");
                let actual_lease = daemon.lease_id.as_deref().ok_or_else(|| {
                    "live remote daemon did not report a lease; refusing to replace it".to_string()
                })?;
                if expected_lease != actual_lease {
                    return Err(format!(
                        "live remote daemon lease `{actual_lease}` does not match persisted session lease `{expected_lease}`; refusing to replace the live daemon"
                    ));
                }
            }
            return Ok(RemoteDaemonConnectAction::Reattach);
        }

        Ok(RemoteDaemonConnectAction::Start)
    }

    fn remote_daemon_pid_dead_with_owned_lease(
        previous_session: Option<&RunnerSession>,
        status: &RemoteDaemonStatus,
    ) -> bool {
        if status.stale_reason_code != Some(DaemonStaleReasonCode::PidDead) {
            return false;
        }
        let Some(daemon) = status.daemon.as_ref() else {
            return false;
        };
        let Some(lease_id) = daemon.lease_id.as_deref() else {
            return false;
        };
        let Some(session) = previous_session.filter(|session| {
            session.mode == RunnerTunnelMode::DirectSsh
                && session.role == RunnerSessionRole::Controller
        }) else {
            return true;
        };

        match session.remote_daemon_lease_id.as_deref() {
            Some(expected_lease) => expected_lease == lease_id,
            None => {
                session.remote_daemon_pid == daemon.pid
                    && session.remote_daemon_address.as_deref() == Some(daemon.address.as_str())
            }
        }
    }

    pub(super) fn remote_daemon_status(
        client: &SshClient,
        homeboy: &str,
    ) -> std::result::Result<RemoteDaemonStatus, String> {
        let command = format!("{} daemon status", shell::quote_arg(homeboy));
        let output = client.execute(&command);
        if !output.success {
            return Err(command_failure_message(
                "remote daemon status failed",
                &output,
            ));
        }
        let envelope = parse_envelope(&output.stdout)
            .map_err(|err| format!("remote daemon status returned invalid JSON: {}", err))?;
        if !envelope.success {
            return Err(format!(
                "remote daemon status returned an error: {}",
                envelope.error.unwrap_or(Value::Null)
            ));
        }
        let data = envelope
            .data
            .ok_or_else(|| "remote daemon status returned no data".to_string())?;
        let stale_reason = data
            .get("stale_reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        let stale_reason_code = data
            .pointer("/freshness/stale_reason_code")
            .cloned()
            .and_then(|code| serde_json::from_value(code).ok());
        if !data
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(RemoteDaemonStatus {
                daemon: data.get("state").map(remote_daemon_from_state),
                stale_reason,
                stale_reason_code,
                fresh: data.get("fresh").and_then(Value::as_bool).unwrap_or(false),
                reachable: data
                    .get("reachable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                active_jobs: remote_daemon_active_jobs(&data),
            });
        }
        let Some(state) = data.get("state") else {
            return Ok(RemoteDaemonStatus {
                daemon: None,
                stale_reason: Some(
                    stale_reason
                        .unwrap_or_else(|| "remote daemon status has no lease state".to_string()),
                ),
                stale_reason_code,
                fresh: data.get("fresh").and_then(Value::as_bool).unwrap_or(false),
                reachable: data
                    .get("reachable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                active_jobs: remote_daemon_active_jobs(&data),
            });
        };
        Ok(RemoteDaemonStatus {
            daemon: Some(remote_daemon_from_state(state)),
            stale_reason,
            stale_reason_code,
            fresh: data.get("fresh").and_then(Value::as_bool).unwrap_or(false),
            reachable: data
                .get("reachable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            active_jobs: remote_daemon_active_jobs(&data),
        })
    }

    fn remote_daemon_from_state(state: &Value) -> RemoteDaemon {
        RemoteDaemon {
            address: state
                .get("address")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            pid: state
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok()),
            lease_id: state
                .get("lease_id")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }

    pub(super) fn remote_daemon_active_jobs(data: &Value) -> usize {
        data.pointer("/freshness/active_jobs")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0)
    }

    pub(super) fn remote_daemon_ensure_running(
        client: &SshClient,
        homeboy: &str,
    ) -> std::result::Result<RemoteDaemon, String> {
        let command = format!(
            "{} daemon ensure-running --addr 127.0.0.1:0",
            shell::quote_arg(homeboy)
        );
        let output = client.execute(&command);
        if !output.success {
            return Err(command_failure_message(
                "remote daemon ensure-running failed",
                &output,
            ));
        }
        let envelope = parse_envelope(&output.stdout).map_err(|err| {
            format!(
                "remote daemon ensure-running returned invalid JSON: {}",
                err
            )
        })?;
        if !envelope.success {
            return Err(format!(
                "remote daemon ensure-running failed: {}",
                envelope.error.unwrap_or(Value::Null)
            ));
        }
        let data = envelope
            .data
            .ok_or_else(|| "remote daemon ensure-running returned no data".to_string())?;
        Ok(RemoteDaemon {
            address: data
                .get("address")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            pid: data
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok()),
            lease_id: data
                .get("lease_id")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    pub(super) fn parse_envelope(stdout: &str) -> serde_json::Result<CliEnvelope> {
        parse_json_from_mixed_stdout(stdout)
    }

    pub(crate) fn parse_json_from_mixed_stdout<T>(stdout: &str) -> serde_json::Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        match serde_json::from_str(stdout.trim()) {
            Ok(value) => Ok(value),
            Err(original) => {
                for (index, ch) in stdout.char_indices() {
                    if ch != '{' {
                        continue;
                    }
                    let mut stream =
                        serde_json::Deserializer::from_str(&stdout[index..]).into_iter();
                    if let Some(Ok(value)) = stream.next() {
                        return Ok(value);
                    }
                }
                Err(original)
            }
        }
    }

    pub(super) fn parse_loopback_daemon_addr(address: &str) -> std::result::Result<SocketAddr, ()> {
        let addr: SocketAddr = address.parse().map_err(|_| ())?;
        if addr.ip().is_loopback() {
            Ok(addr)
        } else {
            Err(())
        }
    }

    pub(super) fn reserve_loopback_port() -> Result<u16> {
        let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("reserve local tunnel port".to_string()),
            )
        })?;
        let port = listener
            .local_addr()
            .map_err(|err| {
                Error::internal_io(err.to_string(), Some("read local tunnel port".to_string()))
            })?
            .port();
        drop(listener);
        Ok(port)
    }

    pub(super) fn wait_for_tcp(port: u16, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }
        false
    }
}
use remote_daemon::*;

mod session_store {
    use super::*;

    pub(super) fn session_is_live(session: &RunnerSession) -> bool {
        if session.mode != RunnerTunnelMode::DirectSsh {
            return false;
        }
        if let Some(pid) = session.tunnel_pid {
            if !crate::core::process::pid_is_running(pid) {
                return false;
            }
        }
        session
            .local_port
            .is_some_and(|port| wait_for_tcp(port, Duration::from_millis(200)))
    }

    pub(super) fn reverse_controller_session_is_live(session: &RunnerSession) -> bool {
        let Some(last_seen_at) = session.last_seen_at.as_deref() else {
            return false;
        };
        let Ok(last_seen_at) = DateTime::parse_from_rfc3339(last_seen_at) else {
            return false;
        };
        let age = Utc::now().signed_duration_since(last_seen_at.with_timezone(&Utc));
        match age.to_std() {
            Ok(age) => age <= REVERSE_RUNNER_HEARTBEAT_TTL,
            Err(_) => true,
        }
    }

    pub(super) fn session_state(session: Option<&RunnerSession>) -> RunnerSessionState {
        match session {
            Some(session)
                if session.mode == RunnerTunnelMode::Reverse
                    && session.role == RunnerSessionRole::Controller =>
            {
                if reverse_controller_session_is_live(session) {
                    RunnerSessionState::Connected
                } else {
                    RunnerSessionState::Recorded
                }
            }
            Some(session) if session.mode == RunnerTunnelMode::Reverse => {
                RunnerSessionState::Recorded
            }
            Some(session) if session_is_live(session) => RunnerSessionState::Connected,
            Some(_) => RunnerSessionState::Disconnected,
            None => RunnerSessionState::Disconnected,
        }
    }

    pub(super) fn hostname_fallback() -> String {
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "unknown-host".to_string())
    }

    pub(super) fn session_path(runner_id: &str) -> Result<PathBuf> {
        paths::runner_session_file(runner_id)
    }

    pub(super) fn read_session(runner_id: &str) -> Result<Option<RunnerSession>> {
        let path = session_path(runner_id)?;
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("read {}", path.display())))
        })?;
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|err| Error::config_invalid_json(path.display().to_string(), err))
    }

    pub(super) fn write_session(session: &RunnerSession) -> Result<()> {
        let path = session_path(&session.runner_id)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!("create {}", parent.display())),
                )
            })?;
        }
        let body = serde_json::to_string_pretty(session).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("serialize runner session".to_string()),
            )
        })?;
        std::fs::write(&path, body).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("write {}", path.display())))
        })
    }

    pub(super) fn failed_connect(
        runner_id: &str,
        session_path: PathBuf,
        failure_kind: RunnerFailureKind,
        failure_message: String,
    ) -> (RunnerConnectReport, i32) {
        (
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
                session_path: Some(session_path.display().to_string()),
                failure_kind: Some(failure_kind),
                failure_message: Some(failure_message),
            },
            20,
        )
    }

    pub(super) fn command_failure_message(
        prefix: &str,
        output: &crate::core::server::CommandOutput,
    ) -> String {
        format!(
            "{} (exit {}): stdout={}, stderr={}",
            prefix,
            output.exit_code,
            output.stdout.trim(),
            output.stderr.trim()
        )
    }

    pub(super) fn is_loopback_host(host: &str) -> bool {
        matches!(host, "localhost" | "127.0.0.1" | "::1")
    }

    pub(super) fn terminate_pid(pid: u32) {
        if pid > i32::MAX as u32 {
            return;
        }
        #[cfg(unix)]
        unsafe {
            let _ = libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}
use session_store::*;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::session::RunnerStaleRuntimePath;
    use super::connection_daemon::{
        daemon_identity_from_body, daemon_runtime_loaded_paths_from_body,
        daemon_runtime_stale_paths_from_body, daemon_version_from_body, versions_match,
    };
    use super::*;
    use crate::test_support;

    #[test]
    fn rejects_non_loopback_remote_daemon_address() {
        assert!(parse_loopback_daemon_addr("0.0.0.0:1234").is_err());
        assert!(parse_loopback_daemon_addr("127.0.0.1:1234").is_ok());
    }

    #[test]
    fn parses_daemon_status_envelope() {
        let envelope = parse_envelope(
            r#"{"success":true,"data":{"action":"status","running":true,"state":{"address":"127.0.0.1:49152","pid":123}}}"#,
        )
        .expect("parse envelope");

        assert!(envelope.success);
        assert_eq!(
            envelope
                .data
                .unwrap()
                .get("state")
                .unwrap()
                .get("address")
                .unwrap(),
            "127.0.0.1:49152"
        );
    }

    #[test]
    fn reads_remote_active_job_count_from_daemon_freshness() {
        let status = serde_json::json!({
            "freshness": { "active_jobs": 2 }
        });

        assert_eq!(remote_daemon_active_jobs(&status), 2);
    }

    #[test]
    fn reattaches_active_daemon_without_changing_lease_or_pid() {
        let session = direct_ssh_session("lease-active");
        let status = remote_daemon_status_for_test(true, true, 1, "lease-active", 4242);

        let action = remote_daemon_connect_action(Some(&session), &status).expect("reattach");

        assert_eq!(action, RemoteDaemonConnectAction::Reattach);
        let daemon = status.daemon.expect("daemon");
        assert_eq!(daemon.lease_id.as_deref(), Some("lease-active"));
        assert_eq!(daemon.pid, Some(4242));
        assert_eq!(
            status.active_jobs, 1,
            "active job must not trigger replacement"
        );
    }

    #[test]
    fn tunnel_only_failure_reattaches_the_persisted_daemon() {
        let mut session = direct_ssh_session("lease-tunnel");
        session.tunnel_pid = Some(999_999);
        session.local_url = Some("http://127.0.0.1:1".to_string());
        let status = remote_daemon_status_for_test(true, true, 0, "lease-tunnel", 4343);

        assert_eq!(
            remote_daemon_connect_action(Some(&session), &status).expect("reattach"),
            RemoteDaemonConnectAction::Reattach
        );
    }

    #[test]
    fn stale_daemon_without_jobs_routes_to_safe_replacement() {
        let status = remote_daemon_status_for_test(false, true, 0, "lease-stale", 4444);

        assert_eq!(
            remote_daemon_connect_action(None, &status).expect("replace stale daemon"),
            RemoteDaemonConnectAction::Start
        );
    }

    #[test]
    fn refuses_to_replace_non_dead_stale_daemon_with_active_jobs() {
        let status = remote_daemon_status_for_test_with_reason(
            false,
            true,
            1,
            "lease-busy",
            4545,
            Some(DaemonStaleReasonCode::VersionMismatch),
        );

        let err = remote_daemon_connect_action(None, &status).expect_err("refuse replacement");

        assert!(err.contains("1 active job(s)"));
        assert!(err.contains("refusing implicit replacement"));
    }

    #[test]
    fn proven_dead_daemon_with_active_jobs_and_matching_lease_routes_to_startup_reconciliation() {
        let session = direct_ssh_session("lease-dead");
        let status = remote_daemon_status_for_test_with_reason(
            false,
            false,
            1,
            "lease-dead",
            4545,
            Some(DaemonStaleReasonCode::PidDead),
        );

        assert_eq!(
            remote_daemon_connect_action(Some(&session), &status).expect("ensure start"),
            RemoteDaemonConnectAction::Start
        );
    }

    #[test]
    fn refuses_to_replace_proven_dead_daemon_with_active_jobs_when_lease_mismatches() {
        let session = direct_ssh_session("lease-recorded");
        let status = remote_daemon_status_for_test_with_reason(
            false,
            false,
            1,
            "lease-dead",
            4545,
            Some(DaemonStaleReasonCode::PidDead),
        );

        let err = remote_daemon_connect_action(Some(&session), &status)
            .expect_err("refuse mismatched dead lease");

        assert!(err.contains("1 active job(s)"));
        assert!(err.contains("refusing implicit replacement"));
    }

    #[test]
    fn dead_recorded_daemon_routes_to_idempotent_ensure_start() {
        let status = RemoteDaemonStatus {
            daemon: None,
            stale_reason: Some("daemon lease pid is not running".to_string()),
            stale_reason_code: Some(DaemonStaleReasonCode::PidDead),
            fresh: false,
            reachable: false,
            active_jobs: 0,
        };

        assert_eq!(
            remote_daemon_connect_action(Some(&direct_ssh_session("lease-dead")), &status)
                .expect("ensure start"),
            RemoteDaemonConnectAction::Start
        );
    }

    #[test]
    fn first_connect_routes_to_idempotent_ensure_start_when_no_daemon_exists() {
        let status = RemoteDaemonStatus {
            daemon: None,
            stale_reason: None,
            stale_reason_code: None,
            fresh: false,
            reachable: false,
            active_jobs: 0,
        };

        assert_eq!(
            remote_daemon_connect_action(None, &status).expect("ensure start"),
            RemoteDaemonConnectAction::Start
        );
    }

    #[test]
    fn refuses_to_replace_live_daemon_with_a_different_persisted_lease() {
        let session = direct_ssh_session("lease-recorded");
        let status = remote_daemon_status_for_test(true, true, 0, "lease-live", 4646);

        let err =
            remote_daemon_connect_action(Some(&session), &status).expect_err("lease mismatch");

        assert!(err.contains("does not match persisted session lease"));
        assert!(err.contains("refusing to replace"));
    }

    #[test]
    fn legacy_session_adopts_consistent_live_daemon_identity() {
        let mut session = direct_ssh_session("lease-old");
        session.remote_daemon_lease_id = None;
        let status = remote_daemon_status_for_test(true, true, 1, "lease-live", 4242);

        assert_eq!(
            remote_daemon_connect_action(Some(&session), &status).expect("adopt live lease"),
            RemoteDaemonConnectAction::Reattach
        );
    }

    #[test]
    fn legacy_session_refuses_pid_reuse_with_different_daemon_identity() {
        let mut session = direct_ssh_session("lease-old");
        session.remote_daemon_lease_id = None;
        let status = remote_daemon_status_for_test(true, true, 0, "lease-live", 5555);

        assert!(remote_daemon_connect_action(Some(&session), &status)
            .expect_err("identity mismatch")
            .contains("does not match the live daemon PID/address"));
    }

    #[test]
    fn parses_daemon_envelope_after_noisy_stdout_preamble() {
        let envelope = parse_envelope(
            "Setting up Swift test infrastructure...\nSwift unavailable; Swift extension installed but not ready...\n{\"success\":true,\"data\":{\"action\":\"start\",\"address\":\"127.0.0.1:49152\",\"pid\":123,\"lease_id\":\"lease-1\"}}\n",
        )
        .expect("parse envelope after preamble");

        assert!(envelope.success);
        assert_eq!(
            envelope
                .data
                .unwrap()
                .get("address")
                .and_then(Value::as_str),
            Some("127.0.0.1:49152")
        );
    }

    #[test]
    fn compares_cli_and_daemon_version_shapes() {
        assert!(versions_match("homeboy 0.204.0", "0.204.0"));
        assert!(versions_match("0.204.0", "homeboy 0.204.0"));
        assert!(versions_match(
            "homeboy 0.204.0+19a41cd5102d",
            "0.204.0+19a41cd5102d"
        ));
        assert!(!versions_match("homeboy 0.201.3", "homeboy 0.204.0"));
    }

    #[test]
    fn extracts_current_daemon_version_shape() {
        assert_eq!(
            daemon_version_from_body(&serde_json::json!({"version":"0.204.0"})),
            Some("0.204.0")
        );
        assert_eq!(
            daemon_version_from_body(&serde_json::json!({
                "success": true,
                "data": {"version": "0.281.2"}
            })),
            Some("0.281.2")
        );
        assert_eq!(
            daemon_identity_from_body(
                &serde_json::json!({"version":"0.228.13","build_identity":{"display":"homeboy 0.228.13+f7569a5e"}})
            ),
            Some("homeboy 0.228.13+f7569a5e")
        );
        assert_eq!(
            daemon_identity_from_body(&serde_json::json!({
                "success": true,
                "data": {
                    "build_identity": {"display": "homeboy 0.281.2+b078972b3edd"}
                }
            })),
            Some("homeboy 0.281.2+b078972b3edd")
        );
        assert_eq!(
            daemon_identity_from_body(&serde_json::json!({"version":"0.228.13"})),
            None
        );
    }

    #[test]
    fn parses_self_identity_json_envelope() {
        let identity = parse_self_identity_output(
            r#"{"success":true,"data":{"version":"0.228.13","display":"homeboy 0.228.13+19a41cd5102d"}}"#,
        )
        .expect("identity");

        assert_eq!(identity.version, "0.228.13");
        assert_eq!(identity.display, "homeboy 0.228.13+19a41cd5102d");
    }

    #[test]
    fn stale_daemon_warning_includes_ordered_restart_recovery_commands() {
        let warning = RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "homeboy 0.201.3".to_string(),
            "homeboy 0.204.0".to_string(),
            Some("homeboy 0.201.3+old".to_string()),
            Some("homeboy 0.204.0+new".to_string()),
        );

        assert_eq!(warning.session_homeboy_version, "homeboy 0.201.3");
        assert_eq!(warning.current_homeboy_version, "homeboy 0.204.0");
        assert_eq!(warning.severity, "warning");
        assert_eq!(
            warning.active_daemon_control_plane_version,
            "homeboy 0.201.3"
        );
        assert_eq!(warning.job_command_binary_version, "homeboy 0.204.0");
        assert_eq!(
            warning.session_homeboy_build_identity.as_deref(),
            Some("homeboy 0.201.3+old")
        );
        assert_eq!(
            warning
                .active_daemon_control_plane_build_identity
                .as_deref(),
            Some("homeboy 0.201.3+old")
        );
        assert_eq!(
            warning.job_command_binary_build_identity.as_deref(),
            Some("homeboy 0.204.0+new")
        );
        assert_eq!(
            warning.refresh_command,
            format!(
                "homeboy runner refresh-homeboy homeboy-lab --ref v{} --reconnect && homeboy runner disconnect homeboy-lab && homeboy runner connect homeboy-lab",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert!(warning.message.contains("daemon control plane"));
        assert!(warning.message.contains("job command binary"));
        assert!(warning.message.contains("active jobs are drained"));
        assert_eq!(
            warning.recovery_commands,
            vec![
                format!(
                    "homeboy runner refresh-homeboy homeboy-lab --ref v{} --reconnect",
                    env!("CARGO_PKG_VERSION")
                ),
                "homeboy runner disconnect homeboy-lab".to_string(),
                "homeboy runner connect homeboy-lab".to_string(),
            ]
        );
    }

    #[test]
    fn parses_daemon_runtime_stale_paths_from_version_body() {
        let paths = daemon_runtime_stale_paths_from_body(&serde_json::json!({
            "version": "0.228.13",
            "runtime_paths": {
                "stale": [{
                    "env": "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH",
                    "path": "/home/chubes/Developer/sample-runtime",
                    "loaded_fingerprint": "files=10",
                    "current_fingerprint": "files=11"
                }]
            }
        }));

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].env, "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH");
        assert_eq!(paths[0].loaded_fingerprint, "files=10");
        assert_eq!(paths[0].current_fingerprint, "files=11");
    }

    #[test]
    fn changed_runtime_paths_reports_runner_config_changes_since_daemon_start() {
        let mut runner_env = HashMap::new();
        runner_env.insert(
            "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH".to_string(),
            "/home/chubes/Developer/sample-runtime@new".to_string(),
        );
        let loaded = daemon_runtime_loaded_paths_from_body(&serde_json::json!({
            "runtime_paths": {
                "loaded": [{
                    "env": "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH",
                    "path": "/home/chubes/Developer/sample-runtime@old",
                    "fingerprint": "files=10"
                }]
            }
        }));

        let changed = changed_runtime_paths(&runner_env, &loaded);

        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].env, "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH");
        assert_eq!(
            changed[0].loaded_path.as_deref(),
            Some("/home/chubes/Developer/sample-runtime@old")
        );
        assert_eq!(
            changed[0].configured_path.as_deref(),
            Some("/home/chubes/Developer/sample-runtime@new")
        );
    }

    #[test]
    fn runtime_path_warning_uses_rebuild_specific_message() {
        let warning = RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "0.228.13".to_string(),
            "0.228.13".to_string(),
            Some("homeboy 0.228.13+same".to_string()),
            Some("homeboy 0.228.13+same".to_string()),
        )
        .with_runtime_paths(
            "homeboy-lab",
            vec![RunnerStaleRuntimePath {
                env: "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH".to_string(),
                path: "/home/chubes/Developer/sample-runtime".to_string(),
                loaded_fingerprint: "files=10".to_string(),
                current_fingerprint: "files=11".to_string(),
            }],
            Vec::new(),
        );

        assert!(warning.message.contains("runner-side rebuilds"));
        assert_eq!(
            warning.recovery_commands,
            vec![
                "homeboy runner disconnect homeboy-lab".to_string(),
                "homeboy runner connect homeboy-lab".to_string(),
            ]
        );
    }

    #[test]
    fn parses_remote_daemon_status_lease_as_single_source_of_truth() {
        let envelope = parse_envelope(
            r#"{"success":true,"data":{"action":"status","running":true,"fresh":true,"reachable":true,"state":{"lease_id":"lease-1","address":"127.0.0.1:49152","pid":123}}}"#,
        )
        .expect("parse envelope");
        let data = envelope.data.expect("status data");
        let state = data.get("state").expect("lease state");

        assert!(data.get("running").and_then(Value::as_bool).unwrap());
        assert_eq!(
            state.get("lease_id").and_then(Value::as_str),
            Some("lease-1")
        );
        assert_eq!(
            state.get("address").and_then(Value::as_str),
            Some("127.0.0.1:49152")
        );
    }

    #[test]
    fn remote_daemon_disconnect_command_guards_recorded_pid() {
        let command = remote_daemon_disconnect_command("/opt/homeboy", Some(42));

        assert!(command.contains("/opt/homeboy daemon stop"));
        assert!(command.contains("pid=42"));
        assert!(command.contains("/proc/$pid/cmdline"));
        assert!(command.contains("*\" daemon serve \"*)"));
        assert!(command.contains("kill -TERM \"$pid\""));
        assert!(command.contains("kill -KILL \"$pid\""));
    }

    #[test]
    fn test_open_loopback_tunnel_noops_for_local_runner() {
        let server = Server {
            id: "local".to_string(),
            aliases: Vec::new(),
            host: "127.0.0.1".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            kind: None,
            auth: None,
            env: HashMap::new(),
            runner: None,
        };

        let tunnel = open_loopback_tunnel(&server, 49100, "127.0.0.1", 49200);

        assert!(tunnel.success);
        assert_eq!(tunnel.pid, None);
        assert_eq!(tunnel.stderr, "");
    }

    #[test]
    fn connect_reports_local_runner_as_unsupported() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(r#"{"id":"lab-local","kind":"local"}"#, false)
                .expect("create runner");

            let (report, exit_code) = connect("lab-local").expect("connect report");

            assert_eq!(exit_code, 20);
            assert!(!report.connected);
            assert_eq!(report.failure_kind, Some(RunnerFailureKind::SshFailure));
            assert!(report
                .failure_message
                .as_deref()
                .unwrap_or_default()
                .contains("only SSH runners"));
        });
    }

    #[test]
    fn disconnect_removes_existing_session_file() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(r#"{"id":"lab-local","kind":"local"}"#, false)
                .expect("create runner");
            let session = RunnerSession {
                runner_id: "lab-local".to_string(),
                mode: RunnerTunnelMode::DirectSsh,
                role: RunnerSessionRole::Controller,
                server_id: None,
                controller_id: None,
                broker_url: None,
                remote_daemon_address: Some("127.0.0.1:49152".to_string()),
                local_port: Some(49153),
                local_url: Some("http://127.0.0.1:49153".to_string()),
                tunnel_pid: None,
                remote_daemon_pid: None,
                remote_daemon_lease_id: None,
                homeboy_version: "test".to_string(),
                homeboy_build_identity: Some("homeboy test+abc123".to_string()),
                connected_at: Utc::now().to_rfc3339(),
                worker_identity: None,
                worker_pid: None,
                last_seen_at: None,
            };
            write_session(&session).expect("write session");
            let path = session_path("lab-local").expect("session path");
            assert!(path.exists());

            let report = disconnect("lab-local").expect("disconnect");

            assert!(report.disconnected);
            assert_eq!(report.session.expect("session").runner_id, "lab-local");
            assert!(!path.exists());
        });
    }

    #[test]
    fn records_reverse_runner_session_without_marking_transport_live() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(
                r#"{"id":"homeboy-lab","kind":"local","workspace_root":"/home/user/Developer"}"#,
                false,
            )
            .expect("create runner");

            let (report, exit_code) = connect_reverse(ReverseRunnerConnectOptions {
                controller_id: "extra-chill".to_string(),
                runner_id: "homeboy-lab".to_string(),
                broker_url: None,
            })
            .expect("record reverse session");

            assert_eq!(exit_code, 0);
            assert!(!report.connected);
            assert_eq!(report.recorded, Some(true));
            assert_eq!(report.mode, Some(RunnerTunnelMode::Reverse));
            assert_eq!(report.role, Some(RunnerSessionRole::Runner));
            assert_eq!(report.controller_id.as_deref(), Some("extra-chill"));

            let status = status("homeboy-lab").expect("status");
            assert!(!status.connected);
            assert_eq!(status.state, RunnerSessionState::Recorded);
            let session = status.session.expect("session");
            assert_eq!(session.mode, RunnerTunnelMode::Reverse);
            assert_eq!(session.role, RunnerSessionRole::Runner);
            assert_eq!(session.controller_id.as_deref(), Some("extra-chill"));
            assert_eq!(session.broker_url, None);
            assert_eq!(session.local_url, None);
            assert_eq!(session.local_port, None);
        });
    }

    #[test]
    fn status_lists_reverse_session_records() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(r#"{"id":"homeboy-lab","kind":"local"}"#, false)
                .expect("create runner");
            connect_reverse(ReverseRunnerConnectOptions {
                controller_id: "extra-chill".to_string(),
                runner_id: "homeboy-lab".to_string(),
                broker_url: None,
            })
            .expect("record reverse session");

            let reports = statuses().expect("statuses");
            let report = reports
                .iter()
                .find(|report| report.runner_id == "homeboy-lab")
                .expect("homeboy-lab status");

            assert_eq!(report.state, RunnerSessionState::Recorded);
        });
    }

    #[test]
    fn status_marks_connected_reverse_active_jobs_unavailable_without_broker_url() {
        test_support::with_isolated_home(|_| {
            crate::core::runner::create(r#"{"id":"homeboy-lab","kind":"local"}"#, false)
                .expect("create runner");
            let mut session = reverse_controller_session();
            session.broker_url = None;
            write_session(&session).expect("write session");

            let report = status("homeboy-lab").expect("status");

            assert!(report.connected);
            assert_eq!(report.active_job_count, 0);
            assert_eq!(report.active_jobs, Vec::new());
            assert_eq!(report.active_runner_jobs, Vec::new());
            assert_eq!(report.active_job_state, RunnerActiveJobState::Unavailable);
            assert_eq!(report.active_job_source, None);
            let error = report.active_job_error.expect("active job error");
            assert_eq!(error.code, "internal.unexpected");
            assert!(error.message.contains("no broker URL"));
        });
    }

    #[test]
    fn child_run_matching_accepts_durable_run_id_or_command_reference() {
        let run = sample_run_summary("run-child-1");
        let by_durable_id = sample_active_job(Some("run-child-1"), "homeboy test wpcom");
        let by_command = sample_active_job(None, "homeboy test wpcom --run run-child-1");
        let unrelated = sample_active_job(Some("other-run"), "homeboy test wpcom");

        assert!(child_run_has_active_job(&run, &[by_durable_id]));
        assert!(child_run_has_active_job(&run, &[by_command]));
        assert!(!child_run_has_active_job(&run, &[unrelated]));
    }

    #[test]
    fn orphaned_child_run_job_reports_stale_retryable_runner_state() {
        let job = orphaned_child_run_job("homeboy-lab", sample_run_summary("run-child-1"));

        assert_eq!(job.runner_id, "homeboy-lab");
        assert_eq!(job.job_id, "orphaned-child-run-run-child-1");
        assert_eq!(job.source, "runner-observation");
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.durable_run_id.as_deref(), Some("run-child-1"));
        assert_eq!(
            job.stale_reason.as_deref(),
            Some("child_run_running_without_active_runner_job")
        );
        assert_eq!(job.lifecycle_state.as_deref(), Some("recoverable_orphan"));
        assert_eq!(job.retryable, Some(true));
    }

    #[test]
    fn synthetic_active_job_run_summaries_are_not_child_runs() {
        let mut synthetic = sample_run_summary("runner-job-job-1");
        synthetic.status_note = Some(
            "active runner job: source=direct-daemon kind=test runner=homeboy-lab".to_string(),
        );

        assert!(is_synthetic_active_job_run_summary(&synthetic));
        assert!(!is_synthetic_active_job_run_summary(&sample_run_summary(
            "run-child-1"
        )));
    }

    #[test]
    fn active_runner_job_source_maps_direct_and_reverse_endpoints() {
        let mut direct = reverse_controller_session();
        direct.mode = RunnerTunnelMode::DirectSsh;
        direct.role = RunnerSessionRole::Controller;
        direct.local_url = Some("http://127.0.0.1:49153".to_string());
        direct.broker_url = None;
        assert_eq!(
            active_runner_job_source(&direct),
            Some(RunnerActiveJobSource::DirectDaemon)
        );

        let reverse = reverse_controller_session();
        assert_eq!(
            active_runner_job_source(&reverse),
            Some(RunnerActiveJobSource::ReverseBroker)
        );
    }

    #[test]
    fn reverse_controller_session_requires_fresh_heartbeat() {
        let mut session = reverse_controller_session();

        assert_eq!(session_state(Some(&session)), RunnerSessionState::Connected);

        session.last_seen_at = Some((Utc::now() - chrono::Duration::seconds(120)).to_rfc3339());
        assert_eq!(session_state(Some(&session)), RunnerSessionState::Recorded);

        session.last_seen_at = None;
        assert_eq!(session_state(Some(&session)), RunnerSessionState::Recorded);
    }

    fn reverse_controller_session() -> RunnerSession {
        RunnerSession {
            runner_id: "homeboy-lab".to_string(),
            mode: RunnerTunnelMode::Reverse,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: Some("extra-chill".to_string()),
            broker_url: Some("http://127.0.0.1:9876".to_string()),
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some("homeboy test+abc123".to_string()),
            connected_at: Utc::now().to_rfc3339(),
            worker_identity: Some("worker-1".to_string()),
            worker_pid: Some(1234),
            last_seen_at: Some(Utc::now().to_rfc3339()),
        }
    }

    fn sample_run_summary(id: &str) -> RunSummary {
        RunSummary {
            id: id.to_string(),
            kind: "test".to_string(),
            status: "running".to_string(),
            started_at: "2026-07-03T13:00:00Z".to_string(),
            finished_at: None,
            component_id: Some("wpcom".to_string()),
            rig_id: None,
            git_sha: None,
            command: Some("homeboy test wpcom".to_string()),
            cwd: Some("/workspace/wpcom".to_string()),
            status_note: None,
        }
    }

    fn sample_active_job(durable_run_id: Option<&str>, command: &str) -> ActiveRunnerJobSummary {
        ActiveRunnerJobSummary {
            runner_id: "homeboy-lab".to_string(),
            job_id: "job-1".to_string(),
            operation: "runner.exec".to_string(),
            source: "direct-daemon".to_string(),
            kind: "test".to_string(),
            status: JobStatus::Running,
            command: command.to_string(),
            cwd: Some("/workspace/wpcom".to_string()),
            started_at_ms: 0,
            updated_at_ms: 0,
            elapsed_ms: 0,
            heartbeat_age_ms: 0,
            claim: JobClaimMetadata {
                claim_id: None,
                claimed_by_runner_id: Some("homeboy-lab".to_string()),
                claimed_at_ms: None,
                claim_expires_at_ms: None,
            },
            claim_expires_in_ms: None,
            lifecycle: None,
            durable_run_id: durable_run_id.map(str::to_string),
            stale_reason: None,
            lifecycle_state: Some("running".to_string()),
            retryable: Some(false),
            active_child_count: None,
            active_cell_count: None,
        }
    }

    fn direct_ssh_session(lease_id: &str) -> RunnerSession {
        RunnerSession {
            runner_id: "homeboy-lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: Some("homeboy-lab".to_string()),
            controller_id: None,
            broker_url: None,
            remote_daemon_address: Some("127.0.0.1:49152".to_string()),
            local_port: Some(49153),
            local_url: Some("http://127.0.0.1:49153".to_string()),
            tunnel_pid: Some(1234),
            remote_daemon_pid: Some(4242),
            remote_daemon_lease_id: Some(lease_id.to_string()),
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some("homeboy test+abc123".to_string()),
            connected_at: Utc::now().to_rfc3339(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
        }
    }

    fn remote_daemon_status_for_test(
        fresh: bool,
        reachable: bool,
        active_jobs: usize,
        lease_id: &str,
        pid: u32,
    ) -> RemoteDaemonStatus {
        remote_daemon_status_for_test_with_reason(
            fresh,
            reachable,
            active_jobs,
            lease_id,
            pid,
            None,
        )
    }

    fn remote_daemon_status_for_test_with_reason(
        fresh: bool,
        reachable: bool,
        active_jobs: usize,
        lease_id: &str,
        pid: u32,
        stale_reason_code: Option<DaemonStaleReasonCode>,
    ) -> RemoteDaemonStatus {
        RemoteDaemonStatus {
            daemon: Some(RemoteDaemon {
                address: "127.0.0.1:49152".to_string(),
                pid: Some(pid),
                lease_id: Some(lease_id.to_string()),
            }),
            stale_reason: (!fresh).then(|| "daemon is stale".to_string()),
            stale_reason_code,
            fresh,
            reachable,
            active_jobs,
        }
    }
}
