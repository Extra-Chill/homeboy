//! Controller-local workflow instance readers.
use std::time::{Duration, Instant};

use homeboy::core::daemon::{workflow_job::chain_successor, LocalControllerJobClient};
use homeboy::core::{Error, Result};
use serde_json::Value;

use super::types::{RunsCommand, RunsOutput};
use super::{CmdResult, RunsArgs, RunsListArgs};

impl RunsArgs {
    pub(crate) fn workflow_reader_id(&self) -> Option<&str> {
        let id = match &self.command {
            RunsCommand::Show { run_id, .. } => run_id,
            RunsCommand::Watch(args) => &args.run_id,
            _ => return None,
        };
        id.starts_with("wf-").then_some(id.as_str())
    }

    pub(crate) fn workflow_discovery(&self) -> bool {
        matches!(&self.command, RunsCommand::List(args) if args.kind.as_deref() == Some("workflow"))
    }
}

pub(crate) fn validate_discovery(args: &RunsListArgs) -> Result<()> {
    if args.runner.is_some()
        || args.component_id.is_some()
        || args.rig.is_some()
        || args.scenario_id.is_some()
        || args.status.is_some()
        || args.running
        || args.since.is_some()
        || args.until.is_some()
        || args.id.is_some()
        || args.command_contains.is_some()
        || args.workspace.is_some()
        || args.correlation.is_some()
        || args.include_mirrors
        || args.include_active_runner_jobs
        || args.limit != super::types::DEFAULT_LIMIT
        || args.active
        || args.repo.is_some()
        || args.task_url.is_some()
        || args.run_placement.is_some()
        || args.parent_id.is_some()
        || args.branch.is_some()
        || args.cursor.is_some()
        || args.full
    {
        return Err(Error::validation_invalid_argument(
            "kind",
            "discovery filters are not supported with --kind workflow",
            args.kind.clone(),
            None,
        ));
    }
    Ok(())
}

pub(crate) fn run(args: RunsArgs) -> CmdResult<RunsOutput> {
    if let RunsCommand::List(list) = &args.command {
        validate_discovery(list)?;
    }
    if matches!(&args.command, RunsCommand::Watch(args) if args.notify) {
        return Err(Error::validation_invalid_argument(
            "notify",
            "--notify is not supported for workflow instances",
            None,
            None,
        ));
    }
    let client = LocalControllerJobClient::connect()?;
    let (view, code) = match args.command {
        RunsCommand::List(_) => (client.workflow_list()?, 0),
        RunsCommand::Show { run_id, .. } => {
            let view = client.workflow(&run_id)?;
            let code = exit_code(&view);
            (view, code)
        }
        RunsCommand::Watch(args) => {
            let interval = super::common::parse_duration(&args.interval)?;
            let timeout = (!args.forever)
                .then(|| super::common::parse_duration(&args.timeout))
                .transpose()?;
            watch_instance(&client, &args.run_id, interval, timeout)?
        }
        _ => unreachable!("workflow reader dispatch"),
    };
    Ok((RunsOutput::Cook(view), code))
}

/// Shared by `runs watch` and `workflow run --wait`. A chain shares one deadline.
pub(crate) fn watch_instance(
    client: &LocalControllerJobClient,
    instance: &str,
    interval: Duration,
    timeout: Option<Duration>,
) -> Result<(Value, i32)> {
    poll_instance(instance, interval, timeout, |id| client.workflow(id))
}

fn poll_instance(
    instance: &str,
    interval: Duration,
    timeout: Option<Duration>,
    mut poll: impl FnMut(&str) -> Result<Value>,
) -> Result<(Value, i32)> {
    let started = Instant::now();
    let mut last = String::new();
    let mut watched = instance.to_string();
    loop {
        let view = poll(&watched)?;
        let line = progress_line(&view);
        if line != last {
            eprintln!("{line}");
            last = line;
        }
        let child = chain_successor(&view);
        if child.is_none() && is_terminal(&view) {
            let code = exit_code(&view);
            return Ok((view, code));
        }
        if timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
            return Ok((view, crate::commands::utils::watch::TIMEOUT_EXIT_CODE));
        }
        if let Some(child) = child {
            watched = child;
            last.clear();
            continue;
        }
        let delay = timeout
            .map(|timeout| interval.min(timeout.saturating_sub(started.elapsed())))
            .unwrap_or(interval);
        std::thread::sleep(delay);
    }
}

