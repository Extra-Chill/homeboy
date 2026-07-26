//! Schedule declarations: what to run, how often, and when to say something.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// How often a schedule is due.
///
/// Interval only, deliberately. Cron expressions would be the first cron
/// parser in the tree; an interval covers "check this periodically", which is
/// what every current caller wants, and can be extended later without
/// changing the stored shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cadence {
    /// Seconds between runs.
    seconds: u64,
}

impl Cadence {
    pub fn from_seconds(seconds: u64) -> Result<Self> {
        if seconds == 0 {
            return Err(Error::validation_invalid_argument(
                "every",
                "A schedule interval must be greater than zero",
                Some(seconds.to_string()),
                None,
            ));
        }
        Ok(Self { seconds })
    }

    pub fn seconds(&self) -> u64 {
        self.seconds
    }

    /// Parse a compact duration such as `30m`, `24h`, `1h30m`, or `7d`.
    ///
    /// A bare number is rejected rather than assumed: `--every 30` is
    /// ambiguous between seconds and minutes, and guessing would silently run
    /// something 60x more often than intended.
    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(Self::invalid(value, "an interval is required"));
        }

        let mut total: u64 = 0;
        let mut digits = String::new();
        let mut saw_unit = false;

        for ch in trimmed.chars() {
            if ch.is_ascii_digit() {
                digits.push(ch);
                continue;
            }
            let unit_seconds = match ch {
                's' => 1,
                'm' => 60,
                'h' => 60 * 60,
                'd' => 24 * 60 * 60,
                _ => {
                    return Err(Self::invalid(
                        value,
                        "use s, m, h, or d — for example 30m, 24h, or 1h30m",
                    ))
                }
            };
            if digits.is_empty() {
                return Err(Self::invalid(value, "each unit needs a number before it"));
            }
            let amount: u64 = digits
                .parse()
                .map_err(|_| Self::invalid(value, "the number is too large"))?;
            total = total
                .checked_add(amount.saturating_mul(unit_seconds))
                .ok_or_else(|| Self::invalid(value, "the interval is too large"))?;
            digits.clear();
            saw_unit = true;
        }

        if !digits.is_empty() || !saw_unit {
            return Err(Self::invalid(
                value,
                "add a unit — s, m, h, or d — for example 30m rather than 30",
            ));
        }

        Self::from_seconds(total)
    }

    fn invalid(value: &str, problem: &str) -> Error {
        Error::validation_invalid_argument(
            "every",
            format!("Invalid interval '{value}': {problem}"),
            Some(value.to_string()),
            None,
        )
    }
}

impl std::fmt::Display for Cadence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut remaining = self.seconds;
        let mut rendered = String::new();
        for (unit, size) in [("d", 86_400u64), ("h", 3_600), ("m", 60), ("s", 1)] {
            let count = remaining / size;
            if count > 0 {
                rendered.push_str(&format!("{count}{unit}"));
                remaining -= count * size;
            }
        }
        write!(f, "{rendered}")
    }
}

/// When a completed run is worth interrupting a human for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyPolicy {
    /// Notify only when the result differs from the previous run. The useful
    /// default for fleet-shaped checks: silence while healthy, a ping when
    /// something drifts.
    #[default]
    Change,
    /// Notify only when the run fails.
    Failure,
    /// Notify on every run.
    Always,
}

impl NotifyPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Change => "change",
            Self::Failure => "failure",
            Self::Always => "always",
        }
    }
}

impl std::str::FromStr for NotifyPolicy {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "change" => Ok(Self::Change),
            "failure" => Ok(Self::Failure),
            "always" => Ok(Self::Always),
            other => Err(Error::validation_invalid_argument(
                "notify-on",
                format!("Unknown notify policy '{other}'"),
                Some(other.to_string()),
                Some(vec!["Use change, failure, or always.".to_string()]),
            )),
        }
    }
}

