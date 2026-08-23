use clap::{ArgMatches, Args, FromArgMatches, Subcommand};
use homeboy_core::Error;
use std::path::PathBuf;

use crate::{
    landing, parse_issue_numbers_file, parse_stale_days, run, watch, TriageCommandOutput,
    TriageLandingOptions, TriageOptions, TriageTarget, TriageWatchOptions,
};

pub const COMMAND_NAME: &str = "triage";

/// The typed Triage command shape, independent of CLI composition.
pub fn command() -> clap::Command {
    TriageArgs::augment_args(
        clap::Command::new(COMMAND_NAME).about(
            "Attention reports and watch utilities for components, projects, fleets, and rigs",
        ),
    )
}

/// Parse and execute Triage from its subcommand matches.
pub fn run_command(matches: &ArgMatches) -> homeboy_core::Result<(serde_json::Value, i32)> {
    let args = TriageArgs::from_arg_matches(matches).map_err(|error| {
        Error::validation_invalid_argument(COMMAND_NAME, error.to_string(), None, None)
    })?;
    let (output, exit_code) = run_triage(args)?;
    Ok((
        serde_json::to_value(output).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize triage output".to_string()),
            )
        })?,
        exit_code,
    ))
}

#[derive(Args)]
struct TriageArgs {
    #[command(subcommand)]
    command: Option<TriageCommand>,
    #[arg(long, global = true)]
    issues: bool,
    #[arg(long, global = true)]
    prs: bool,
    #[arg(long, global = true)]
    mine: bool,
    #[arg(long, global = true, conflicts_with = "mine")]
    all: bool,
    #[arg(long, global = true, value_name = "USER")]
    assigned: Option<String>,
    #[arg(long, global = true, value_name = "LABEL")]
    label: Vec<String>,
    #[arg(long, global = true, value_name = "NUMBER")]
    issue: Vec<u64>,
    #[arg(long, global = true, value_name = "PATH")]
    issues_from_file: Option<PathBuf>,
    #[arg(long, global = true)]
    needs_review: bool,
    #[arg(long, global = true)]
    failing_checks: bool,
    #[arg(long, global = true)]
    drilldown: bool,
    #[arg(long, global = true, value_name = "DAYS")]
    stale: Option<String>,
    #[arg(long, global = true, default_value_t = 30)]
    limit: usize,
    #[arg(long, global = true, value_name = "REF")]
    watch: Vec<String>,
    #[arg(long, global = true, value_name = "STATE")]
    until: Option<String>,
    #[arg(long, global = true)]
    auto_merge: bool,
    #[arg(long, global = true, value_name = "METHOD", default_value = "squash", value_parser = ["squash", "rebase", "merge"])]
    merge_method: String,
    #[arg(long, global = true, value_name = "DURATION", default_value = "30m")]
    timeout: String,
    #[arg(
        long = "interval",
        visible_alias = "poll-interval",
        global = true,
        value_name = "DURATION",
        default_value = "60s"
    )]
    poll_interval: String,
}

#[derive(Subcommand)]
enum TriageCommand {
    Component {
        component_id: Option<String>,
        #[arg(long, value_name = "CHECKOUT")]
        path: Option<String>,
    },
    Project {
        project_id: String,
    },
    Fleet {
        fleet_id: String,
    },
    Rig {
        rig_id: String,
    },
    Workspace,
    Landing {
        pr_refs: Vec<String>,
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        #[arg(long, value_name = "PATTERN")]
        branch: Vec<String>,
        #[arg(long, value_name = "NUMBER")]
        source_issue: Vec<u64>,
        #[arg(long)]
        ordered: bool,
        #[command(flatten)]
        scope: ScopeArgs,
    },
}

#[derive(Args, Default)]
struct ScopeArgs {
    #[arg(long, conflicts_with_all = ["fleet", "component", "path", "workspace"])]
    project: Option<String>,
    #[arg(long, conflicts_with_all = ["project", "component", "path", "workspace"])]
    fleet: Option<String>,
    #[arg(long, conflicts_with_all = ["project", "fleet", "path", "workspace"])]
    component: Option<String>,
    #[arg(long, conflicts_with_all = ["project", "fleet", "component", "workspace"])]
    path: Option<String>,
    #[arg(long, conflicts_with_all = ["project", "fleet", "component", "path"])]
    workspace: bool,
}

impl ScopeArgs {
    fn resolve(self) -> TriageTarget {
        if let Some(project) = self.project {
            TriageTarget::Project(project)
        } else if let Some(fleet) = self.fleet {
            TriageTarget::Fleet(fleet)
        } else if let Some(component) = self.component {
            TriageTarget::Component(component)
        } else if let Some(path) = self.path {
            TriageTarget::Path {
                path,
                component_id: None,
            }
        } else {
            TriageTarget::Workspace
        }
    }
}

