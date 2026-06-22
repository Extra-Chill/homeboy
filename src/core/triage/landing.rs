//! `triage --landing`: resolve landing-candidate PRs across a scope and classify
//! their mergeability/check state into an actionable dashboard.

use std::collections::BTreeSet;

use crate::core::deploy::release_download::{parse_github_url, GitHubRepo};
use crate::core::error::{Error, Result};
use crate::core::observation::TriagePullRequestSignals;
use crate::core::scope::ScopeOutput;

use super::gh::{ensure_gh_ready, non_empty, run_gh, summarize_checks};
use super::observation::usize_to_i64;
use super::report::{fetch_linked_prs, resolve_repo, resolve_target_components};
use super::shared::{
    latest_comment_at, latest_review_at, RawNamedNode, RawPr, RawPrHeadRepository,
};
use super::types::{
    TriageLandingCheckState, TriageLandingClassification, TriageLandingMergeabilityState,
    TriageLandingOptions, TriageLandingOutput, TriageLandingPr, TriageLandingRebasePlan,
    TriageLandingSummary, TriageUnresolved,
};

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

    if options.ordered {
        dedupe_landing_prs_preserving_order(&mut pull_requests);
        annotate_ordered_dependent_rebases(&mut pull_requests);
    } else {
        pull_requests.sort_by(|a, b| a.repo.cmp(&b.repo).then(a.number.cmp(&b.number)));
        pull_requests.dedup_by(|a, b| a.repo == b.repo && a.number == b.number);
    }
    let summary = summarize_landing(&pull_requests);

    Ok(TriageLandingOutput {
        command: "triage.landing",
        target: ScopeOutput::from(&target),
        summary,
        pull_requests,
        unresolved,
    })
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
pub(super) struct LandingPrRef {
    pub(super) owner: String,
    pub(super) repo: String,
    pub(super) number: u64,
}

