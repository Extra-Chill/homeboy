//! Issue-tracker abstraction.
//!
//! [`Tracker`] is the I/O seam: list issues, create / update / close them.
//! [`reconcile`](super::reconcile::reconcile) is generic over this trait
//! through the [`apply_plan`](super::apply::apply_plan) executor — it never
//! sees a `gh` shell command directly. This is what unlocks future GitLab,
//! Linear, or local-file trackers without touching the decision logic.
//!
//! The default implementation [`GithubTracker`] wraps the existing
//! `core/git/github.rs` primitives.

use crate::error::Result;
use crate::git::{
    issue_close, issue_create, issue_edit, issue_find, IssueCloseOptions, IssueCloseReason,
    IssueCreateOptions, IssueEditOptions, IssueFindOptions, IssueState,
};

use super::plan::{TrackedIssue, TrackedIssueState};

/// Abstract issue-tracker contract. All operations are component-scoped:
/// the tracker resolves which repo/project to talk to from the component
/// at construction time, NOT per call. This matches the existing
/// `core/git/github.rs` shape.
pub trait Tracker {
    /// Return every issue in the tracker matching the given label. Includes
    /// open AND closed issues — reconcile needs `state_reason` on closed
    /// issues to distinguish completed from not_planned.
    ///
    /// `command_label` is the reconciler's category-class label
    /// (e.g. `"audit"`, `"lint"`, `"test"`). Implementations should restrict
    /// the result set by this label so we don't paginate the entire tracker
    /// for every reconcile run.
    fn list_issues(&self, command_label: &str, limit: usize) -> Result<Vec<TrackedIssue>>;

    /// File a new issue with the given title, body, and labels. Returns the
    /// new issue number on success.
    fn create_issue(&self, title: &str, body: &str, labels: &[String]) -> Result<u64>;

    /// Update the title and/or body of an existing issue (open OR closed).
    /// Closed issues stay closed — this is the "refresh closed-not_planned
    /// body" path.
    fn update_issue(&self, number: u64, title: Option<&str>, body: Option<&str>) -> Result<()>;

    /// Close an issue with a typed reason. Reconcile uses `Completed` for
    /// "no findings remaining" closes, and `NotPlanned` for race-condition
    /// duplicate consolidation.
    fn close_issue(&self, number: u64, reason: CloseReason, comment: Option<&str>) -> Result<()>;
}

/// Tracker-agnostic close reason. Maps onto GitHub's `state_reason` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Completed,
    NotPlanned,
}

impl CloseReason {
    fn to_github(self) -> IssueCloseReason {
        match self {
            CloseReason::Completed => IssueCloseReason::Completed,
            CloseReason::NotPlanned => IssueCloseReason::NotPlanned,
        }
    }
}

/// Default tracker: GitHub via the `gh` CLI shellouts in `core/git/github.rs`.
pub struct GithubTracker {
    component_id: String,
    /// Optional workspace path for unregistered checkouts (CI / ad-hoc clones).
    /// Forwarded to every `core/git/github.rs` call. Mirrors the `--path` flag
    /// every other `homeboy git *` command takes.
    path: Option<String>,
}

impl GithubTracker {
    pub fn new(component_id: impl Into<String>) -> Self {
        Self {
            component_id: component_id.into(),
            path: None,
        }
    }

    pub fn with_path(mut self, path: Option<String>) -> Self {
        self.path = path;
        self
    }
}

impl Tracker for GithubTracker {
    fn list_issues(&self, command_label: &str, limit: usize) -> Result<Vec<TrackedIssue>> {
        let out = issue_find(
            Some(&self.component_id),
            IssueFindOptions {
                title: None,
                labels: vec![command_label.to_string()],
                state: IssueState::All,
                limit,
                path: self.path.clone(),
            },
        )?;

        let issues = out
            .items
            .into_iter()
            .filter_map(github_to_tracked)
            .collect();
        Ok(issues)
    }

    fn create_issue(&self, title: &str, body: &str, labels: &[String]) -> Result<u64> {
        let out = issue_create(
            Some(&self.component_id),
            IssueCreateOptions {
                title: title.to_string(),
                body: body.to_string(),
                labels: labels.to_vec(),
                path: self.path.clone(),
            },
        )?;
        out.number.ok_or_else(|| {
            crate::error::Error::internal_io(
                "issue.create succeeded but returned no number".to_string(),
                Some("gh issue create".into()),
            )
        })
    }

