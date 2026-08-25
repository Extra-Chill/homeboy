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

    /// Version to treat as installed instead of the running binary's version.
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

    /// Version to treat as installed instead of the running binary's version.
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

    #[derive(Parser)]
    struct GapHarness {
        #[command(flatten)]
        args: GapArgs,
    }

    fn parse_contains(argv: &[&str]) -> ContainsArgs {
        ContainsHarness::try_parse_from(argv)
            .expect("contains invocation should parse")
            .args
    }

    fn parse_gap(argv: &[&str]) -> GapArgs {
        GapHarness::try_parse_from(argv)
            .expect("gap invocation should parse")
            .args
    }

    /// The shipped positional form remains valid: `homeboy release contains <sha>`.
    #[test]
    fn positional_commit_is_accepted_without_a_flag() {
        let query = parse_contains(&["contains", "6043c013d"]).into_query();

        assert!(query.component_id.is_none());
        assert_eq!(query.commit.as_deref(), Some("6043c013d"));
        assert!(query.issue.is_none());
    }

    #[test]
    fn component_then_commit_is_the_canonical_positional_order() {
        let query = parse_contains(&["contains", "homeboy", "6043c013d"]).into_query();

        assert_eq!(query.component_id.as_deref(), Some("homeboy"));
        assert_eq!(query.commit.as_deref(), Some("6043c013d"));
    }

    /// The ergonomic half: without `--issue` the command is barely better than
    /// the git one-liner it replaces.
    #[test]
    fn issue_number_is_accepted_without_a_commit() {
        let args = parse_contains(&["contains", "--issue", "11702"]);

        assert_eq!(args.issue, Some(11702));
        assert!(args.commit.is_none());
    }

    #[test]
    fn issue_query_accepts_a_positional_component() {
        let query = parse_contains(&["contains", "homeboy", "--issue", "11702"]).into_query();

        assert_eq!(query.component_id.as_deref(), Some("homeboy"));
        assert_eq!(query.issue, Some(11702));
    }

    /// The installed reference must be overridable, so an operator can ask
    /// about a version other than the one they happen to be running.
    #[test]
    fn installed_override_is_parsed() {
        let args = parse_contains(&["contains", "abc123", "--installed", "0.327.0"]);

        assert_eq!(args.target.as_deref(), Some("abc123"));
        assert_eq!(args.installed.as_deref(), Some("0.327.0"));
    }

    #[test]
    fn checkout_and_component_selectors_are_parsed() {
        let args = parse_contains(&[
            "contains",
            "abc123",
            "--component",
            "homeboy",
            "--path",
            "/tmp/checkout",
        ]);

        assert_eq!(args.component.as_deref(), Some("homeboy"));
        assert_eq!(args.target.as_deref(), Some("abc123"));
        assert_eq!(args.path.as_deref(), Some("/tmp/checkout"));
    }

    /// A non-numeric issue number is a typo, not an issue. Clap rejects it
    /// before any network call is made.
    #[test]
    fn non_numeric_issue_is_rejected_by_the_parser() {
        assert!(ContainsHarness::try_parse_from(["contains", "--issue", "eleven"]).is_err());
    }

    /// `gap` is the query an operator actually reaches for, so it must work
    /// with no arguments at all: the checkout is discovered, the installed
    /// reference is the running binary.
    #[test]
    fn gap_parses_with_no_arguments() {
        let args = parse_gap(&["gap"]);

        assert!(args.component.is_none());
        assert!(args.component_id.is_none());
        assert!(args.path.is_none());
        assert!(args.installed.is_none());
    }

    #[test]
    fn gap_accepts_the_same_selectors_as_contains() {
        let args = parse_gap(&[
            "gap",
            "--component",
            "homeboy",
            "--path",
            "/tmp/checkout",
            "--installed",
            "0.327.0",
        ]);

        assert_eq!(args.component.as_deref(), Some("homeboy"));
        assert_eq!(args.path.as_deref(), Some("/tmp/checkout"));
        assert_eq!(args.installed.as_deref(), Some("0.327.0"));
    }

    #[test]
    fn gap_accepts_a_positional_component() {
        let args = parse_gap(&["gap", "homeboy"]);

        assert_eq!(args.component_id.as_deref(), Some("homeboy"));
        assert!(args.component.is_none());
    }
}
