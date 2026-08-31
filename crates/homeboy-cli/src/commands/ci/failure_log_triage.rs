//! Concise GitHub Actions failure triage for PRs.
//!
//! This module intentionally returns bounded snippets and structured step/job
//! metadata instead of raw Actions logs. Callers can render the `human_summary`
//! field for operators while still keeping the machine-readable fields.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Instant;

use super::external_check_detail_resolver::{
    normalize_target_url, skipped_for_budget, HydratedDetail, ResolverDiagnostic, ResolverSession,
    MAX_RESOLVERS, TOTAL_BUDGET,
};
use homeboy::core::error::{Error, Result};
use homeboy::core::git::GhClient;

const DEFAULT_MAX_RUNS: usize = 5;
const DEFAULT_MAX_SNIPPETS_PER_JOB: usize = 4;
const DEFAULT_CONTEXT_LINES: usize = 2;
const MAX_SNIPPET_LINE_CHARS: usize = 220;
const MAX_GH_ERROR_CHARS: usize = 512;

mod types {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CiFailureTriageRequest {
        pub reference: String,
        pub repo: Option<String>,
        pub max_runs: usize,
        pub max_snippets_per_job: usize,
        pub context_lines: usize,
    }

    impl CiFailureTriageRequest {
        pub(crate) fn effective_max_runs(&self) -> usize {
            if self.max_runs == 0 {
                DEFAULT_MAX_RUNS
            } else {
                self.max_runs
            }
        }

        pub(crate) fn effective_max_snippets_per_job(&self) -> usize {
            if self.max_snippets_per_job == 0 {
                DEFAULT_MAX_SNIPPETS_PER_JOB
            } else {
                self.max_snippets_per_job
            }
        }

        pub(crate) fn effective_context_lines(&self) -> usize {
            if self.context_lines == 0 {
                DEFAULT_CONTEXT_LINES
            } else {
                self.context_lines
            }
        }
    }

    #[derive(Debug, Serialize, Clone, PartialEq, Eq)]
    pub struct CiFailureTriageOutput {
        pub command: &'static str,
        pub repo: String,
        pub pr_number: u64,
        pub pr_url: String,
        pub head_sha: String,
        pub total_failed_runs: usize,
        pub runs_inspected: usize,
        pub baseline_failures: usize,
        pub pr_head_failures: usize,
        pub suspected_categories: Vec<CiFailureCategory>,
        pub human_summary: String,
        pub next_actions: Vec<String>,
        pub runs: Vec<CiFailedRunSummary>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub external_checks: Vec<CiExternalCheckSummary>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub api_degradations: Vec<CiApiDegradation>,
    }

    #[derive(Debug, Serialize, Clone, PartialEq, Eq)]
    pub struct CiFailedRunSummary {
        pub run_id: u64,
        pub run_attempt: Option<u64>,
        pub workflow: String,
        pub status: Option<String>,
        pub conclusion: Option<String>,
        pub branch: Option<String>,
        pub event: Option<String>,
        pub started_at: Option<String>,
        pub html_url: Option<String>,
        pub suspected_category: CiFailureCategory,
        pub failure_origin: CiFailureOrigin,
        pub jobs: Vec<CiFailedJobSummary>,
    }

    #[derive(Debug, Serialize, Clone, PartialEq, Eq)]
    pub struct CiFailedJobSummary {
        pub job_id: u64,
        pub name: String,
        pub status: Option<String>,
        pub conclusion: Option<String>,
        pub html_url: Option<String>,
        pub failed_steps: Vec<String>,
        pub snippets: Vec<CiLogSnippet>,
        pub suspected_category: CiFailureCategory,
        pub touched_files: Vec<String>,
        pub next_action_hint: String,
    }

    #[derive(Debug, Serialize, Clone, PartialEq, Eq)]
    pub struct CiLogSnippet {
        pub line: usize,
        pub text: String,
    }

    #[derive(Debug, Serialize, Clone, PartialEq, Eq)]
    pub struct CiExternalCheckSummary {
        pub provider: String,
        pub status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub target_url: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub details: Vec<HydratedDetail>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub resolver_diagnostics: Vec<ResolverDiagnostic>,
    }

    #[derive(Debug, Serialize, Clone, PartialEq, Eq)]
    pub struct CiApiDegradation {
        pub api: String,
        pub message: String,
    }

    #[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    #[serde(rename_all = "snake_case")]
    pub enum CiFailureCategory {
        Fmt,
        Clippy,
        Compile,
        UnitTest,
        BaselineDifferential,
        InfraTimeout,
        Queueing,
        Unknown,
    }

    #[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum CiFailureOrigin {
        Baseline,
        PrHead,
        Unknown,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ParsedCiPrReference {
        pub repo: Option<String>,
        pub number: u64,
    }
}

mod engine {
    use super::*;

