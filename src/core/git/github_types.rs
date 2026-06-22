use serde::Serialize;

/// Result of a GitHub issue operation (create, comment, find-one).
#[derive(Debug, Clone, Serialize)]
pub struct GithubIssueOutput {
    pub component_id: String,
    pub owner: String,
    pub repo: String,
    pub action: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// Result of a GitHub PR operation (create, edit, find-one, comment).
#[derive(Debug, Clone, Default, Serialize)]
pub struct GithubPrOutput {
    pub component_id: String,
    pub owner: String,
    pub repo: String,
    pub action: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// Canonical comment id (sectioned flow). Omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment_id: Option<u64>,
    /// Non-fatal warnings. Currently used for duplicate-comment deletes that
    /// failed during race consolidation - the canonical comment was still
    /// updated successfully, so we report the stuck ids and exit 0.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrReadinessBlocker {
    pub kind: String,
    pub message: String,
    pub guidance: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrMergeReadiness {
    pub raw_merge_state: Option<String>,
    pub interpreted_state: String,
    pub mergeable: bool,
    pub blockers: Vec<PrReadinessBlocker>,
    pub check_guidance: String,
    pub conflict_guidance: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GithubPrReadinessOutput {
    pub component_id: String,
    pub owner: String,
    pub repo: String,
    pub action: String,
    pub success: bool,
    pub number: u64,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state: String,
    pub draft: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    pub ci_state: String,
    pub ci_summary: String,
    pub readiness: PrMergeReadiness,
}

#[derive(Debug, Clone, Serialize)]
pub struct GithubPrView {
    pub component_id: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub title: Option<String>,
    pub state: String,
    pub draft: bool,
    pub author: Option<String>,
    pub base: String,
    pub head: String,
    pub head_repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_state: Option<String>,
    pub ci_state: String,
    pub ci_summary: String,
    pub ci_next_action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrMergeabilityReconcileOutput {
    pub component_id: String,
    pub owner: String,
    pub repo: String,
    pub action: String,
    pub number: u64,
    pub classification: String,
    pub recommended_action: String,
    pub github: PrMergeabilityGithubEvidence,
    pub git: PrMergeabilityGitEvidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrMergeabilityGithubEvidence {
    pub state: String,
    pub base: String,
    pub head: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_state: Option<String>,
    pub ci_state: String,
    pub ci_summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrMergeabilityGitEvidence {
    pub base_ref: String,
    pub base_sha: String,
    pub head_ref: String,
    pub head_sha: String,
    pub merge_tree_clean: bool,
    pub merge_tree_exit_code: Option<i32>,
    pub merge_tree_stdout: String,
    pub merge_tree_stderr: String,
    pub head_matches_github: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct PrFleetOptions {
    pub refs: Vec<String>,
    pub update_branches: bool,
    pub apply: bool,
    pub merge_method: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GithubPrFleetOutput {
    pub component_id: String,
    pub owner: String,
    pub repo: String,
    pub action: String,
    pub success: bool,
    pub apply: bool,
    pub update_branches: bool,
    pub summary: GithubPrFleetSummary,
    pub items: Vec<GithubPrFleetItem>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GithubPrFleetSummary {
    pub total: usize,
    pub mergeable: usize,
    pub merged: usize,
    pub updated: usize,
    pub blocked: usize,
    pub errors: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct GithubPrFleetItem {
    pub input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    pub check_rollup: GithubPrCheckRollup,
    pub stale_base: bool,
    pub conflicts: bool,
    pub mergeable: bool,
    pub required_action: String,
    pub updated: bool,
    pub merged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GithubPrCheckRollup {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub pending: usize,
    pub unknown: usize,
}

/// Result of a find-many operation (list of matches).
#[derive(Debug, Clone, Serialize)]
pub struct GithubFindOutput {
    pub component_id: String,
    pub owner: String,
    pub repo: String,
    pub action: String,
    pub success: bool,
    pub items: Vec<GithubFindItem>,
}

/// Minimal identifier for a found issue or PR.
#[derive(Debug, Clone, Serialize)]
pub struct GithubFindItem {
    pub number: u64,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    pub url: String,
    pub state: String,
    /// GitHub `stateReason` (issues only). One of `completed`, `not_planned`,
    /// `reopened`, or `null`. Empty string when absent or for PRs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state_reason: String,
    /// GitHub `closedAt` ISO-8601 timestamp (issues only). Empty when absent
    /// (open issues) or for PRs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub closed_at: String,
    /// Labels attached to the issue/PR. Used for label-based suppression.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

/// Parameters for creating a new issue.
#[derive(Debug, Clone, Default)]
pub struct IssueCreateOptions {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    /// Optional workspace path. When set, the component is discovered from
    /// `<path>/homeboy.json` instead of the global registry - required for
    /// CI runners and other unregistered-checkout contexts.
    pub path: Option<String>,
}

/// Parameters for filtering issues.
#[derive(Debug, Clone, Default)]
pub struct IssueFindOptions {
    /// Exact title match (case-sensitive).
    pub title: Option<String>,
    /// All labels must be present.
    pub labels: Vec<String>,
    /// `open` (default), `closed`, or `all`.
    pub state: IssueState,
    /// Cap the number of returned items. Defaults to 30.
    pub limit: usize,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IssueState {
    #[default]
    Open,
    Closed,
    All,
}

impl IssueState {
    pub(super) fn as_gh_flag(self) -> &'static str {
        match self {
            IssueState::Open => "open",
            IssueState::Closed => "closed",
            IssueState::All => "all",
        }
    }
}

/// Parameters for creating a new PR.
#[derive(Debug, Clone, Default)]
pub struct PrCreateOptions {
    pub base: String,
    pub head: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

/// Parameters for editing an existing PR.
#[derive(Debug, Clone, Default)]
pub struct PrEditOptions {
    pub number: u64,
    pub title: Option<String>,
    pub body: Option<String>,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

/// Parameters for filtering PRs.
#[derive(Debug, Clone, Default)]
pub struct PrFindOptions {
    pub base: Option<String>,
    pub head: Option<String>,
    pub state: PrState,
    pub limit: usize,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

/// Parameters for reconciling GitHub PR mergeability with local git evidence.
#[derive(Debug, Clone, Default)]
pub struct PrMergeabilityReconcileOptions {
    pub number: u64,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PrMergeOptions {
    pub number: u64,
    pub method: String,
    pub delete_branch: bool,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PrState {
    #[default]
    Open,
    Closed,
    Merged,
    All,
}

impl PrState {
    pub(super) fn as_gh_flag(self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Closed => "closed",
            PrState::Merged => "merged",
            PrState::All => "all",
        }
    }
}

/// Parameters for closing an existing issue with a reason.
///
/// `reason` defaults to `Completed` (the GitHub-native signal for "the
/// underlying problem was resolved"). `NotPlanned` is preserved across CI
/// runs by `homeboy issues reconcile` as the "do not re-file" signal.
#[derive(Debug, Clone, Default)]
pub struct IssueCloseOptions {
    pub number: u64,
    /// Close-reason. `Completed` (default) is the GitHub-native signal for
    /// "the underlying problem was resolved." `NotPlanned` is the GitHub-native
    /// signal for "we've decided not to fix this" - used by `homeboy issues
    /// reconcile` to suppress re-filing on subsequent runs.
    pub reason: IssueCloseReason,
    /// Optional closing comment posted before the state transition. Useful
    /// for explaining why the issue is being closed (e.g. "All findings have
    /// been resolved" / "Closed as duplicate of #N" / "Closed as upstream bug").
    pub comment: Option<String>,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IssueCloseReason {
    #[default]
    Completed,
    NotPlanned,
}

impl IssueCloseReason {
    /// Render for the `gh issue close --reason` flag. GitHub's CLI uses the
    /// space-form (`"not planned"`) but the underlying GraphQL `state_reason`
    /// uses the underscore form (`not_planned`). `IssueState::Reason` parsing
    /// reads the underscore form from `gh ... --json stateReason`.
    pub(super) fn as_gh_flag(self) -> &'static str {
        match self {
            IssueCloseReason::Completed => "completed",
            IssueCloseReason::NotPlanned => "not planned",
        }
    }
}

/// Parameters for editing an existing issue.
#[derive(Debug, Clone, Default)]
pub struct IssueEditOptions {
    pub number: u64,
    pub title: Option<String>,
    pub body: Option<String>,
    /// Labels to add. Mirrors `gh issue edit --add-label` (repeatable).
    pub add_labels: Vec<String>,
    /// Labels to remove. Mirrors `gh issue edit --remove-label`.
    pub remove_labels: Vec<String>,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}

/// Parameters for posting a comment on an existing issue.
#[derive(Debug, Clone, Default)]
pub struct IssueCommentOptions {
    pub number: u64,
    pub body: String,
    /// Optional workspace path. See [`IssueCreateOptions::path`].
    pub path: Option<String>,
}
