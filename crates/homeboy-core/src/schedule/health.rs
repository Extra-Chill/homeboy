//! Operator-facing schedule run health.
//!
//! Runtime state already lives on disk. This projection is the product path
//! for the questions that used to require reading those files: last status,
//! consecutive failures, a stale `running` marker, and cadence-relative
//! staleness.

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::state::{load_state, ScheduleState};
use super::types::Schedule;

/// Health of one schedule relative to `now`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScheduleHealth {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    pub consecutive_failures: u32,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// `running` marker older than the reclaim window — abandoned, not live.
    pub stale_running: bool,
    /// In-flight longer than this schedule's own cadence.
    pub cadence_stale: bool,
    pub unhealthy: bool,
    pub command: String,
}

/// Classify one schedule's runtime record.
pub fn assess(schedule: &Schedule, state: &ScheduleState, now: DateTime<Utc>) -> ScheduleHealth {
    let stale_running = state.is_stale_running(now);
    let cadence_stale = running_longer_than_cadence(state, schedule, now);
    let failed = state.is_unhealthy()
        || state
            .last_status
            .as_deref()
            .is_some_and(|status| status != "succeeded");
    ScheduleHealth {
        id: schedule.id.clone(),
        last_run_at: state.last_run_at.clone(),
        last_status: state.last_status.clone(),
        consecutive_failures: state.consecutive_failures,
        running: state.running,
        started_at: state.started_at.clone(),
        stale_running,
        cadence_stale,
        unhealthy: stale_running || cadence_stale || failed,
        command: format!("homeboy schedule show {}", schedule.id),
    }
}

/// Health of every declared schedule.
pub fn list_health(now: DateTime<Utc>) -> crate::Result<Vec<ScheduleHealth>> {
    Ok(super::list()?
        .into_iter()
        .map(|schedule| {
            let state = load_state(&schedule.id);
            assess(&schedule, &state, now)
        })
        .collect())
}

fn running_longer_than_cadence(
    state: &ScheduleState,
    schedule: &Schedule,
    now: DateTime<Utc>,
) -> bool {
    if !state.running {
        return false;
    }
    let cadence = schedule.every.seconds() as i64;
    match age_secs(state.started_at.as_deref(), now) {
        Some(age) => age >= cadence,
        None => true,
    }
}

fn age_secs(timestamp: Option<&str>, now: DateTime<Utc>) -> Option<i64> {
    let parsed = chrono::DateTime::parse_from_rfc3339(timestamp?).ok()?;
    Some((now - parsed.with_timezone(&Utc)).num_seconds())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::state::STALE_RUN_RECLAIM_SECS;
    use crate::schedule::types::{Cadence, NotifyPolicy, OverlapPolicy};

    fn schedule(id: &str, cadence_secs: u64) -> Schedule {
        Schedule {
            id: id.to_string(),
            command: Some(vec!["cleanup".to_string()]),
            exec: None,
            steps: Vec::new(),
            every: Cadence::from_seconds(cadence_secs).expect("cadence"),
            notify_on: NotifyPolicy::default(),
            on_overlap: OverlapPolicy::default(),
            notification_transport: None,
            notification_route: None,
            jitter_seconds: None,
            enabled: true,
            description: None,
            aliases: Vec::new(),
        }
    }

    #[test]
    fn a_live_run_shorter_than_cadence_is_healthy() {
        let now = Utc::now();
        let state = ScheduleState {
            running: true,
            started_at: Some((now - chrono::Duration::minutes(5)).to_rfc3339()),
            last_status: Some("succeeded".to_string()),
            ..Default::default()
        };
        let health = assess(&schedule("live", 3_600), &state, now);
        assert!(!health.stale_running);
        assert!(!health.cadence_stale);
        assert!(!health.unhealthy);
    }

    #[test]
    fn running_longer_than_cadence_is_cadence_stale_before_the_reclaim_window() {
        let now = Utc::now();
        let state = ScheduleState {
            running: true,
            started_at: Some((now - chrono::Duration::hours(2)).to_rfc3339()),
            last_run_at: Some((now - chrono::Duration::hours(2)).to_rfc3339()),
            last_status: Some("succeeded".to_string()),
            ..Default::default()
        };
        let health = assess(&schedule("wedged", 3_600), &state, now);
        assert!(!health.stale_running, "two hours is under the 6h reclaim window");
        assert!(health.cadence_stale);
        assert!(health.unhealthy);
        assert_eq!(health.command, "homeboy schedule show wedged");
    }

    #[test]
    fn consecutive_failures_are_unhealthy_without_a_running_marker() {
        let now = Utc::now();
        let state = ScheduleState {
            last_run_at: Some(now.to_rfc3339()),
            last_status: Some("partial_failure".to_string()),
            consecutive_failures: 106,
            ..Default::default()
        };
        let health = assess(&schedule("failing", 3_600), &state, now);
        assert!(!health.stale_running);
        assert!(!health.cadence_stale);
        assert_eq!(health.consecutive_failures, 106);
        assert_eq!(health.last_status.as_deref(), Some("partial_failure"));
        assert!(health.unhealthy);
    }

    #[test]
    fn a_stale_running_marker_past_the_reclaim_window_is_unhealthy() {
        let now = Utc::now();
        let state = ScheduleState {
            running: true,
            started_at: Some(
                (now - chrono::Duration::seconds(STALE_RUN_RECLAIM_SECS + 1)).to_rfc3339(),
            ),
            last_status: Some("failed".to_string()),
            ..Default::default()
        };
        let health = assess(&schedule("abandoned", 3_600), &state, now);
        assert!(health.stale_running);
        assert!(health.cadence_stale);
        assert!(health.unhealthy);
    }

    #[test]
    fn list_health_reports_declared_schedules_from_runtime_state() {
        crate::test_support::with_isolated_home(|_| {
            let failing = schedule("failing", 3_600);
            crate::schedule::save(&failing).expect("save schedule");
            crate::schedule::save_state(
                &failing.id,
                &ScheduleState {
                    last_status: Some("failed".to_string()),
                    consecutive_failures: 106,
                    ..Default::default()
                },
            )
            .expect("save state");

            let reports = list_health(Utc::now()).expect("list health");
            let failing = reports
                .iter()
                .find(|report| report.id == "failing")
                .expect("failing schedule is listed");
            assert_eq!(failing.consecutive_failures, 106);
            assert!(failing.unhealthy);
        });
    }
}
