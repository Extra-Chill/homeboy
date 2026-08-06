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
    raw_digest, result_digest, run_schedule, sequence_digest, ScheduleCommandResult,
    ScheduleCommandRunner, ScheduleRunOutcome, ScheduleStepOutcome, SubprocessRunner,
};
pub use state::{load_state, remove_state, save_state, ScheduleState};
pub use ticker::{reclaim_stale_runs, ScheduleTicker};
pub use types::{
    Cadence, ExecCommand, NotifyPolicy, OverlapPolicy, Schedule, ScheduleStep, ScheduledCommand,
};

crate::entity_crud!(Schedule; list_ids);

pub const AUTOMATIC_RETENTION_SCHEDULE_ID: &str = "automatic-retention";

/// Install the built-in bounded retention pass unless an operator already owns
/// this schedule's declaration. Disabling the installed schedule is the opt-out.
pub fn ensure_automatic_retention_schedule() -> crate::Result<Schedule> {
    if exists(AUTOMATIC_RETENTION_SCHEDULE_ID) {
        return load(AUTOMATIC_RETENTION_SCHEDULE_ID);
    }

    let schedule = Schedule {
        id: AUTOMATIC_RETENTION_SCHEDULE_ID.to_string(),
        command: Some(vec![
            "cleanup".to_string(),
            "automatic-retention".to_string(),
        ]),
        exec: None,
        steps: Vec::new(),
        every: Cadence::from_seconds(60 * 60)?,
        notify_on: NotifyPolicy::Change,
        on_overlap: OverlapPolicy::Skip,
        notification_transport: None,
        notification_route: None,
        jitter_seconds: None,
        enabled: true,
        description: Some("Bounded automatic retention of Homeboy-managed storage.".to_string()),
        aliases: Vec::new(),
    };
    save(&schedule)?;
    Ok(schedule)
}

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
            command: Some(vec!["triage".to_string()]),
            exec: None,
            steps: Vec::new(),
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

    #[test]
    fn automatic_retention_schedule_is_installed_once_and_preserves_opt_out() {
        crate::test_support::with_isolated_home(|_| {
            let installed = ensure_automatic_retention_schedule().expect("install schedule");
            assert_eq!(installed.id, AUTOMATIC_RETENTION_SCHEDULE_ID);
            assert_eq!(
                installed.command,
                Some(vec![
                    "cleanup".to_string(),
                    "automatic-retention".to_string()
                ])
            );
            assert_eq!(installed.every.seconds(), 3_600);
            assert_eq!(installed.on_overlap, OverlapPolicy::Skip);

            let mut opted_out = installed;
            opted_out.enabled = false;
            opted_out.every = Cadence::from_seconds(7_200).expect("cadence");
            save(&opted_out).expect("save opt-out");

            let preserved = ensure_automatic_retention_schedule().expect("preserve schedule");
            assert!(!preserved.enabled);
            assert_eq!(preserved.every.seconds(), 7_200);
        });
    }
}