pub(super) fn parse_landing_pr_ref(
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

pub(super) fn is_bare_pr_number(raw: &str) -> bool {
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
    "number,title,url,state,isDraft,reviewDecision,mergeStateStatus,statusCheckRollup,baseRefName,headRefName,headRepository,headRepositoryOwner,comments,reviews,updatedAt,mergedAt"
}

fn effective_landing_limit(options: &TriageLandingOptions) -> usize {
    if options.limit == 0 {
        30
    } else {
        options.limit
    }
}

pub(super) fn branch_matches(pattern: &str, branch: &str) -> bool {
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

pub(super) fn parse_landing_prs(
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

pub(super) fn parse_landing_pr(
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
        super::report::summarize_check_failures(&item.status_check_rollup)
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
        base_branch: non_empty(item.base_ref_name),
        head_branch: non_empty(item.head_ref_name),
        mergeability_state: landing_mergeability_state(signals.merge_state.as_deref()),
        check_state: landing_check_state(signals.checks.as_deref()),
        head_repo: raw_head_repo_name_with_owner(item.head_repository, item.head_repository_owner),
        classification,
        suggested_next_command: landing_next_command(classification, repo, item.number),
        dependent_rebase: None,
        signals,
        check_failures,
    }
}

pub(super) fn landing_mergeability_state(
    merge_state: Option<&str>,
) -> TriageLandingMergeabilityState {
    match merge_state {
        Some("CLEAN") => TriageLandingMergeabilityState::Clean,
        Some("DIRTY" | "BEHIND" | "BLOCKED" | "HAS_HOOKS") => {
            TriageLandingMergeabilityState::Conflicting
        }
        Some("UNKNOWN") | None => TriageLandingMergeabilityState::Unknown,
        Some("UNSTABLE") => TriageLandingMergeabilityState::Unstable,
        Some(_) => TriageLandingMergeabilityState::Other,
    }
}

pub(super) fn landing_check_state(checks: Option<&str>) -> TriageLandingCheckState {
    match checks {
        Some("SUCCESS") => TriageLandingCheckState::Clean,
        Some("PENDING") => TriageLandingCheckState::Pending,
        Some("FAILURE") => TriageLandingCheckState::Failed,
        None => TriageLandingCheckState::Unknown,
        Some(_) => TriageLandingCheckState::Other,
    }
}

fn raw_head_repo_name_with_owner(
    repo: Option<RawPrHeadRepository>,
    owner: Option<RawNamedNode>,
) -> Option<String> {
    let repo = repo?;
    if let Some(name_with_owner) = non_empty(repo.name_with_owner) {
        return Some(name_with_owner);
    }
    let owner = owner.and_then(|owner| owner.login)?;
    let name = non_empty(repo.name)?;
    Some(format!("{owner}/{name}"))
}

pub(super) fn dedupe_landing_prs_preserving_order(items: &mut Vec<TriageLandingPr>) {
    let mut seen = BTreeSet::new();
    items.retain(|item| seen.insert((item.repo.clone(), item.number)));
}

pub(super) fn annotate_ordered_dependent_rebases(items: &mut [TriageLandingPr]) {
    for index in 1..items.len() {
        let previous = items[index - 1].number;
        let plan = dependent_rebase_plan(&items[index], previous);
        items[index].dependent_rebase = Some(plan);
    }
}

pub(super) fn dependent_rebase_plan(
    item: &TriageLandingPr,
    after_pr: u64,
) -> TriageLandingRebasePlan {
    let Some(head_branch) = item.head_branch.as_deref() else {
        return TriageLandingRebasePlan {
            after_pr,
            safe_to_update: false,
            reason: "missing_head_branch".to_string(),
            command: None,
        };
    };
    let Some(base_branch) = item.base_branch.as_deref() else {
        return TriageLandingRebasePlan {
            after_pr,
            safe_to_update: false,
            reason: "missing_base_branch".to_string(),
            command: None,
        };
    };
    if item.head_repo.as_deref() != Some(item.repo.as_str()) {
        return TriageLandingRebasePlan {
            after_pr,
            safe_to_update: false,
            reason: "head_branch_not_in_base_repo".to_string(),
            command: None,
        };
    }

    TriageLandingRebasePlan {
        after_pr,
        safe_to_update: true,
        reason: "same_repo_head_branch".to_string(),
        command: Some(format!(
            "gh pr checkout {} -R {} && git fetch origin {} && git rebase origin/{} && git push --force-with-lease origin HEAD:{}",
            item.number, item.repo, base_branch, base_branch, head_branch
        )),
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
        match item.mergeability_state {
            TriageLandingMergeabilityState::Clean => summary.mergeability_clean += 1,
            TriageLandingMergeabilityState::Conflicting => summary.mergeability_conflicting += 1,
            TriageLandingMergeabilityState::Unknown => summary.mergeability_unknown += 1,
            TriageLandingMergeabilityState::Unstable => summary.mergeability_unstable += 1,
            TriageLandingMergeabilityState::Other => {}
        }
        match item.check_state {
            TriageLandingCheckState::Clean => summary.checks_clean += 1,
            TriageLandingCheckState::Pending => summary.checks_pending += 1,
            TriageLandingCheckState::Failed => summary.checks_failed += 1,
            TriageLandingCheckState::Unknown => summary.checks_unknown += 1,
            TriageLandingCheckState::Other => {}
        }
        match item.classification {
            TriageLandingClassification::Merged => summary.merged += 1,
            TriageLandingClassification::CleanMergeable => summary.clean_mergeable += 1,
            TriageLandingClassification::ConflictRepairNeeded => {
                summary.conflict_repair_needed += 1
            }
            TriageLandingClassification::ChecksPending => {}
            TriageLandingClassification::BaselineRedInconclusive => {
                summary.baseline_red_inconclusive += 1
            }
            TriageLandingClassification::CandidateRed => summary.candidate_red += 1,
            TriageLandingClassification::Unknown => summary.unknown += 1,
        }
    }
    summary
}
