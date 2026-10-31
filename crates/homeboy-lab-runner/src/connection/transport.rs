//! One transport to the runner's one daemon.
//!
//! Every observer (job polling, job logs, job cancel) reaches the daemon the
//! runner's current session names through [`ensure_transport`]. It reuses a
//! transport whose `/health` still proves that daemon's lease and PID, or opens
//! a fresh one and accepts it only when `/health` proves the same identity.
//!
//! It never starts, restarts or repairs the daemon: a daemon that cannot be
//! reached is reported as a typed [`TransportFailure`], and restart and repair
//! stay explicit policy in their own callers. The session file is a cache and
//! the lease fence for the persisted tunnel, never proof that the daemon is
//! alive.

use super::*;

/// How long the transport [`ensure_transport`] returns is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportLifetime {
    /// Retained as the runner's current session. A live current session is
    /// reused; otherwise the fresh tunnel replaces it by compare-and-replace
    /// on the lease and the replaced tunnel is closed. Taken under the restart
    /// recovery lock, so a poller waits for an in-place restart to publish the
    /// restarted session instead of reattaching the stopped lease.
    Persisted,
    /// A fresh tunnel for one log or cancel read. It is never persisted and
    /// never the current session's tunnel; the caller closes it with
    /// [`close_transport`].
    Temporary,
}

/// Why a transport to the runner's daemon could not be established. Carried
/// on the returned error as `details.transport_failure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailure {
    /// SSH configuration or authentication failed. The daemon's liveness is
    /// unknown, so this is never evidence that it died.
    Ssh,
    /// The tunnel did not open or the daemon did not answer `/health` within
    /// the settle window. This is transport loss and stays retryable.
    Unreachable,
    /// The daemon answered with another lease, PID or build identity. This is
    /// authoritative and is never retried.
    IdentityMismatch,
}

impl TransportFailure {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Unreachable => "unreachable",
            Self::IdentityMismatch => "identity_mismatch",
        }
    }

    /// The failure class an [`ensure_transport`] error carries, if any.
    pub fn of(error: &Error) -> Option<Self> {
        match error.details.get("transport_failure")?.as_str()? {
            "ssh" => Some(Self::Ssh),
            "unreachable" => Some(Self::Unreachable),
            "identity_mismatch" => Some(Self::IdentityMismatch),
            _ => None,
        }
    }

    /// Classify a failed tunnel open plus `/health` identity proof.
    fn classify(report: &RunnerConnectReport) -> Self {
        let stage = report
            .failure_evidence
            .as_ref()
            .and_then(|evidence| evidence.durability_stage.as_deref());
        let message = report.failure_message.as_deref().unwrap_or("");
        if matches!(stage, Some("health_identity" | "endpoint_identity")) {
            return Self::IdentityMismatch;
        }
        if report.failure_kind == Some(RunnerFailureKind::SshFailure)
            || server::ssh_auth_failure(message).is_some()
            || server::ssh_host_key_rejected(message)
        {
            return Self::Ssh;
        }
        if matches!(
            report.failure_kind,
            Some(RunnerFailureKind::TunnelFailure | RunnerFailureKind::TransportUnreachable)
        ) || matches!(stage, Some("health_settle" | "tunnel_durability"))
        {
            return Self::Unreachable;
        }
        // Any other answer, such as a non-loopback daemon address, does not
        // prove the recorded daemon and is refused like a mismatch.
        Self::IdentityMismatch
    }
}

/// Reach the runner's one current daemon without ever restarting it.
pub fn ensure_transport(runner_id: &str, lifetime: TransportLifetime) -> Result<RunnerSession> {
    ensure_transport_with(runner_id, lifetime, &LiveTransport)
}

/// Close a transport [`ensure_transport`] returned for
/// [`TransportLifetime::Temporary`]. This only signals the exact local tunnel
/// process; it never stops the remote daemon that owns the durable job.
pub fn close_transport(session: &RunnerSession) {
    terminate_tunnel_if_owned(session);
}

