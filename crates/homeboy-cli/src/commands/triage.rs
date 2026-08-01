use clap::{Args, Subcommand};
use homeboy::core::Error;
use homeboy_triage::{
    self as triage, TriageCommandOutput, TriageLandingOptions, TriageOptions, TriageTarget,
    TriageWatchOptions,
};
use std::path::PathBuf;

use super::utils::args::ScopeArgs;
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
    ///
    /// Spelled `--interval` to match `activity watch` and `runs watch`, whose
    /// operators otherwise have to remember a third vocabulary for the same
    /// concept (#10315). `--poll-interval` is retained as a visible alias
    /// because it is a published spelling; nothing that worked stops working.
    #[arg(
        long = "interval",
        visible_alias = "poll-interval",
        global = true,
        value_name = "DURATION",
        default_value = "60s"
    )]
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

        // Landing scope. Defaults to every configured workspace repo when no
        // selector is supplied, which is what the removed `--workspace` bool
        // already meant.
        #[command(flatten)]
        scope: ScopeArgs,
    },
}

pub fn run(args: TriageArgs) -> CmdResult<TriageCommandOutput> {
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
        scope,
    } = command
    {
        let output = triage::landing(TriageLandingOptions {
            target: scope.resolve(),
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

/// Parse `triage --timeout` / `--interval`.
///
/// Delegates to the shared unit table in `commands::utils::watch`, which is why
/// `--timeout 7d` and `--interval 500ms` now work here as they already did on
/// `runs watch` and `observe`.
fn parse_watch_duration(name: &str, raw: &str) -> Result<std::time::Duration, Error> {
    crate::commands::utils::watch::parse_duration(&format!("--{name}"), raw)
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
    use homeboy_triage::TriageTarget;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: TriageArgs,
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

    /// `--interval` is the canonical spelling shared with `activity watch` and
    /// `runs watch`; `--poll-interval` is the published spelling this command
    /// shipped with. Both must resolve to the same field, and the default must
    /// not move.
    #[test]
    fn watch_interval_accepts_both_spellings() {
        for spelling in ["--interval", "--poll-interval"] {
            let cli = TestCli::try_parse_from([
                "triage",
                "--watch",
                "Extra-Chill/homeboy#2238",
                spelling,
                "45s",
            ])
            .unwrap_or_else(|error| panic!("{spelling} should parse: {error}"));
            assert_eq!(cli.args.poll_interval, "45s", "{spelling}");
        }

        let default = TestCli::try_parse_from(["triage", "--watch", "Extra-Chill/homeboy#2238"])
            .expect("bare watch parses");
        assert_eq!(default.args.poll_interval, "60s");
    }

    /// The two spellings are one argument, not two: supplying both is a
    /// duplicate value for the same id, and the last one wins rather than
    /// silently producing two independent intervals.
    #[test]
    fn watch_interval_spellings_are_one_argument() {
        let cli = TestCli::try_parse_from([
            "triage",
            "--watch",
            "Extra-Chill/homeboy#2238",
            "--poll-interval",
            "30s",
            "--interval",
            "45s",
        ])
        .expect("both spellings parse");
        assert_eq!(cli.args.poll_interval, "45s");
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

    fn landing_scope(argv: &[&str]) -> TriageTarget {
        let cli = TestCli::try_parse_from(argv).expect("landing invocation should parse");
        match cli.args.command.expect("landing subcommand") {
            TriageCommand::Landing { scope, .. } => scope.resolve(),
            other => panic!("expected landing subcommand, got {other:?}"),
        }
    }

    /// Every landing scope spelling that parsed before `ScopeArgs` must still
    /// parse to the same target. These are user-facing flags other tooling
    /// invokes; a silently changed spelling is a breaking change.
    #[test]
    fn landing_scope_flags_keep_their_previous_spellings() {
        assert_eq!(
            landing_scope(&["triage", "landing", "--project", "growth"]),
            TriageTarget::Project("growth".to_string())
        );
        assert_eq!(
            landing_scope(&["triage", "landing", "--fleet", "growth"]),
            TriageTarget::Fleet("growth".to_string())
        );
        assert_eq!(
            landing_scope(&["triage", "landing", "--component", "homeboy"]),
            TriageTarget::Component("homeboy".to_string())
        );
        assert_eq!(
            landing_scope(&["triage", "landing", "--path", "/src/homeboy"]),
            TriageTarget::Path {
                path: "/src/homeboy".to_string(),
                component_id: None,
            }
        );
        assert_eq!(
            landing_scope(&["triage", "landing", "--workspace"]),
            TriageTarget::Workspace
        );
    }

    #[test]
    fn landing_without_a_scope_still_defaults_to_workspace() {
        assert_eq!(
            landing_scope(&["triage", "landing"]),
            TriageTarget::Workspace
        );
    }

    /// The documented landing invocations from `docs/commands/triage.md`.
    #[test]
    fn documented_landing_invocations_still_parse() {
        for argv in [
            vec![
                "triage",
                "landing",
                "Extra-Chill/homeboy#2238",
                "Automattic/static-site-importer#118",
                "--drilldown",
            ],
            vec![
                "triage",
                "landing",
                "--repo",
                "Extra-Chill/homeboy",
                "--branch",
                "fixture/*",
                "--drilldown",
            ],
            vec![
                "triage",
                "landing",
                "--workspace",
                "--branch",
                "e2e/*",
                "--limit",
                "100",
            ],
            vec![
                "triage",
                "landing",
                "--repo",
                "Extra-Chill/homeboy",
                "--ordered",
                "2238",
                "2239",
                "2240",
            ],
        ] {
            assert!(
                TestCli::try_parse_from(&argv).is_ok(),
                "documented invocation should parse: {argv:?}"
            );
        }
    }

    #[test]
    fn landing_scope_selectors_still_conflict() {
        for argv in [
            vec!["triage", "landing", "--project", "a", "--fleet", "b"],
            vec!["triage", "landing", "--project", "a", "--component", "b"],
            vec!["triage", "landing", "--component", "a", "--path", "/src/a"],
            vec!["triage", "landing", "--fleet", "a", "--workspace"],
            vec!["triage", "landing", "--path", "/src/a", "--workspace"],
        ] {
            assert!(
                TestCli::try_parse_from(&argv).is_err(),
                "conflicting landing scopes should be rejected: {argv:?}"
            );
        }
    }

    /// `--rig` is the one selector landing did not previously accept. It comes
    /// free with the shared group and resolves through the same
    /// `resolve_target_components` path every other scope uses.
    #[test]
    fn landing_gains_the_rig_scope_from_the_shared_group() {
        assert_eq!(
            landing_scope(&["triage", "landing", "--rig", "studio"]),
            TriageTarget::Rig("studio".to_string())
        );
    }

    /// The scope *subcommands* are a separate, still-supported surface.
    #[test]
    fn scope_subcommands_are_unchanged() {
        for argv in [
            vec!["triage", "component", "homeboy"],
            vec!["triage", "component", "--path", "/src/homeboy"],
            vec!["triage", "project", "growth"],
            vec!["triage", "fleet", "growth"],
            vec!["triage", "rig", "studio"],
            vec!["triage", "workspace"],
        ] {
            assert!(
                TestCli::try_parse_from(&argv).is_ok(),
                "scope subcommand should parse: {argv:?}"
            );
        }
    }
}
