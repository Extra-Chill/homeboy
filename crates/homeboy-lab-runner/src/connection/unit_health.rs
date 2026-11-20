//! Runner unit health: one read-only probe of the runner's systemd unit, the
//! incident it implies, and one notification per incident (#18538).
//!
//! The controller daemon observes every service-managed runner once per
//! orchestration tick. A unit that failed, keeps restarting, or a runner that
//! stays unreachable past [`UNREACHABLE_BOUND`] opens an incident, announced
//! once through the operations default transport; a running unit closes it
//! with one recovery notice. The record is persisted per runner, so `status`
//! and `runner doctor` read the same account without probing again, and a
//! daemon restart neither re-announces nor forgets an open incident.

use std::path::PathBuf;
use std::time::Duration;

use homeboy_core::notification_payload::{
    NotifyAction, NotifyEventKind, NotifyPayload, NotifySubject,
};
use homeboy_core::notify::NotifyEvent;
use homeboy_core::notify_outbox::{self, NotifyOnceMarker};
use serde::{Deserialize, Serialize};

use super::service::service_unit_name;
use super::*;
use crate::list;

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a runner may stay unreachable before it is an incident: a single
/// dropped SSH probe or a deliberate restart is not one.
pub const UNREACHABLE_BOUND: Duration = Duration::from_secs(5 * 60);
/// Restarts between two observations (one tick apart) that mark a loop.
const RESTART_LOOP_DELTA: u64 = 2;
const JOURNAL_MARKER: &str = "--homeboy-unit-journal--";

/// The runner unit's systemd account of itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerUnitState {
    pub active_state: String,
    pub sub_state: String,
    pub result: String,
    pub n_restarts: u64,
    /// The first journal error of the latest start attempt: the start failure
    /// itself (`Failed to set up mount namespacing: ...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl RunnerUnitState {
    pub fn running(&self) -> bool {
        self.active_state == "active"
    }

    /// One line for humans: `failed (Result: resources, 54 restarts)`.
    pub fn summary(&self) -> String {
        format!(
            "{}/{} (Result: {}, {} restarts)",
            self.active_state, self.sub_state, self.result, self.n_restarts
        )
    }
}

/// Read-only shell: the unit's state properties, then its recent journal.
pub(crate) fn unit_state_script() -> String {
    let unit = shell::quote_arg(&service_unit_name());
    format!(
        "systemctl --user show -p ActiveState -p SubState -p Result -p NRestarts {unit} </dev/null; echo {JOURNAL_MARKER}; journalctl --user -u {unit} -n 30 --no-pager -o cat </dev/null 2>/dev/null"
    )
}

