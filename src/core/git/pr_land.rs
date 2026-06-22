use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::process::Command;

use crate::core::error::{Error, Result};

use super::gh_client::GhClient;

#[derive(Debug, Clone, Default)]
pub struct PrLandOptions {
    pub repo: String,
    pub prs: Vec<String>,
    pub merge_method: String,
    pub delete_branch: bool,
    pub dry_run: bool,
    pub refresh_helper: Option<PrLandRefreshHelper>,
    pub max_base_retries: usize,
}

#[derive(Debug, Clone, Default)]
pub struct PrLandRefreshHelper {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrLandOutput {
    pub command: &'static str,
    pub repo: String,
    pub merge_method: String,
    pub dry_run: bool,
    pub summary: PrLandSummary,
    pub items: Vec<PrLandItem>,
    pub fleet_status_table: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PrLandSummary {
    pub total: usize,
    pub landed: usize,
    pub already_merged: usize,
    pub blocked: usize,
    pub refreshed: usize,
    pub merge_retries: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrLandItem {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub status: PrLandStatus,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrLandStatus {
    Landed,
    AlreadyMerged,
    Blocked,
    Refreshed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PrReadiness {
    Ready,
    AlreadyMerged,
    Refreshable(String),
    Blocked(String),
}

#[derive(Debug, Clone, Deserialize)]
struct RawPrView {
    number: u64,
    title: String,
    url: String,
    state: String,
    #[serde(default, rename = "isDraft")]
    is_draft: bool,
    #[serde(default, rename = "mergeStateStatus")]
    merge_state_status: Option<String>,
    #[serde(default, rename = "statusCheckRollup")]
    status_check_rollup: Vec<Value>,
    #[serde(default, rename = "headRefOid")]
    head_ref_oid: Option<String>,
    #[serde(default, rename = "mergedAt")]
    merged_at: Option<String>,
}

#[derive(Debug, Clone)]
struct PrView {
    number: u64,
    title: String,
    url: String,
    state: String,
    draft: bool,
    checks: Option<String>,
    merge_state: Option<String>,
    head_sha: Option<String>,
    merged_at: Option<String>,
}

trait PrLandClient {
    fn view_pr(&mut self, repo: &str, number: u64) -> Result<PrView>;
    fn merge_pr(
        &mut self,
        repo: &str,
        number: u64,
        method: &str,
        delete_branch: bool,
    ) -> Result<()>;
    fn refresh_pr(&mut self, repo: &str, pr: &PrView, helper: &PrLandRefreshHelper) -> Result<()>;
}

struct GhPrLandClient {
    gh: GhClient,
    repo: String,
}

impl GhPrLandClient {
    fn new(repo: &str) -> Result<Self> {
        let gh = GhClient::from_repo_arg(repo)?;
        gh.ensure_ready()?;
        let repo = gh.repo_path()?.to_string();
        Ok(Self { gh, repo })
    }
}

impl PrLandClient for GhPrLandClient {
    fn view_pr(&mut self, _repo: &str, number: u64) -> Result<PrView> {
        let raw = self.gh.run(&vec![
            "pr".to_string(),
            "view".to_string(),
            number.to_string(),
            "-R".to_string(),
            self.repo.clone(),
            "--json".to_string(),
            "number,title,url,state,isDraft,mergeStateStatus,statusCheckRollup,headRefOid,mergedAt"
                .to_string(),
        ])?;
        let parsed: RawPrView = serde_json::from_str(raw.trim()).map_err(|e| {
            Error::internal_json(e.to_string(), Some(format!("parse gh pr view #{number}")))
        })?;
        Ok(PrView {
            number: parsed.number,
            title: parsed.title,
            url: parsed.url,
            state: parsed.state,
            draft: parsed.is_draft,
            checks: summarize_checks(&parsed.status_check_rollup),
            merge_state: non_empty(parsed.merge_state_status),
            head_sha: non_empty(parsed.head_ref_oid),
            merged_at: parsed.merged_at,
        })
    }

    fn merge_pr(
        &mut self,
        _repo: &str,
        number: u64,
        method: &str,
        delete_branch: bool,
    ) -> Result<()> {
        let mut args = vec![
            "pr".to_string(),
            "merge".to_string(),
            number.to_string(),
            "-R".to_string(),
            self.repo.clone(),
            format!("--{method}"),
        ];
        if delete_branch {
            args.push("--delete-branch".to_string());
        }
        self.gh.run(&args).map(|_| ())
    }

    fn refresh_pr(&mut self, repo: &str, pr: &PrView, helper: &PrLandRefreshHelper) -> Result<()> {
        run_refresh_helper(repo, pr, helper)
    }
}

pub fn land_prs(options: PrLandOptions) -> Result<PrLandOutput> {
    let mut client = GhPrLandClient::new(&options.repo)?;
    land_prs_with_client(options, &mut client)
}

fn land_prs_with_client(
    options: PrLandOptions,
    client: &mut impl PrLandClient,
) -> Result<PrLandOutput> {
    if options.prs.is_empty() {
        return Err(Error::validation_missing_argument(vec!["pr".to_string()]));
    }
    validate_merge_method(&options.merge_method)?;
    let numbers = parse_pr_refs(&options.repo, &options.prs)?;
    let mut items = Vec::new();
    let mut summary = PrLandSummary {
        total: numbers.len(),
        ..Default::default()
    };

    for number in numbers {
        let mut pr = client.view_pr(&options.repo, number)?;
        match readiness(&pr, options.refresh_helper.is_some()) {
            PrReadiness::AlreadyMerged => {
                summary.already_merged += 1;
                items.push(item_from_pr(
                    &pr,
                    PrLandStatus::AlreadyMerged,
                    "already merged",
                ));
            }
            PrReadiness::Refreshable(reason) => {
                if let Some(helper) = &options.refresh_helper {
                    if options.dry_run {
                        summary.blocked += 1;
                        items.push(item_from_pr(
                            &pr,
                            PrLandStatus::Blocked,
                            &format!("would refresh: {reason}"),
                        ));
                        break;
                    }
                    if !options.dry_run {
                        client.refresh_pr(&options.repo, &pr, helper)?;
                    }
                    summary.refreshed += 1;
                    pr = client.view_pr(&options.repo, number)?;
                    if !matches!(readiness(&pr, false), PrReadiness::Ready) {
                        summary.blocked += 1;
                        items.push(item_from_pr(&pr, PrLandStatus::Refreshed, &reason));
                        break;
                    }
                } else {
                    summary.blocked += 1;
                    items.push(item_from_pr(&pr, PrLandStatus::Blocked, &reason));
                    break;
                }
                merge_ready_pr(&options, client, &mut summary, &mut items, pr)?;
            }
            PrReadiness::Blocked(reason) => {
                summary.blocked += 1;
                items.push(item_from_pr(&pr, PrLandStatus::Blocked, &reason));
                break;
            }
            PrReadiness::Ready => {
                merge_ready_pr(&options, client, &mut summary, &mut items, pr)?;
            }
        }
    }

    let fleet_status_table = render_fleet_status_table(&items);
    Ok((PrLandOutput {
        command: "git.pr.land",
        repo: options.repo,
        merge_method: options.merge_method,
        dry_run: options.dry_run,
        summary,
        items,
        fleet_status_table,
    }))
}

fn merge_ready_pr(
    options: &PrLandOptions,
    client: &mut impl PrLandClient,
    summary: &mut PrLandSummary,
    items: &mut Vec<PrLandItem>,
    mut pr: PrView,
) -> Result<()> {
    if options.dry_run {
        summary.landed += 1;
        items.push(item_from_pr(
            &pr,
            PrLandStatus::Landed,
            "ready; dry-run did not merge",
        ));
        return Ok(());
    }

    let mut attempts = 0;
    loop {
        match client.merge_pr(
            &options.repo,
            pr.number,
            &options.merge_method,
            options.delete_branch,
        ) {
            Ok(()) => {
                summary.landed += 1;
                items.push(item_from_pr(&pr, PrLandStatus::Landed, "merged"));
                return Ok(());
            }
            Err(err)
                if is_base_modified_race(&err.message) && attempts < options.max_base_retries =>
            {
                attempts += 1;
                summary.merge_retries += 1;
                pr = client.view_pr(&options.repo, pr.number)?;
                match readiness(&pr, false) {
                    PrReadiness::Ready => continue,
                    PrReadiness::AlreadyMerged => {
                        summary.already_merged += 1;
                        items.push(item_from_pr(
                            &pr,
                            PrLandStatus::AlreadyMerged,
                            "merged after retry recompute",
                        ));
                        return Ok(());
                    }
                    PrReadiness::Blocked(reason) | PrReadiness::Refreshable(reason) => {
                        summary.blocked += 1;
                        items.push(item_from_pr(&pr, PrLandStatus::Blocked, &reason));
                        return Ok(());
                    }
                }
            }
            Err(err) => return Err(err),
        }
    }
}

fn readiness(pr: &PrView, can_refresh: bool) -> PrReadiness {
    if pr.merged_at.is_some() || pr.state == "MERGED" {
        return PrReadiness::AlreadyMerged;
    }
    if pr.state != "OPEN" {
        return PrReadiness::Blocked(format!("PR state is {}", pr.state));
    }
    if pr.draft {
        return PrReadiness::Blocked("PR is draft".to_string());
    }
    if pr.checks.as_deref() == Some("FAILURE") {
        return PrReadiness::Blocked("required checks are failing".to_string());
    }
    if pr.checks.as_deref() != Some("SUCCESS") {
        return PrReadiness::Blocked("required checks are pending or not reported".to_string());
    }
    if pr.merge_state.as_deref() == Some("CLEAN") {
        return PrReadiness::Ready;
    }
    let reason = format!(
        "merge state is {}",
        pr.merge_state.as_deref().unwrap_or("unknown")
    );
    if can_refresh && matches!(pr.merge_state.as_deref(), Some("BEHIND" | "DIRTY")) {
        PrReadiness::Refreshable(reason)
    } else {
        PrReadiness::Blocked(reason)
    }
}

fn item_from_pr(pr: &PrView, status: PrLandStatus, reason: &str) -> PrLandItem {
    PrLandItem {
        number: pr.number,
        url: pr.url.clone(),
        title: pr.title.clone(),
        status,
        reason: reason.to_string(),
        checks: pr.checks.clone(),
        merge_state: pr.merge_state.clone(),
        head_sha: pr.head_sha.clone(),
    }
}

fn parse_pr_refs(repo: &str, refs: &[String]) -> Result<Vec<u64>> {
    let mut numbers = Vec::new();
    for raw in refs {
        let number = parse_pr_ref(repo, raw)?;
        if !numbers.contains(&number) {
            numbers.push(number);
        }
    }
    Ok(numbers)
}

fn parse_pr_ref(repo: &str, raw: &str) -> Result<u64> {
    let trimmed = raw.trim();
    if let Ok(number) = trimmed.parse::<u64>() {
        if number > 0 {
            return Ok(number);
        }
    }
    let marker = "/pull/";
    if let Some(index) = trimmed.find(marker) {
        let number_part = &trimmed[index + marker.len()..];
        let number = number_part
            .split(|ch: char| !ch.is_ascii_digit())
            .next()
            .unwrap_or_default()
            .parse::<u64>()
            .map_err(|_| invalid_pr_ref(raw))?;
        if !url_matches_repo(repo, trimmed) {
            return Err(Error::validation_invalid_argument(
                "pr",
                "PR URL does not match --repo",
                Some(raw.to_string()),
                Some(vec![repo.to_string()]),
            ));
        }
        return Ok(number);
    }
    Err(invalid_pr_ref(raw))
}

fn invalid_pr_ref(raw: &str) -> Error {
    Error::validation_invalid_argument(
        "pr",
        "expected a PR number or URL containing /pull/<number>",
        Some(raw.to_string()),
        None,
    )
}

fn url_matches_repo(repo: &str, url: &str) -> bool {
    let repo = repo.trim().trim_end_matches('/').trim_end_matches(".git");
    let parts: Vec<&str> = repo.split('/').collect();
    let slug = match parts.as_slice() {
        [owner, name] => format!("/{owner}/{}/pull/", name.trim_end_matches(".git")),
        [_, owner, name] => format!("/{owner}/{}/pull/", name.trim_end_matches(".git")),
        _ => return false,
    };
    url.contains(&slug)
}

fn validate_merge_method(method: &str) -> Result<()> {
    if matches!(method, "merge" | "squash" | "rebase") {
        Ok(())
    } else {
        Err(Error::validation_invalid_argument(
            "merge-method",
            "expected merge, squash, or rebase",
            Some(method.to_string()),
            None,
        ))
    }
}

fn is_base_modified_race(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("base branch was modified") || lower.contains("base branch modified")
}

fn render_fleet_status_table(items: &[PrLandItem]) -> String {
    let mut rows = vec![
        "| PR | Status | Checks | Merge State | Reason |".to_string(),
        "| --- | --- | --- | --- | --- |".to_string(),
    ];
    for item in items {
        rows.push(format!(
            "| #{} | {:?} | {} | {} | {} |",
            item.number,
            item.status,
            table_cell(item.checks.as_deref().unwrap_or("")),
            table_cell(item.merge_state.as_deref().unwrap_or("")),
            table_cell(&item.reason)
        ));
    }
    rows.join("\n")
}

fn table_cell(value: &str) -> String {
    value.replace('|', "\\|")
}

fn run_refresh_helper(repo: &str, pr: &PrView, helper: &PrLandRefreshHelper) -> Result<()> {
    if helper.program.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "refresh-helper",
            "helper program must not be empty",
            None,
            None,
        ));
    }
    let args = helper
        .args
        .iter()
        .map(|arg| render_helper_arg(arg, repo, pr))
        .collect::<Vec<_>>();
    let output = Command::new(&helper.program)
        .args(&args)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("failed to invoke refresh helper {}: {e}", helper.program),
                Some(helper.program.clone()),
            )
        })?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Err(Error::git_command_failed(format!(
            "refresh helper failed: {}",
            if stderr.is_empty() { stdout } else { stderr }
        )))
    }
}