    fn update_issue(&self, number: u64, title: Option<&str>, body: Option<&str>) -> Result<()> {
        issue_edit(
            Some(&self.component_id),
            IssueEditOptions {
                number,
                title: title.map(|s| s.to_string()),
                body: body.map(|s| s.to_string()),
                add_labels: Vec::new(),
                remove_labels: Vec::new(),
                path: self.path.clone(),
            },
        )?;
        Ok(())
    }

    fn close_issue(&self, number: u64, reason: CloseReason, comment: Option<&str>) -> Result<()> {
        issue_close(
            Some(&self.component_id),
            IssueCloseOptions {
                number,
                reason: reason.to_github(),
                comment: comment.map(|s| s.to_string()),
                path: self.path.clone(),
            },
        )?;
        Ok(())
    }
}

/// Translate a GitHub `GithubFindItem` into the tracker-agnostic
/// [`TrackedIssue`] shape. Returns `None` when the issue's state shape is
/// unrecognized — defensive against future GitHub state additions.
fn github_to_tracked(item: crate::git::GithubFindItem) -> Option<TrackedIssue> {
    let state = match item.state.to_lowercase().as_str() {
        "open" => TrackedIssueState::Open,
        "closed" => match item.state_reason.as_str() {
            "not_planned" | "NOT_PLANNED" => TrackedIssueState::ClosedNotPlanned,
            // Empty state_reason on a closed issue happens for older issues
            // closed before GitHub introduced state_reason. Treat as
            // completed (the safer default — "we resolved this once").
            "" | "completed" | "COMPLETED" => TrackedIssueState::ClosedCompleted,
            // Unknown state_reason (e.g. "duplicate", future additions):
            // treat as completed too. The reconcile policy on completed is
            // "file new if findings return," which is the safer fallback
            // for an unrecognized close.
            _ => TrackedIssueState::ClosedCompleted,
        },
        _ => return None,
    };

    Some(TrackedIssue {
        number: item.number,
        title: item.title,
        url: item.url,
        state,
        labels: item.labels,
    })
}

// ---------------------------------------------------------------------------
// Tests — github_to_tracked translation table
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GithubFindItem;

    fn item(state: &str, state_reason: &str) -> GithubFindItem {
        GithubFindItem {
            number: 1,
            title: "t".into(),
            url: "u".into(),
            state: state.into(),
            state_reason: state_reason.into(),
            closed_at: String::new(),
            labels: vec!["audit".into()],
        }
    }

    #[test]
    fn translates_open() {
        assert_eq!(
            github_to_tracked(item("OPEN", "")).unwrap().state,
            TrackedIssueState::Open
        );
        assert_eq!(
            github_to_tracked(item("open", "")).unwrap().state,
            TrackedIssueState::Open
        );
    }

    #[test]
    fn translates_closed_completed() {
        assert_eq!(
            github_to_tracked(item("CLOSED", "completed"))
                .unwrap()
                .state,
            TrackedIssueState::ClosedCompleted
        );
    }

    #[test]
    fn translates_closed_not_planned() {
        assert_eq!(
            github_to_tracked(item("closed", "not_planned"))
                .unwrap()
                .state,
            TrackedIssueState::ClosedNotPlanned
        );
        assert_eq!(
            github_to_tracked(item("CLOSED", "NOT_PLANNED"))
                .unwrap()
                .state,
            TrackedIssueState::ClosedNotPlanned
        );
    }

    #[test]
    fn empty_state_reason_on_closed_defaults_to_completed() {
        // Older GitHub issues closed before state_reason existed.
        assert_eq!(
            github_to_tracked(item("CLOSED", "")).unwrap().state,
            TrackedIssueState::ClosedCompleted
        );
    }

    #[test]
    fn unknown_state_reason_falls_back_to_completed() {
        // Future state_reason values we don't model yet (e.g. "duplicate")
        // are safer treated as completed than as not_planned — completed
        // means "file new if findings return."
        assert_eq!(
            github_to_tracked(item("CLOSED", "duplicate"))
                .unwrap()
                .state,
            TrackedIssueState::ClosedCompleted
        );
    }

    #[test]
    fn unknown_state_returns_none() {
        assert!(github_to_tracked(item("merged", "")).is_none());
        assert!(github_to_tracked(item("draft", "")).is_none());
    }
}
