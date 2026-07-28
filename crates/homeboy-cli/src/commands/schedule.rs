//! `homeboy schedule` — declare homeboy commands that run on a cadence.

use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::core::error::{Error, Result};
use homeboy::core::schedule::{
    self, Cadence, ExecCommand, NotifyPolicy, OverlapPolicy, Schedule, ScheduleRunOutcome,
    ScheduleState, ScheduleStep, SubprocessRunner,
};

use super::CmdResult;

#[derive(Args)]
pub struct ScheduleArgs {
    #[command(subcommand)]
    command: ScheduleCommand,
}

#[derive(Subcommand)]
enum ScheduleCommand {
    /// Declare a scheduled run
    Add(Box<AddArgs>),
    /// List declared schedules with their last and next run
    List,
    /// Show one schedule and its runtime state
    Show { id: String },
    /// Remove a schedule and its runtime state
    Remove { id: String },
    /// Run a schedule now, regardless of whether it is due
    Run { id: String },
    /// Enable a disabled schedule
    Enable { id: String },
    /// Disable a schedule without deleting it
    Disable { id: String },
    /// Run every schedule that is currently due
    Tick(TickArgs),
}

#[derive(Args)]
pub struct AddArgs {
    /// Schedule id
    id: String,

    /// Homeboy command to run, without the leading binary name
    /// (for example: --command "fleet check prod").
    ///
    /// Repeat to declare an ordered sequence. Steps run in order and stop at
    /// the first failure.
    #[arg(long)]
    command: Vec<String>,

    /// External program to run. Executed directly, never through a shell.
    ///
    /// Repeat to declare an ordered sequence. Pair each with --exec-arg and
    /// --working-dir, which apply to the most recent --exec.
    #[arg(long)]
    exec: Vec<String>,

    /// Argument for the preceding --exec. Repeat for each argument; values are
    /// passed through untouched, so an argument may contain spaces.
    #[arg(long = "exec-arg", requires = "exec")]
    exec_arg: Vec<String>,

    /// Directory to run the preceding --exec from
    #[arg(long, requires = "exec")]
    working_dir: Option<String>,

    /// How often to run: 30m, 24h, 1h30m, 7d
    #[arg(long)]
    every: String,

    /// When to notify: change (default), failure, or always
    #[arg(long, default_value = "change")]
    notify_on: String,

    /// What to do if the previous run is still going: skip (default) or allow
    #[arg(long, default_value = "skip")]
    on_overlap: String,

    /// Notification transport id (requires --notification-route)
    #[arg(long, requires = "notification_route")]
    notification_transport: Option<String>,

    /// Notification route (requires --notification-transport)
    #[arg(long, requires = "notification_transport")]
    notification_route: Option<String>,

    /// Spread runs across a window, in seconds, so many schedules sharing a
    /// cadence do not fire at the same instant
    #[arg(long)]
    jitter_seconds: Option<u64>,

    /// Human-readable note about why this schedule exists
    #[arg(long)]
    description: Option<String>,

