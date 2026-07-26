//! Schedules as reviewable configuration entities.

use std::path::PathBuf;

use crate::config::ConfigEntity;
use crate::error::{Error, Result};
use crate::paths;

use super::types::Schedule;

impl ConfigEntity for Schedule {
    const ENTITY_TYPE: &'static str = "schedule";
    const DIR_NAME: &'static str = "schedules";

    fn id(&self) -> &str {
        &self.id
    }

    fn set_id(&mut self, id: String) {
        self.id = id;
    }

    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::schedule_not_found(id, suggestions)
    }

    fn config_path(id: &str) -> Result<PathBuf> {
        Ok(paths::homeboy()?
            .join("schedules")
            .join(format!("{}.json", id)))
    }

    fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "id",
                "A schedule needs an id",
                None,
                None,
            ));
        }
        if self.command.is_empty() {
            return Err(Error::validation_invalid_argument(
                "command",
                "A schedule needs a command to run",
                Some(self.id.clone()),
                Some(vec![
                    "Pass the homeboy arguments to run, for example: --command 'fleet check prod'"
                        .to_string(),
                ]),
            ));
        }
        if self.command.iter().any(|arg| arg.trim().is_empty()) {
            return Err(Error::validation_invalid_argument(
                "command",
                "A schedule command cannot contain empty arguments",
                Some(self.command.join(" ")),
                None,
            ));
        }
        // Scheduling a schedule command would let a tick mutate its own
        // definitions, or recurse.
        if self
            .command
            .first()
            .is_some_and(|first| first.trim() == "schedule")
        {
            return Err(Error::validation_invalid_argument(
                "command",
                "A schedule cannot run the schedule command itself",
                Some(self.command.join(" ")),
                Some(vec![
                    "Schedule the work you want repeated, not the scheduler.".to_string(),
                ]),
            ));
        }
        // A transport without a route (or the reverse) silently never
        // notifies, which is worse than refusing it.
        match (
            self.notification_transport.as_deref(),
            self.notification_route.as_deref(),
        ) {
            (Some(transport), Some(route)) => {
                crate::notification_route::NotificationRoute::new(transport, route)?;
            }
            (None, None) => {}
            (Some(_), None) => {
                return Err(Error::validation_invalid_argument(
                    "notification-route",
                    "A notification transport also needs a route",
                    Some(self.id.clone()),
                    None,
                ));
            }
            (None, Some(_)) => {
                return Err(Error::validation_invalid_argument(
                    "notification-transport",
                    "A notification route also needs a transport",
                    Some(self.id.clone()),
                    None,
                ));
            }
        }
        Ok(())
    }

    fn aliases(&self) -> &[String] {
        &self.aliases
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::types::{Cadence, NotifyPolicy, OverlapPolicy};

    fn schedule() -> Schedule {
        Schedule {
            id: "nightly-harvest".to_string(),
            command: vec!["harvest".to_string(), "prod".to_string()],
            every: Cadence::from_seconds(86_400).expect("cadence"),
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
    fn a_complete_schedule_validates() {
        schedule().validate().expect("valid schedule");
    }

    #[test]
    fn a_schedule_without_a_command_is_rejected() {
        let invalid = Schedule {
            command: Vec::new(),
            ..schedule()
        };
        assert!(invalid.validate().is_err());
    }

    /// Half-configured notification silently never delivers, so it is refused
    /// at declaration time rather than at 3am.
    #[test]
    fn a_transport_without_a_route_is_rejected() {
        let invalid = Schedule {
            notification_transport: Some("discord.run-completion".to_string()),
            notification_route: None,
            ..schedule()
        };
        let error = invalid
            .validate()
            .expect_err("half-configured notification");
        assert!(error.to_string().contains("route"), "got: {error}");

        let invalid = Schedule {
            notification_transport: None,
            notification_route: Some("discord:v1:channel:1".to_string()),
            ..schedule()
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn scheduling_the_scheduler_is_rejected() {
        let invalid = Schedule {
            command: vec!["schedule".to_string(), "run".to_string(), "x".to_string()],
            ..schedule()
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn schedules_round_trip_through_config_storage() {
        crate::test_support::with_isolated_home(|_| {
            let schedule = schedule();
            crate::config::save(&schedule).expect("save schedule");

            let loaded = crate::config::load::<Schedule>("nightly-harvest").expect("load");
            assert_eq!(loaded.command, vec!["harvest", "prod"]);
            assert_eq!(loaded.every.seconds(), 86_400);
            assert!(loaded.enabled);

            let all = crate::config::list::<Schedule>().expect("list");
            assert_eq!(all.len(), 1);

            crate::config::delete::<Schedule>("nightly-harvest").expect("delete");
            assert!(!crate::config::exists::<Schedule>("nightly-harvest"));
        });
    }
}
