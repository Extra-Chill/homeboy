//! On-demand daemons own durable work, not an indefinite detached process.

use std::time::{Duration, Instant};

use super::generation_store;
use crate::error::Result;

pub(super) const IDLE_TIMEOUT_ENV: &str = "HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS";
pub(super) const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;
const LAUNCHER_ENV: &str = "HOMEBOY_DAEMON_LAUNCHER_IDENTITY";

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct LauncherIdentity {
    pid: u32,
    start: crate::process::ProcessStartIdentity,
}

impl LauncherIdentity {
    pub(super) fn configure(command: &mut std::process::Command) {
        // A very short configured idle window must not expire while its
        // launcher is still validating and returning the startup handoff.
        command.env_remove(LAUNCHER_ENV);
        let pid = std::process::id();
        if let Ok(Some(start)) = crate::process::process_start_identity(pid) {
            let identity = Self { pid, start };
            command.env(
                LAUNCHER_ENV,
                serde_json::to_string(&identity).expect("launcher identity"),
            );
        }
    }

    pub(super) fn inherited() -> Option<Self> {
        serde_json::from_str(&std::env::var(LAUNCHER_ENV).ok()?).ok()
    }

    pub(super) fn is_live(&self) -> bool {
        crate::process::process_identity_state_with_start_identity(
            self.pid,
            None,
            Some(&self.start),
        ) == crate::process::ProcessIdentityState::Live
    }
}

pub(super) fn launch_idle_timeout(default: u64) -> Result<u64> {
    Ok(configured_idle_timeout()?.map_or_else(
        || {
            if std::env::var_os(IDLE_TIMEOUT_ENV).is_some() {
                0
            } else {
                default
            }
        },
        |timeout| timeout.as_secs(),
    ))
}

/// Explicit foreground/service launches remain resident. Automatic launchers
/// supply the bounded default; zero explicitly selects a resident daemon.
pub(super) fn configured_idle_timeout() -> Result<Option<Duration>> {
    std::env::var(IDLE_TIMEOUT_ENV)
        .ok()
        .map(|value| {
            value.trim().parse::<u64>().map_err(|_| {
                crate::error::Error::validation_invalid_argument(
                    IDLE_TIMEOUT_ENV,
                    "daemon idle timeout must be a non-negative number of seconds",
                    Some(value),
                    None,
                )
            })
        })
        .transpose()
        .map(|seconds| {
            seconds
                .filter(|seconds| *seconds > 0)
                .map(Duration::from_secs)
        })
}

/// Global schedules, mirrored observations, and orchestration belong to the
/// admission owner. Draining generations only maintain their own durable work.
pub(super) fn owns_global_work(lease_id: &str) -> bool {
    generation_store::admitting()
        .is_ok_and(|endpoint| endpoint.is_some_and(|endpoint| endpoint.lease_id == lease_id))
}

pub(super) struct IdleLifetime {
    timeout: Option<Duration>,
    last_activity: Instant,
}

impl IdleLifetime {
    pub(super) fn new(timeout: Option<Duration>) -> Self {
        Self {
            timeout,
            last_activity: Instant::now(),
        }
    }

    pub(super) fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    pub(super) fn expired(&self, draining: bool) -> bool {
        // Even an explicitly resident generation becomes bounded once replaced.
        let timeout = if draining {
            Some(
                self.timeout
                    .unwrap_or(Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS)),
            )
        } else {
            self.timeout
        };
        timeout.is_some_and(|timeout| self.last_activity.elapsed() >= timeout)
    }
}