/// The side effects of [`ensure_transport`], separated so the reuse, persist
/// and close policy is testable without SSH. There is deliberately no restart
/// operation: reaching the daemon never changes which daemon runs.
pub(super) trait TransportOperations {
    /// The runner's current session: the cache and the lease fence.
    fn current_session(&self, runner_id: &str) -> Result<Option<RunnerSession>>;
    /// Whether the session's own tunnel still reaches its daemon, proven by
    /// `/health` reporting the session's lease and PID.
    fn is_live(&self, session: &RunnerSession) -> bool;
    /// Open a fresh tunnel to the daemon `owner` names and prove its identity.
    fn open(&self, runner_id: &str, owner: &RunnerSession) -> Result<RunnerSession>;
    /// Compare-and-replace the current session on its lease; returns the
    /// replaced session.
    fn persist(&self, runner_id: &str, session: &RunnerSession) -> Result<RunnerSession>;
    fn close(&self, session: &RunnerSession);
}

pub(super) fn ensure_transport_with(
    runner_id: &str,
    lifetime: TransportLifetime,
    operations: &impl TransportOperations,
) -> Result<RunnerSession> {
    let owner = || {
        operations.current_session(runner_id)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner",
                "runner has no current daemon session; run `homeboy runner connect <runner-id>` first",
                Some(runner_id.to_string()),
                None,
            )
        })
    };
    match lifetime {
        // A temporary tunnel never reuses the current session's tunnel: the
        // caller closes what it receives.
        TransportLifetime::Temporary => operations.open(runner_id, &owner()?),
        TransportLifetime::Persisted => {
            restart_recovery::with_restart_recovery_lock(runner_id, || {
                let owner = owner()?;
                if operations.is_live(&owner) {
                    return Ok(owner);
                }
                let session = operations.open(runner_id, &owner)?;
                let replaced = match operations.persist(runner_id, &session) {
                    Ok(replaced) => replaced,
                    Err(error) => {
                        operations.close(&session);
                        return Err(error);
                    }
                };
                operations.close(&replaced);
                Ok(session)
            })
        }
    }
}

struct LiveTransport;

impl TransportOperations for LiveTransport {
    fn current_session(&self, runner_id: &str) -> Result<Option<RunnerSession>> {
        read_session_or_live_peer(runner_id)
    }

    fn is_live(&self, session: &RunnerSession) -> bool {
        session_is_live(session)
    }

    fn open(&self, runner_id: &str, owner: &RunnerSession) -> Result<RunnerSession> {
        open_transport(runner_id, owner)
    }

    fn persist(&self, runner_id: &str, session: &RunnerSession) -> Result<RunnerSession> {
        restart_recovery::persist_reconnected_session(runner_id, session)
    }

    fn close(&self, session: &RunnerSession) {
        close_transport(session);
    }
}

/// Open a fresh ephemeral tunnel to the daemon `owner` names.
///
/// This deliberately never tests the recorded local port: a dead controller
/// tunnel can have its port reused by an unrelated process. The fresh tunnel
/// is accepted only when `/health` proves the exact lease and PID `owner`
/// records. `connect_remote_daemon` only opens the tunnel and probes it; it
/// never starts the daemon.
fn open_transport(runner_id: &str, owner: &RunnerSession) -> Result<RunnerSession> {
    let runner = load(runner_id)?;
    let Some((_server_id, server, _client)) = resolve_ssh_runner(&runner)? else {
        return Err(transport_error(
            runner_id,
            TransportFailure::Ssh,
            "runner daemon transport requires an SSH-backed runner".to_string(),
        ));
    };
    let daemon = RemoteDaemon {
        address: owner.remote_daemon_address.clone().ok_or_else(|| {
            Error::internal_unexpected("job-owning daemon session has no remote address")
        })?,
        pid: owner.remote_daemon_pid,
        lease_id: owner.remote_daemon_lease_id.clone(),
        version: Some(owner.homeboy_version.clone()),
        build_identity: owner.homeboy_build_identity.clone(),
        inspected_freshness: None,
    };
    let session_path = session_path(runner_id)?;
    let (local_port, tunnel_pid, tunnel_process_start_identity, local_url, daemon) =
        connect_remote_daemon(connection_daemon::RemoteDaemonConnectRequest {
            server: &server,
            homeboy: "",
            daemon,
            expected_version: &owner.homeboy_version,
            expected_identity: owner.homeboy_build_identity.as_deref().unwrap_or(""),
            runner_id,
            session_path: &session_path,
        })
        .map_err(|report| report_error(runner_id, report.0))?;
    Ok(RunnerSession {
        local_port: Some(local_port),
        local_url: Some(local_url),
        tunnel_pid,
        tunnel_process_start_identity,
        remote_daemon_address: Some(daemon.address),
        remote_daemon_pid: daemon.pid,
        remote_daemon_lease_id: daemon.lease_id,
        ..owner.clone()
    })
}

