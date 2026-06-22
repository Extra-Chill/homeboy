//! Read-only triage reports for component sets.
//!
//! The primitive resolves a scope (component/project/fleet/rig/path/workspace) to component
//! references, then overlays GitHub issue/PR state. It intentionally keeps the
//! GitHub calls read-only so `homeboy triage ...` is safe as a dashboard verb.
//! The separate `triage --watch --auto-merge` path is the explicit opt-in
//! exception for state-transition automation.

use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::core::defaults;
use crate::core::deploy::release_download::{detect_remote_url, parse_github_url, GitHubRepo};
use crate::core::error::{Error, Result};
use crate::core::git::{gh_probe_succeeds, GhClient};
use crate::core::observation::TriagePullRequestSignals;
#[cfg(test)]
use crate::core::observation::{NewTriageItemRecord, TriageItemRecord};
use crate::core::scope::{self, Scope, ScopeComponentRef, ScopeKind, ScopeOutput};

mod observation;
mod watch;

pub use crate::core::scope::Scope as TriageTarget;
#[cfg(test)]
use observation::{compare_triage_observations, triage_observation_metadata};
use observation::{usize_to_i64, TriageObservation};

pub use watch::{
    run as watch, TriageWatchEvent, TriageWatchItemState, TriageWatchOptions, TriageWatchOutput,
    TriageWatchTargetOutput,
};

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
    pub target: TriageTarget,
    pub repo: Option<String>,
    pub pr_refs: Vec<String>,
    pub branch_patterns: Vec<String>,
    pub source_issues: Vec<u64>,
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
    pub conflict_repair_needed: usize,
    pub checks_pending: usize,
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
    pub head_branch: Option<String>,
    pub classification: TriageLandingClassification,
    pub suggested_next_command: String,
    #[serde(flatten)]
    pub signals: TriagePullRequestSignals,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub check_failures: Vec<TriageCheckFailure>,
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

type ComponentRef = ScopeComponentRef;

pub fn run(target: TriageTarget, options: TriageOptions) -> Result<TriageOutput> {
    let observation = TriageObservation::start(&target, &options);
    let refs = resolve_target_components(&target)?;
    let global_priority_labels = defaults::load_config().triage.priority_labels;
    let mut components = Vec::new();
    let mut unresolved = Vec::new();

    for component_ref in refs {
        match resolve_repo(&component_ref) {
            Ok(repo) => components.push(fetch_component_report(
                &component_ref,
                repo,
                &options,
                global_priority_labels.as_ref(),
            )),
            Err(reason) => unresolved.push(TriageUnresolved {
                component_id: component_ref.component_id,
                local_path: component_ref.local_path,
                reason,
                sources: component_ref.sources.into_iter().collect(),
            }),
        }
    }

    if options.mine {
        components.retain(triage_component_has_items);
        unresolved.clear();
    }

    let summary = summarize(&components, &unresolved);
    let unresolved_summary = summarize_unresolved(&unresolved);
    let command = triage_command(&target);
    let mut output = TriageOutput {
        command: command.clone(),
        target: ScopeOutput::from(&target),
        observation: None,
        summary,
        unresolved_summary,
        components,
        unresolved,
    };

    if let Some(observation) = observation {
        output.observation = observation.finish(&output);
    }

    Ok(output)
}

pub fn ci_failure(options: CiFailureTriageOptions) -> Result<CiFailureTriageOutput> {
    let target = parse_pr_target(&options.target, options.repo.as_deref())?;
    let repo = format!("{}/{}", target.repo.owner, target.repo.repo);
    let gh = GhClient::for_repo(&target.repo);
    gh.ensure_ready()?;

    let pr = fetch_pull_request(&gh, target.number)?;
    let head_sha = pr.head.sha;
    let check_runs = fetch_failed_check_runs(&gh, &head_sha)?;
    let failed_checks = check_runs.len();
    let max_checks = options.max_checks.max(1);
    let snippet_lines = options.snippet_lines.max(1);
    let mut failures = Vec::new();

    for check in check_runs.into_iter().take(max_checks) {
        failures.push(summarize_failed_check(&gh, check, snippet_lines)?);
    }

    let categories = unique_sorted(failures.iter().map(|failure| failure.category.clone()));
    let baseline_vs_head_detected = failures
        .iter()
        .any(|failure| failure.baseline_vs_head.is_some());
    Ok(CiFailureTriageOutput {
        command: "triage.ci-failure",
        repo,
        pull_request: target.number,
        pr_url: format!(
            "https://{}/{}/{}/pull/{}",
            target.repo.host, target.repo.owner, target.repo.repo, target.number
        ),
        head_sha,
        summary: CiFailureSummary {
            failed_checks,
            checks_summarized: failures.len(),
            categories,
            baseline_vs_head_detected,
        },
        failures,
    })
}

#[derive(Debug, Clone)]
struct ParsedPrTarget {
    repo: GitHubRepo,
    number: u64,
}

fn parse_pr_target(target: &str, repo: Option<&str>) -> Result<ParsedPrTarget> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Err(Error::validation_invalid_argument(
            "target",
            "expected PR number or GitHub PR URL",
            Some(target.to_string()),
            None,
        ));
    }

    if trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        let repo = repo.ok_or_else(|| Error::validation_missing_argument(vec!["--repo".into()]))?;
        let gh_repo = parse_repo_arg(repo)?;
        let number = trimmed.parse::<u64>().map_err(|_| {
            Error::validation_invalid_argument(
                "target",
                "invalid PR number",
                Some(trimmed.into()),
                None,
            )
        })?;
        return Ok(ParsedPrTarget {
            repo: gh_repo,
            number,
        });
    }

    parse_pr_url(trimmed)
}

fn parse_repo_arg(repo: &str) -> Result<GitHubRepo> {
    let parts: Vec<&str> = repo.trim().split('/').collect();
    match parts.as_slice() {
        [owner, name] if !owner.is_empty() && !name.is_empty() => Ok(GitHubRepo {
            host: std::env::var("GH_HOST")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "github.com".to_string()),
            owner: (*owner).to_string(),
            repo: (*name).to_string(),
        }),
        [host, owner, name] if !host.is_empty() && !owner.is_empty() && !name.is_empty() => {
            Ok(GitHubRepo {
                host: (*host).to_string(),
                owner: (*owner).to_string(),
                repo: (*name).to_string(),
            })
        }
        _ => Err(Error::validation_invalid_argument(
            "repo",
            "expected owner/repo or host/owner/repo",
            Some(repo.to_string()),
            None,
        )),
    }
}