    /// Replace an existing schedule with the same id
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
pub struct TickArgs {
    /// Report what is due without running anything
    #[arg(long)]
    dry_run: bool,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ScheduleOutput {
    One(Box<ScheduleView>),
    Many(Vec<ScheduleView>),
    Run(Box<ScheduleRunOutcome>),
    Tick(TickReport),
    Removed { id: String, removed: bool },
}

#[derive(Serialize)]
pub struct ScheduleView {
    #[serde(flatten)]
    schedule: ScheduleSummary,
    state: ScheduleState,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_run_at: Option<String>,
    due: bool,
}

#[derive(Serialize)]
pub struct ScheduleSummary {
    id: String,
    command: String,
    every: String,
    notify_on: String,
    on_overlap: String,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notification_transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    jitter_seconds: Option<u64>,
}

#[derive(Serialize)]
pub struct TickReport {
    due: Vec<String>,
    dry_run: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    runs: Vec<ScheduleRunOutcome>,
}

fn view(schedule: Schedule) -> ScheduleView {
    let state = schedule::load_state(&schedule.id);
    let next_run_at = state
        .next_run_at(&schedule)
        .map(|next| next.to_rfc3339())
        .or_else(|| Some("due now".to_string()));
    let due = state.is_due(&schedule, chrono::Utc::now());
    ScheduleView {
        schedule: ScheduleSummary {
            id: schedule.id.clone(),
            command: schedule.command_display(),
            every: schedule.every.to_string(),
            notify_on: schedule.notify_on.as_str().to_string(),
            on_overlap: schedule.on_overlap.as_str().to_string(),
            enabled: schedule.enabled,
            description: schedule.description.clone(),
            notification_transport: schedule.notification_transport.clone(),
            jitter_seconds: schedule.jitter_seconds,
        },
        state,
        next_run_at,
        due,
    }
}

/// Build the declared step sequence.
///
/// `--command` and `--exec` may each be repeated. Sequencing is
/// commands-then-programs rather than interleaved, because clap does not
/// preserve relative order across different flags — an interleaved sequence
/// would silently reorder itself, which is worse than not offering it.
fn build_steps(add: &AddArgs) -> Result<Vec<ScheduleStep>> {
    let mut steps: Vec<ScheduleStep> = Vec::new();

    for raw in &add.command {
        steps.push(ScheduleStep {
            command: Some(split_command(raw)?),
            exec: None,
        });
    }

    if add.exec.len() > 1 && (!add.exec_arg.is_empty() || add.working_dir.is_some()) {
        return Err(Error::validation_invalid_argument(
            "exec",
            "--exec-arg and --working-dir cannot be shared across multiple --exec steps",
            None,
            Some(vec![
                "Declare one --exec per schedule, or edit the schedule file to give each step its own arguments."
                    .to_string(),
            ]),
        ));
    }

    for program in &add.exec {
        steps.push(ScheduleStep {
            command: None,
            exec: Some(ExecCommand {
                program: program.clone(),
                args: add.exec_arg.clone(),
                working_dir: add.working_dir.clone(),
            }),
        });
    }

    if steps.is_empty() {
        return Err(Error::validation_invalid_argument(
            "command",
            "A schedule needs something to run",
            Some(add.id.clone()),
            Some(vec![
                "Pass --command 'fleet check prod', or --exec with a program.".to_string(),
            ]),
        ));
    }
    Ok(steps)
}

/// Split a command string into argv.
///
/// Supports quoting so an argument may contain spaces.
fn split_command(raw: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has_current = false;

    for ch in raw.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                has_current = true;
            }
            None if ch.is_whitespace() => {
                if has_current {
                    args.push(std::mem::take(&mut current));
                    has_current = false;
                }
            }
            None => {
                current.push(ch);
                has_current = true;
            }
        }
    }
    if quote.is_some() {
        return Err(Error::validation_invalid_argument(
            "command",
            "Unbalanced quote in the scheduled command",
            Some(raw.to_string()),
            None,
        ));
    }
    if has_current {
        args.push(current);
    }
    if args.is_empty() {
        return Err(Error::validation_invalid_argument(
            "command",
            "A schedule needs a command to run",
            Some(raw.to_string()),
            None,
        ));
    }
    Ok(args)
}

