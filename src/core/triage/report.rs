//! `triage` report: resolve a scope to component repos and overlay GitHub
//! issue/PR state, with priority-action rollups and observation tracking.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::core::defaults;
use crate::core::deploy::release_download::{detect_remote_url, parse_github_url, GitHubRepo};
use crate::core::error::{Error, Result};
use crate::core::observation::TriagePullRequestSignals;
use crate::core::scope::{self, Scope, ScopeKind, ScopeOutput};

use super::gh::{ensure_gh_ready, non_empty, run_gh, summarize_checks};
use super::observation::usize_to_i64;
use super::observation::TriageObservation;
use super::shared::{
    bool_field, is_stale, latest_comment_at, latest_review_at, pluralize, string_field, RawComment,
    RawNamedNode,
};
use super::types::{
    ComponentRef, TriageAction, TriageCheckFailure, TriageCiCheckStateCounts, TriageCiReadiness,
    TriageCiReadinessBuckets, TriageComponentReport, TriageIssueBucket, TriageIssueItem,
    TriageLinkedPr, TriageOptions, TriageOutput, TriagePrBucket, TriagePrItem, TriageRepo,
    TriageRepoRef, TriageSummary, TriageUnresolved,
};

pub fn run(target: super::TriageTarget, options: TriageOptions) -> Result<TriageOutput> {
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

pub(super) fn resolve_target_components(target: &super::TriageTarget) -> Result<Vec<ComponentRef>> {
    let refs = scope::resolve_scope_components(target)?;
    if matches!(target, Scope::Workspace) {
        Ok(dedupe_refs_by_repo(refs))
    } else {
        Ok(refs)
    }
}

pub(crate) fn triage_command(target: &super::TriageTarget) -> String {
    // `--path` is an escape hatch on subcommands (currently `component`); keep
    // the same command identity so JSON consumers don't see a phantom
    // `triage.path` verb.
    target.command_name("triage", ScopeKind::Component)
}

pub(super) fn dedupe_refs_by_repo(component_refs: Vec<ComponentRef>) -> Vec<ComponentRef> {
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
pub(super) struct ResolvedRepo {
    pub(super) repo: GitHubRepo,
    pub(super) triage_remote_url: Option<String>,
    pub(super) source_repo: Option<GitHubRepo>,
}

pub(super) fn resolve_repo(
    component_ref: &ComponentRef,
) -> std::result::Result<ResolvedRepo, String> {
    resolve_repo_with_parent_resolver(component_ref, github_parent_repo)
}

pub(super) fn resolve_repo_with_parent_resolver(
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

pub(super) fn parse_github_parent_repo(
    raw: &str,
) -> std::result::Result<Option<GitHubRepo>, String> {
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

pub(super) fn fetch_component_report(
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

pub(super) fn issue_bucket(items: Vec<TriageIssueItem>) -> TriageIssueBucket {
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

fn effective_limit(options: &TriageOptions) -> usize {
    if options.limit == 0 {
        30
    } else {
        options.limit
    }
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

pub(super) fn parse_issues(
    raw: &str,
    stale_cutoff: Option<DateTime<Utc>>,
) -> std::result::Result<Vec<TriageIssueItem>, String> {
    let parsed: Vec<RawIssue> = serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
    Ok(parsed
        .into_iter()
        .map(|item| raw_issue_to_item(item, stale_cutoff))
        .collect())
}

pub(super) fn parse_issue(
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

pub(super) fn fetch_linked_prs(
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

pub(super) fn parse_linked_prs(raw: &str) -> std::result::Result<Vec<TriageLinkedPr>, String> {
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

pub(super) fn parse_prs(
    raw: &str,
    stale_cutoff: Option<DateTime<Utc>>,
    include_drilldown: bool,
) -> std::result::Result<Vec<TriagePrItem>, String> {
    let parsed: Vec<super::shared::RawPr> =
        serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
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
                ci_readiness: summarize_ci_readiness(&item.status_check_rollup, Utc::now()),
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
    if pr
        .ci_readiness
        .as_ref()
        .is_some_and(TriageCiReadiness::has_pending_required_checks)
    {
        return Some("required_checks_pending".to_string());
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

pub(super) fn summarize_check_failures(checks: &[Value]) -> Vec<TriageCheckFailure> {
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

pub(super) fn summarize_ci_readiness(
    checks: &[Value],
    now: DateTime<Utc>,
) -> Option<TriageCiReadiness> {
    if checks.is_empty() {
        return None;
    }

    let mut buckets = TriageCiReadinessBuckets::default();
    let mut failure_urls = Vec::new();
    let mut oldest_pending_started_at: Option<DateTime<Utc>> = None;

    for check in checks {
        let state = classify_check_state(check);
        let counts = match bool_field(check, &["required", "isRequired"]) {
            Some(true) => &mut buckets.required,
            Some(false) => &mut buckets.optional,
            None => &mut buckets.unknown_requirement,
        };
        counts.increment(state);

        if state == TriageCiCheckState::Failed {
            if let Some(url) = string_field(check, &["detailsUrl", "targetUrl", "url"]) {
                failure_urls.push(url);
            }
        }

        if matches!(
            state,
            TriageCiCheckState::Queued | TriageCiCheckState::Pending | TriageCiCheckState::Running
        ) {
            if let Some(started_at) = check_started_at(check) {
                oldest_pending_started_at = Some(match oldest_pending_started_at {
                    Some(existing) => existing.min(started_at),
                    None => started_at,
                });
            }
        }
    }

    failure_urls.sort();
    failure_urls.dedup();

    let oldest_pending_duration_seconds = oldest_pending_started_at
        .map(|started_at| now.signed_duration_since(started_at).num_seconds().max(0));
    let oldest_pending_started_at = oldest_pending_started_at.map(|dt| dt.to_rfc3339());
    let next_steps = ci_readiness_next_steps(&buckets, oldest_pending_duration_seconds);

    Some(TriageCiReadiness {
        checks: buckets,
        oldest_pending_started_at,
        oldest_pending_duration_seconds,
        failure_urls,
        next_steps,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriageCiCheckState {
    Queued,
    Pending,
    Running,
    Failed,
    Skipped,
    Passed,
}

impl TriageCiCheckStateCounts {
    fn increment(&mut self, state: TriageCiCheckState) {
        match state {
            TriageCiCheckState::Queued => self.queued += 1,
            TriageCiCheckState::Pending => self.pending += 1,
            TriageCiCheckState::Running => self.running += 1,
            TriageCiCheckState::Failed => self.failed += 1,
            TriageCiCheckState::Skipped => self.skipped += 1,
            TriageCiCheckState::Passed => self.passed += 1,
        }
    }

    fn active(&self) -> usize {
        self.queued + self.pending + self.running
    }
}

impl TriageCiReadiness {
    fn has_pending_required_checks(&self) -> bool {
        self.checks.required.active() > 0
    }
}

fn classify_check_state(check: &Value) -> TriageCiCheckState {
    let conclusion = check.get("conclusion").and_then(Value::as_str);
    if matches!(
        conclusion,
        Some("FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED")
    ) {
        return TriageCiCheckState::Failed;
    }
    if matches!(conclusion, Some("SKIPPED" | "NEUTRAL")) {
        return TriageCiCheckState::Skipped;
    }
    if matches!(conclusion, Some("SUCCESS")) {
        return TriageCiCheckState::Passed;
    }

    match check.get("status").and_then(Value::as_str) {
        Some("QUEUED" | "REQUESTED" | "WAITING") => TriageCiCheckState::Queued,
        Some("IN_PROGRESS") => TriageCiCheckState::Running,
        Some("COMPLETED") => TriageCiCheckState::Passed,
        _ => TriageCiCheckState::Pending,
    }
}

fn check_started_at(check: &Value) -> Option<DateTime<Utc>> {
    string_field(check, &["startedAt", "createdAt", "updatedAt"])
        .and_then(|raw| DateTime::parse_from_rfc3339(&raw).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

fn ci_readiness_next_steps(
    buckets: &TriageCiReadinessBuckets,
    oldest_pending_duration_seconds: Option<i64>,
) -> Vec<String> {
    let mut steps = Vec::new();
    if buckets.required.failed > 0 {
        steps.push("Fix or rerun failing required checks before merge.".to_string());
    }
    if buckets.optional.failed > 0 || buckets.unknown_requirement.failed > 0 {
        steps.push(
            "Review failing optional/unknown checks and decide whether they block this PR."
                .to_string(),
        );
    }
    if buckets.required.active() > 0 {
        let label = oldest_pending_duration_seconds
            .map(format_duration)
            .map(|duration| format!("Wait for required checks to finish; oldest pending check has been active for {duration}."))
            .unwrap_or_else(|| "Wait for required checks to finish.".to_string());
        steps.push(label);
    } else if buckets.optional.active() > 0 || buckets.unknown_requirement.active() > 0 {
        steps.push(
            "Required checks are not pending; monitor optional/unknown checks for signal."
                .to_string(),
        );
    }
    if buckets.required.failed == 0 && buckets.required.active() == 0 {
        steps.push("Required checks are not blocking merge.".to_string());
    }
    steps
}

fn format_duration(seconds: i64) -> String {
    if seconds >= 3600 {
        format!("{}h", seconds / 3600)
    } else if seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}s", seconds)
    }
}

pub(super) fn build_actions(
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

pub(super) const DEFAULT_PRIORITY_LABELS: &[&str] = &["security", "P0", "P1", "bug"];

pub(super) fn resolve_priority_labels(
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
    "required_checks_pending",
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
        | "required_checks_pending"
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
        "required_checks_pending" => pluralize(
            count,
            "PR is waiting on required checks",
            "PRs are waiting on required checks",
        ),
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

pub(super) fn summarize(
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

pub(super) fn summarize_unresolved(unresolved: &[TriageUnresolved]) -> Option<String> {
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

pub(super) fn parse_issue_numbers(input: &str) -> Result<Vec<u64>> {
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
