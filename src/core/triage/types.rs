//! Public data types for triage reports, CI-failure digests, and landing dashboards.

use serde::Serialize;

use crate::core::observation::TriagePullRequestSignals;
use crate::core::scope::{ScopeComponentRef, ScopeOutput};

use super::TriageWatchOutput;

pub(crate) type ComponentRef = ScopeComponentRef;

#[derive(Debug, Clone, Default)]
pub struct TriageOptions {
    pub include_issues: bool,
    pub include_prs: bool,
    pub mine: bool,
    pub assigned: Option<String>,
    pub labels: Vec<String>,
    pub needs_review: bool,
    pub failing_checks: bool,
    pub drilldown: bool,
    pub issue_numbers: Vec<u64>,
    pub stale_days: Option<i64>,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct TriageLandingOptions {
    pub target: super::TriageTarget,
    pub repo: Option<String>,
    pub pr_refs: Vec<String>,
    pub branch_patterns: Vec<String>,
    pub source_issues: Vec<u64>,
    pub ordered: bool,
    pub drilldown: bool,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum TriageCommandOutput {
    Report(TriageOutput),
    Watch(TriageWatchOutput),
    CiFailure(CiFailureTriageOutput),
    Landing(TriageLandingOutput),
}

#[derive(Debug, Clone, Serialize)]
pub struct CiFailureTriageOutput {
    pub command: &'static str,
    pub repo: String,
    pub pull_request: u64,
    pub pr_url: String,
    pub head_sha: String,
    pub summary: CiFailureSummary,
    pub failures: Vec<CiFailureDigest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CiFailureSummary {
    pub failed_checks: usize,
    pub checks_summarized: usize,
    pub categories: Vec<String>,
    pub baseline_vs_head_detected: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CiFailureDigest {
    pub workflow: Option<String>,
    pub job: String,
    pub step: Option<String>,
    pub conclusion: Option<String>,
    pub category: String,
    pub baseline_vs_head: Option<String>,
    pub details_url: Option<String>,
    pub log_url: Option<String>,
    pub snippets: Vec<CiFailureSnippet>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CiFailureSnippet {
    pub line_start: usize,
    pub line_end: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct CiFailureTriageOptions {
    pub target: String,
    pub repo: Option<String>,
    pub max_checks: usize,
    pub snippet_lines: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageLandingOutput {
    pub command: &'static str,
    pub target: ScopeOutput,
    pub summary: TriageLandingSummary,
    pub pull_requests: Vec<TriageLandingPr>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<TriageUnresolved>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TriageLandingSummary {
    pub total: usize,
    pub merged: usize,
    pub clean_mergeable: usize,
    pub mergeability_clean: usize,
    pub mergeability_conflicting: usize,
    pub mergeability_unknown: usize,
    pub mergeability_unstable: usize,
    pub conflict_repair_needed: usize,
    pub checks_clean: usize,
    pub checks_pending: usize,
    pub checks_failed: usize,
    pub checks_unknown: usize,
    pub baseline_red_inconclusive: usize,
    pub candidate_red: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageLandingPr {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
    pub mergeability_state: TriageLandingMergeabilityState,
    pub check_state: TriageLandingCheckState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_repo: Option<String>,
    pub classification: TriageLandingClassification,
    pub suggested_next_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependent_rebase: Option<TriageLandingRebasePlan>,
    #[serde(flatten)]
    pub signals: TriagePullRequestSignals,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub check_failures: Vec<TriageCheckFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageLandingRebasePlan {
    pub after_pr: u64,
    pub safe_to_update: bool,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriageLandingClassification {
    Merged,
    CleanMergeable,
    ConflictRepairNeeded,
    ChecksPending,
    BaselineRedInconclusive,
    CandidateRed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriageLandingMergeabilityState {
    Clean,
    Conflicting,
    Unknown,
    Unstable,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriageLandingCheckState {
    Clean,
    Pending,
    Failed,
    Unknown,
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageOutput {
    pub command: String,
    pub target: ScopeOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<TriageObservationOutput>,
    pub summary: TriageSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unresolved_summary: Option<String>,
    pub components: Vec<TriageComponentReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<TriageUnresolved>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageObservationOutput {
    pub run_id: String,
    pub item_count: usize,
    pub store_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison: Option<TriageObservationComparison>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageObservationComparison {
    pub previous_run_id: String,
    pub previous_item_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub new_items: Vec<TriageObservationItemRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resolved_items: Vec<TriageObservationItemRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_items: Vec<TriageObservationChangedItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TriageObservationItemRef {
    pub repo: String,
    pub item_type: String,
    pub number: u64,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TriageObservationChangedItem {
    #[serde(flatten)]
    pub item: TriageObservationItemRef,
    pub changed_fields: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TriageSummary {
    pub components: usize,
    pub repos_resolved: usize,
    pub repos_unresolved: usize,
    pub open_issues: usize,
    pub open_prs: usize,
    pub needs_review: usize,
    pub failing_checks: usize,
    pub stale: usize,
    pub actions: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageComponentReport {
    pub component_id: String,
    pub local_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub usage: Vec<String>,
    pub repo: TriageRepo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issues: Option<TriageIssueBucket>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<TriagePrBucket>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<TriageAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageRepo {
    pub provider: &'static str,
    pub owner: String,
    pub name: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<TriageRepoRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triage_remote_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TriageRepoRef {
    pub owner: String,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TriageIssueBucket {
    pub open: usize,
    pub items: Vec<TriageIssueItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageIssueItem {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assignees: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_comment_at: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub linked_prs: Vec<TriageLinkedPr>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageLinkedPr {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TriagePrBucket {
    pub open: usize,
    pub items: Vec<TriagePrItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriagePrItem {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub draft: bool,
    #[serde(flatten)]
    pub signals: TriagePullRequestSignals,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_readiness: Option<TriageCiReadiness>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub check_failures: Vec<TriageCheckFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assignees: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageCiReadiness {
    pub checks: TriageCiReadinessBuckets,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_pending_started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_pending_duration_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failure_urls: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct TriageCiReadinessBuckets {
    pub required: TriageCiCheckStateCounts,
    pub optional: TriageCiCheckStateCounts,
    pub unknown_requirement: TriageCiCheckStateCounts,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct TriageCiCheckStateCounts {
    pub queued: usize,
    pub pending: usize,
    pub running: usize,
    pub failed: usize,
    pub skipped: usize,
    pub passed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageCheckFailure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageAction {
    pub kind: String,
    pub severity: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageUnresolved {
    pub component_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub local_path: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}