pub fn run(args: ScheduleArgs) -> CmdResult<ScheduleOutput> {
    match args.command {
        ScheduleCommand::Add(add) => {
            if schedule::exists(&add.id) && !add.force {
                return Err(Error::validation_invalid_argument(
                    "id",
                    format!("Schedule '{}' already exists", add.id),
                    Some(add.id.clone()),
                    Some(vec!["Pass --force to replace it.".to_string()]),
                ));
            }
            let steps = build_steps(&add)?;
            // A single step is stored in the flat form so simple schedules stay
            // simple to read and to diff.
            let (command, exec, steps) = if steps.len() == 1 {
                let only = steps.into_iter().next().unwrap_or_default();
                (only.command, only.exec, Vec::new())
            } else {
                (None, None, steps)
            };
            let declared = Schedule {
                id: add.id.clone(),
                command,
                exec,
                steps,
                every: Cadence::parse(&add.every)?,
                notify_on: add.notify_on.parse::<NotifyPolicy>()?,
                on_overlap: match add.on_overlap.trim().to_ascii_lowercase().as_str() {
                    "skip" => OverlapPolicy::Skip,
                    "allow" => OverlapPolicy::Allow,
                    other => {
                        return Err(Error::validation_invalid_argument(
                            "on-overlap",
                            format!("Unknown overlap policy '{other}'"),
                            Some(other.to_string()),
                            Some(vec!["Use skip or allow.".to_string()]),
                        ))
                    }
                },
                notification_transport: add.notification_transport,
                notification_route: add.notification_route,
                jitter_seconds: add.jitter_seconds,
                enabled: true,
                description: add.description,
                aliases: Vec::new(),
            };
            schedule::save(&declared)?;
            Ok((ScheduleOutput::One(Box::new(view(declared))), 0))
        }
        ScheduleCommand::List => {
            let mut all: Vec<Schedule> = schedule::list()?;
            all.sort_by(|a, b| a.id.cmp(&b.id));
            Ok((ScheduleOutput::Many(all.into_iter().map(view).collect()), 0))
        }
        ScheduleCommand::Show { id } => {
            let declared = schedule::load(&id)?;
            Ok((ScheduleOutput::One(Box::new(view(declared))), 0))
        }
        ScheduleCommand::Remove { id } => {
            schedule::delete(&id)?;
            schedule::remove_state(&id);
            Ok((ScheduleOutput::Removed { id, removed: true }, 0))
        }
        ScheduleCommand::Run { id } => {
            let declared = schedule::load(&id)?;
            let runner = SubprocessRunner::new()?;
            let outcome = schedule::run_schedule(&declared, &runner);
            // A scheduled run that failed should fail the invoking command, so
            // an external trigger can react without parsing the payload.
            let exit_code = i32::from(outcome.status != "succeeded");
            Ok((ScheduleOutput::Run(Box::new(outcome)), exit_code))
        }
        ScheduleCommand::Enable { id } => set_enabled(&id, true),
        ScheduleCommand::Disable { id } => set_enabled(&id, false),
        ScheduleCommand::Tick(tick) => {
            let due = schedule::due_schedules(chrono::Utc::now())?;
            let ids: Vec<String> = due.iter().map(|s| s.id.clone()).collect();
            if tick.dry_run {
                return Ok((
                    ScheduleOutput::Tick(TickReport {
                        due: ids,
                        dry_run: true,
                        runs: Vec::new(),
                    }),
                    0,
                ));
            }
            let runner = SubprocessRunner::new()?;
            let runs: Vec<ScheduleRunOutcome> = due
                .iter()
                .map(|declared| schedule::run_schedule(declared, &runner))
                .collect();
            let failed = runs.iter().any(|run| run.status != "succeeded");
            Ok((
                ScheduleOutput::Tick(TickReport {
                    due: ids,
                    dry_run: false,
                    runs,
                }),
                i32::from(failed),
            ))
        }
    }
}

fn set_enabled(id: &str, enabled: bool) -> CmdResult<ScheduleOutput> {
    let mut declared = schedule::load(id)?;
    declared.enabled = enabled;
    schedule::save(&declared)?;
    Ok((ScheduleOutput::One(Box::new(view(declared))), 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_plain_and_quoted_commands() {
        assert_eq!(
            split_command("fleet check prod").expect("split"),
            vec!["fleet", "check", "prod"]
        );
        assert_eq!(
            split_command("  triage   --json  ").expect("split"),
            vec!["triage", "--json"]
        );
        assert_eq!(
            split_command(r#"deploy prod --note "nightly drift check""#).expect("split"),
            vec!["deploy", "prod", "--note", "nightly drift check"]
        );
    }

    /// An empty quoted argument is meaningful (it is still an argument), so it
    /// must survive splitting rather than being dropped as whitespace.
    #[test]
    fn preserves_an_explicitly_empty_argument() {
        assert_eq!(
            split_command(r#"cmd "" tail"#).expect("split"),
            vec!["cmd", "", "tail"]
        );
    }

    #[test]
    fn rejects_unbalanced_quotes_and_empty_commands() {
        assert!(split_command(r#"deploy "unclosed"#).is_err());
        assert!(split_command("   ").is_err());
    }
}