pub(crate) fn parse_unit_state(stdout: &str) -> Option<RunnerUnitState> {
    let (properties, journal) = stdout.split_once(JOURNAL_MARKER).unwrap_or((stdout, ""));
    let mut state = RunnerUnitState::default();
    for line in properties.lines() {
        match line.trim().split_once('=') {
            Some(("ActiveState", value)) => state.active_state = value.to_string(),
            Some(("SubState", value)) => state.sub_state = value.to_string(),
            Some(("Result", value)) => state.result = value.to_string(),
            Some(("NRestarts", value)) => state.n_restarts = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    if state.active_state.is_empty() {
        return None;
    }
    // The first error of the latest start attempt names its cause; the lines
    // after it are systemd's consequences of that failure.
    let lines: Vec<&str> = journal.lines().map(str::trim).collect();
    let attempt = lines
        .iter()
        .rposition(|line| line.starts_with("Starting "))
        .unwrap_or(0);
    let is_error = |line: &&&str| {
        let lower = line.to_ascii_lowercase();
        lower.contains("error") || lower.contains("fail")
    };
    state.last_error = lines[attempt..]
        .iter()
        .find(is_error)
        .or_else(|| lines.iter().find(is_error))
        .map(|line| line.to_string());
    Some(state)
}

/// Probe the unit over SSH. `Err` carries why the runner could not be read.
pub fn probe_unit_state(client: &SshClient) -> std::result::Result<RunnerUnitState, String> {
    let output = client.execute_with_timeout(&unit_state_script(), PROBE_TIMEOUT);
    parse_unit_state(&output.stdout).ok_or_else(|| {
        let stderr = output.stderr.trim();
        if stderr.is_empty() {
            "the runner unit state could not be read".to_string()
        } else {
            stderr.lines().last().unwrap_or(stderr).to_string()
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerUnitIncident {
    pub since_ms: u64,
    pub reason: String,
}

/// What the last observation of one runner established.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerUnitHealth {
    pub observed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<RunnerUnitState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreachable_since_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreachable_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incident: Option<RunnerUnitIncident>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerUnitTransition {
    Opened(RunnerUnitIncident),
    Recovered(RunnerUnitIncident),
}

/// Fold one probe into the runner's health. Pure: the caller persists the
/// record before announcing the transition, so each is announced at most once.
pub fn observe_unit_health(
    previous: &RunnerUnitHealth,
    probe: std::result::Result<RunnerUnitState, String>,
    now_ms: u64,
) -> (RunnerUnitHealth, Option<RunnerUnitTransition>) {
    let mut next = RunnerUnitHealth {
        observed_at_ms: now_ms,
        incident: previous.incident.clone(),
        ..Default::default()
    };
    let (unhealthy, healthy) = match probe {
        Ok(state) => {
            let restarted = previous
                .unit
                .as_ref()
                .map(|before| state.n_restarts.saturating_sub(before.n_restarts))
                .unwrap_or(0);
            let restarting = state.sub_state == "auto-restart"
                && previous
                    .unit
                    .as_ref()
                    .is_some_and(|before| before.sub_state == "auto-restart");
            let unhealthy = if state.active_state == "failed" {
                Some(format!("runner unit failed: {}", state.summary()))
            } else if restarted >= RESTART_LOOP_DELTA || restarting {
                Some(format!(
                    "runner unit is restart-looping: {}",
                    state.summary()
                ))
            } else {
                None
            };
            let healthy = unhealthy.is_none() && state.running();
            next.unit = Some(state);
            (unhealthy, healthy)
        }
        Err(reason) => {
            let since = previous.unreachable_since_ms.unwrap_or(now_ms);
            next.unreachable_since_ms = Some(since);
            next.unreachable_reason = Some(reason.clone());
            next.unit = previous.unit.clone();
            let elapsed = Duration::from_millis(now_ms.saturating_sub(since));
            let unhealthy = (elapsed >= UNREACHABLE_BOUND).then(|| {
                format!(
                    "runner unreachable for {}m: {reason}",
                    elapsed.as_secs() / 60
                )
            });
            (unhealthy, false)
        }
    };
    let transition = match (&previous.incident, unhealthy) {
        (None, Some(reason)) => {
            let incident = RunnerUnitIncident {
                since_ms: now_ms,
                reason,
            };
            next.incident = Some(incident.clone());
            Some(RunnerUnitTransition::Opened(incident))
        }
        (Some(incident), None) if healthy => {
            next.incident = None;
            Some(RunnerUnitTransition::Recovered(incident.clone()))
        }
        _ => None,
    };
    (next, transition)
}

fn health_path(runner_id: &str) -> Result<PathBuf> {
    Ok(homeboy_core::paths::homeboy()?
        .join("runner-health")
        .join(format!("{runner_id}.json")))
}

/// The last persisted observation of this runner, read-only.
pub fn runner_unit_health(runner_id: &str) -> Option<RunnerUnitHealth> {
    let text = std::fs::read_to_string(health_path(runner_id).ok()?).ok()?;
    serde_json::from_str(&text).ok()
}

fn persist(runner_id: &str, health: &RunnerUnitHealth) -> Result<()> {
    let path = health_path(runner_id)?;
    let io = |error: std::io::Error| {
        Error::internal_io(error.to_string(), Some(path.display().to_string()))
    };
    std::fs::create_dir_all(path.parent().expect("runner-health dir")).map_err(io)?;
    let staged = path.with_extension("json.next");
    std::fs::write(
        &staged,
        serde_json::to_vec_pretty(health).unwrap_or_default(),
    )
    .map_err(io)?;
    std::fs::rename(&staged, &path).map_err(io)
}

/// Observe every service-managed SSH runner and announce its transitions.
pub fn observe_service_runner_health(now_ms: u64) -> Result<()> {
    for runner in list()?
        .into_iter()
        .filter(|runner| runner.settings.service_managed)
    {
        let probe = match resolve_ssh_runner(&runner) {
            Ok(Some((_, _, client))) => probe_unit_state(&client),
            Ok(None) => continue,
            Err(error) => Err(error.message),
        };
        let previous = runner_unit_health(&runner.id).unwrap_or_default();
        let (next, transition) = observe_unit_health(&previous, probe, now_ms);
        persist(&runner.id, &next)?;
        if let Some(transition) = transition {
            announce(&runner.id, &next, &transition);
        }
    }
    Ok(())
}

struct ServiceRunnerHealth;

impl homeboy_core::daemon::runner_health::RunnerHealthProvider for ServiceRunnerHealth {
    fn observe(&self) -> Result<()> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or_default();
        observe_service_runner_health(now_ms)
    }
}

/// Let the daemon's orchestration tick observe service-managed runners.
pub fn register_runner_health_provider() {
    homeboy_core::daemon::runner_health::register_runner_health_provider(Box::new(
        ServiceRunnerHealth,
    ));
}

fn announce(runner_id: &str, health: &RunnerUnitHealth, transition: &RunnerUnitTransition) {
    let (kind, status, incident) = match transition {
        RunnerUnitTransition::Opened(incident) => {
            (NotifyEventKind::NeedsAttention, "unhealthy", incident)
        }
        RunnerUnitTransition::Recovered(incident) => {
            (NotifyEventKind::Completed, "recovered", incident)
        }
    };
    let unit = health.unit.as_ref();
    let payload = NotifyPayload::new(kind, NotifySubject::new("runner", runner_id))
        .with_fact("Runner", runner_id)
        .with_fact("Status", status)
        .with_fact("Reason", incident.reason.clone())
        .with_optional_fact("Unit", unit.map(RunnerUnitState::summary))
        .with_optional_fact(
            "Start error",
            unit.and_then(|unit| unit.last_error.clone())
                .filter(|_| status == "unhealthy"),
        )
        .with_action(NotifyAction::new(
            "Doctor",
            format!("homeboy runner doctor {runner_id}"),
        ));
    let event = NotifyEvent::lifecycle(kind, runner_id, status)
        .with_title(format!("runner {runner_id} {status}"))
        .with_payload(payload)
        .with_route(None);
    let _ = notify_outbox::dispatch_with_outbox(
        &event,
        Some(NotifyOnceMarker::new(
            "runner_unit",
            format!("{runner_id}/{}/{status}", incident.since_ms),
            "runner-health",
        )),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(active: &str, sub: &str, result: &str, restarts: u64) -> RunnerUnitState {
        RunnerUnitState {
            active_state: active.into(),
            sub_state: sub.into(),
            result: result.into(),
            n_restarts: restarts,
            last_error: None,
        }
    }

    /// One incident per outage, opened by a failed unit, a restart loop or a
    /// bounded disconnect, and closed once by a running unit (#18538).
    #[test]
    fn each_outage_opens_one_incident_and_recovers_once() {
        let minute = 60_000;
        let failed = unit("failed", "failed", "resources", 54);
        let running = unit("active", "running", "success", 54);
        let unreachable = || Err("ssh: connect timed out".to_string());
        let cases: Vec<(
            &str,
            Vec<std::result::Result<RunnerUnitState, String>>,
            &str,
        )> = vec![
            (
                "failed",
                vec![
                    Ok(running.clone()),
                    Ok(failed.clone()),
                    Ok(failed.clone()),
                    Ok(running.clone()),
                ],
                "runner unit failed: failed/failed (Result: resources, 54 restarts)",
            ),
            (
                "loop",
                vec![
                    Ok(unit("active", "running", "success", 1)),
                    Ok(unit("active", "running", "exit-code", 4)),
                    Ok(unit("active", "running", "exit-code", 9)),
                    Ok(unit("active", "running", "success", 9)),
                ],
                "runner unit is restart-looping: active/running (Result: exit-code, 4 restarts)",
            ),
            (
                "unreachable",
                vec![
                    Ok(running.clone()),
                    unreachable(),
                    unreachable(),
                    unreachable(),
                    unreachable(),
                    unreachable(),
                    unreachable(),
                    Ok(running.clone()),
                ],
                "runner unreachable for 5m: ssh: connect timed out",
            ),
        ];
        for (name, probes, reason) in cases {
            let mut health = RunnerUnitHealth::default();
            let mut transitions = Vec::new();
            for (tick, probe) in probes.into_iter().enumerate() {
                let (next, transition) = observe_unit_health(&health, probe, tick as u64 * minute);
                transitions.extend(transition);
                health = next;
            }
            assert_eq!(transitions.len(), 2, "{name}: {transitions:?}");
            let RunnerUnitTransition::Opened(opened) = &transitions[0] else {
                panic!("{name}: {transitions:?}");
            };
            assert_eq!(opened.reason, reason, "{name}");
            assert_eq!(
                transitions[1],
                RunnerUnitTransition::Recovered(opened.clone()),
                "{name}"
            );
            assert!(health.incident.is_none(), "{name}");
        }
        // One dropped probe or a single self-healed restart is not an outage.
        let mut health = RunnerUnitHealth::default();
        for probe in [
            Ok(running.clone()),
            unreachable(),
            Ok(unit("active", "running", "success", 55)),
        ] {
            let (next, transition) = observe_unit_health(&health, probe, minute);
            assert!(transition.is_none());
            health = next;
        }
    }

    #[test]
    fn unit_state_parses_properties_and_the_last_journal_error() {
        let state = parse_unit_state(
            "ActiveState=failed\nSubState=failed\nResult=resources\nNRestarts=54\n--homeboy-unit-journal--\nerror: an older start\nStarting homeboy runner...\nhomeboy-runner.service: Failed to set up mount namespacing: Too many open files\nhomeboy-runner.service: Failed with result 'resources'.\n",
        )
        .unwrap();
        assert_eq!(
            state.summary(),
            "failed/failed (Result: resources, 54 restarts)"
        );
        assert_eq!(
            state.last_error.as_deref(),
            Some("homeboy-runner.service: Failed to set up mount namespacing: Too many open files")
        );
        assert!(parse_unit_state("").is_none());
    }
}