fn parse_pr_url(url: &str) -> Result<ParsedPrTarget> {
    let re = Regex::new(r#"^https://([^/]+)/([^/]+)/([^/]+)/pull/(\d+)(?:[/?#].*)?$"#)
        .expect("valid PR URL regex");
    let captures = re.captures(url).ok_or_else(|| {
        Error::validation_invalid_argument(
            "target",
            "expected PR number or GitHub PR URL",
            Some(url.to_string()),
            None,
        )
    })?;
    let number = captures[4].parse::<u64>().map_err(|_| {
        Error::validation_invalid_argument(
            "target",
            "invalid PR number",
            Some(url.to_string()),
            None,
        )
    })?;
    Ok(ParsedPrTarget {
        repo: GitHubRepo {
            host: captures[1].to_string(),
            owner: captures[2].to_string(),
            repo: captures[3].trim_end_matches(".git").to_string(),
        },
        number,
    })
}

#[derive(Debug, Deserialize)]
struct RawPullRequestRest {
    head: RawPullRequestHead,
}

#[derive(Debug, Deserialize)]
struct RawPullRequestHead {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct RawCheckRunsPage {
    #[serde(default)]
    check_runs: Vec<RawCheckRun>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawCheckRun {
    name: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    details_url: Option<String>,
    #[serde(default)]
    check_suite: Option<RawCheckSuite>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawCheckSuite {
    #[serde(default)]
    conclusion: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawActionsJob {
    name: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    steps: Vec<RawActionsStep>,
}

#[derive(Debug, Deserialize)]
struct RawActionsStep {
    name: String,
    #[serde(default)]
    conclusion: Option<String>,
}

fn fetch_pull_request(gh: &GhClient, number: u64) -> Result<RawPullRequestRest> {
    let api_path = format!("repos/{}/pulls/{number}", gh.repo_path()?);
    gh.api_json(&api_path)
}

fn fetch_failed_check_runs(gh: &GhClient, head_sha: &str) -> Result<Vec<RawCheckRun>> {
    let api_path = format!(
        "repos/{}/commits/{head_sha}/check-runs?filter=latest&per_page=100",
        gh.repo_path()?
    );
    let page: RawCheckRunsPage = gh.api_json(&api_path)?;
    Ok(page
        .check_runs
        .into_iter()
        .filter(|check| {
            matches!(
                check.conclusion.as_deref().or(check
                    .check_suite
                    .as_ref()
                    .and_then(|suite| suite.conclusion.as_deref())),
                Some("failure" | "cancelled" | "timed_out" | "action_required")
            )
        })
        .collect())
}

fn summarize_failed_check(
    gh: &GhClient,
    check: RawCheckRun,
    snippet_lines: usize,
) -> Result<CiFailureDigest> {
    let details_url = check.details_url.clone().or(check.html_url.clone());
    let job_id = details_url.as_deref().and_then(extract_actions_job_id);
    let job = job_id.and_then(|id| fetch_actions_job(gh, id).ok());
    let log = job_id.and_then(|id| fetch_actions_job_log(gh, id).ok());
    let log_url = match (job_id, gh.repo()) {
        (Some(id), Some(repo)) if gh.host() == "github.com" => Some(format!(
            "https://api.github.com/repos/{repo}/actions/jobs/{id}/logs"
        )),
        (Some(id), Some(repo)) => Some(format!(
            "https://{}/api/v3/repos/{repo}/actions/jobs/{id}/logs",
            gh.host()
        )),
        _ => None,
    };

    let job_name = job
        .as_ref()
        .map(|job| job.name.clone())
        .unwrap_or_else(|| check.name.clone());
    let step = job.as_ref().and_then(failed_step_name);
    let mut haystack = vec![check.name.as_str(), job_name.as_str()];
    if let Some(step) = step.as_deref() {
        haystack.push(step);
    }
    let snippet_source = log.as_deref().unwrap_or("");
    let snippets = extract_failure_snippets(snippet_source, snippet_lines);
    let snippet_text = snippets
        .iter()
        .map(|snippet| snippet.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    haystack.push(snippet_text.as_str());
    let category = classify_failure(&haystack);
    let baseline_vs_head = detect_baseline_vs_head(&haystack);

    Ok(CiFailureDigest {
        workflow: infer_workflow_name(details_url.as_deref(), &check.name),
        job: job_name,
        step,
        conclusion: job
            .as_ref()
            .and_then(|job| job.conclusion.clone())
            .or(check.conclusion),
        category,
        baseline_vs_head,
        details_url,
        log_url,
        snippets,
    })
}

fn fetch_actions_job(gh: &GhClient, job_id: u64) -> Result<RawActionsJob> {
    let api_path = format!("repos/{}/actions/jobs/{job_id}", gh.repo_path()?);
    gh.api_json(&api_path)
}

fn fetch_actions_job_log(gh: &GhClient, job_id: u64) -> Result<String> {
    let api_path = format!("repos/{}/actions/jobs/{job_id}/logs", gh.repo_path()?);
    let bytes = gh.api_bytes(&api_path)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn failed_step_name(job: &RawActionsJob) -> Option<String> {
    job.steps
        .iter()
        .find(|step| {
            matches!(
                step.conclusion.as_deref(),
                Some("failure" | "cancelled" | "timed_out" | "action_required")
            )
        })
        .map(|step| step.name.clone())
}

fn extract_actions_job_id(url: &str) -> Option<u64> {
    let re = Regex::new(r#"/actions/runs/\d+/job/(\d+)"#).expect("valid job URL regex");
    re.captures(url)
        .and_then(|captures| captures[1].parse().ok())
}

fn infer_workflow_name(details_url: Option<&str>, check_name: &str) -> Option<String> {
    check_name
        .split_once('/')
        .map(|(workflow, _)| workflow.trim().to_string())
        .filter(|workflow| !workflow.is_empty())
        .or_else(|| details_url.and_then(|_| None))
}

fn extract_failure_snippets(log: &str, context_lines: usize) -> Vec<CiFailureSnippet> {
    if log.trim().is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = log.lines().collect();
    let mut snippets = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if !looks_like_failure_line(line) {
            continue;
        }
        let start = index.saturating_sub(context_lines / 2);
        let end = (index + (context_lines / 2) + 1).min(lines.len());
        snippets.push(CiFailureSnippet {
            line_start: start + 1,
            line_end: end,
            text: lines[start..end].join("\n"),
        });
        if snippets.len() >= 3 {
            break;
        }
    }
    snippets
}

fn looks_like_failure_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("::error")
        || lower.contains("error:")
        || lower.contains("failed")
        || lower.contains("failure")
        || lower.contains("panicked")
        || lower.contains("test result: failed")
}

fn classify_failure(haystack: &[&str]) -> String {
    let text = haystack.join("\n").to_ascii_lowercase();
    if text.contains("rustfmt") || text.contains("cargo fmt") || text.contains("formatting") {
        "fmt"
    } else if text.contains("clippy") {
        "clippy"
    } else if text.contains("error[e")
        || text.contains("could not compile")
        || text.contains("compile")
    {
        "compile"
    } else if text.contains("test result: failed")
        || text.contains("panicked")
        || text.contains("assertion")
    {
        "unit-test"
    } else if text.contains("baseline") || text.contains("golden") || text.contains("snapshot") {
        "baseline"
    } else if text.contains("queued") || text.contains("queue") || text.contains("runner") {
        "queueing"
    } else if text.contains("timed out") || text.contains("network") || text.contains("rate limit")
    {
        "infra"
    } else {
        "unknown"
    }
    .to_string()
}

fn detect_baseline_vs_head(haystack: &[&str]) -> Option<String> {
    let text = haystack.join("\n").to_ascii_lowercase();
    match (text.contains("baseline"), text.contains("head")) {
        (true, true) => Some("baseline-vs-head".to_string()),
        (true, false) => Some("baseline".to_string()),
        (false, true) => Some("head".to_string()),
        (false, false) => None,
    }
}

fn unique_sorted(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut values: Vec<String> = values.collect();
    values.sort();
    values.dedup();
    values
}

pub fn landing(options: TriageLandingOptions) -> Result<TriageLandingOutput> {
    let target = options.target.clone();
    let repos = resolve_landing_repos(&options)?;
    if repos.len() > 1 && options.pr_refs.iter().any(|raw| is_bare_pr_number(raw)) {
        return Err(Error::validation_invalid_argument(
            "pr_refs",
            "bare PR numbers require --repo or a landing scope that resolves to one repo",
            None,
            Some(vec![
                "Use owner/repo#number or a GitHub PR URL for fleet-wide landing dashboards"
                    .to_string(),
            ]),
        ));
    }
    let mut pull_requests = Vec::new();
    let mut unresolved = Vec::new();

    for repo in repos {
        let repo_slug = format!("{}/{}", repo.owner, repo.repo);
        match fetch_landing_prs_for_repo(&repo, &options) {
            Ok(mut prs) => pull_requests.append(&mut prs),
            Err(reason) => unresolved.push(TriageUnresolved {
                component_id: repo_slug,
                local_path: String::new(),
                reason,
                sources: vec!["triage.landing".to_string()],
            }),
        }
    }

    pull_requests.sort_by(|a, b| a.repo.cmp(&b.repo).then(a.number.cmp(&b.number)));
    pull_requests.dedup_by(|a, b| a.repo == b.repo && a.number == b.number);
    let summary = summarize_landing(&pull_requests);

    Ok(TriageLandingOutput {
        command: "triage.landing",
        target: ScopeOutput::from(&target),
        summary,
        pull_requests,
        unresolved,
    })
}

fn triage_component_has_items(component: &TriageComponentReport) -> bool {
    component
        .issues
        .as_ref()
        .is_some_and(|bucket| !bucket.items.is_empty())
        || component
            .pull_requests
            .as_ref()
            .is_some_and(|bucket| !bucket.items.is_empty())
}

fn resolve_target_components(target: &TriageTarget) -> Result<Vec<ComponentRef>> {
    let refs = scope::resolve_scope_components(target)?;
    if matches!(target, Scope::Workspace) {
        Ok(dedupe_refs_by_repo(refs))
    } else {
        Ok(refs)
    }
}

fn triage_command(target: &TriageTarget) -> String {
    // `--path` is an escape hatch on subcommands (currently `component`); keep
    // the same command identity so JSON consumers don't see a phantom
    // `triage.path` verb.
    target.command_name("triage", ScopeKind::Component)
}

fn dedupe_refs_by_repo(component_refs: Vec<ComponentRef>) -> Vec<ComponentRef> {
    let mut resolved = BTreeMap::new();
    let mut unresolved = Vec::new();

    for component_ref in component_refs {
        match resolve_repo(&component_ref) {
            Ok(resolved_repo) => {
                let key = format!(
                    "{}/{}",
                    resolved_repo.repo.owner.to_lowercase(),
                    resolved_repo.repo.repo.to_lowercase()
                );
                let entry = resolved.entry(key).or_insert_with(|| component_ref.clone());
                entry.sources.extend(component_ref.sources);
                entry.usage.extend(component_ref.usage);
                if entry.local_path.is_empty() && !component_ref.local_path.is_empty() {
                    entry.local_path = component_ref.local_path;
                }
                if entry.remote_url.is_none() {
                    entry.remote_url = component_ref.remote_url;
                }
                if entry.triage_remote_url.is_none() {
                    entry.triage_remote_url = component_ref.triage_remote_url;
                }
                if entry.priority_labels.is_none() {
                    entry.priority_labels = component_ref.priority_labels;
                }
            }
            Err(_) => unresolved.push(component_ref),
        }
    }

    let mut refs: Vec<ComponentRef> = resolved.into_values().collect();
    refs.extend(unresolved);
    refs.sort_by(|a, b| a.component_id.cmp(&b.component_id));
    refs
}

#[derive(Debug, Clone)]
struct ResolvedRepo {
    repo: GitHubRepo,
    triage_remote_url: Option<String>,
    source_repo: Option<GitHubRepo>,
}

fn resolve_repo(component_ref: &ComponentRef) -> std::result::Result<ResolvedRepo, String> {
    resolve_repo_with_parent_resolver(component_ref, github_parent_repo)
}

fn resolve_repo_with_parent_resolver(
    component_ref: &ComponentRef,
    parent_resolver: impl Fn(&GitHubRepo) -> std::result::Result<Option<GitHubRepo>, String>,
) -> std::result::Result<ResolvedRepo, String> {
    let source_remote_url = component_ref
        .remote_url
        .clone()
        .or_else(|| detect_remote_url(Path::new(&component_ref.local_path)));

    let triage_remote_url = component_ref
        .triage_remote_url
        .clone()
        .or_else(|| source_remote_url.clone())
        .ok_or_else(|| "missing_remote_url_and_no_git_origin".to_string())?;
    let mut repo = parse_github_url(&triage_remote_url).ok_or_else(|| {
        if component_ref.triage_remote_url.is_some() {
            "triage_remote_url_is_not_github".to_string()
        } else {
            "remote_url_is_not_github".to_string()
        }
    })?;

    let mut source_repo = source_remote_url
        .and_then(|url| parse_github_url(&url))
        .filter(|source| source.owner != repo.owner || source.repo != repo.repo);

    if component_ref.triage_remote_url.is_none() {
        if let Ok(Some(parent)) = parent_resolver(&repo) {
            source_repo = Some(repo);
            repo = parent;
        }
    }

    Ok(ResolvedRepo {
        repo,
        triage_remote_url: component_ref.triage_remote_url.clone(),
        source_repo,
    })
}

#[cfg(not(test))]
fn github_parent_repo(repo: &GitHubRepo) -> std::result::Result<Option<GitHubRepo>, String> {
    let args = vec![
        "repo".to_string(),
        "view".to_string(),
        format!("{}/{}", repo.owner, repo.repo),
        "--json".to_string(),
        "isFork,parent".to_string(),
    ];
    parse_github_parent_repo(&run_gh(&args)?)
}

#[cfg(test)]
fn github_parent_repo(_repo: &GitHubRepo) -> std::result::Result<Option<GitHubRepo>, String> {
    Ok(None)
}

#[derive(Debug, Deserialize)]
struct RawRepoParent {
    #[serde(default, rename = "isFork")]
    is_fork: bool,
    #[serde(default)]
    parent: Option<RawRepoParentRepo>,
}

#[derive(Debug, Deserialize)]
struct RawRepoParentRepo {
    name: String,
    owner: RawRepoParentOwner,
}

#[derive(Debug, Deserialize)]
struct RawRepoParentOwner {
    login: String,
}

fn parse_github_parent_repo(raw: &str) -> std::result::Result<Option<GitHubRepo>, String> {
    let parsed: RawRepoParent = serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
    Ok(if parsed.is_fork {
        parsed.parent.map(|parent| GitHubRepo {
            host: "github.com".to_string(),
            owner: parent.owner.login,
            repo: parent.name,
        })
    } else {
        None
    })
}

fn fetch_component_report(
    component_ref: &ComponentRef,
    resolved: ResolvedRepo,
    options: &TriageOptions,
    global_priority_labels: Option<&Vec<String>>,
) -> TriageComponentReport {
    let repo = resolved.repo;
    let source_repo = resolved.source_repo.clone();
    let repo_output = TriageRepo {
        provider: "github",
        owner: repo.owner.clone(),
        name: repo.repo.clone(),
        url: format!("https://github.com/{}/{}", repo.owner, repo.repo),
        source_repo: source_repo.clone().map(|source| TriageRepoRef {
            owner: source.owner.clone(),
            name: source.repo.clone(),
            url: format!("https://github.com/{}/{}", source.owner, source.repo),
        }),
        triage_remote_url: resolved.triage_remote_url,
    };
    let stale_cutoff = options
        .stale_days
        .map(|days| Utc::now() - Duration::days(days));

    let mut error = None;
    macro_rules! record_fetch_error {
        ($next_error:expr) => {
            error = Some(match error.take() {
                Some(existing) => format!("{existing}; {}", $next_error),
                None => $next_error,
            });
        };
    }
    let issues = if options.include_issues {
        fetch_issues(&repo, options, stale_cutoff)
            .map(issue_bucket)
            .map(Some)
            .unwrap_or_else(|e| {
                record_fetch_error!(e);
                Some(TriageIssueBucket::default())
            })
    } else {
        None
    };

    let pull_requests = if options.include_prs {
        match fetch_prs(&repo, source_repo.as_ref(), options, stale_cutoff) {
            Ok(items) => Some(TriagePrBucket {
                open: items.len(),
                items,
            }),
            Err(e) => {
                record_fetch_error!(e);
                Some(TriagePrBucket::default())
            }
        }
    } else {
        None
    };

    let priority_labels = resolve_priority_labels(component_ref, global_priority_labels);
    let actions = build_actions(issues.as_ref(), pull_requests.as_ref(), &priority_labels);

    TriageComponentReport {
        component_id: component_ref.component_id.clone(),
        local_path: component_ref.local_path.clone(),
        sources: component_ref.sources.iter().cloned().collect(),
        usage: component_ref.usage.iter().cloned().collect(),
        repo: repo_output,
        issues,
        pull_requests,
        actions,
        error,
    }
}

fn issue_bucket(items: Vec<TriageIssueItem>) -> TriageIssueBucket {
    TriageIssueBucket {
        open: items.iter().filter(|item| item.state == "OPEN").count(),
        items,
    }
}

fn fetch_issues(
    repo: &GitHubRepo,
    options: &TriageOptions,
    stale_cutoff: Option<DateTime<Utc>>,
) -> std::result::Result<Vec<TriageIssueItem>, String> {
    ensure_gh_ready()?;
    if !options.issue_numbers.is_empty() {
        return fetch_targeted_issues(repo, options, stale_cutoff);
    }

    let mut args = vec![
        "issue".to_string(),
        "list".to_string(),
        "-R".to_string(),
        format!("{}/{}", repo.owner, repo.repo),
        "--state".to_string(),
        "open".to_string(),
        "--limit".to_string(),
        effective_limit(options).to_string(),
        "--json".to_string(),
        "number,title,url,state,labels,assignees,comments,updatedAt".to_string(),
    ];
    if options.mine {
        args.push("--assignee".to_string());
        args.push("@me".to_string());
    }
    if let Some(assigned) = &options.assigned {
        args.push("--assignee".to_string());
        args.push(assigned.clone());
    }
    for label in &options.labels {
        args.push("--label".to_string());
        args.push(label.clone());
    }

    let raw = run_gh(&args)?;
    parse_issues(&raw, stale_cutoff)
}

fn fetch_targeted_issues(
    repo: &GitHubRepo,
    options: &TriageOptions,
    stale_cutoff: Option<DateTime<Utc>>,
) -> std::result::Result<Vec<TriageIssueItem>, String> {
    let mut items = Vec::new();
    for number in &options.issue_numbers {
        let args = vec![
            "issue".to_string(),
            "view".to_string(),
            number.to_string(),
            "-R".to_string(),
            format!("{}/{}", repo.owner, repo.repo),
            "--json".to_string(),
            "number,title,url,state,labels,assignees,comments,updatedAt".to_string(),
        ];
        let raw = run_gh(&args)?;
        let mut issue = parse_issue(&raw, stale_cutoff)?;
        issue.linked_prs = fetch_linked_prs(repo, issue.number)?;
        items.push(issue);
    }
    Ok(items)
}

fn fetch_prs(
    repo: &GitHubRepo,
    source_repo: Option<&GitHubRepo>,
    options: &TriageOptions,
    stale_cutoff: Option<DateTime<Utc>>,
) -> std::result::Result<Vec<TriagePrItem>, String> {
    ensure_gh_ready()?;
    let mut args = vec![
        "pr".to_string(),
        "list".to_string(),
        "-R".to_string(),
        format!("{}/{}", repo.owner, repo.repo),
        "--state".to_string(),
        "open".to_string(),
        "--limit".to_string(),
        effective_limit(options).to_string(),
        "--json".to_string(),
        "number,title,url,state,isDraft,reviewDecision,mergeStateStatus,statusCheckRollup,labels,assignees,author,comments,reviews,updatedAt".to_string(),
    ];
    if options.mine {
        args.push("--author".to_string());
        args.push("@me".to_string());
    } else if let Some(source_repo) = source_repo {
        args.push("--author".to_string());
        args.push(source_repo.owner.clone());
    }
    for label in &options.labels {
        args.push("--label".to_string());
        args.push(label.clone());
    }

    let raw = run_gh(&args)?;
    let mut items = parse_prs(&raw, stale_cutoff, options.drilldown)?;
    if options.needs_review {
        items.retain(|item| item.signals.review_decision.as_deref() == Some("REVIEW_REQUIRED"));
    }
    if options.failing_checks {
        items.retain(|item| item.signals.checks.as_deref() == Some("FAILURE"));
    }
    if let Some(assigned) = &options.assigned {
        items.retain(|item| item.assignees.iter().any(|a| a == assigned));
    }
    Ok(items)
}

fn resolve_landing_repos(options: &TriageLandingOptions) -> Result<Vec<GitHubRepo>> {
    if let Some(repo) = &options.repo {
        return parse_landing_repo(repo).map(|repo| vec![repo]);
    }

    let refs = resolve_target_components(&options.target)?;
    let mut repos = Vec::new();
    let mut unresolved = Vec::new();
    for component_ref in refs {
        match resolve_repo(&component_ref) {
            Ok(resolved) => repos.push(resolved.repo),
            Err(reason) => unresolved.push(reason),
        }
    }
    repos.sort_by(|a, b| a.owner.cmp(&b.owner).then(a.repo.cmp(&b.repo)));
    repos.dedup_by(|a, b| a.owner == b.owner && a.repo == b.repo);
    if repos.is_empty() {
        return Err(Error::internal_unexpected(format!(
            "No GitHub repos resolved for landing target: {}",
            unresolved.join("; ")
        )));
    }
    Ok(repos)
}

fn parse_landing_repo(raw: &str) -> Result<GitHubRepo> {
    parse_github_url(raw)
        .or_else(|| {
            let (owner, repo) = raw.split_once('/')?;
            Some(GitHubRepo {
                host: "github.com".to_string(),
                owner: owner.trim().to_string(),
                repo: repo.trim_end_matches(".git").trim().to_string(),
            })
        })
        .filter(|repo| !repo.owner.is_empty() && !repo.repo.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "repo",
                "expected GitHub repo as owner/name or GitHub URL",
                Some(raw.to_string()),
                None,
            )
        })
}

fn fetch_landing_prs_for_repo(
    repo: &GitHubRepo,
    options: &TriageLandingOptions,
) -> std::result::Result<Vec<TriageLandingPr>, String> {
    ensure_gh_ready()?;
    let mut items = Vec::new();
    let mut explicit_numbers = Vec::new();
    for raw in &options.pr_refs {
        if let Some(reference) = parse_landing_pr_ref(raw, repo)? {
            if reference.owner == repo.owner && reference.repo == repo.repo {
                explicit_numbers.push(reference.number);
            }
        }
    }

    for number in explicit_numbers {
        items.push(fetch_landing_pr(repo, number, options.drilldown)?);
    }

    if !options.branch_patterns.is_empty()
        || (options.pr_refs.is_empty() && options.source_issues.is_empty())
    {
        let mut listed = fetch_landing_open_prs(repo, options)?;
        if !options.branch_patterns.is_empty() {
            listed.retain(|item| {
                item.head_branch.as_deref().is_some_and(|branch| {
                    options
                        .branch_patterns
                        .iter()
                        .any(|pattern| branch_matches(pattern, branch))
                })
            });
        }
        items.append(&mut listed);
    }

    for issue_number in &options.source_issues {
        for linked in fetch_linked_prs(repo, *issue_number)? {
            items.push(fetch_landing_pr(repo, linked.number, options.drilldown)?);
        }
    }

    Ok(items)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LandingPrRef {
    owner: String,
    repo: String,
    number: u64,
}

fn parse_landing_pr_ref(
    raw: &str,
    default_repo: &GitHubRepo,
) -> std::result::Result<Option<LandingPrRef>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if let Ok(number) = trimmed.trim_start_matches('#').parse::<u64>() {
        return Ok(Some(LandingPrRef {
            owner: default_repo.owner.clone(),
            repo: default_repo.repo.clone(),
            number,
        }));
    }
    let (repo_raw, number_raw) = trimmed
        .rsplit_once('#')
        .or_else(|| trimmed.rsplit_once("/pull/"))
        .ok_or_else(|| format!("PR ref must be a number, owner/repo#number, or PR URL: {raw}"))?;
    let number = number_raw
        .parse::<u64>()
        .map_err(|_| format!("PR ref number must be a positive integer: {raw}"))?;
    let repo = parse_github_url(repo_raw)
        .or_else(|| {
            let (owner, repo) = repo_raw.split_once('/')?;
            Some(GitHubRepo {
                host: "github.com".to_string(),
                owner: owner.to_string(),
                repo: repo.trim_end_matches(".git").to_string(),
            })
        })
        .ok_or_else(|| format!("PR ref repo must be owner/name or GitHub URL: {raw}"))?;
    Ok(Some(LandingPrRef {
        owner: repo.owner,
        repo: repo.repo,
        number,
    }))
}

fn is_bare_pr_number(raw: &str) -> bool {
    let trimmed = raw.trim().trim_start_matches('#');
    !trimmed.is_empty() && trimmed.chars().all(|ch| ch.is_ascii_digit())
}

fn fetch_landing_open_prs(
    repo: &GitHubRepo,
    options: &TriageLandingOptions,
) -> std::result::Result<Vec<TriageLandingPr>, String> {
    let args = vec![
        "pr".to_string(),
        "list".to_string(),
        "-R".to_string(),
        format!("{}/{}", repo.owner, repo.repo),
        "--state".to_string(),
        "open".to_string(),
        "--limit".to_string(),
        effective_landing_limit(options).to_string(),
        "--json".to_string(),
        landing_pr_json_fields().to_string(),
    ];
    let raw = run_gh(&args)?;
    parse_landing_prs(&raw, repo, options.drilldown)
}

fn fetch_landing_pr(
    repo: &GitHubRepo,
    number: u64,
    drilldown: bool,
) -> std::result::Result<TriageLandingPr, String> {
    let args = vec![
        "pr".to_string(),
        "view".to_string(),
        number.to_string(),
        "-R".to_string(),
        format!("{}/{}", repo.owner, repo.repo),
        "--json".to_string(),
        landing_pr_json_fields().to_string(),
    ];
    let raw = run_gh(&args)?;
    parse_landing_pr(&raw, repo, drilldown)
}

fn landing_pr_json_fields() -> &'static str {
    "number,title,url,state,isDraft,reviewDecision,mergeStateStatus,statusCheckRollup,headRefName,comments,reviews,updatedAt,mergedAt"
}

fn effective_landing_limit(options: &TriageLandingOptions) -> usize {
    if options.limit == 0 {
        30
    } else {
        options.limit
    }
}

fn branch_matches(pattern: &str, branch: &str) -> bool {
    if pattern == "*" || pattern == branch {
        return true;
    }
    if let Some(contains) = pattern
        .strip_prefix('*')
        .and_then(|value| value.strip_suffix('*'))
    {
        return branch.contains(contains);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return branch.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return branch.ends_with(suffix);
    }
    branch.contains(pattern)
}

fn effective_limit(options: &TriageOptions) -> usize {
    if options.limit == 0 {
        30
    } else {
        options.limit
    }
}

fn ensure_gh_ready() -> std::result::Result<(), String> {
    if !gh_probe_succeeds(&["--version"]) {
        return Err("gh CLI not found on PATH".to_string());
    }
    if !gh_probe_succeeds(&["auth", "status", "--hostname", "github.com"]) {
        return Err("gh is not authenticated for github.com".to_string());
    }
    Ok(())
}

pub(super) fn run_gh(args: &[String]) -> std::result::Result<String, String> {
    let output = Command::new("gh")
        .args(args.iter().map(|s| s.as_str()))
        .output()
        .map_err(|e| format!("failed to invoke gh: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Err(if stderr.is_empty() { stdout } else { stderr });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[derive(Debug, Deserialize)]
struct RawNamedNode {
    name: Option<String>,
    login: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawIssue {
    number: u64,
    title: String,
    url: String,
    state: String,
    #[serde(default)]
    labels: Vec<RawNamedNode>,
    #[serde(default)]
    assignees: Vec<RawNamedNode>,
    #[serde(default)]
    comments: Vec<RawComment>,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawComment {
    #[serde(default, rename = "createdAt")]
    created_at: Option<String>,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawReview {
    #[serde(default, rename = "submittedAt")]
    submitted_at: Option<String>,
}

fn parse_issues(
    raw: &str,
    stale_cutoff: Option<DateTime<Utc>>,
) -> std::result::Result<Vec<TriageIssueItem>, String> {
    let parsed: Vec<RawIssue> = serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
    Ok(parsed
        .into_iter()
        .map(|item| raw_issue_to_item(item, stale_cutoff))
        .collect())
}

fn parse_issue(
    raw: &str,
    stale_cutoff: Option<DateTime<Utc>>,
) -> std::result::Result<TriageIssueItem, String> {
    let parsed: RawIssue = serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
    Ok(raw_issue_to_item(parsed, stale_cutoff))
}

fn raw_issue_to_item(item: RawIssue, stale_cutoff: Option<DateTime<Utc>>) -> TriageIssueItem {
    let stale = is_stale(item.updated_at.as_deref(), stale_cutoff);
    TriageIssueItem {
        number: item.number,
        title: item.title,
        url: item.url,
        state: item.state,
        labels: item.labels.into_iter().filter_map(|l| l.name).collect(),
        assignees: item.assignees.into_iter().filter_map(|a| a.login).collect(),
        comments_count: Some(item.comments.len()),
        last_comment_at: latest_comment_at(&item.comments),
        updated_at: item.updated_at,
        stale,
        linked_prs: Vec::new(),
    }
}

#[derive(Debug, Deserialize)]
struct RawLinkedPr {
    number: u64,
    title: String,
    url: String,
    state: String,
    #[serde(default, rename = "mergedAt")]
    merged_at: Option<String>,
}

fn fetch_linked_prs(
    repo: &GitHubRepo,
    issue_number: u64,
) -> std::result::Result<Vec<TriageLinkedPr>, String> {
    let args = vec![
        "pr".to_string(),
        "list".to_string(),
        "-R".to_string(),
        format!("{}/{}", repo.owner, repo.repo),
        "--state".to_string(),
        "all".to_string(),
        "--search".to_string(),
        format!("#{issue_number}"),
        "--limit".to_string(),
        "30".to_string(),
        "--json".to_string(),
        "number,title,url,state,mergedAt".to_string(),
    ];
    let raw = run_gh(&args)?;
    parse_linked_prs(&raw)
}

fn parse_linked_prs(raw: &str) -> std::result::Result<Vec<TriageLinkedPr>, String> {
    let parsed: Vec<RawLinkedPr> = serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
    Ok(parsed
        .into_iter()
        .map(|item| TriageLinkedPr {
            number: item.number,
            title: item.title,
            url: item.url,
            state: item.state,
            merged_at: item.merged_at,
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct RawPr {
    number: u64,
    title: String,
    url: String,
    state: String,
    #[serde(default, rename = "isDraft")]
    is_draft: bool,
    #[serde(default, rename = "reviewDecision")]
    review_decision: Option<String>,
    #[serde(default, rename = "mergeStateStatus")]
    merge_state_status: Option<String>,
    #[serde(default, rename = "statusCheckRollup")]
    status_check_rollup: Vec<Value>,
    #[serde(default, rename = "headRefName")]
    head_ref_name: Option<String>,
    #[serde(default, rename = "mergedAt")]
    merged_at: Option<String>,
    #[serde(default)]
    labels: Vec<RawNamedNode>,
    #[serde(default)]
    assignees: Vec<RawNamedNode>,
    #[serde(default)]
    author: Option<RawNamedNode>,
    #[serde(default)]
    comments: Vec<RawComment>,
    #[serde(default)]
    reviews: Vec<RawReview>,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<String>,
}

fn parse_landing_prs(
    raw: &str,
    repo: &GitHubRepo,
    drilldown: bool,
) -> std::result::Result<Vec<TriageLandingPr>, String> {
    let parsed: Vec<RawPr> = serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
    Ok(parsed
        .into_iter()
        .map(|item| raw_pr_to_landing_pr(item, repo, drilldown))
        .collect())
}

fn parse_landing_pr(
    raw: &str,
    repo: &GitHubRepo,
    drilldown: bool,
) -> std::result::Result<TriageLandingPr, String> {
    let parsed: RawPr = serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
    Ok(raw_pr_to_landing_pr(parsed, repo, drilldown))
}

fn raw_pr_to_landing_pr(item: RawPr, repo: &GitHubRepo, drilldown: bool) -> TriageLandingPr {
    let signals = TriagePullRequestSignals {
        checks: summarize_checks(&item.status_check_rollup),
        review_decision: non_empty(item.review_decision),
        merge_state: non_empty(item.merge_state_status),
        comments_count: usize_to_i64(item.comments.len()),
        reviews_count: usize_to_i64(item.reviews.len()),
        last_comment_at: latest_comment_at(&item.comments),
        last_review_at: latest_review_at(&item.reviews),
        ..TriagePullRequestSignals::default()
    };
    let check_failures = if drilldown {
        summarize_check_failures(&item.status_check_rollup)
    } else {
        Vec::new()
    };
    let classification = classify_landing_pr(
        &item.state,
        item.merged_at.as_deref(),
        signals.checks.as_deref(),
        signals.merge_state.as_deref(),
    );
    TriageLandingPr {
        repo: format!("{}/{}", repo.owner, repo.repo),
        number: item.number,
        title: item.title,
        url: item.url,
        state: item.state,
        head_branch: non_empty(item.head_ref_name),
        classification,
        suggested_next_command: landing_next_command(classification, repo, item.number),
        signals,
        check_failures,
    }
}

pub(crate) fn classify_landing_pr(
    state: &str,
    merged_at: Option<&str>,
    checks: Option<&str>,
    merge_state: Option<&str>,
) -> TriageLandingClassification {
    if state == "MERGED" || merged_at.is_some() {
        return TriageLandingClassification::Merged;
    }
    if matches!(
        merge_state,
        Some("DIRTY" | "BEHIND" | "BLOCKED" | "HAS_HOOKS")
    ) {
        return TriageLandingClassification::ConflictRepairNeeded;
    }
    if matches!(checks, Some("PENDING")) || (merge_state == Some("CLEAN") && checks.is_none()) {
        return TriageLandingClassification::ChecksPending;
    }
    if checks == Some("FAILURE") {
        return TriageLandingClassification::CandidateRed;
    }
    if merge_state == Some("CLEAN") && checks == Some("SUCCESS") {
        return TriageLandingClassification::CleanMergeable;
    }
    if checks.is_none() || matches!(merge_state, None | Some("UNKNOWN" | "UNSTABLE")) {
        return TriageLandingClassification::BaselineRedInconclusive;
    }
    TriageLandingClassification::Unknown
}

fn landing_next_command(
    classification: TriageLandingClassification,
    repo: &GitHubRepo,
    number: u64,
) -> String {
    let reference = format!("{}/{}#{}", repo.owner, repo.repo, number);
    match classification {
        TriageLandingClassification::Merged => {
            format!("homeboy triage --watch {reference} --until merged")
        }
        TriageLandingClassification::CleanMergeable => {
            format!("homeboy triage --watch {reference} --until green-mergeable")
        }
        TriageLandingClassification::ConflictRepairNeeded => {
            format!(
                "gh pr checkout {number} -R {}/{} && git rebase origin/main",
                repo.owner, repo.repo
            )
        }
        TriageLandingClassification::ChecksPending => {
            format!("homeboy triage --watch {reference} --until green")
        }
        TriageLandingClassification::BaselineRedInconclusive => {
            format!("gh pr checks {number} -R {}/{}", repo.owner, repo.repo)
        }
        TriageLandingClassification::CandidateRed => {
            format!(
                "gh pr checks {number} -R {}/{} --fail-fast",
                repo.owner, repo.repo
            )
        }
        TriageLandingClassification::Unknown => {
            format!("gh pr view {number} -R {}/{} --web", repo.owner, repo.repo)
        }
    }
}

fn summarize_landing(items: &[TriageLandingPr]) -> TriageLandingSummary {
    let mut summary = TriageLandingSummary {
        total: items.len(),
        ..Default::default()
    };
    for item in items {
        match item.classification {
            TriageLandingClassification::Merged => summary.merged += 1,
            TriageLandingClassification::CleanMergeable => summary.clean_mergeable += 1,
            TriageLandingClassification::ConflictRepairNeeded => {
                summary.conflict_repair_needed += 1
            }
            TriageLandingClassification::ChecksPending => summary.checks_pending += 1,
            TriageLandingClassification::BaselineRedInconclusive => {
                summary.baseline_red_inconclusive += 1
            }
            TriageLandingClassification::CandidateRed => summary.candidate_red += 1,
            TriageLandingClassification::Unknown => summary.unknown += 1,
        }
    }
    summary
}

fn parse_prs(
    raw: &str,
    stale_cutoff: Option<DateTime<Utc>>,
    include_drilldown: bool,
) -> std::result::Result<Vec<TriagePrItem>, String> {
    let parsed: Vec<RawPr> = serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
    Ok(parsed
        .into_iter()
        .map(|item| {
            let stale = is_stale(item.updated_at.as_deref(), stale_cutoff);
            let mut pr = TriagePrItem {
                number: item.number,
                title: item.title,
                url: item.url,
                state: item.state,
                draft: item.is_draft,
                signals: TriagePullRequestSignals {
                    checks: summarize_checks(&item.status_check_rollup),
                    review_decision: non_empty(item.review_decision),
                    merge_state: non_empty(item.merge_state_status),
                    comments_count: usize_to_i64(item.comments.len()),
                    reviews_count: usize_to_i64(item.reviews.len()),
                    last_comment_at: latest_comment_at(&item.comments),
                    last_review_at: latest_review_at(&item.reviews),
                    ..TriagePullRequestSignals::default()
                },
                check_failures: if include_drilldown {
                    summarize_check_failures(&item.status_check_rollup)
                } else {
                    Vec::new()
                },
                labels: item.labels.into_iter().filter_map(|l| l.name).collect(),
                assignees: item.assignees.into_iter().filter_map(|a| a.login).collect(),
                author: item.author.and_then(|a| a.login),
                updated_at: item.updated_at,
                stale,
            };
            pr.signals.next_action = derive_pr_next_action(&pr);
            pr
        })
        .collect())
}

fn latest_comment_at(comments: &[RawComment]) -> Option<String> {
    comments
        .iter()
        .filter_map(|comment| comment.updated_at.as_ref().or(comment.created_at.as_ref()))
        .max()
        .cloned()
}

fn latest_review_at(reviews: &[RawReview]) -> Option<String> {
    reviews
        .iter()
        .filter_map(|review| review.submitted_at.as_ref())
        .max()
        .cloned()
}

fn derive_pr_next_action(pr: &TriagePrItem) -> Option<String> {
    let checks = pr.signals.checks.as_deref();
    let review = pr.signals.review_decision.as_deref();
    let merge = pr.signals.merge_state.as_deref();

    if pr.draft && checks == Some("FAILURE") {
        return Some("draft_with_failing_checks".to_string());
    }
    if checks == Some("FAILURE") {
        return Some("checks_failed".to_string());
    }
    if review == Some("APPROVED") && is_dirty_merge_state(merge) {
        return Some("approved_but_dirty".to_string());
    }
    // GitHub reports mergeStateStatus=CLEAN with an empty statusCheckRollup during
    // the force-push/rebase window before CI registers on the new head SHA. A CLEAN
    // PR whose required checks have not reported yet is NOT mergeable — gating merge
    // automation on CLEAN alone would merge a commit whose CI has never run (#4872).
    // Surface this race explicitly so the silent neutral state never reads as ready.
    if merge == Some("CLEAN") && checks.is_none() {
        return Some("clean_but_checks_not_reported".to_string());
    }
    if review == Some("APPROVED") && merge == Some("CLEAN") && checks == Some("PENDING") {
        return Some("approved_but_pending_checks".to_string());
    }
    if review == Some("APPROVED") && merge == Some("CLEAN") && checks == Some("SUCCESS") {
        return Some("clean_and_ready".to_string());
    }
    if matches!(merge, Some("BEHIND" | "DIRTY")) {
        return Some("needs_rebase".to_string());
    }
    if review == Some("REVIEW_REQUIRED") {
        return Some("review_required".to_string());
    }
    if pr.stale {
        return Some("stale_pr".to_string());
    }
    None
}

fn is_dirty_merge_state(merge: Option<&str>) -> bool {
    matches!(
        merge,
        Some("BEHIND" | "BLOCKED" | "DIRTY" | "HAS_HOOKS" | "UNSTABLE")
    )
}

pub(super) fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub(super) fn summarize_checks(checks: &[Value]) -> Option<String> {
    if checks.is_empty() {
        return None;
    }
    let mut saw_pending = false;
    for check in checks {
        let conclusion = check.get("conclusion").and_then(Value::as_str);
        let status = check.get("status").and_then(Value::as_str);
        if matches!(
            conclusion,
            Some("FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED")
        ) {
            return Some("FAILURE".to_string());
        }
        if conclusion.is_none() && !matches!(status, Some("COMPLETED")) {
            saw_pending = true;
        }
    }
    Some(if saw_pending { "PENDING" } else { "SUCCESS" }.to_string())
}

fn summarize_check_failures(checks: &[Value]) -> Vec<TriageCheckFailure> {
    checks
        .iter()
        .filter(|check| {
            matches!(
                check.get("conclusion").and_then(Value::as_str),
                Some("FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED")
            )
        })
        .map(|check| TriageCheckFailure {
            workflow: string_field(check, &["workflowName", "workflow"]),
            name: string_field(check, &["name", "context"])
                .unwrap_or_else(|| "unknown check".to_string()),
            status: string_field(check, &["status"]),
            conclusion: string_field(check, &["conclusion"]),
            url: string_field(check, &["detailsUrl", "targetUrl", "url"]),
        })
        .collect()
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    })
}

fn is_stale(updated_at: Option<&str>, stale_cutoff: Option<DateTime<Utc>>) -> bool {
    let Some(cutoff) = stale_cutoff else {
        return false;
    };
    let Some(updated_at) = updated_at else {
        return false;
    };
    DateTime::parse_from_rfc3339(updated_at)
        .map(|dt| dt.with_timezone(&Utc) < cutoff)
        .unwrap_or(false)
}

fn build_actions(
    issues: Option<&TriageIssueBucket>,
    pull_requests: Option<&TriagePrBucket>,
    priority_labels: &[String],
) -> Vec<TriageAction> {
    let mut actions = Vec::new();
    if let Some(prs) = pull_requests {
        let mut action_counts = BTreeMap::<String, usize>::new();
        for pr in &prs.items {
            if let Some(next_action) = &pr.signals.next_action {
                *action_counts.entry(next_action.clone()).or_default() += 1;
            }
        }
        for &kind in PR_ACTION_PRIORITY {
            if let Some(count) = action_counts.get(kind) {
                actions.push(TriageAction {
                    kind: kind.to_string(),
                    severity: pr_action_severity(kind).to_string(),
                    label: pr_action_label(kind, *count),
                });
            }
        }
    }
    if let Some(issues) = issues {
        let urgent = issues
            .items
            .iter()
            .filter(|issue| issue.state == "OPEN")
            .filter(|issue| issue_has_priority_label(issue, priority_labels))
            .count();
        if urgent > 0 {
            actions.push(TriageAction {
                kind: "priority_issues".to_string(),
                severity: "high".to_string(),
                label: pluralize(urgent, "priority issue", "priority issues"),
            });
        }
        let untriaged = issues
            .items
            .iter()
            .filter(|issue| issue.state == "OPEN")
            .filter(|issue| issue.labels.is_empty() && issue.assignees.is_empty())
            .count();
        if untriaged > 0 {
            actions.push(TriageAction {
                kind: "untriaged_issues".to_string(),
                severity: "low".to_string(),
                label: pluralize(untriaged, "untriaged issue", "untriaged issues"),
            });
        }
        let stale = issues
            .items
            .iter()
            .filter(|issue| issue.state == "OPEN")
            .filter(|issue| issue.stale)
            .count();
        if stale > 0 {
            actions.push(TriageAction {
                kind: "stale_issues".to_string(),
                severity: "low".to_string(),
                label: pluralize(stale, "stale issue", "stale issues"),
            });
        }
    }
    actions
}

const DEFAULT_PRIORITY_LABELS: &[&str] = &["security", "P0", "P1", "bug"];

fn resolve_priority_labels(
    component_ref: &ComponentRef,
    global_priority_labels: Option<&Vec<String>>,
) -> Vec<String> {
    component_ref
        .priority_labels
        .as_ref()
        .or(global_priority_labels)
        .cloned()
        .unwrap_or_else(|| {
            DEFAULT_PRIORITY_LABELS
                .iter()
                .map(|label| label.to_string())
                .collect()
        })
}

fn issue_has_priority_label(issue: &TriageIssueItem, priority_labels: &[String]) -> bool {
    issue
        .labels
        .iter()
        .any(|label| priority_labels.iter().any(|priority| priority == label))
}

const PR_ACTION_PRIORITY: &[&str] = &[
    "draft_with_failing_checks",
    "checks_failed",
    "approved_but_dirty",
    "needs_rebase",
    "review_required",
    "clean_but_checks_not_reported",
    "approved_but_pending_checks",
    "clean_and_ready",
    "stale_pr",
];

fn pr_action_severity(kind: &str) -> &'static str {
    match kind {
        "draft_with_failing_checks" | "checks_failed" | "approved_but_dirty" => "high",
        "needs_rebase"
        | "review_required"
        | "clean_but_checks_not_reported"
        | "approved_but_pending_checks"
        | "clean_and_ready" => "medium",
        _ => "low",
    }
}

fn pr_action_label(kind: &str, count: usize) -> String {
    match kind {
        "draft_with_failing_checks" => pluralize(
            count,
            "draft PR has failing checks",
            "draft PRs have failing checks",
        ),
        "checks_failed" => pluralize(count, "PR has failed checks", "PRs have failed checks"),
        "approved_but_dirty" => pluralize(count, "approved PR is dirty", "approved PRs are dirty"),
        "needs_rebase" => pluralize(count, "PR needs rebase", "PRs need rebase"),
        "review_required" => pluralize(count, "PR needs review", "PRs need review"),
        "clean_but_checks_not_reported" => pluralize(
            count,
            "PR is CLEAN but checks have not reported yet",
            "PRs are CLEAN but checks have not reported yet",
        ),
        "approved_but_pending_checks" => pluralize(
            count,
            "approved PR is waiting on checks",
            "approved PRs are waiting on checks",
        ),
        "clean_and_ready" => pluralize(count, "PR is clean and ready", "PRs are clean and ready"),
        "stale_pr" => pluralize(count, "stale PR", "stale PRs"),
        _ => pluralize(count, "PR needs action", "PRs need action"),
    }
}

fn pluralize(count: usize, singular: &str, plural: &str) -> String {
    format!("{} {}", count, if count == 1 { singular } else { plural })
}

fn summarize(
    components: &[TriageComponentReport],
    unresolved: &[TriageUnresolved],
) -> TriageSummary {
    let mut summary = TriageSummary {
        components: components.len() + unresolved.len(),
        repos_resolved: components.len(),
        repos_unresolved: unresolved.len(),
        ..Default::default()
    };
    for component in components {
        if let Some(issues) = &component.issues {
            summary.open_issues += issues.open;
            summary.stale += issues
                .items
                .iter()
                .filter(|item| item.state == "OPEN")
                .filter(|item| item.stale)
                .count();
        }
        if let Some(prs) = &component.pull_requests {
            summary.open_prs += prs.open;
            summary.needs_review += prs
                .items
                .iter()
                .filter(|item| item.signals.review_decision.as_deref() == Some("REVIEW_REQUIRED"))
                .count();
            summary.failing_checks += prs
                .items
                .iter()
                .filter(|item| item.signals.checks.as_deref() == Some("FAILURE"))
                .count();
            summary.stale += prs.items.iter().filter(|item| item.stale).count();
        }
        summary.actions += component.actions.len();
    }
    summary
}

fn summarize_unresolved(unresolved: &[TriageUnresolved]) -> Option<String> {
    if unresolved.is_empty() {
        return None;
    }

    let mut summary = format!("{} unresolved component target(s):", unresolved.len());
    for target in unresolved {
        let path = if target.local_path.is_empty() {
            "<no local_path>"
        } else {
            target.local_path.as_str()
        };
        summary.push_str(&format!(
            " {} ({}) - {};",
            target.component_id, path, target.reason
        ));
    }
    Some(summary)
}

pub fn parse_stale_days(input: &str) -> Result<i64> {
    let trimmed = input.trim();
    let digits = trimmed.strip_suffix('d').unwrap_or(trimmed);
    let days: i64 = digits.parse().map_err(|_| {
        Error::validation_invalid_argument(
            "stale",
            "Expected stale duration as days, e.g. 14d or 14",
            Some(input.to_string()),
            None,
        )
    })?;
    if days <= 0 {
        return Err(Error::validation_invalid_argument(
            "stale",
            "Stale duration must be greater than zero days",
            Some(input.to_string()),
            None,
        ));
    }
    Ok(days)
}

pub fn parse_issue_numbers_file(path: &Path) -> Result<Vec<u64>> {
    let content = fs::read_to_string(path).map_err(|e| {
        Error::validation_invalid_argument(
            "issues-from-file",
            format!("Failed to read issue list: {e}"),
            Some(path.display().to_string()),
            None,
        )
    })?;
    parse_issue_numbers(&content)
}

fn parse_issue_numbers(input: &str) -> Result<Vec<u64>> {
    let mut numbers = Vec::new();
    for (index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(value) = parse_issue_number_line(trimmed) else {
            continue;
        };
        let number: u64 = value.parse().map_err(|_| {
            Error::validation_invalid_argument(
                "issues-from-file",
                format!("Expected issue number on line {}", index + 1),
                Some(trimmed.to_string()),
                None,
            )
        })?;
        numbers.push(number);
    }
    Ok(numbers)
}

fn parse_issue_number_line(line: &str) -> Option<&str> {
    if let Some(value) = line.strip_prefix('#') {
        return value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
            .then_some(value);
    }
    Some(line)
}

#[cfg(test)]
mod tests;