/// The typed error for a failed tunnel open plus `/health` identity proof.
pub(super) fn report_error(runner_id: &str, report: RunnerConnectReport) -> Error {
    let failure = TransportFailure::classify(&report);
    let message = report
        .failure_message
        .unwrap_or_else(|| "unable to reattach the job-owning daemon".to_string());
    transport_error(runner_id, failure, message)
}

/// An unreachable daemon is the same transport loss a job poll already
/// observed, so it keeps the typed, retryable transport signal and the poll's
/// grace window still applies. SSH and identity failures are refusals.
pub(super) fn transport_error(
    runner_id: &str,
    failure: TransportFailure,
    message: String,
) -> Error {
    let mut error = match failure {
        TransportFailure::Unreachable => crate::daemon_http_get::daemon_transport_error(
            crate::daemon_http_get::DaemonHttpErrorKind::Connect,
            "/health",
            None,
            &format!("reattach runner `{runner_id}` job-owning daemon"),
            message,
        ),
        TransportFailure::Ssh | TransportFailure::IdentityMismatch => {
            Error::validation_invalid_argument("runner", message, Some(runner_id.to_string()), None)
        }
    };
    error.details["runner_id"] = Value::String(runner_id.to_string());
    error.details["transport_failure"] = Value::String(failure.as_str().to_string());
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_report(
        kind: RunnerFailureKind,
        message: &str,
        durability_stage: Option<&str>,
    ) -> RunnerConnectReport {
        let (mut report, _) = failed_connect(
            "runner-a",
            PathBuf::from("/tmp/session.json"),
            kind,
            message.to_string(),
        );
        if let Some(evidence) = report.failure_evidence.as_mut() {
            evidence.durability_stage = durability_stage.map(str::to_string);
        }
        report
    }

    #[test]
    fn ssh_authentication_failures_are_ssh_not_a_dead_daemon() {
        for message in [
            "SSH tunnel setup failed: user@lab: Permission denied (publickey).",
            "SSH tunnel setup failed: Host key verification failed.",
        ] {
            let report = failed_report(RunnerFailureKind::TunnelFailure, message, None);
            assert_eq!(
                TransportFailure::classify(&report),
                TransportFailure::Ssh,
                "{message}"
            );
        }
        let report = failed_report(RunnerFailureKind::SshFailure, "ssh exited 255", None);
        assert_eq!(TransportFailure::classify(&report), TransportFailure::Ssh);
    }

    #[test]
    fn unopened_tunnels_and_silent_health_are_unreachable() {
        for (kind, message, stage) in [
            (
                RunnerFailureKind::TunnelFailure,
                "local tunnel 127.0.0.1:4000 did not become reachable",
                None,
            ),
            (
                RunnerFailureKind::TunnelFailure,
                "SSH tunnel setup failed: connect to host lab port 22: Connection refused",
                None,
            ),
            (
                RunnerFailureKind::DaemonStartupFailure,
                "remote daemon health endpoint failed after 4 bounded requests",
                Some("health_settle"),
            ),
            (
                RunnerFailureKind::DaemonStartupFailure,
                "owned SSH tunnel exited",
                Some("tunnel_durability"),
            ),
        ] {
            let report = failed_report(kind, message, stage);
            assert_eq!(
                TransportFailure::classify(&report),
                TransportFailure::Unreachable,
                "{message}"
            );
        }
    }

    #[test]
    fn another_daemon_answering_health_is_an_identity_mismatch() {
        for stage in ["health_identity", "endpoint_identity"] {
            let report = failed_report(
                RunnerFailureKind::DaemonStartupFailure,
                "remote daemon health identity changed",
                Some(stage),
            );
            assert_eq!(
                TransportFailure::classify(&report),
                TransportFailure::IdentityMismatch,
                "{stage}"
            );
        }
    }

    #[test]
    fn only_unreachable_failures_stay_retryable_and_every_error_is_typed() {
        for failure in [
            TransportFailure::Ssh,
            TransportFailure::Unreachable,
            TransportFailure::IdentityMismatch,
        ] {
            let error = transport_error("runner-a", failure, "failed".to_string());
            assert_eq!(TransportFailure::of(&error), Some(failure));
            assert_eq!(
                error.retryable == Some(true),
                failure == TransportFailure::Unreachable,
                "{failure:?}"
            );
            assert_eq!(error.details["runner_id"], "runner-a");
        }
    }
}
