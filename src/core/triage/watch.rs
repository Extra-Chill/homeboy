use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration as StdDuration, Instant};

use crate::core::deploy::release_download::{parse_github_url, GitHubRepo};
use crate::core::error::{Error, Result};
use crate::core::git::gh_probe_succeeds;

use super::{non_empty, run_gh, summarize_checks};

#[derive(Debug, Clone)]
pub struct TriageWatchOptions {
    pub refs: Vec<String>,
    pub until: Option<String>,
    pub timeout: StdDuration,
    pub poll_interval: StdDuration,
    pub auto_merge: bool,
    pub merge_method: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageWatchOutput {
    pub command: &'static str,
    pub until: String,
    pub target_reached: bool,
    pub timed_out: bool,
    pub duration_ms: u128,
    pub watched: Vec<TriageWatchTargetOutput>,
    pub events: Vec<TriageWatchEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageWatchTargetOutput {
    pub reference: String,
    pub repo: String,
    pub number: u64,
    pub item_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_state: Option<TriageWatchItemState>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageWatchEvent {
    pub event: String,
    pub repo: String,
    pub number: u64,
    pub item_type: String,
    pub ts: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<TriageWatchItemState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TriageWatchItemState {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub draft: bool,
}

pub fn run(options: TriageWatchOptions) -> Result<TriageWatchOutput> {
    if options.refs.is_empty() {
        return Err(Error::validation_missing_argument(vec![
            "--watch".to_string()
        ]));
    }
    let refs = options
        .refs
        .iter()
        .map(|raw| parse_watch_ref(raw))
        .collect::<Result<Vec<_>>>()?;
    let until = options
        .until
        .unwrap_or_else(|| "target-default".to_string());
    validate_watch_until(&until)?;
    ensure_gh_ready().map_err(Error::internal_unexpected)?;

    let started = Instant::now();
    let deadline = started + options.timeout;
    let mut states: BTreeMap<String, TriageWatchItemState> = BTreeMap::new();
    let mut targets = refs
        .iter()
        .map(|reference| TriageWatchTargetOutput {
            reference: reference.raw.clone(),
            repo: reference.repo_slug(),
            number: reference.number,
            item_type: "unknown".to_string(),
            final_state: None,
        })
        .collect::<Vec<_>>();
    let mut events = Vec::new();
    let mut target_reached = false;
    let mut timed_out = false;

    loop {
        let mut all_reached = true;
        for (index, reference) in refs.iter().enumerate() {
            let item = fetch_watch_item(reference).map_err(Error::internal_unexpected)?;
            targets[index].item_type = item.item_type.clone();
            targets[index].final_state = Some(item.state.clone());
            let key = reference.key();
            if let Some(previous) = states.get(&key) {
                events.extend(watch_transition_events(reference, &item, previous));
            } else {
                events.push(watch_event(
                    reference,
                    &item,
                    "watch.started",
                    None,
                    None,
                    None,
                ));
            }
            let effective_until = effective_watch_until(&until, &item.item_type);
            let reached = watch_until_reached(effective_until, &item, states.get(&key));
            if reached
                && options.auto_merge
                && effective_until == "green-mergeable"
                && item.item_type == "pull_request"
            {
                merge_pr_rest(reference, &options.merge_method)
                    .map_err(Error::internal_unexpected)?;
                events.push(watch_event(
                    reference,
                    &item,
                    "pr.merge_requested",
                    None,
                    None,
                    Some(options.merge_method.clone()),
                ));
            }
            all_reached &= reached;
            states.insert(key, item.state);
        }

        if all_reached {
            target_reached = true;
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(std::cmp::min(options.poll_interval, remaining));
    }

    events.push(TriageWatchEvent {
        event: "watch.exit".to_string(),
        repo: refs
            .first()
            .map(TriageWatchRef::repo_slug)
            .unwrap_or_default(),
        number: refs
            .first()
            .map(|reference| reference.number)
            .unwrap_or_default(),
        item_type: "watch".to_string(),
        ts: Utc::now().to_rfc3339(),
        state: None,
        from: None,
        to: None,
        reason: Some(if target_reached {
            "target-reached".to_string()
        } else {
            "timeout".to_string()
        }),
    });

    Ok(TriageWatchOutput {
        command: "triage.watch",
        until,
        target_reached,
        timed_out,
        duration_ms: started.elapsed().as_millis(),
        watched: targets,
        events,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TriageWatchRef {
    raw: String,
    owner: String,
    repo: String,
    number: u64,
}

impl TriageWatchRef {
    fn repo_slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    fn key(&self) -> String {
        format!("{}#{}", self.repo_slug(), self.number)
    }
}

#[derive(Debug, Clone)]
struct TriageWatchItem {
    item_type: String,
    state: TriageWatchItemState,
}

fn parse_watch_ref(raw: &str) -> Result<TriageWatchRef> {
    let (repo, number) = raw.rsplit_once('#').ok_or_else(|| {
        Error::validation_invalid_argument(
            "--watch",
            "watch ref must look like owner/repo#123",
            Some(raw.to_string()),
            None,
        )
    })?;
    let number = number.parse::<u64>().map_err(|_| {
        Error::validation_invalid_argument(
            "--watch",
            "watch ref number must be a positive integer",
            Some(raw.to_string()),
            None,
        )
    })?;
    let repo = parse_github_url(repo).unwrap_or_else(|| GitHubRepo {
        host: "github.com".to_string(),
        owner: repo
            .split('/')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string(),
        repo: repo
            .split('/')
            .nth(1)
            .unwrap_or_default()
            .trim_end_matches(".git")
            .to_string(),
    });
    if repo.owner.is_empty() || repo.repo.is_empty() || number == 0 {
        return Err(Error::validation_invalid_argument(
            "--watch",
            "watch ref must look like owner/repo#123",
            Some(raw.to_string()),
            None,
        ));
    }
    Ok(TriageWatchRef {
        raw: raw.to_string(),
        owner: repo.owner,
        repo: repo.repo,
        number,
    })
}

fn validate_watch_until(until: &str) -> Result<()> {
    if matches!(
        until,
        "target-default"
            | "merged"
            | "closed"
            | "green"
            | "green-mergeable"
            | "failed"
            | "state-changed"
            | "commit-pushed"
    ) {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "--until",
        "unsupported watch target state",
        Some(until.to_string()),
        Some(vec![
            "merged".to_string(),
            "closed".to_string(),
            "target-default".to_string(),
            "green".to_string(),
            "green-mergeable".to_string(),
            "failed".to_string(),
            "state-changed".to_string(),
            "commit-pushed".to_string(),
        ]),
    ))
}

fn effective_watch_until<'a>(until: &'a str, item_type: &str) -> &'a str {
    if until != "target-default" {
        return until;
    }
    if item_type == "issue" {
        "closed"
    } else {
        "merged"
    }
}

fn watch_until_reached(
    until: &str,
    current: &TriageWatchItem,
    previous: Option<&TriageWatchItemState>,
) -> bool {
    match until {
        "merged" => current.state.merged_at.is_some() || current.state.state == "MERGED",
        "closed" => matches!(current.state.state.as_str(), "CLOSED" | "MERGED"),
        "green" => current.state.checks.as_deref() == Some("SUCCESS"),
        "green-mergeable" => {
            // A PR is only mergeable once its checks have reported SUCCESS on the
            // current head. `checks` is `None` when GitHub returns an empty
            // statusCheckRollup (e.g. the force-push window where mergeStateStatus
            // flips to CLEAN before CI registers on the new head SHA). Requiring an
            // explicit SUCCESS — never a bare CLEAN — keeps that race from merging
            // untested code (#4872).
            current.item_type == "pull_request"
                && !current.state.draft
                && current.state.checks.as_deref() == Some("SUCCESS")
                && current.state.merge_state.as_deref() == Some("CLEAN")
        }
        "failed" => current.state.checks.as_deref() == Some("FAILURE"),
        "state-changed" => previous.is_some_and(|previous| previous.state != current.state.state),
        "commit-pushed" => {
            previous.is_some_and(|previous| previous.head_sha != current.state.head_sha)
        }
        _ => false,
    }
}

fn watch_transition_events(
    reference: &TriageWatchRef,
    current: &TriageWatchItem,
    previous: &TriageWatchItemState,
) -> Vec<TriageWatchEvent> {
    let mut events = Vec::new();
    if previous.state != current.state.state {
        events.push(watch_event(
            reference,
            current,
            "item.state_changed",
            Some(previous.state.clone()),
            Some(current.state.state.clone()),
            None,
        ));
    }
    if previous.head_sha != current.state.head_sha {
        events.push(watch_event(
            reference,
            current,
            "pr.commit.pushed",
            previous.head_sha.clone(),
            current.state.head_sha.clone(),
            None,
        ));
    }
    if previous.checks != current.state.checks {
        events.push(watch_event(
            reference,
            current,
            "pr.ci.transitioned",
            previous.checks.clone(),
            current.state.checks.clone(),
            None,
        ));
    }
    if previous.merged_at.is_none() && current.state.merged_at.is_some() {
        events.push(watch_event(
            reference,
            current,
            "pr.merged",
            None,
            current.state.merged_at.clone(),
            None,
        ));
    }
    events
}

fn watch_event(
    reference: &TriageWatchRef,
    item: &TriageWatchItem,
    event: &str,
    from: Option<String>,
    to: Option<String>,
    reason: Option<String>,
) -> TriageWatchEvent {
    TriageWatchEvent {
        event: event.to_string(),
        repo: reference.repo_slug(),
        number: reference.number,
        item_type: item.item_type.clone(),
        ts: Utc::now().to_rfc3339(),
        state: Some(item.state.clone()),
        from,
        to,
        reason,
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

fn fetch_watch_item(reference: &TriageWatchRef) -> std::result::Result<TriageWatchItem, String> {
    match fetch_watch_pr(reference) {
        Ok(item) => Ok(item),
        Err(pr_error) => fetch_watch_issue(reference).map_err(|issue_error| {
            format!(
                "failed to fetch {} as PR ({}) or issue ({})",
                reference.key(),
                pr_error,
                issue_error
            )
        }),
    }
}

fn fetch_watch_pr(reference: &TriageWatchRef) -> std::result::Result<TriageWatchItem, String> {
    let args = vec![
        "pr".to_string(),
        "view".to_string(),
        reference.number.to_string(),
        "-R".to_string(),
        reference.repo_slug(),
        "--json".to_string(),
        "number,title,url,state,isDraft,reviewDecision,mergeStateStatus,statusCheckRollup,mergedAt,headRefOid".to_string(),
    ];
    let raw = run_gh(&args)?;
    let parsed: RawWatchPr = serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
    Ok(TriageWatchItem {
        item_type: "pull_request".to_string(),
        state: TriageWatchItemState {
            state: parsed.state,
            title: Some(parsed.title),
            url: Some(parsed.url),
            checks: summarize_checks(&parsed.status_check_rollup),
            review_decision: non_empty(parsed.review_decision),
            merge_state: non_empty(parsed.merge_state_status),
            head_sha: non_empty(parsed.head_ref_oid),
            merged_at: parsed.merged_at,
            draft: parsed.is_draft,
        },
    })
}

fn fetch_watch_issue(reference: &TriageWatchRef) -> std::result::Result<TriageWatchItem, String> {
    let args = vec![
        "issue".to_string(),
        "view".to_string(),
        reference.number.to_string(),
        "-R".to_string(),
        reference.repo_slug(),
        "--json".to_string(),
        "number,title,url,state".to_string(),
    ];
    let raw = run_gh(&args)?;
    let parsed: RawWatchIssue = serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
    Ok(TriageWatchItem {
        item_type: "issue".to_string(),
        state: TriageWatchItemState {
            state: parsed.state,
            title: Some(parsed.title),
            url: Some(parsed.url),
            checks: None,
            review_decision: None,
            merge_state: None,
            head_sha: None,
            merged_at: None,
            draft: false,
        },
    })
}

fn merge_pr_rest(reference: &TriageWatchRef, method: &str) -> std::result::Result<(), String> {
    let args = vec![
        "api".to_string(),
        "-X".to_string(),
        "PUT".to_string(),
        format!(
            "repos/{}/{}/pulls/{}/merge",
            reference.owner, reference.repo, reference.number
        ),
        "-f".to_string(),
        format!("merge_method={method}"),
    ];
    run_gh(&args).map(|_| ())
}

#[derive(Debug, Deserialize)]
struct RawWatchPr {
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
    #[serde(default, rename = "mergedAt")]
    merged_at: Option<String>,
    #[serde(default, rename = "headRefOid")]
    head_ref_oid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawWatchIssue {
    title: String,
    url: String,
    state: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_watch_ref_accepts_owner_repo_number() {
        let reference = parse_watch_ref("Extra-Chill/homeboy#2238").unwrap();

        assert_eq!(reference.owner, "Extra-Chill");
        assert_eq!(reference.repo, "homeboy");
        assert_eq!(reference.number, 2238);
        assert_eq!(reference.repo_slug(), "Extra-Chill/homeboy");
    }

    #[test]
    fn parse_watch_ref_accepts_github_url() {
        let reference = parse_watch_ref("https://github.com/Extra-Chill/homeboy#2238").unwrap();

        assert_eq!(reference.owner, "Extra-Chill");
        assert_eq!(reference.repo, "homeboy");
        assert_eq!(reference.number, 2238);
    }

    #[test]
    fn watch_until_green_mergeable_requires_clean_successful_pr() {
        let item = TriageWatchItem {
            item_type: "pull_request".to_string(),
            state: TriageWatchItemState {
                state: "OPEN".to_string(),
                title: None,
                url: None,
                checks: Some("SUCCESS".to_string()),
                review_decision: Some("APPROVED".to_string()),
                merge_state: Some("CLEAN".to_string()),
                head_sha: Some("abc123".to_string()),
                merged_at: None,
                draft: false,
            },
        };

        assert!(watch_until_reached("green-mergeable", &item, None));
        assert!(watch_until_reached("green", &item, None));
        assert!(!watch_until_reached("merged", &item, None));
    }

    #[test]
    fn watch_until_green_mergeable_rejects_clean_with_zero_checks() {
        // Force-push window: mergeStateStatus flips to CLEAN before CI registers on
        // the new head, so statusCheckRollup is empty and `checks` is None (#4872).
        // A bare CLEAN must not satisfy green-mergeable.
        let item = TriageWatchItem {
            item_type: "pull_request".to_string(),
            state: TriageWatchItemState {
                state: "OPEN".to_string(),
                title: None,
                url: None,
                checks: None,
                review_decision: Some("APPROVED".to_string()),
                merge_state: Some("CLEAN".to_string()),
                head_sha: Some("abc123".to_string()),
                merged_at: None,
                draft: false,
            },
        };

        assert!(!watch_until_reached("green-mergeable", &item, None));
        assert!(!watch_until_reached("green", &item, None));
    }

    #[test]
    fn watch_until_detects_transitions_from_previous_state() {
        let previous = TriageWatchItemState {
            state: "OPEN".to_string(),
            title: None,
            url: None,
            checks: Some("PENDING".to_string()),
            review_decision: None,
            merge_state: None,
            head_sha: Some("abc123".to_string()),
            merged_at: None,
            draft: false,
        };
        let current = TriageWatchItem {
            item_type: "pull_request".to_string(),
            state: TriageWatchItemState {
                state: "OPEN".to_string(),
                title: None,
                url: None,
                checks: Some("SUCCESS".to_string()),
                review_decision: None,
                merge_state: None,
                head_sha: Some("def456".to_string()),
                merged_at: None,
                draft: false,
            },
        };

        assert!(watch_until_reached(
            "commit-pushed",
            &current,
            Some(&previous)
        ));
        assert!(!watch_until_reached(
            "state-changed",
            &current,
            Some(&previous)
        ));
    }

    #[test]
    fn target_default_uses_closed_for_issues_and_merged_for_prs() {
        assert_eq!(effective_watch_until("target-default", "issue"), "closed");
        assert_eq!(
            effective_watch_until("target-default", "pull_request"),
            "merged"
        );
        assert_eq!(effective_watch_until("failed", "issue"), "failed");
    }
}