/// What to do when a schedule comes due while its previous run is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    /// Leave the in-flight run alone and try again next tick. Prevents a slow
    /// check from stacking copies of itself.
    #[default]
    Skip,
    /// Start the run regardless.
    Allow,
}

impl OverlapPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Allow => "allow",
        }
    }
}

/// An external program run by a schedule.
///
/// Program and arguments are kept separate and passed as an argument vector.
/// There is deliberately no shell: nothing here is word-split, so an argument
/// containing spaces stays one argument and there is no quoting or injection
/// surface to reason about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecCommand {
    pub program: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Directory to run from. Many useful commands — test runners, build
    /// tools — only work from their project root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

/// What a schedule runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduledCommand<'a> {
    /// Homeboy arguments, run through the homeboy binary.
    Homeboy(&'a [String]),
    /// An external program.
    Exec(&'a ExecCommand),
}

/// A declared periodic run.
///
/// The declaration is stable, reviewable configuration. Everything that
/// changes as it runs — last status, next due time, in-flight marker — lives
/// in a separate runtime record so this file stays diffable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,

    /// Homeboy arguments to run, without the leading binary name.
    /// For example `["fleet", "check", "prod"]`.
    ///
    /// Mutually exclusive with `exec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,

    /// An external program to run instead of a homeboy command.
    ///
    /// Mutually exclusive with `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<ExecCommand>,

    pub every: Cadence,

    #[serde(default)]
    pub notify_on: NotifyPolicy,

    #[serde(default)]
    pub on_overlap: OverlapPolicy,

    /// Notification transport id, paired with `notification_route`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_transport: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_route: Option<String>,

    /// Spread load when many schedules share a cadence. Each run is delayed by
    /// a deterministic offset derived from the schedule id, up to this many
    /// seconds, so a fleet does not stampede on the hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jitter_seconds: Option<u64>,

    /// A disabled schedule is never due, but is kept so it can be re-enabled
    /// without re-deriving its definition.
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Schedule {
    /// What this schedule runs.
    ///
    /// Validation guarantees exactly one of `command` / `exec` is set, so a
    /// stored schedule always resolves.
    pub fn scheduled_command(&self) -> Option<ScheduledCommand<'_>> {
        match (&self.command, &self.exec) {
            (Some(argv), None) => Some(ScheduledCommand::Homeboy(argv.as_slice())),
            (None, Some(exec)) => Some(ScheduledCommand::Exec(exec)),
            _ => None,
        }
    }

    /// Human-readable rendering of what runs, for logs and notifications.
    pub fn command_display(&self) -> String {
        match self.scheduled_command() {
            Some(ScheduledCommand::Homeboy(argv)) => format!("homeboy {}", argv.join(" ")),
            Some(ScheduledCommand::Exec(exec)) => {
                if exec.args.is_empty() {
                    exec.program.clone()
                } else {
                    format!("{} {}", exec.program, exec.args.join(" "))
                }
            }
            None => "<no command>".to_string(),
        }
    }

    /// Deterministic per-schedule jitter offset, in seconds.
    ///
    /// Derived from the id so a given schedule always lands at the same offset
    /// rather than walking across the window on every restart.
    pub fn jitter_offset_seconds(&self) -> u64 {
        let Some(window) = self.jitter_seconds.filter(|window| *window > 0) else {
            return 0;
        };
        let mut hash: u64 = 1469598103934665603;
        for byte in self.id.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(1099511628211);
        }
        hash % window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compound_and_single_unit_intervals() {
        assert_eq!(Cadence::parse("30m").expect("30m").seconds(), 1_800);
        assert_eq!(Cadence::parse("24h").expect("24h").seconds(), 86_400);
        assert_eq!(Cadence::parse("1h30m").expect("1h30m").seconds(), 5_400);
        assert_eq!(Cadence::parse("7d").expect("7d").seconds(), 604_800);
        assert_eq!(Cadence::parse(" 45s ").expect("45s").seconds(), 45);
    }

    /// A bare number is ambiguous between seconds and minutes. Guessing wrong
    /// runs the command 60x more often than intended, so it is rejected.
    #[test]
    fn rejects_a_unitless_interval_rather_than_guessing() {
        let error = Cadence::parse("30").expect_err("unitless must be rejected");
        assert!(
            error.to_string().contains("add a unit"),
            "error should name the fix, got: {error}"
        );
    }

    #[test]
    fn rejects_zero_and_unknown_units() {
        assert!(Cadence::parse("0s").is_err());
        assert!(Cadence::parse("5w").is_err());
        assert!(Cadence::parse("h").is_err());
        assert!(Cadence::parse("").is_err());
    }

    #[test]
    fn renders_intervals_in_the_form_it_accepts() {
        let rendered = Cadence::from_seconds(5_400).expect("cadence").to_string();
        assert_eq!(rendered, "1h30m");
        assert_eq!(
            Cadence::parse(&rendered).expect("round trip").seconds(),
            5_400
        );
    }

    #[test]
    fn jitter_is_stable_per_id_and_bounded_by_the_window() {
        let schedule = |id: &str| Schedule {
            id: id.to_string(),
            command: Some(vec!["triage".to_string()]),
            exec: None,
            every: Cadence::from_seconds(3_600).expect("cadence"),
            notify_on: NotifyPolicy::default(),
            on_overlap: OverlapPolicy::default(),
            notification_transport: None,
            notification_route: None,
            jitter_seconds: Some(300),
            enabled: true,
            description: None,
            aliases: Vec::new(),
        };

        let first = schedule("nightly-harvest").jitter_offset_seconds();
        assert_eq!(first, schedule("nightly-harvest").jitter_offset_seconds());
        assert!(first < 300);
        assert_eq!(
            0,
            Schedule {
                jitter_seconds: None,
                ..schedule("no-jitter")
            }
            .jitter_offset_seconds()
        );
    }

    /// Schedules declared before `exec` existed stored `command` as a bare
    /// array with no `exec` key. Those files must keep loading untouched.
    #[test]
    fn a_pre_exec_declaration_still_deserializes() {
        let stored = r#"{
            "id": "nightly",
            "command": ["harvest", "production", "--check"],
            "every": 86400,
            "notify_on": "change",
            "on_overlap": "skip",
            "enabled": true
        }"#;

        let schedule: Schedule = serde_json::from_str(stored).expect("legacy declaration loads");
        assert_eq!(
            schedule.command,
            Some(vec![
                "harvest".to_string(),
                "production".to_string(),
                "--check".to_string()
            ])
        );
        assert!(schedule.exec.is_none());
        assert!(matches!(
            schedule.scheduled_command(),
            Some(ScheduledCommand::Homeboy(_))
        ));
        assert_eq!(
            schedule.command_display(),
            "homeboy harvest production --check"
        );
    }

    #[test]
    fn an_exec_declaration_resolves_to_a_program() {
        let stored = r#"{
            "id": "probe",
            "exec": { "program": "/usr/bin/true", "args": ["--now"] },
            "every": 3600
        }"#;

        let schedule: Schedule = serde_json::from_str(stored).expect("exec declaration loads");
        assert!(schedule.command.is_none());
        assert!(matches!(
            schedule.scheduled_command(),
            Some(ScheduledCommand::Exec(_))
        ));
        assert_eq!(schedule.command_display(), "/usr/bin/true --now");
    }

    #[test]
    fn notify_policy_round_trips_through_its_string_form() {
        for policy in [
            NotifyPolicy::Change,
            NotifyPolicy::Failure,
            NotifyPolicy::Always,
        ] {
            let parsed: NotifyPolicy = policy.as_str().parse().expect("parse policy");
            assert_eq!(parsed, policy);
        }
        assert!("hourly".parse::<NotifyPolicy>().is_err());
    }
}
