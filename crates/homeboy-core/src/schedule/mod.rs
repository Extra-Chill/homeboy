//! Scheduled runs — declared homeboy commands that execute on a cadence.
//!
//! Homeboy already owns durable jobs, typed notification transports, and a
//! structured result envelope. What it lacked was a time trigger, so every
//! operator bolted periodic work onto external cron or systemd timers and
//! re-solved notification wiring, overlap, and change detection themselves
//! (#10073).
//!
//! A schedule is split deliberately in two:
//!
//! - the **declaration** (`Schedule`) is reviewable configuration under
//!   `~/.config/homeboy/schedules/{id}.json`
//! - the **runtime record** (`ScheduleState`) churns on every run and lives
//!   apart, so the declaration stays diffable
//!
//! The default reporting policy is *notify on change*. A fleet check that
//! keeps returning "healthy" should be silent; a notification should mean
//! something needs a human.

mod entity;
pub mod execution;
pub mod state;
pub mod ticker;
pub mod types;

pub use execution::{
    result_digest, run_schedule, ScheduleCommandRunner, ScheduleRunOutcome, SubprocessRunner,
};
pub use state::{load_state, remove_state, save_state, ScheduleState};
pub use ticker::{reclaim_stale_runs, ScheduleTicker};
pub use types::{Cadence, NotifyPolicy, OverlapPolicy, Schedule};

crate::entity_crud!(Schedule; list_ids);

/// Schedules that are due at `now`, in declaration order.
pub fn due_schedules(now: chrono::DateTime<chrono::Utc>) -> crate::Result<Vec<Schedule>> {
    Ok(list()?
        .into_iter()
        .filter(|schedule| load_state(&schedule.id).is_due(schedule, now))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(id: &str, enabled: bool) -> Schedule {
        Schedule {
            id: id.to_string(),
            command: vec!["triage".to_string()],
            every: Cadence::from_seconds(3_600).expect("cadence"),
            notify_on: NotifyPolicy::default(),
            on_overlap: OverlapPolicy::default(),
            notification_transport: None,
            notification_route: None,
            jitter_seconds: None,
            enabled,
            description: None,
            aliases: Vec::new(),
        }
    }

    #[test]
    fn due_schedules_skips_disabled_and_recently_run_definitions() {
        crate::test_support::with_isolated_home(|_| {
            save(&schedule("never-run", true)).expect("save");
            save(&schedule("disabled", false)).expect("save");
            save(&schedule("just-ran", true)).expect("save");

            save_state(
                "just-ran",
                &ScheduleState {
                    last_run_at: Some(chrono::Utc::now().to_rfc3339()),
                    ..Default::default()
                },
            )
            .expect("save state");

            let due = due_schedules(chrono::Utc::now()).expect("due schedules");
            let ids: Vec<&str> = due.iter().map(|s| s.id.as_str()).collect();

            assert!(ids.contains(&"never-run"));
            assert!(!ids.contains(&"disabled"));
            assert!(!ids.contains(&"just-ran"));
        });
    }
}
