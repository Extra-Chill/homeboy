use clap::{Args, Subcommand};
use homeboy::core::triage::{
    self, TriageCommandOutput, TriageLandingOptions, TriageOptions, TriageTarget,
    TriageWatchOptions,
};
use homeboy::core::Error;
use std::path::PathBuf;

use super::CmdResult;

#[derive(Args)]
pub struct TriageArgs {
    #[command(subcommand)]
    command: Option<TriageCommand>,

    /// Include issues in the report. Defaults to issues + PRs when neither is set.
    #[arg(long, global = true)]
    issues: bool,

    /// Include pull requests in the report. Defaults to issues + PRs when neither is set.
    #[arg(long, global = true)]
    prs: bool,

    /// Show work assigned to or authored by the authenticated GitHub user.
    #[arg(long, global = true)]
    mine: bool,

    /// Show the broad repo firehose instead of the default personal workload.
    #[arg(long, global = true, conflicts_with = "mine")]
    all: bool,

    /// Restrict to issues/PRs assigned to this GitHub user.
    #[arg(long, global = true, value_name = "USER")]
    assigned: Option<String>,

    /// Restrict to items carrying this label. Repeatable.
    #[arg(long, global = true, value_name = "LABEL")]
    label: Vec<String>,

    /// Fetch this issue number exactly. Repeatable.
    #[arg(long, global = true, value_name = "NUMBER")]
    issue: Vec<u64>,

    /// Read issue numbers from a newline-separated file.
    #[arg(long, global = true, value_name = "PATH")]
    issues_from_file: Option<PathBuf>,

    /// Restrict PRs to review-required items.
    #[arg(long, global = true)]
    needs_review: bool,

    /// Restrict PRs to failing-check items.
    #[arg(long, global = true)]
    failing_checks: bool,

    /// Include compact failing check names and URLs for failing PRs.
    #[arg(long, global = true)]
    drilldown: bool,

    /// Mark issues/PRs stale after this many days (`14` or `14d`).
    #[arg(long, global = true, value_name = "DAYS")]
    stale: Option<String>,

    /// Maximum items fetched per repo for each item type.
    #[arg(long, global = true, default_value_t = 30)]
    limit: usize,

    /// Watch a GitHub PR/issue ref like owner/repo#123 until a target state.
    #[arg(long, global = true, value_name = "REF")]
    watch: Vec<String>,

    /// Target watch state: merged, closed, green, green-mergeable, failed, state-changed, or commit-pushed.
    #[arg(long, global = true, value_name = "STATE")]
    until: Option<String>,

    /// Merge a PR through the GitHub REST API when green-mergeable is reached.
    #[arg(long, global = true)]
    auto_merge: bool,

    /// Merge method used with --auto-merge.
    #[arg(long, global = true, value_name = "METHOD", default_value = "squash", value_parser = ["squash", "rebase", "merge"])]
    merge_method: String,

    /// Maximum time to watch before exiting.
    #[arg(long, global = true, value_name = "DURATION", default_value = "30m")]
    timeout: String,

    /// Delay between GitHub polls.
    #[arg(long, global = true, value_name = "DURATION", default_value = "60s")]
    poll_interval: String,
}

#[derive(Subcommand, Debug)]
enum TriageCommand {
    /// Triage one registered component.
    ///
    /// When `--path <CHECKOUT>` is supplied, the registry is bypassed and the
    /// GitHub remote is resolved directly from the checkout's `origin`. Useful
    /// for unregistered checkouts (CI runners, ad-hoc clones, worktrees) or
    /// when a component's registry record is broken.
    Component {
        /// Component ID. Optional when `--path` is supplied.
        component_id: Option<String>,

        /// Workspace path to triage directly, bypassing the registry.
        #[arg(long, value_name = "CHECKOUT")]
        path: Option<String>,
    },
    /// Triage every component attached to a project.
    Project { project_id: String },
    /// Triage unique components used across a fleet.
    Fleet { fleet_id: String },
    /// Triage components declared in a local rig spec.
    Rig { rig_id: String },
    /// Triage every configured project, rig, and registered component once per repo.
    Workspace,
    /// Summarize mergeability and check blockers for a PR landing fleet.
    Landing {
        /// PR numbers, owner/repo#number refs, or GitHub PR URLs.
        pr_refs: Vec<String>,

        /// Resolve bare PR numbers against this GitHub repo (`owner/name` or URL).
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,

        /// Include open PRs whose source branch matches this pattern. Repeatable.
        #[arg(long, value_name = "PATTERN")]
        branch: Vec<String>,

        /// Include PRs linked to this issue number in each resolved repo. Repeatable.
        #[arg(long, value_name = "NUMBER")]
        source_issue: Vec<u64>,

        /// Preserve supplied PR order and emit dependent-branch rebase plans.
        #[arg(long)]
        ordered: bool,

        /// Landing scope: project id.
        #[arg(long, conflicts_with_all = ["fleet", "component", "path", "workspace"])]
        project: Option<String>,

        /// Landing scope: fleet id.
        #[arg(long, conflicts_with_all = ["project", "component", "path", "workspace"])]
        fleet: Option<String>,

        /// Landing scope: registered component id.
        #[arg(long, conflicts_with_all = ["project", "fleet", "path", "workspace"])]
        component: Option<String>,

        /// Landing scope: checkout path, bypassing the registry.
        #[arg(long, conflicts_with_all = ["project", "fleet", "component", "workspace"])]
        path: Option<String>,

        /// Landing scope: all configured workspace repos. This is the default when no scope is supplied.
        #[arg(long, conflicts_with_all = ["project", "fleet", "component", "path"])]
        workspace: bool,
    },
}

