//! Whether a Cook's tracked work was already resolved elsewhere (#15984).
//!
//! A Cook can reach a failed terminal long after its issue was fixed by other
//! means: an operator merges the same change by hand while the controller is
//! still promoting the candidate. Paging the operator about that Cook is noise —
//! the work it was asked to do is done. The tracker is the authority on that,
//! so a needs-attention announcement first asks it.
//!
//! The probe fails open. Anything short of a definite "closed as completed"
//! answer — no recipe, no GitHub issue ref, `gh` missing, unauthenticated,
//! slow, or malformed output — leaves the announcement exactly as it was.

use serde_json::Value;
use std::time::Duration;

/// Upper bound on one tracker probe. A terminal announcement must not wait on a
/// hung network call.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// A GitHub issue named by a Cook source ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GithubIssueRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

/// Parse `https://github.com/<owner>/<repo>/issues/<number>`.
///
/// Owner and repo are restricted to GitHub's name charset, so the parsed parts
/// are safe to place in a probe command line.
pub(super) fn github_issue_ref(url: &str) -> Option<GithubIssueRef> {
    let path = url.trim().strip_prefix("https://github.com/")?;
    let path = path.split(['#', '?']).next()?.trim_end_matches('/');
    let mut parts = path.split('/');
    let (owner, repo, kind, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if kind != "issues" || parts.next().is_some() {
        return None;
    }
    let valid = |name: &str| {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    if !valid(owner) || !valid(repo) {
        return None;
    }
    Some(GithubIssueRef {
        owner: owner.to_string(),
        repo: repo.to_string(),
        number: number.parse().ok()?,
    })
}

/// `gh issue view --json state,stateReason` says the issue is closed as
/// completed. Closed as not planned is not resolution of the work.
pub(super) fn closed_as_completed(view: &Value) -> bool {
    view["state"].as_str() == Some("CLOSED")
        && view["stateReason"]
            .as_str()
            .is_some_and(|reason| reason.eq_ignore_ascii_case("completed"))
}

/// The tracker that resolves `source_refs`, when every GitHub issue among them
/// is closed as completed. `None` when there is no GitHub issue to ask or any
/// of them is still open, not planned, or unknown.
pub(super) fn resolved_tracker_in(
    source_refs: &[String],
    closed_completed: impl Fn(&GithubIssueRef) -> bool,
) -> Option<String> {
    let issues: Vec<(&String, GithubIssueRef)> = source_refs
        .iter()
        .filter_map(|source| github_issue_ref(source).map(|issue| (source, issue)))
        .collect();
    let (first, _) = issues.first()?;
    issues
        .iter()
        .all(|(_, issue)| closed_completed(issue))
        .then(|| (*first).clone())
}

/// Production probe: the Cook's durable recipe names its trackers, and `gh`
/// answers their state.
pub(super) fn resolved_tracker(cook_id: &str) -> Option<String> {
    let recipe = crate::agent_task_service::load_recipe(cook_id).ok()?;
    resolved_tracker_in(&recipe.source_refs, gh_issue_closed_as_completed)
}

fn gh_issue_closed_as_completed(issue: &GithubIssueRef) -> bool {
    let command = format!(
        "gh issue view {} -R {}/{} --json state,stateReason",
        issue.number, issue.owner, issue.repo
    );
    let output = homeboy_core::server::execute_local_command_in_dir_with_timeout(
        &command,
        None,
        None,
        PROBE_TIMEOUT,
    );
    output.success
        && serde_json::from_str::<Value>(&output.stdout)
            .is_ok_and(|view| closed_as_completed(&view))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_only_github_issue_urls_with_safe_names() {
        assert_eq!(
            github_issue_ref("https://github.com/Extra-Chill/wp-coding-agents/issues/628"),
            Some(GithubIssueRef {
                owner: "Extra-Chill".into(),
                repo: "wp-coding-agents".into(),
                number: 628,
            })
        );
        assert_eq!(
            github_issue_ref("https://github.com/o/r/issues/7#issuecomment-1")
                .map(|issue| issue.number),
            Some(7)
        );
        for rejected in [
            "https://github.com/o/r/pull/7",
            "https://github.com/o/r/issues/7/timeline",
            "https://github.com/o/r/issues/x",
            "https://github.com/o;rm/r/issues/7",
            "https://example.com/o/r/issues/7",
            "fix the bug",
        ] {
            assert_eq!(github_issue_ref(rejected), None, "{rejected}");
        }
    }

    #[test]
    fn only_closed_as_completed_counts_as_resolution() {
        assert!(closed_as_completed(
            &json!({"state": "CLOSED", "stateReason": "COMPLETED"})
        ));
        assert!(!closed_as_completed(
            &json!({"state": "CLOSED", "stateReason": "NOT_PLANNED"})
        ));
        assert!(!closed_as_completed(
            &json!({"state": "OPEN", "stateReason": ""})
        ));
        assert!(!closed_as_completed(&json!({})));
    }

    #[test]
    fn every_tracked_issue_must_be_resolved() {
        let refs = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let a = "https://github.com/o/r/issues/1";
        let b = "https://github.com/o/r/issues/2";

        assert_eq!(
            resolved_tracker_in(&refs(&[a, "notes.md"]), |_| true),
            Some(a.to_string())
        );
        assert_eq!(
            resolved_tracker_in(&refs(&[a, b]), |issue| issue.number == 1),
            None
        );
        assert_eq!(resolved_tracker_in(&refs(&["notes.md"]), |_| true), None);
        assert_eq!(resolved_tracker_in(&[], |_| true), None);
    }
}
