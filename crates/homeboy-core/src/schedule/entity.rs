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
        match (&self.command, &self.exec) {
            (Some(argv), None) => validate_homeboy_command(argv)?,
            (None, Some(exec)) => validate_exec_command(exec)?,
            (Some(_), Some(_)) => {
                return Err(Error::validation_invalid_argument(
                    "command",
                    "A schedule runs either a homeboy command or a program, not both",
                    Some(self.id.clone()),
                    Some(vec![
                        "Declare one schedule per thing you want run.".to_string()
                    ]),
                ));
            }
            (None, None) => {
                return Err(Error::validation_invalid_argument(
                    "command",
                    "A schedule needs something to run",
                    Some(self.id.clone()),
                    Some(vec![
                        "Pass --command 'fleet check prod', or --exec with a program.".to_string(),
                    ]),
                ));
            }
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

fn validate_homeboy_command(argv: &[String]) -> Result<()> {
    if argv.is_empty() {
        return Err(Error::validation_invalid_argument(
            "command",
            "A schedule needs a command to run",
            None,
            None,
        ));
    }
    if argv.iter().any(|arg| arg.trim().is_empty()) {
        return Err(Error::validation_invalid_argument(
            "command",
            "A schedule command cannot contain empty arguments",
            Some(argv.join(" ")),
            None,
        ));
    }
    // Scheduling a schedule command would let a tick mutate its own
    // definitions, or recurse.
    if argv.first().is_some_and(|first| first.trim() == "schedule") {
        return Err(Error::validation_invalid_argument(
            "command",
            "A schedule cannot run the schedule command itself",
            Some(argv.join(" ")),
            Some(vec![
                "Schedule the work you want repeated, not the scheduler.".to_string(),
            ]),
        ));
    }
    Ok(())
}

fn validate_exec_command(exec: &super::types::ExecCommand) -> Result<()> {
    if exec.program.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "exec",
            "A scheduled program needs a program to run",
            None,
            None,
        ));
    }
    // The program is executed directly, never through a shell. Shell
    // metacharacters in the program name almost always mean the operator
    // expected shell semantics they are not getting, so say so rather than
    // failing later with a confusing "no such file".
    if exec
        .program
        .contains(['|', ';', '&', '>', '<', '$', '`', '\n'])
    {
        return Err(Error::validation_invalid_argument(
            "exec",
            "A scheduled program is run directly, not through a shell",
            Some(exec.program.clone()),
            Some(vec![
                "Shell operators are not interpreted. To run a pipeline, put it in a script and schedule the script."
                    .to_string(),
            ]),
        ));
    }
    if let Some(dir) = exec.working_dir.as_deref() {
        if dir.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "working-dir",
                "A scheduled program's working directory cannot be empty",
                None,
                None,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::types::{Cadence, NotifyPolicy, OverlapPolicy};

    fn schedule() -> Schedule {
        Schedule {
            id: "nightly-harvest".to_string(),
            command: Some(vec!["harvest".to_string(), "prod".to_string()]),
            exec: None,
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
            command: Some(Vec::new()),
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
            command: Some(vec![
                "schedule".to_string(),
                "run".to_string(),
                "x".to_string(),
            ]),
            ..schedule()
        };
        assert!(invalid.validate().is_err());
    }

    fn exec_schedule() -> Schedule {
        Schedule {
            id: "cert-expiry".to_string(),
            command: None,
            exec: Some(super::super::types::ExecCommand {
                program: "/usr/local/bin/check-cert.sh".to_string(),
                args: vec!["example.com".to_string()],
                working_dir: None,
            }),
            every: Cadence::from_seconds(43_200).expect("cadence"),
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
    fn an_exec_schedule_validates() {
        exec_schedule().validate().expect("valid exec schedule");
    }

    #[test]
    fn declaring_both_a_command_and_a_program_is_rejected() {
        let invalid = Schedule {
            command: Some(vec!["triage".to_string()]),
            ..exec_schedule()
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn declaring_neither_is_rejected() {
        let invalid = Schedule {
            command: None,
            exec: None,
            ..exec_schedule()
        };
        assert!(invalid.validate().is_err());
    }

    /// Programs run directly, never through a shell. An operator writing a
    /// pipeline is expecting semantics they will not get, so say so at
    /// declaration time rather than failing later with "no such file".
    #[test]
    fn shell_metacharacters_in_the_program_are_rejected() {
        for program in [
            "backup.sh | tee log",
            "a && b",
            "echo $HOME",
            "run > out.txt",
        ] {
            let invalid = Schedule {
                exec: Some(super::super::types::ExecCommand {
                    program: program.to_string(),
                    args: Vec::new(),
                    working_dir: None,
                }),
                ..exec_schedule()
            };
            let error = invalid
                .validate()
                .expect_err("shell syntax must be refused");
            assert!(
                error.to_string().contains("not through a shell"),
                "error should explain why, got: {error}"
            );
        }
    }

    /// Arguments are passed through untouched — only the program is checked.
    /// A legitimate argument may contain characters that look shell-ish.
    #[test]
    fn arguments_may_contain_shell_looking_characters() {
        let valid = Schedule {
            exec: Some(super::super::types::ExecCommand {
                program: "grep".to_string(),
                args: vec!["a|b".to_string(), "$PATH".to_string()],
                working_dir: None,
            }),
            ..exec_schedule()
        };
        valid
            .validate()
            .expect("arguments are not shell-interpreted");
    }

    #[test]
    fn exec_schedules_round_trip_through_config_storage() {
        crate::test_support::with_isolated_home(|_| {
            crate::config::save(&exec_schedule()).expect("save");
            let loaded = crate::config::load::<Schedule>("cert-expiry").expect("load");
            let exec = loaded.exec.expect("exec preserved");
            assert_eq!(exec.program, "/usr/local/bin/check-cert.sh");
            assert_eq!(exec.args, vec!["example.com".to_string()]);
            assert!(loaded.command.is_none());
        });
    }

    #[test]
    fn schedules_round_trip_through_config_storage() {
        crate::test_support::with_isolated_home(|_| {
            let schedule = schedule();
            crate::config::save(&schedule).expect("save schedule");

            let loaded = crate::config::load::<Schedule>("nightly-harvest").expect("load");
            assert_eq!(
                loaded.command,
                Some(vec!["harvest".to_string(), "prod".to_string()])
            );
            assert_eq!(loaded.every.seconds(), 86_400);
            assert!(loaded.enabled);

            let all = crate::config::list::<Schedule>().expect("list");
            assert_eq!(all.len(), 1);

            crate::config::delete::<Schedule>("nightly-harvest").expect("delete");
            assert!(!crate::config::exists::<Schedule>("nightly-harvest"));
        });
    }
}