pub fn run(args: TriageArgs, _global: &super::GlobalArgs) -> CmdResult<TriageCommandOutput> {
    if !args.watch.is_empty() {
        let options = TriageWatchOptions {
            refs: args.watch,
            until: args.until.or_else(|| {
                if args.auto_merge {
                    Some("green-mergeable".to_string())
                } else {
                    None
                }
            }),
            timeout: parse_watch_duration("timeout", &args.timeout)?,
            poll_interval: parse_watch_duration("poll-interval", &args.poll_interval)?,
            auto_merge: args.auto_merge,
            merge_method: args.merge_method,
        };
        let output = triage::watch(options)?;
        let exit_code = if output.target_reached { 0 } else { 1 };
        return Ok((TriageCommandOutput::Watch(output), exit_code));
    }

    let mut issue_numbers = args.issue;
    if let Some(path) = args.issues_from_file {
        issue_numbers.extend(triage::parse_issue_numbers_file(&path)?);
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
        project,
        fleet,
        component,
        path,
        workspace: _,
    } = command
    {
        let target = resolve_landing_target(project, fleet, component, path)?;
        let output = triage::landing(TriageLandingOptions {
            target,
            repo,
            pr_refs,
            branch_patterns: branch,
            source_issues: source_issue,
            ordered,
            drilldown: args.drilldown,
            limit: args.limit,
        })?;
        return Ok((TriageCommandOutput::Landing(output), 0));
    }

    let target = match command {
        TriageCommand::Component { component_id, path } => {
            resolve_component_target(component_id, path)?
        }
        TriageCommand::Project { project_id } => TriageTarget::Project(project_id),
        TriageCommand::Fleet { fleet_id } => TriageTarget::Fleet(fleet_id),
        TriageCommand::Rig { rig_id } => TriageTarget::Rig(rig_id),
        TriageCommand::Workspace => TriageTarget::Workspace,
        TriageCommand::Landing { .. } => unreachable!("landing handled above"),
    };

    let include_issues = args.issues || !args.prs || !issue_numbers.is_empty();
    let include_prs = args.prs || !args.issues;
    let default_to_personal_workload = matches!(target, TriageTarget::Workspace) && !args.all;
    let options = TriageOptions {
        include_issues,
        include_prs,
        mine: args.mine || default_to_personal_workload,
        assigned: args.assigned,
        labels: args.label,
        needs_review: args.needs_review,
        failing_checks: args.failing_checks,
        drilldown: args.drilldown,
        issue_numbers,
        stale_days: match args.stale {
            Some(value) => Some(triage::parse_stale_days(&value)?),
            None => None,
        },
        limit: args.limit,
    };

    Ok((
        TriageCommandOutput::Report(triage::run(target, options)?),
        0,
    ))
}

fn resolve_landing_target(
    project: Option<String>,
    fleet: Option<String>,
    component: Option<String>,
    path: Option<String>,
) -> Result<TriageTarget, Error> {
    if let Some(project) = project {
        return Ok(TriageTarget::Project(project));
    }
    if let Some(fleet) = fleet {
        return Ok(TriageTarget::Fleet(fleet));
    }
    if let Some(component) = component {
        return Ok(TriageTarget::Component(component));
    }
    if let Some(path) = path {
        return Ok(TriageTarget::Path {
            path,
            component_id: None,
        });
    }
    Ok(TriageTarget::Workspace)
}

fn parse_watch_duration(name: &str, raw: &str) -> Result<std::time::Duration, Error> {
    let trimmed = raw.trim();
    let split = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (amount, unit) = trimmed.split_at(split);
    let amount = amount.parse::<u64>().map_err(|_| {
        Error::validation_invalid_argument(
            format!("--{name}"),
            "expected duration like 30s, 5m, or 1h",
            Some(raw.to_string()),
            None,
        )
    })?;
    if amount == 0 {
        return Err(Error::validation_invalid_argument(
            format!("--{name}"),
            "duration amount must be greater than zero",
            Some(raw.to_string()),
            None,
        ));
    }
    let seconds = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => amount,
        "m" | "min" | "mins" | "minute" | "minutes" => amount * 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => amount * 60 * 60,
        _ => {
            return Err(Error::validation_invalid_argument(
                format!("--{name}"),
                "duration unit must be one of s, m, or h",
                Some(raw.to_string()),
                None,
            ))
        }
    };
    Ok(std::time::Duration::from_secs(seconds))
}

