//! Mutable per-schedule runtime state.
//!
//! Deliberately stored apart from the declaration. The declaration is
//! reviewable configuration a human writes; this record churns on every run
//! and would make that file useless to diff.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::paths;

use super::types::Schedule;

/// The outcome of the most recent run, and enough context to decide whether
/// the next one is worth reporting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_exit_code: Option<i32>,

    /// Fingerprint of the last run's reportable payload. Drives
    /// `NotifyPolicy::Change` — a run whose fingerprint matches the previous
    /// one is healthy-and-unchanged, and stays silent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_digest: Option<String>,

    /// Set while a run is in flight so an overlapping tick can decline.
    #[serde(default)]
    pub running: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,

    #[serde(default)]
    pub consecutive_failures: u32,
}

impl ScheduleState {
    /// Whether `schedule` is due at `now`, given this state.
    ///
    /// Uses the recorded start time rather than completion so a long run does
    /// not push the cadence out by its own duration.
    pub fn is_due(&self, schedule: &Schedule, now: chrono::DateTime<chrono::Utc>) -> bool {
        if !schedule.enabled {
            return false;
        }
        if self.running && matches!(schedule.on_overlap, super::types::OverlapPolicy::Skip) {
            return false;
        }
        let Some(next) = self.next_run_at(schedule) else {
            // Never run: due immediately.
            return true;
        };
        now >= next
    }

    /// When this schedule next becomes due, or `None` if it has never run.
    pub fn next_run_at(&self, schedule: &Schedule) -> Option<chrono::DateTime<chrono::Utc>> {
        let last = self.last_run_at.as_ref()?;
        let parsed = chrono::DateTime::parse_from_rfc3339(last).ok()?;
        let interval =
            chrono::Duration::try_seconds(schedule.every.seconds() as i64).unwrap_or_default();
        let jitter = chrono::Duration::try_seconds(schedule.jitter_offset_seconds() as i64)
            .unwrap_or_default();
        Some(parsed.with_timezone(&chrono::Utc) + interval + jitter)
    }
}

fn state_path(id: &str) -> Result<std::path::PathBuf> {
    let segment = paths::sanitize_path_segment(id);
    Ok(paths::homeboy_data()?
        .join("schedules")
        .join(segment)
        .join("state.json"))
}

/// Read a schedule's runtime state, treating absent or unreadable state as
/// "never run" rather than an error — a missing record must not stop a
/// schedule from running for the first time.
pub fn load_state(id: &str) -> ScheduleState {
    let Ok(path) = state_path(id) else {
        return ScheduleState::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return ScheduleState::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save_state(id: &str, state: &ScheduleState) -> Result<()> {
    let path = state_path(id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            crate::error::Error::internal_io(
                format!("Failed to create schedule state directory: {error}"),
                Some(parent.display().to_string()),
            )
        })?;
    }
    let body = serde_json::to_string_pretty(state).map_err(|error| {
        crate::error::Error::internal_io(
            format!("Failed to serialize schedule state: {error}"),
            Some(path.display().to_string()),
        )
    })?;
    std::fs::write(&path, body).map_err(|error| {
        crate::error::Error::internal_io(
            format!("Failed to write schedule state: {error}"),
            Some(path.display().to_string()),
        )
    })
}

/// Drop a schedule's runtime state. Best effort: removing a schedule must not
/// fail because its state was already gone.
pub fn remove_state(id: &str) {
    if let Ok(path) = state_path(id) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::types::{Cadence, NotifyPolicy, OverlapPolicy};

    fn schedule() -> Schedule {
        Schedule {
            id: "nightly".to_string(),
            command: Some(vec!["triage".to_string()]),
            exec: None,
            every: Cadence::from_seconds(3_600).expect("cadence"),
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
    fn a_schedule_that_never_ran_is_due_immediately() {
        assert!(ScheduleState::default().is_due(&schedule(), chrono::Utc::now()));
    }

    #[test]
    fn a_schedule_is_not_due_until_its_interval_elapses() {
        let now = chrono::Utc::now();
        let state = ScheduleState {
            last_run_at: Some((now - chrono::Duration::minutes(30)).to_rfc3339()),
            ..Default::default()
        };
        assert!(!state.is_due(&schedule(), now), "30m into a 1h cadence");

        let state = ScheduleState {
            last_run_at: Some((now - chrono::Duration::minutes(61)).to_rfc3339()),
            ..Default::default()
        };
        assert!(state.is_due(&schedule(), now), "61m into a 1h cadence");
    }

    #[test]
    fn a_disabled_schedule_is_never_due() {
        let disabled = Schedule {
            enabled: false,
            ..schedule()
        };
        assert!(!ScheduleState::default().is_due(&disabled, chrono::Utc::now()));
    }

    /// A slow run must not stack copies of itself.
    #[test]
    fn an_in_flight_run_blocks_the_next_one_under_skip() {
        let state = ScheduleState {
            running: true,
            ..Default::default()
        };
        assert!(!state.is_due(&schedule(), chrono::Utc::now()));

        let allow = Schedule {
            on_overlap: OverlapPolicy::Allow,
            ..schedule()
        };
        assert!(state.is_due(&allow, chrono::Utc::now()));
    }

    /// Cadence is measured from the previous start, so a run that takes longer
    /// than usual does not push every later run out by its own duration.
    #[test]
    fn next_run_is_measured_from_the_last_start_plus_jitter() {
        let base = chrono::Utc::now();
        let state = ScheduleState {
            last_run_at: Some(base.to_rfc3339()),
            ..Default::default()
        };
        let jittered = Schedule {
            jitter_seconds: Some(600),
            ..schedule()
        };
        let expected = base
            + chrono::Duration::seconds(3_600)
            + chrono::Duration::seconds(jittered.jitter_offset_seconds() as i64);
        assert_eq!(state.next_run_at(&jittered), Some(expected));
    }

    #[test]
    fn unreadable_state_reads_as_never_run_rather_than_failing() {
        crate::test_support::with_isolated_home(|_| {
            let state = load_state("definitely-absent-schedule");
            assert!(state.last_run_at.is_none());
            assert!(!state.running);
        });
    }

    #[test]
    fn state_round_trips_through_disk() {
        crate::test_support::with_isolated_home(|_| {
            let state = ScheduleState {
                last_run_at: Some("2026-07-26T00:00:00+00:00".to_string()),
                last_status: Some("succeeded".to_string()),
                last_exit_code: Some(0),
                last_digest: Some("abc123".to_string()),
                running: false,
                started_at: None,
                consecutive_failures: 0,
            };
            save_state("round-trip", &state).expect("save state");
            let loaded = load_state("round-trip");
            assert_eq!(loaded.last_status.as_deref(), Some("succeeded"));
            assert_eq!(loaded.last_digest.as_deref(), Some("abc123"));

            remove_state("round-trip");
            assert!(load_state("round-trip").last_status.is_none());
        });
    }
}
