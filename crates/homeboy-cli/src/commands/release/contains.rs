//! `homeboy release contains` and `homeboy release gap`.
//!
//! Homeboy cuts the tag, builds the artifacts, and publishes, so it is the
//! subsystem that can answer "is my fix released yet?" without the operator
//! hand-rolling `git merge-base --is-ancestor <sha> <tag>` once per candidate
//! tag (#11754). All ordering, status, and message logic lives in
//! [`homeboy_release::release::containment`]; this module is argument parsing
//! and output shaping only.

use clap::Args;

use homeboy_release::release::containment::{
    self, ContainsQuery, ReleaseContainsReport, ReleaseGapReport,
};

use crate::commands::CmdResult;

#[derive(Args)]
#[command(
    after_help = "Examples:\n  homeboy release contains data-machine-code 6043c013d\n  homeboy release contains data-machine-code --issue 11702"
)]
pub struct ContainsArgs {
    /// Component ID when followed by COMMIT or paired with --issue. A lone
    /// value remains the commit-ish for compatibility.
    #[arg(value_name = "COMPONENT_ID_OR_COMMIT")]
    pub target: Option<String>,

    /// Commit sha (or any commit-ish) to locate after COMPONENT_ID.
    #[arg(conflicts_with = "issue")]
    pub commit: Option<String>,

    /// Resolve the commit through the merged pull request that closed this
    /// issue, so the operator does not have to find the sha first.
    ///
    /// This is the only part of the command that touches the network.
    #[arg(long, value_name = "N")]
    pub issue: Option<u64>,

    /// Component whose release tag namespace to search
    /// (default: the component discovered from the working directory).
    #[arg(long, value_name = "COMPONENT_ID", conflicts_with = "commit")]
    pub component: Option<String>,

    /// Checkout to inspect directly. Useful for unregistered clones,
    /// CI runners, and worktrees.
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,

    /// Explicit installed version (otherwise binary identity or labeled checkout).
    #[arg(long, value_name = "VERSION")]
    pub installed: Option<String>,
}

#[derive(Args)]
#[command(
    after_help = "Examples:\n  homeboy release gap data-machine-code\n  homeboy release gap data-machine-code --installed 0.360.4"
)]
pub struct GapArgs {
    /// Component whose release tag namespace to search
    /// (default: the component discovered from the working directory).
    pub component_id: Option<String>,

    /// Component whose release tag namespace to search
    /// (alternative to the positional component target).
    #[arg(long, value_name = "COMPONENT_ID", conflicts_with = "component_id")]
    pub component: Option<String>,

    /// Checkout to inspect directly. Useful for unregistered clones,
    /// CI runners, and worktrees.
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,

    /// Explicit installed version (otherwise binary identity or labeled checkout).
    #[arg(long, value_name = "VERSION")]
    pub installed: Option<String>,
}

/// Report which release first contained a commit.
///
/// Exits 0 for every answered query: the verdict is the `status` field, not the
/// exit code. "Not yet released" is a legitimate answer, not a command failure,
/// and failing on it would break every wrapper that asks the question routinely.
pub(crate) fn run_contains(args: ContainsArgs) -> CmdResult<ReleaseContainsReport> {
    let report = containment::contains(&args.into_query())?;

    Ok((report, 0))
}

impl ContainsArgs {
    fn into_query(self) -> ContainsQuery {
        let (positional_component, commit) = match (self.target, self.commit) {
            (Some(component), Some(commit)) => (Some(component), Some(commit)),
            (Some(component), None) if self.issue.is_some() => (Some(component), None),
            (commit, None) => (None, commit),
            (None, Some(_)) => unreachable!("a second positional requires the first"),
        };
        ContainsQuery {
            component_id: self.component.or(positional_component),
            path: self.path,
            commit,
            issue: self.issue,
            installed_version: self.installed,
        }
    }
}

/// Report how far the installed build is behind the newest release.
pub(crate) fn run_gap(args: GapArgs) -> CmdResult<ReleaseGapReport> {
    let component_id = args.component.or(args.component_id);
    let report = containment::gap(
        component_id.as_deref(),
        args.path.as_deref(),
        args.installed.as_deref(),
    )?;

    Ok((report, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct ContainsHarness {
        #[command(flatten)]
        args: ContainsArgs,
    }

    /// `into_query` decides whether the first positional is a component or a
    /// commit, and lets `--component` override a positional component.
    #[test]
    fn positionals_resolve_to_component_commit_and_issue() {
        // (argv, component, commit, issue)
        let cases: &[(&[&str], Option<&str>, Option<&str>, Option<u64>)] = &[
            // shipped single-positional form: the value is the commit
            (&["contains", "6043c013d"], None, Some("6043c013d"), None),
            // canonical component-first form
            (
                &["contains", "homeboy", "6043c013d"],
                Some("homeboy"),
                Some("6043c013d"),
                None,
            ),
            (&["contains", "--issue", "11702"], None, None, Some(11702)),
            // with --issue a lone positional is the component
            (
                &["contains", "homeboy", "--issue", "11702"],
                Some("homeboy"),
                None,
                Some(11702),
            ),
            // --component keeps a lone positional as the commit
            (
                &["contains", "abc123", "--component", "homeboy"],
                Some("homeboy"),
                Some("abc123"),
                None,
            ),
        ];

        for (argv, component, commit, issue) in cases {
            let query = ContainsHarness::try_parse_from(*argv)
                .expect("contains invocation should parse")
                .args
                .into_query();
            assert_eq!(query.component_id.as_deref(), *component, "{argv:?}");
            assert_eq!(query.commit.as_deref(), *commit, "{argv:?}");
            assert_eq!(query.issue, *issue, "{argv:?}");
        }
    }
}