fn resolve_component_target(
    component_id: Option<String>,
    path: Option<String>,
) -> Result<TriageTarget, Error> {
    match (component_id, path) {
        (None, None) => Err(Error::validation_missing_argument(vec![
            "component_id".into(),
            "path".into(),
        ])),
        (Some(component_id), None) => Ok(TriageTarget::Component(component_id)),
        (component_id, Some(path)) => {
            // When both are supplied, verify the registry record (if any) points at
            // the same checkout. If it does not, surface a clear error rather than
            // silently picking one side. If the component is not registered, we
            // accept the explicit id as the synthetic component_id.
            if let Some(ref id) = component_id {
                if let Ok(comp) = homeboy::core::component::load(id) {
                    let registered = canonicalize_for_compare(&comp.local_path);
                    let supplied = canonicalize_for_compare(&path);
                    if registered != supplied {
                        return Err(Error::validation_invalid_argument(
                            "path",
                            format!(
                                "Disagrees with registered component '{id}' (local_path={})",
                                comp.local_path
                            ),
                            Some(path),
                            None,
                        ));
                    }
                }
            }
            Ok(TriageTarget::Path { path, component_id })
        }
    }
}

fn canonicalize_for_compare(path: &str) -> String {
    std::path::Path::new(path)
        .canonicalize()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::{resolve_component_target, TriageArgs, TriageCommand};
    use clap::Parser;
    use homeboy::core::triage::TriageTarget;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: TriageArgs,
    }

    #[test]
    fn bare_triage_defaults_to_workspace() {
        let cli = TestCli::parse_from(["triage"]);

        assert!(cli.args.command.is_none());
    }

    #[test]
    fn explicit_triage_subcommand_still_parses() {
        let cli = TestCli::parse_from(["triage", "workspace"]);

        assert!(matches!(cli.args.command, Some(TriageCommand::Workspace)));
    }

    #[test]
    fn all_flag_opts_out_of_personal_workload_default() {
        let cli = TestCli::parse_from(["triage", "--all"]);

        assert!(cli.args.all);
        assert!(!cli.args.mine);
    }

    #[test]
    fn watch_flags_parse_without_subcommand() {
        let cli = TestCli::parse_from([
            "triage",
            "--watch",
            "Extra-Chill/homeboy#2238",
            "--until",
            "green-mergeable",
            "--timeout",
            "5m",
            "--poll-interval",
            "30s",
        ]);

        assert_eq!(cli.args.watch, vec!["Extra-Chill/homeboy#2238"]);
        assert_eq!(cli.args.until.as_deref(), Some("green-mergeable"));
        assert_eq!(cli.args.timeout, "5m");
        assert_eq!(cli.args.poll_interval, "30s");
        assert!(cli.args.command.is_none());
    }

    #[test]
    fn landing_ordered_flag_parses() {
        let cli = TestCli::parse_from([
            "triage",
            "landing",
            "42",
            "43",
            "--repo",
            "Extra-Chill/homeboy",
            "--ordered",
        ]);

        match cli.args.command {
            Some(TriageCommand::Landing { ordered, .. }) => assert!(ordered),
            other => panic!("expected Landing subcommand, got {other:?}"),
        }
    }

    #[test]
    fn component_subcommand_accepts_path_without_id() {
        let cli = TestCli::parse_from(["triage", "component", "--path", "/tmp/checkout"]);

        match cli.args.command {
            Some(TriageCommand::Component { component_id, path }) => {
                assert_eq!(component_id, None);
                assert_eq!(path.as_deref(), Some("/tmp/checkout"));
            }
            other => panic!("expected Component subcommand, got {other:?}"),
        }
    }

    #[test]
    fn component_subcommand_accepts_id_and_path() {
        let cli =
            TestCli::parse_from(["triage", "component", "homeboy", "--path", "/tmp/checkout"]);

        match cli.args.command {
            Some(TriageCommand::Component { component_id, path }) => {
                assert_eq!(component_id.as_deref(), Some("homeboy"));
                assert_eq!(path.as_deref(), Some("/tmp/checkout"));
            }
            other => panic!("expected Component subcommand, got {other:?}"),
        }
    }

    #[test]
    fn component_subcommand_requires_id_or_path() {
        let err = resolve_component_target(None, None).unwrap_err();
        assert_eq!(err.code.as_str(), "validation.missing_argument");
    }

    #[test]
    fn component_subcommand_routes_path_to_path_target() {
        let target = resolve_component_target(None, Some("/tmp/some-checkout".into())).unwrap();
        match target {
            TriageTarget::Path { path, component_id } => {
                assert_eq!(path, "/tmp/some-checkout");
                assert_eq!(component_id, None);
            }
            other => panic!("expected TriageTarget::Path, got {other:?}"),
        }
    }
}