    pub(crate) fn triage_pr_failures(
        request: CiFailureTriageRequest,
    ) -> Result<CiFailureTriageOutput> {
        let parsed = parse_pr_reference(&request.reference)?;
        let repo = request.repo.clone().or(parsed.repo).ok_or_else(|| {
            Error::validation_missing_argument(vec!["--repo <owner/repo>".to_string()])
        })?;
        let gh = GhClient::from_repo_arg(&repo)?;
        gh.ensure_ready()?;
        let repo = gh.repo_path()?.to_string();
        let pr = fetch_pr(&gh, parsed.number)?;
        let runs = fetch_runs_for_head(&gh, &pr.head_sha)?;
        // One fixed deadline is shared by every resolver invocation in this triage.
        let resolver_deadline = Instant::now() + TOTAL_BUDGET;
        let resolver_session = ResolverSession::discover();
        let (external_checks, api_degradations) =
            fetch_external_checks(&gh, &pr.head_sha, &resolver_session, resolver_deadline);

        let failed_runs: Vec<GhWorkflowRun> = runs.into_iter().filter(is_failed_run).collect();
        let total_failed_runs = failed_runs.len();
        let runs_to_inspect: Vec<GhWorkflowRun> = failed_runs
            .into_iter()
            .take(request.effective_max_runs())
            .collect();
        let mut summaries = Vec::new();

        for run in runs_to_inspect {
            let jobs = fetch_run_jobs(&gh, run.id, run.run_attempt)?;
            let failed_jobs: Vec<GhJob> = jobs.into_iter().filter(is_failed_job).collect();
            let mut job_summaries = Vec::new();
            for job in failed_jobs {
                let log = gh.api_bytes(&format!("repos/{repo}/actions/jobs/{}/logs", job.id));
                let log_text = log
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .unwrap_or_default();
                let failed_steps = failed_step_names(&job);
                let snippets = extract_relevant_snippets(
                    &log_text,
                    &failed_steps,
                    request.effective_max_snippets_per_job(),
                    request.effective_context_lines(),
                );
                let evidence = format!(
                    "{}\n{}\n{}\n{}",
                    run.name,
                    job.name,
                    failed_steps.join("\n"),
                    snippets
                        .iter()
                        .map(|snippet| snippet.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                let category = classify_failure(&evidence);
                let touched_files = detect_touched_files(&evidence);
                job_summaries.push(CiFailedJobSummary {
                    job_id: job.id,
                    name: job.name,
                    status: job.status,
                    conclusion: job.conclusion,
                    html_url: job.html_url,
                    failed_steps,
                    snippets,
                    suspected_category: category,
                    touched_files,
                    next_action_hint: next_action_hint(category),
                });
            }

            let run_evidence = format!(
                "{}\n{}\n{}\n{}",
                run.name,
                run.event.as_deref().unwrap_or_default(),
                run.head_branch.as_deref().unwrap_or_default(),
                job_summaries
                    .iter()
                    .map(|job| format!("{} {:?}", job.name, job.suspected_category))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let run_category = classify_failure(&run_evidence);
            summaries.push(CiFailedRunSummary {
                run_id: run.id,
                run_attempt: run.run_attempt,
                workflow: run.name,
                status: run.status,
                conclusion: run.conclusion,
                branch: run.head_branch,
                event: run.event,
                started_at: run.run_started_at.or(run.created_at),
                html_url: run.html_url,
                suspected_category: run_category,
                failure_origin: classify_origin(&run_evidence),
                jobs: job_summaries,
            });
        }

        let suspected_categories = collect_categories(&summaries);
        let baseline_failures = summaries
            .iter()
            .filter(|run| run.failure_origin == CiFailureOrigin::Baseline)
            .count();
        let pr_head_failures = summaries
            .iter()
            .filter(|run| run.failure_origin == CiFailureOrigin::PrHead)
            .count();
        let mut next_actions = build_next_actions(&repo, pr.number, &summaries);
        if !api_degradations.is_empty() {
            next_actions.push("Retry CI triage after the degraded provider API is available; retained evidence may be incomplete.".to_string());
        }
        let human_summary = render_human_summary(
            &repo,
            &pr,
            total_failed_runs,
            &summaries,
            &external_checks,
            &api_degradations,
            &next_actions,
        );
        let runs_inspected = summaries.len();

        Ok(CiFailureTriageOutput {
            command: "ci.triage",
            repo,
            pr_number: pr.number,
            pr_url: pr.url,
            head_sha: pr.head_sha,
            total_failed_runs,
            runs_inspected,
            baseline_failures,
            pr_head_failures,
            suspected_categories,
            human_summary,
            next_actions,
            runs: summaries,
            external_checks,
            api_degradations,
        })
    }

    pub(crate) fn parse_pr_reference(raw: &str) -> Result<ParsedCiPrReference> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(Error::validation_invalid_argument(
                "reference",
                "expected PR number, owner/repo#number, or GitHub pull request URL",
                Some(raw.to_string()),
                None,
            ));
        }

        if let Some((repo, number)) = trimmed.rsplit_once('#') {
            let number = parse_pr_number(number, raw)?;
            let repo = repo.trim().trim_start_matches("https://github.com/");
            return Ok(ParsedCiPrReference {
                repo: Some(repo.trim_matches('/').to_string()),
                number,
            });
        }

        if let Some((repo, number)) = parse_pull_url(trimmed) {
            return Ok(ParsedCiPrReference {
                repo: Some(repo),
                number: parse_pr_number(number, raw)?,
            });
        }

        Ok(ParsedCiPrReference {
            repo: None,
            number: parse_pr_number(trimmed, raw)?,
        })
    }

    pub(crate) fn parse_pull_url(raw: &str) -> Option<(String, &str)> {
        let marker = "/pull/";
        let (repo_part, number_part) = raw.split_once(marker)?;
        let repo_part = repo_part
            .trim_end_matches('/')
            .strip_prefix("https://")
            .or_else(|| repo_part.trim_end_matches('/').strip_prefix("http://"))?;
        let mut parts = repo_part.split('/');
        let host = parts.next()?;
        let owner = parts.next()?;
        let name = parts.next()?;
        let repo = if host == "github.com" {
            format!("{owner}/{name}")
        } else {
            format!("{host}/{owner}/{name}")
        };
        Some((repo, number_part.trim_matches('/')))
    }

    pub(crate) fn parse_pr_number(raw: &str, original: &str) -> Result<u64> {
        raw.trim().parse::<u64>().map_err(|_| {
            Error::validation_invalid_argument(
                "reference",
                "PR number must be an integer",
                Some(original.to_string()),
                None,
            )
        })
    }

    pub(crate) fn fetch_pr(gh: &GhClient, number: u64) -> Result<GhPullRequest> {
        let repo = gh.repo_path()?;
        let host = gh.host();
        let raw: GhPullRequestRaw = gh.api_json(&format!("repos/{repo}/pulls/{number}"))?;
        Ok(GhPullRequest {
            number: raw.number,
            title: raw.title.unwrap_or_else(|| format!("PR #{}", raw.number)),
            url: raw
                .html_url
                .unwrap_or_else(|| format!("https://{host}/{repo}/pull/{number}")),
            head_sha: raw.head.sha,
        })
    }

    pub(crate) fn fetch_runs_for_head(gh: &GhClient, head_sha: &str) -> Result<Vec<GhWorkflowRun>> {
        let repo = gh.repo_path()?;
        let raw: GhWorkflowRunsResponse = gh.api_json(&format!(
            "repos/{repo}/actions/runs?head_sha={head_sha}&per_page=50"
        ))?;
        Ok(raw.workflow_runs)
    }

    const MAX_EXTERNAL_STATUS_PAGES: usize = 2;
    const MAX_CHECK_RUN_PAGES: usize = 4;

    pub(crate) fn fetch_external_checks(
        gh: &GhClient,
        head_sha: &str,
        resolver_session: &ResolverSession,
        resolver_deadline: Instant,
    ) -> (Vec<CiExternalCheckSummary>, Vec<CiApiDegradation>) {
        let Ok(repo) = gh.repo_path() else {
            return (
                Vec::new(),
                vec![CiApiDegradation {
                    api: "check discovery".into(),
                    message: "Repository resolution failed.".into(),
                }],
            );
        };
        let mut checks = Vec::new();
        let mut degradations = Vec::new();
        let mut resolver_slots = MAX_RESOLVERS;
        let mut latest_status_contexts = HashSet::new();
        let mut seen_checks = HashSet::new();
        for page in 1..=MAX_EXTERNAL_STATUS_PAGES {
            if Instant::now() >= resolver_deadline {
                degradations.push(deadline_degradation());
                break;
            }
            match gh.api_json_until::<GhStatusesResponse>(
                &format!("repos/{repo}/commits/{head_sha}/statuses?per_page=100&page={page}"),
                resolver_deadline,
            ) {
                Ok(response) => {
                    let complete = response.len() < 100;
                    for status in latest_failed_statuses(response, &mut latest_status_contexts) {
                        if !seen_checks.insert(check_identity(
                            &status.context,
                            status.target_url.as_deref(),
                            None,
                        )) {
                            continue;
                        }
                        checks.push(hydrate_external(
                            resolver_session,
                            status.context,
                            status.state.unwrap_or_default(),
                            status.description,
                            status.target_url,
                            &mut resolver_slots,
                            resolver_deadline,
                        ));
                    }
                    if complete {
                        break;
                    } else if page == MAX_EXTERNAL_STATUS_PAGES {
                        degradations.push(CiApiDegradation {
                            api: "legacy commit statuses".into(),
                            message: format!("Pagination truncated after {MAX_EXTERNAL_STATUS_PAGES} pages; older statuses were not inspected."),
                        });
                    }
                }
                Err(error) => {
                    degradations.push(CiApiDegradation {
                        api: "legacy commit statuses".into(),
                        message: gh_error_diagnostic(&error),
                    });
                    break;
                }
            }
        }
        for page in 1..=MAX_CHECK_RUN_PAGES {
            if Instant::now() >= resolver_deadline {
                degradations.push(deadline_degradation());
                break;
            }
            match gh.api_json_until::<GhCheckRunsResponse>(
                &format!(
                "repos/{repo}/commits/{head_sha}/check-runs?filter=latest&per_page=100&page={page}"
            ),
                resolver_deadline,
            ) {
                Ok(response) => {
                    for check in response.check_runs.into_iter().filter(is_failed_check_run) {
                        if !seen_checks.insert(check_identity(
                            &check.name,
                            check.details_url.as_deref(),
                            check.external_id.as_deref(),
                        )) {
                            continue;
                        }
                        checks.push(hydrate_external(
                            resolver_session,
                            check.name,
                            check.conclusion.unwrap_or_default(),
                            None,
                            check.details_url,
                            &mut resolver_slots,
                            resolver_deadline,
                        ));
                    }
                    if response.total_count <= page * 100 {
                        break;
                    } else if page == MAX_CHECK_RUN_PAGES {
                        degradations.push(CiApiDegradation {
                            api: "GitHub Check Runs".into(),
                            message: format!("Pagination truncated after {MAX_CHECK_RUN_PAGES} pages; older checks were not inspected."),
                        });
                    }
                }
                Err(error) => {
                    degradations.push(CiApiDegradation {
                        api: "GitHub Check Runs".into(),
                        message: gh_error_diagnostic(&error),
                    });
                    break;
                }
            }
        }
        (checks, degradations)
    }

    pub(crate) fn hydrate_external(
        resolver_session: &ResolverSession,
        provider: String,
        status: String,
        description: Option<String>,
        target_url: Option<String>,
        resolver_slots: &mut usize,
        resolver_deadline: Instant,
    ) -> CiExternalCheckSummary {
        let target_url = target_url.map(|url| normalize_target_url(&url));
        let (details, resolver_diagnostics) = if resolver_session.has_unique_provider(&provider)
            && *resolver_slots == 0
        {
            (Vec::new(), vec![skipped_for_budget(&provider)])
        } else {
            // Unknown and ambiguous providers are diagnostic-only and never
            // consume a bounded resolver invocation slot.
            if resolver_session.has_unique_provider(&provider) {
                *resolver_slots -= 1;
            }
            resolver_session.hydrate(&provider, &status, target_url.as_deref(), resolver_deadline)
        };
        CiExternalCheckSummary {
            provider,
            status: homeboy::core::redaction::RedactionPolicy::default()
                .redact_embedded_urls(&normalize_log_line(&status)),
            description: description.map(|description| {
                homeboy::core::redaction::RedactionPolicy::default()
                    .redact_embedded_urls(&normalize_log_line(&description))
            }),
            target_url,
            details,
            resolver_diagnostics,
        }
    }

    pub(crate) fn check_identity(
        provider: &str,
        target_url: Option<&str>,
        external_id: Option<&str>,
    ) -> String {
        // Target URLs are shared by legacy Statuses and Check Runs. Provider
        // plus that normalized build handle is the cross-API identity; a check
        // run's external id covers providers that omit a details URL.
        let build = target_url
            .map(normalize_target_url)
            .or_else(|| external_id.map(str::to_string))
            .unwrap_or_default();
        format!("{provider}\u{0}{build}")
    }

    fn deadline_degradation() -> CiApiDegradation {
        CiApiDegradation {
            api: "external check discovery".into(),
            message: "Shared external-check discovery and resolver deadline exhausted.".into(),
        }
    }

    /// CiApiDegradation is rendered both as JSON and operator-facing text. Keep
    /// its GhClient error projection canonical, redacted, and bounded at entry.
    pub(crate) fn gh_error_diagnostic(error: &Error) -> String {
        homeboy::core::redaction::RedactionPolicy::default()
            .redact_embedded_urls(&homeboy::core::redaction::redact_string(&error.to_string()))
            .chars()
            .take(MAX_GH_ERROR_CHARS)
            .collect()
    }

    fn is_failed_status(status: &GhCommitStatus) -> bool {
        matches!(status.state.as_deref(), Some("failure" | "error"))
    }

    pub(crate) fn latest_failed_statuses(
        statuses: Vec<GhCommitStatus>,
        contexts: &mut HashSet<String>,
    ) -> Vec<GhCommitStatus> {
        statuses
            .into_iter()
            // GitHub returns legacy statuses newest first. Record the first context
            // even when it succeeded so stale failures cannot hydrate.
            .filter(|status| contexts.insert(status.context.clone()) && is_failed_status(status))
            .collect()
    }
    pub(crate) fn is_failed_check_run(run: &GhCheckRun) -> bool {
        matches!(
            run.conclusion.as_deref(),
            Some("failure" | "timed_out" | "cancelled" | "action_required")
        )
    }

    pub(crate) fn fetch_run_jobs(
        gh: &GhClient,
        run_id: u64,
        attempt: Option<u64>,
    ) -> Result<Vec<GhJob>> {
        let repo = gh.repo_path()?;
        let path = if let Some(attempt) = attempt {
            format!(
                "repos/{repo}/actions/runs/{run_id}/attempts/{attempt}/jobs?filter=latest&per_page=100"
            )
        } else {
            format!("repos/{repo}/actions/runs/{run_id}/jobs?filter=latest&per_page=100")
        };
        let raw: GhJobsResponse = gh.api_json(&path)?;
        Ok(raw.jobs)
    }

    pub(crate) fn is_failed_run(run: &GhWorkflowRun) -> bool {
        matches!(
            run.conclusion.as_deref(),
            Some("failure" | "timed_out" | "cancelled" | "action_required")
        )
    }

    pub(crate) fn is_failed_job(job: &GhJob) -> bool {
        matches!(
            job.conclusion.as_deref(),
            Some("failure" | "timed_out" | "cancelled" | "action_required")
        )
    }

    pub(crate) fn failed_step_names(job: &GhJob) -> Vec<String> {
        let mut names: Vec<String> = job
            .steps
            .iter()
            .filter(|step| {
                matches!(
                    step.conclusion.as_deref(),
                    Some("failure" | "timed_out" | "cancelled" | "action_required")
                )
            })
            .map(|step| step.name.clone())
            .collect();
        names.dedup();
        names
    }

    pub(crate) fn extract_relevant_snippets(
        log: &str,
        failed_steps: &[String],
        max_snippets: usize,
        context_lines: usize,
    ) -> Vec<CiLogSnippet> {
        if log.trim().is_empty() || max_snippets == 0 {
            return Vec::new();
        }
        let lines: Vec<&str> = log.lines().collect();
        let mut hits = Vec::new();

        for (idx, line) in lines.iter().enumerate() {
            if is_relevant_log_line(line, failed_steps) {
                let start = idx.saturating_sub(context_lines);
                let end = (idx + context_lines + 1).min(lines.len());
                for (snippet_idx, line) in lines.iter().enumerate().take(end).skip(start) {
                    if hits.len() >= max_snippets {
                        return hits;
                    }
                    let text = normalize_log_line(line);
                    if text.is_empty()
                        || hits
                            .iter()
                            .any(|hit: &CiLogSnippet| hit.line == snippet_idx + 1)
                    {
                        continue;
                    }
                    hits.push(CiLogSnippet {
                        line: snippet_idx + 1,
                        text,
                    });
                }
            }
        }

        hits
    }

    pub(crate) fn is_relevant_log_line(line: &str, failed_steps: &[String]) -> bool {
        let normalized = normalize_log_line(line).to_ascii_lowercase();
        if normalized.is_empty() {
            return false;
        }
        let has_step = failed_steps
            .iter()
            .any(|step| !step.trim().is_empty() && normalized.contains(&step.to_ascii_lowercase()));
        has_step
            || normalized.contains("::error")
            || normalized.contains("error:")
            || normalized.contains("failed")
            || normalized.contains("failure")
            || normalized.contains("panicked")
            || normalized.contains("timed out")
            || normalized.contains("timeout")
            || normalized.contains("could not compile")
            || normalized.contains("test result: failed")
    }

    pub(crate) fn normalize_log_line(line: &str) -> String {
        let without_timestamp = line
            .split_once(' ')
            .filter(|(prefix, _)| prefix.contains('T') && prefix.ends_with('Z'))
            .map(|(_, rest)| rest)
            .unwrap_or(line);
        let stripped = without_timestamp
            .replace("\u{001b}[0m", "")
            .replace("\u{001b}[31m", "")
            .replace("\u{001b}[32m", "")
            .replace("\u{001b}[33m", "")
            .replace("\u{001b}[34m", "")
            .trim()
            .to_string();
        stripped.chars().take(MAX_SNIPPET_LINE_CHARS).collect()
    }

    pub(crate) fn classify_failure(evidence: &str) -> CiFailureCategory {
        let lower = evidence.to_ascii_lowercase();
        if lower.contains("baseline")
            || lower.contains("changed-since")
            || lower.contains("differential")
        {
            return CiFailureCategory::BaselineDifferential;
        }
        if lower.contains("rustfmt")
            || lower.contains("cargo fmt")
            || lower.contains("fmt --check")
            || lower.contains("would be reformatted")
            || lower.contains("formatting")
        {
            return CiFailureCategory::Fmt;
        }
        if lower.contains("clippy") {
            return CiFailureCategory::Clippy;
        }
        if lower.contains("could not compile")
            || lower.contains("compilation failed")
            || lower.contains("error[e")
            || lower.contains("error:")
        {
            return CiFailureCategory::Compile;
        }
        if lower.contains("test result: failed")
            || lower.contains("failures:")
            || lower.contains("panicked at")
            || lower.contains("assertion failed")
        {
            return CiFailureCategory::UnitTest;
        }
        if lower.contains("timed out") || lower.contains("timeout") || lower.contains("cancelled") {
            return CiFailureCategory::InfraTimeout;
        }
        if lower.contains("queued")
            || lower.contains("waiting for a runner")
            || lower.contains("runner")
        {
            return CiFailureCategory::Queueing;
        }
        CiFailureCategory::Unknown
    }

    pub(crate) fn classify_origin(evidence: &str) -> CiFailureOrigin {
        let lower = evidence.to_ascii_lowercase();
        if lower.contains("baseline")
            || lower.contains("base branch")
            || lower.contains("origin/main")
            || lower.contains("upstream main")
        {
            CiFailureOrigin::Baseline
        } else if lower.contains("pr") || lower.contains("head") || lower.contains("pull_request") {
            CiFailureOrigin::PrHead
        } else {
            CiFailureOrigin::Unknown
        }
    }

    pub(crate) fn detect_touched_files(evidence: &str) -> Vec<String> {
        let mut files = Vec::new();
        for token in evidence.split_whitespace() {
            let cleaned = token
                .trim_matches(|c: char| {
                    matches!(c, ',' | ':' | ';' | ')' | '(' | '[' | ']' | '"' | '\'')
                })
                .trim_start_matches("./");
            if looks_like_source_path(cleaned) && !files.iter().any(|file| file == cleaned) {
                files.push(cleaned.to_string());
            }
            if files.len() >= 12 {
                break;
            }
        }
        files
    }

    pub(crate) fn looks_like_source_path(value: &str) -> bool {
        value.contains('/')
            && [
                ".rs", ".ts", ".tsx", ".js", ".jsx", ".php", ".py", ".go", ".rb", ".java", ".css",
                ".scss", ".json", ".yml", ".yaml", ".toml", ".md",
            ]
            .iter()
            .any(|suffix| value.ends_with(suffix))
    }

    pub(crate) fn collect_categories(runs: &[CiFailedRunSummary]) -> Vec<CiFailureCategory> {
        let mut categories = Vec::new();
        for run in runs {
            if !categories.contains(&run.suspected_category) {
                categories.push(run.suspected_category);
            }
            for job in &run.jobs {
                if !categories.contains(&job.suspected_category) {
                    categories.push(job.suspected_category);
                }
            }
        }
        categories.sort();
        categories
    }

    pub(crate) fn build_next_actions(
        repo: &str,
        pr_number: u64,
        runs: &[CiFailedRunSummary],
    ) -> Vec<String> {
        let mut actions = Vec::new();
        if runs.is_empty() {
            actions.push(format!("gh pr checks {pr_number} -R {repo}"));
            return actions;
        }

        if runs
            .iter()
            .any(|run| run.failure_origin == CiFailureOrigin::Baseline)
        {
            actions.push("Inspect the baseline failure first; PR-head reruns may stay red until the base job is green.".to_string());
        }
        for category in collect_categories(runs) {
            let hint = next_action_hint(category);
            if !actions.contains(&hint) {
                actions.push(hint);
            }
        }
        actions.push(format!("gh pr checks {pr_number} -R {repo} --fail-fast"));
        actions
    }

    pub(crate) fn next_action_hint(category: CiFailureCategory) -> String {
        match category {
            CiFailureCategory::Fmt => "Run the formatter for the reported files, then re-check the formatting job.".to_string(),
            CiFailureCategory::Clippy => "Fix the lint diagnostic in the reported step; prefer the compiler suggestion when present.".to_string(),
            CiFailureCategory::Compile => "Fix the first compiler error before chasing downstream test failures.".to_string(),
            CiFailureCategory::UnitTest => "Reproduce the named test/job locally or in the configured CI runner and inspect the assertion failure.".to_string(),
            CiFailureCategory::BaselineDifferential => "Compare baseline and PR-head jobs; repair baseline red separately before attributing the failure to this PR.".to_string(),
            CiFailureCategory::InfraTimeout => "Check whether the job timed out, was cancelled, or hit runner capacity before changing code.".to_string(),
            CiFailureCategory::Queueing => "Inspect runner availability and queued jobs before rerunning the workflow.".to_string(),
            CiFailureCategory::Unknown => "Open the linked job log and inspect the bounded snippets around the failed step.".to_string(),
        }
    }

    pub(crate) fn render_human_summary(
        repo: &str,
        pr: &GhPullRequest,
        total_failed_runs: usize,
        runs: &[CiFailedRunSummary],
        external_checks: &[CiExternalCheckSummary],
        api_degradations: &[CiApiDegradation],
        next_actions: &[String],
    ) -> String {
        let mut lines = vec![format!(
            "CI triage for {repo}#{} ({}): {} failed run(s), {} inspected.",
            pr.number,
            pr.title,
            total_failed_runs,
            runs.len()
        )];
        for run in runs {
            lines.push(format!(
                "- {} ({:?}, {:?}) {}",
                run.workflow,
                run.suspected_category,
                run.failure_origin,
                run.html_url.as_deref().unwrap_or("")
            ));
            for job in &run.jobs {
                let steps = if job.failed_steps.is_empty() {
                    "failed step unknown".to_string()
                } else {
                    format!("failed step(s): {}", job.failed_steps.join(", "))
                };
                lines.push(format!(
                    "  - job {}: {:?}; {}",
                    job.name, job.suspected_category, steps
                ));
                for snippet in job.snippets.iter().take(2) {
                    lines.push(format!("    line {}: {}", snippet.line, snippet.text));
                }
            }
        }
        for check in external_checks {
            lines.push(format!(
                "- external check {}: {} {}",
                check.provider,
                check.status,
                check.target_url.as_deref().unwrap_or("")
            ));
            if let Some(description) = &check.description {
                lines.push(format!("  - {description}"));
            }
            for detail in &check.details {
                if !detail.summary.is_empty() {
                    lines.push(format!("  - {}", detail.summary));
                }
                for action in &detail.actions {
                    lines.push(format!("  - replay: {action}"));
                }
                for reference in &detail.artifact_refs {
                    lines.push(format!("  - artifact: {reference}"));
                }
                for reference in &detail.log_refs {
                    lines.push(format!("  - log: {reference}"));
                }
            }
            for diagnostic in &check.resolver_diagnostics {
                lines.push(format!(
                    "  - resolver {}: {}",
                    diagnostic.kind, diagnostic.message
                ));
            }
        }
        for degradation in api_degradations {
            lines.push(format!(
                "API degradation ({}): {}",
                degradation.api, degradation.message
            ));
        }
        if !next_actions.is_empty() {
            lines.push("Next actions:".to_string());
            for action in next_actions {
                lines.push(format!("- {action}"));
            }
        }
        lines.join("\n")
    }

    #[derive(Debug)]
    pub(crate) struct GhPullRequest {
        pub(crate) number: u64,
        pub(crate) title: String,
        pub(crate) url: String,
        pub(crate) head_sha: String,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct GhPullRequestRaw {
        number: u64,
        title: Option<String>,
        html_url: Option<String>,
        head: GhPullRequestHead,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct GhPullRequestHead {
        sha: String,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct GhWorkflowRunsResponse {
        #[serde(default)]
        workflow_runs: Vec<GhWorkflowRun>,
    }

    #[derive(Debug, Deserialize, Clone)]
    pub(crate) struct GhWorkflowRun {
        id: u64,
        #[serde(default)]
        name: String,
        status: Option<String>,
        conclusion: Option<String>,
        head_branch: Option<String>,
        event: Option<String>,
        html_url: Option<String>,
        run_started_at: Option<String>,
        created_at: Option<String>,
        run_attempt: Option<u64>,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct GhJobsResponse {
        #[serde(default)]
        jobs: Vec<GhJob>,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct GhJob {
        id: u64,
        #[serde(default)]
        name: String,
        status: Option<String>,
        conclusion: Option<String>,
        html_url: Option<String>,
        #[serde(default)]
        steps: Vec<GhStep>,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct GhStep {
        #[serde(default)]
        name: String,
        conclusion: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct GhCommitStatus {
        #[serde(default)]
        pub(crate) context: String,
        pub(crate) state: Option<String>,
        pub(crate) description: Option<String>,
        pub(crate) target_url: Option<String>,
    }
    type GhStatusesResponse = Vec<GhCommitStatus>;
    #[derive(Debug, Deserialize)]
    pub(crate) struct GhCheckRunsResponse {
        #[serde(default)]
        total_count: usize,
        #[serde(default)]
        check_runs: Vec<GhCheckRun>,
    }
    #[derive(Debug, Deserialize)]
    pub(crate) struct GhCheckRun {
        #[serde(default)]
        name: String,
        conclusion: Option<String>,
        details_url: Option<String>,
        external_id: Option<String>,
    }
}

pub(crate) use engine::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_references() {
        assert_eq!(
            parse_pr_reference("123").unwrap(),
            ParsedCiPrReference {
                repo: None,
                number: 123
            }
        );
        assert_eq!(
            parse_pr_reference("Extra-Chill/homeboy#5808").unwrap(),
            ParsedCiPrReference {
                repo: Some("Extra-Chill/homeboy".to_string()),
                number: 5808
            }
        );
        assert_eq!(
            parse_pr_reference("https://github.com/Extra-Chill/homeboy/pull/5808").unwrap(),
            ParsedCiPrReference {
                repo: Some("Extra-Chill/homeboy".to_string()),
                number: 5808
            }
        );
    }

    #[test]
    fn classifies_common_failure_categories() {
        assert_eq!(
            classify_failure("cargo fmt --check would be reformatted"),
            CiFailureCategory::Fmt
        );
        assert_eq!(
            classify_failure("cargo clippy failed"),
            CiFailureCategory::Clippy
        );
        assert_eq!(
            classify_failure("error[E0425]: cannot find value"),
            CiFailureCategory::Compile
        );
        assert_eq!(
            classify_failure("test result: FAILED. 1 failed"),
            CiFailureCategory::UnitTest
        );
        assert_eq!(
            classify_failure("baseline differential failed"),
            CiFailureCategory::BaselineDifferential
        );
    }

    #[test]
    fn extracts_bounded_relevant_snippets() {
        let log = "2026-01-01T00:00:00Z setup\n2026-01-01T00:00:01Z cargo test\n2026-01-01T00:00:02Z error: src/main.rs failed\n2026-01-01T00:00:03Z tail";
        let snippets = extract_relevant_snippets(log, &["cargo test".to_string()], 3, 1);

        assert_eq!(snippets.len(), 3);
        assert_eq!(snippets[0].text, "setup");
        assert_eq!(snippets[1].text, "cargo test");
        assert_eq!(snippets[2].text, "error: src/main.rs failed");
    }

    #[test]
    fn ignores_stale_legacy_failure_after_latest_success() {
        let mut contexts = HashSet::new();
        let failures = latest_failed_statuses(
            vec![
                GhCommitStatus {
                    context: "example-ci".into(),
                    state: Some("success".into()),
                    description: None,
                    target_url: None,
                },
                GhCommitStatus {
                    context: "example-ci".into(),
                    state: Some("failure".into()),
                    description: None,
                    target_url: None,
                },
            ],
            &mut contexts,
        );
        assert!(failures.is_empty());
    }

    #[test]
    fn dedupes_legacy_status_and_check_run_by_provider_and_build_url() {
        let legacy = check_identity(
            "example-ci",
            Some("https://example.test/build/42?token=secret#credential"),
            None,
        );
        let check_run = check_identity("example-ci", Some("https://example.test/build/42"), None);

        assert_eq!(legacy, check_run);
    }

    #[test]
    fn external_status_redacts_embedded_credential_url() {
        let status = "failure: see (https://user:pass@example.test/build?token=secret#credential).";
        let redacted =
            homeboy::core::redaction::RedactionPolicy::default().redact_embedded_urls(status);

        assert!(!redacted.contains("pass"));
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("credential"));
        assert!(redacted.ends_with(")."));
    }

    #[test]
    fn human_summary_preserves_hydration_and_degradation_diagnostics() {
        let pr = GhPullRequest {
            number: 1,
            title: "Test".into(),
            url: "https://example.test/pr/1".into(),
            head_sha: "abc".into(),
        };
        let check = CiExternalCheckSummary {
            provider: "example-ci".into(),
            status: "failure".into(),
            description: None,
            target_url: Some("https://example.test/job".into()),
            details: vec![HydratedDetail {
                provider: "example-ci".into(),
                build_id: Some("42".into()),
                summary: "compile failed".into(),
                actions: vec!["example-ci replay 1".into()],
                artifact_refs: vec!["https://example.test/artifact/42".into()],
                log_refs: vec!["https://example.test/log/42".into()],
            }],
            resolver_diagnostics: vec![ResolverDiagnostic {
                provider: "example-ci".into(),
                kind: "unavailable".into(),
                message: "network unavailable".into(),
            }],
        };
        let text = render_human_summary(
            "owner/repo",
            &pr,
            0,
            &[],
            &[check],
            &[CiApiDegradation {
                api: "Check Runs".into(),
                message: "rate limited".into(),
            }],
            &[],
        );
        assert!(text.contains("compile failed"));
        assert!(text.contains("replay: example-ci replay 1"));
        assert!(text.contains("API degradation (Check Runs)"));
    }

    #[test]
    fn legacy_status_keeps_failure_state_when_description_is_missing_or_redacted() {
        let resolver_session = ResolverSession::discover();
        let empty = hydrate_external(
            &resolver_session,
            "example-ci".into(),
            "failure".into(),
            None,
            None,
            &mut 0,
            Instant::now(),
        );
        assert_eq!(empty.status, "failure");
        assert_eq!(empty.description, None);

        let described = hydrate_external(
            &resolver_session,
            "example-ci".into(),
            "error".into(),
            Some("see https://user:token@example.test/job?token=secret".into()),
            None,
            &mut 0,
            Instant::now(),
        );
        assert_eq!(described.status, "error");
        let description = described.description.unwrap();
        assert!(!description.contains("user"));
        assert!(!description.contains("secret"));
    }

    #[test]
    fn discovery_normalizes_multiple_failed_legacy_statuses_and_check_runs() {
        let mut contexts = HashSet::new();
        let legacy = latest_failed_statuses(
            vec![
                GhCommitStatus {
                    context: "legacy-failure".into(),
                    state: Some("failure".into()),
                    description: None,
                    target_url: None,
                },
                GhCommitStatus {
                    context: "legacy-error".into(),
                    state: Some("error".into()),
                    description: Some("provider error".into()),
                    target_url: None,
                },
            ],
            &mut contexts,
        );
        assert_eq!(
            legacy
                .iter()
                .map(|status| status.state.as_deref())
                .collect::<Vec<_>>(),
            [Some("failure"), Some("error")]
        );

        let check_runs = ["failure", "timed_out", "cancelled", "action_required"]
            .into_iter()
            .map(|conclusion| {
                serde_json::from_value::<GhCheckRun>(serde_json::json!({
                    "name": conclusion,
                    "conclusion": conclusion,
                }))
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            check_runs
                .iter()
                .filter(|check| is_failed_check_run(check))
                .count(),
            4
        );
    }

    #[test]
    fn gh_api_degradation_errors_are_redacted_and_bounded() {
        let error = Error::internal_io(
            format!(
                "request failed https://user:token@example.test/api?token=secret {}",
                "x".repeat(MAX_GH_ERROR_CHARS * 2)
            ),
            None,
        );
        let diagnostic = engine::gh_error_diagnostic(&error);

        assert!(!diagnostic.contains("user"));
        assert!(!diagnostic.contains("secret"));
        assert!(diagnostic.chars().count() <= MAX_GH_ERROR_CHARS);
    }
}