fn progress_line(view: &Value) -> String {
    let steps = view["steps"]
        .as_object()
        .map(|steps| {
            steps
                .iter()
                .map(|(id, step)| {
                    let state = step["outcome"]
                        .as_str()
                        .filter(|_| step["phase"] == "done")
                        .unwrap_or_else(|| step["phase"].as_str().unwrap_or("?"));
                    let items = step["items"].as_array().filter(|items| !items.is_empty());
                    match items {
                        Some(items) => format!(
                            "{id}={state}[{}]",
                            items
                                .iter()
                                .map(|item| {
                                    item["outcome"]
                                        .as_str()
                                        .filter(|_| item["phase"] == "done")
                                        .or_else(|| item["run"]["phase"].as_str())
                                        .unwrap_or_else(|| item["phase"].as_str().unwrap_or("?"))
                                        .to_string()
                                })
                                .collect::<Vec<_>>()
                                .join(",")
                        ),
                        None => format!("{id}={state}"),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    format!(
        "{} {} ({} step runs): {steps}",
        view["instance"].as_str().unwrap_or_default(),
        view["status"].as_str().unwrap_or_default(),
        view["step_runs"]
    )
}

fn is_terminal(view: &Value) -> bool {
    !matches!(view["status"].as_str(), Some("running" | "queued"))
}

pub(crate) fn exit_code(view: &Value) -> i32 {
    match view["status"].as_str() {
        Some("succeeded" | "running" | "queued") => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;
    use serde_json::json;

    #[test]
    fn removed_forms_do_not_parse() {
        assert!(Cli::try_parse_from(["homeboy", "workflow", "status", "wf-x"]).is_err());
        assert!(Cli::try_parse_from(["homeboy", "workflow", "list", "--registered"]).is_err());
        assert!(Cli::try_parse_from(["homeboy", "workflow", "list"]).is_ok());
    }

    #[test]
    fn workflow_discovery_rejects_filters() {
        for flags in [
            vec!["--status", "running"],
            vec!["--since", "1h"],
            vec!["--workspace", "x"],
            vec!["--limit", "1"],
            vec!["--runner", "lab"],
            vec!["--include-mirrors"],
            vec!["--repo", "x"],
            vec!["--active"],
        ] {
            let mut argv = vec!["homeboy", "runs", "list", "--kind", "workflow"];
            argv.extend(flags);
            let Commands::Runs(args) = Cli::try_parse_from(argv).unwrap().command else {
                unreachable!()
            };
            let RunsCommand::List(list) = args.command else {
                unreachable!()
            };
            assert!(validate_discovery(&list).is_err());
        }
        let Commands::Runs(args) =
            Cli::try_parse_from(["homeboy", "runs", "list", "--kind", "workflow"])
                .unwrap()
                .command
        else {
            unreachable!()
        };
        let RunsCommand::List(list) = args.command else {
            unreachable!()
        };
        validate_discovery(&list).unwrap();
    }

    #[test]
    fn notify_is_rejected_before_connecting() {
        let Commands::Runs(args) =
            Cli::try_parse_from(["homeboy", "runs", "watch", "wf-x", "--notify"])
                .unwrap()
                .command
        else {
            unreachable!()
        };
        let error = run(args).err().expect("notify must fail validation");
        assert!(error.message.contains("--notify"));
    }

    #[test]
    fn terminal_views_and_exit_codes_are_preserved() {
        for (status, code) in [
            ("succeeded", 0),
            ("failed", 1),
            ("cancelled", 1),
            ("budget_exhausted", 1),
        ] {
            let view = json!({"instance": "wf-x", "status": status});
            let result = poll_instance("wf-x", Duration::ZERO, None, |_| Ok(view.clone())).unwrap();
            assert_eq!(result, (view, code));
        }
        assert_eq!(exit_code(&json!({"status":"running"})), 0);
        assert_eq!(exit_code(&json!({"status":"queued"})), 0);
    }

    #[test]
    fn poll_follows_chain_and_timeout_returns_last_view() {
        let result = poll_instance("wf-parent", Duration::ZERO, None, |id| {
            Ok(if id == "wf-parent" {
                json!({"instance":id,"status":"succeeded","children":["wf-child"]})
            } else {
                json!({"instance":id,"status":"failed"})
            })
        })
        .unwrap();
        assert_eq!(result.0["instance"], "wf-child");
        assert_eq!(result.1, 1);
        let view = json!({"instance":"wf-x", "status":"running"});
        let result = poll_instance("wf-x", Duration::ZERO, Some(Duration::ZERO), |_| {
            Ok(view.clone())
        })
        .unwrap();
        assert_eq!(result.0, view);
        assert_eq!(result.1, crate::commands::utils::watch::TIMEOUT_EXIT_CODE);
    }
}