fn render_helper_arg(arg: &str, repo: &str, pr: &PrView) -> String {
    arg.replace("{repo}", repo)
        .replace("{number}", &pr.number.to_string())
        .replace("{url}", &pr.url)
        .replace("{head_sha}", pr.head_sha.as_deref().unwrap_or(""))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn summarize_checks(items: &[Value]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut pending = false;
    for item in items {
        let conclusion = item.get("conclusion").and_then(Value::as_str).unwrap_or("");
        let status = item.get("status").and_then(Value::as_str).unwrap_or("");
        if matches!(
            conclusion,
            "FAILURE" | "failure" | "CANCELLED" | "cancelled" | "TIMED_OUT" | "timed_out"
        ) {
            return Some("FAILURE".to_string());
        }
        if conclusion.is_empty() && !matches!(status, "COMPLETED" | "completed") {
            pending = true;
        }
    }
    Some(if pending { "PENDING" } else { "SUCCESS" }.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};

    #[derive(Default)]
    struct FakeClient {
        views: BTreeMap<u64, VecDeque<PrView>>,
        merge_errors: VecDeque<Error>,
        merged: Vec<u64>,
        refreshed: Vec<u64>,
    }

    impl FakeClient {
        fn push_view(&mut self, pr: PrView) {
            self.views.entry(pr.number).or_default().push_back(pr);
        }
    }

    impl PrLandClient for FakeClient {
        fn view_pr(&mut self, _repo: &str, number: u64) -> Result<PrView> {
            let queue = self.views.get_mut(&number).unwrap();
            if queue.len() > 1 {
                Ok(queue.pop_front().unwrap())
            } else {
                Ok(queue.front().unwrap().clone())
            }
        }

        fn merge_pr(
            &mut self,
            _repo: &str,
            number: u64,
            _method: &str,
            _delete_branch: bool,
        ) -> Result<()> {
            if let Some(err) = self.merge_errors.pop_front() {
                return Err(err);
            }
            self.merged.push(number);
            Ok(())
        }

        fn refresh_pr(
            &mut self,
            _repo: &str,
            pr: &PrView,
            _helper: &PrLandRefreshHelper,
        ) -> Result<()> {
            self.refreshed.push(pr.number);
            Ok(())
        }
    }

    fn pr(number: u64, checks: Option<&str>, merge_state: Option<&str>) -> PrView {
        PrView {
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/Extra-Chill/homeboy/pull/{number}"),
            state: "OPEN".to_string(),
            draft: false,
            checks: checks.map(str::to_string),
            merge_state: merge_state.map(str::to_string),
            head_sha: Some(format!("sha{number}")),
            merged_at: None,
        }
    }

    fn options(prs: Vec<&str>) -> PrLandOptions {
        PrLandOptions {
            repo: "Extra-Chill/homeboy".to_string(),
            prs: prs.into_iter().map(str::to_string).collect(),
            merge_method: "squash".to_string(),
            max_base_retries: 1,
            ..Default::default()
        }
    }

    #[test]
    fn parses_numbers_and_urls_for_same_repo() {
        let parsed = parse_pr_refs(
            "Extra-Chill/homeboy",
            &[
                "123".to_string(),
                "https://github.com/Extra-Chill/homeboy/pull/124".to_string(),
            ],
        )
        .unwrap();

        assert_eq!(parsed, vec![123, 124]);
    }

    #[test]
    fn clean_success_pr_is_ready() {
        assert_eq!(
            readiness(&pr(1, Some("SUCCESS"), Some("CLEAN")), false),
            PrReadiness::Ready
        );
        assert!(matches!(
            readiness(&pr(1, None, Some("CLEAN")), false),
            PrReadiness::Blocked(_)
        ));
    }

    #[test]
    fn lands_clean_prs_sequentially_and_stops_on_pending_checks() {
        let mut client = FakeClient::default();
        client.push_view(pr(1, Some("SUCCESS"), Some("CLEAN")));
        client.push_view(pr(2, Some("PENDING"), Some("CLEAN")));
        client.push_view(pr(3, Some("SUCCESS"), Some("CLEAN")));

        let output = land_prs_with_client(options(vec!["1", "2", "3"]), &mut client).unwrap();

        assert_eq!(client.merged, vec![1]);
        assert_eq!(output.summary.landed, 1);
        assert_eq!(output.summary.blocked, 1);
        assert_eq!(output.items.len(), 2);
    }

    #[test]
    fn retries_base_modified_race_after_recompute() {
        let mut client = FakeClient::default();
        client.push_view(pr(1, Some("SUCCESS"), Some("CLEAN")));
        client.push_view(pr(1, Some("SUCCESS"), Some("CLEAN")));
        client
            .merge_errors
            .push_back(Error::git_command_failed("Base branch was modified"));

        let output = land_prs_with_client(options(vec!["1"]), &mut client).unwrap();

        assert_eq!(client.merged, vec![1]);
        assert_eq!(output.summary.merge_retries, 1);
        assert_eq!(output.summary.landed, 1);
    }

    #[test]
    fn refreshes_dirty_pr_only_when_helper_is_configured() {
        let mut client = FakeClient::default();
        client.push_view(pr(1, Some("SUCCESS"), Some("BEHIND")));
        client.push_view(pr(1, Some("SUCCESS"), Some("CLEAN")));
        let mut opts = options(vec!["1"]);
        opts.refresh_helper = Some(PrLandRefreshHelper {
            program: "helper".to_string(),
            args: vec!["{repo}".to_string(), "{number}".to_string()],
        });

        let output = land_prs_with_client(opts, &mut client).unwrap();

        assert_eq!(client.refreshed, vec![1]);
        assert_eq!(client.merged, vec![1]);
        assert_eq!(output.summary.refreshed, 1);
    }
}
