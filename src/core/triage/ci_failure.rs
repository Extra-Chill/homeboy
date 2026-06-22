//! `triage --ci-failure`: fetch failed CI check runs for a PR and digest their logs.

use regex::Regex;
use serde::Deserialize;

use crate::core::deploy::release_download::GitHubRepo;
use crate::core::error::{Error, Result};
use crate::core::git::GhClient;

use super::types::{
    CiFailureDigest, CiFailureSnippet, CiFailureSummary, CiFailureTriageOptions,
    CiFailureTriageOutput,
};

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
pub(super) struct ParsedPrTarget {
    pub(super) repo: GitHubRepo,
    pub(super) number: u64,
}

pub(super) fn parse_pr_target(target: &str, repo: Option<&str>) -> Result<ParsedPrTarget> {
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

pub(super) fn parse_repo_arg(repo: &str) -> Result<GitHubRepo> {
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

pub(super) fn parse_pr_url(url: &str) -> Result<ParsedPrTarget> {
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

pub(super) fn extract_actions_job_id(url: &str) -> Option<u64> {
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

pub(super) fn extract_failure_snippets(log: &str, context_lines: usize) -> Vec<CiFailureSnippet> {
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
        if snippets
            .last()
            .is_some_and(|snippet: &CiFailureSnippet| start < snippet.line_end)
        {
            continue;
        }
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

pub(super) fn classify_failure(haystack: &[&str]) -> String {
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

pub(super) fn detect_baseline_vs_head(haystack: &[&str]) -> Option<String> {
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