fn run_triage(args: TriageArgs) -> homeboy_core::Result<(TriageCommandOutput, i32)> {
    if !args.watch.is_empty() {
        let output = watch(TriageWatchOptions {
            refs: args.watch,
            until: args
                .until
                .or_else(|| args.auto_merge.then_some("green-mergeable".to_string())),
            timeout: parse_duration("timeout", &args.timeout)?,
            poll_interval: parse_duration("poll-interval", &args.poll_interval)?,
            auto_merge: args.auto_merge,
            merge_method: args.merge_method,
        })?;
        return Ok((
            TriageCommandOutput::Watch(output.clone()),
            if output.target_reached { 0 } else { 124 },
        ));
    }
    let mut issue_numbers = args.issue;
    if let Some(path) = args.issues_from_file {
        issue_numbers.extend(parse_issue_numbers_file(&path)?);
    }
    issue_numbers.sort_unstable();
    issue_numbers.dedup();
    let command = args.command.unwrap_or(TriageCommand::Workspace);
    if let TriageCommand::Landing {
        pr_refs,
        repo,
        branch,
        source_issue,
        ordered,
        scope,
    } = command
    {
        return Ok((
            TriageCommandOutput::Landing(landing(TriageLandingOptions {
                target: scope.resolve(),
                repo,
                pr_refs,
                branch_patterns: branch,
                source_issues: source_issue,
                ordered,
                drilldown: args.drilldown,
                limit: args.limit,
            })?),
            0,
        ));
    }
    let target = match command {
        TriageCommand::Component { component_id, path } => {
            resolve_component_target(component_id, path)?
        }
        TriageCommand::Project { project_id } => TriageTarget::Project(project_id),
        TriageCommand::Fleet { fleet_id } => TriageTarget::Fleet(fleet_id),
        TriageCommand::Rig { rig_id } => TriageTarget::Rig(rig_id),
        TriageCommand::Workspace => TriageTarget::Workspace,
        TriageCommand::Landing { .. } => unreachable!(),
    };
    let include_issues = args.issues || !args.prs || !issue_numbers.is_empty();
    let include_prs = args.prs || !args.issues;
    let output = run(
        target.clone(),
        TriageOptions {
            include_issues,
            include_prs,
            mine: args.mine || (matches!(target, TriageTarget::Workspace) && !args.all),
            assigned: args.assigned,
            labels: args.label,
            needs_review: args.needs_review,
            failing_checks: args.failing_checks,
            drilldown: args.drilldown,
            issue_numbers,
            stale_days: args
                .stale
                .map(|value| parse_stale_days(&value))
                .transpose()?,
            limit: args.limit,
        },
    )?;
    Ok((TriageCommandOutput::Report(output), 0))
}

fn resolve_component_target(
    component_id: Option<String>,
    path: Option<String>,
) -> homeboy_core::Result<TriageTarget> {
    match (component_id, path) {
        (None, None) => Err(Error::validation_missing_argument(vec![
            "component_id".into(),
            "path".into(),
        ])),
        (Some(component_id), None) => Ok(TriageTarget::Component(component_id)),
        (component_id, Some(path)) => Ok(TriageTarget::Path { path, component_id }),
    }
}

fn parse_duration(name: &str, raw: &str) -> homeboy_core::Result<std::time::Duration> {
    let seconds = raw
        .strip_suffix("ms")
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value as f64 / 1000.0)
        .or_else(|| {
            raw.strip_suffix('s')
                .and_then(|value| value.parse::<u64>().ok())
                .map(|value| value as f64)
        })
        .or_else(|| {
            raw.strip_suffix('m')
                .and_then(|value| value.parse::<u64>().ok())
                .map(|value| value as f64 * 60.0)
        })
        .or_else(|| {
            raw.strip_suffix('h')
                .and_then(|value| value.parse::<u64>().ok())
                .map(|value| value as f64 * 3600.0)
        })
        .or_else(|| {
            raw.strip_suffix('d')
                .and_then(|value| value.parse::<u64>().ok())
                .map(|value| value as f64 * 86400.0)
        });
    seconds
        .map(std::time::Duration::from_secs_f64)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                name,
                "expected a duration such as 500ms, 30s, 5m, 1h, or 7d",
                Some(raw.to_string()),
                None,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_triage_spellings_stay_in_the_product_descriptor() {
        let command = command();
        for argv in [
            vec!["triage", "landing", "--workspace", "--branch", "e2e/*"],
            vec![
                "triage",
                "--watch",
                "Extra-Chill/homeboy#2238",
                "--poll-interval",
                "30s",
            ],
            vec!["triage", "component", "--path", "/tmp/homeboy"],
        ] {
            assert!(
                command.clone().try_get_matches_from(argv.clone()).is_ok(),
                "published invocation should parse: {argv:?}"
            );
        }
    }
}
