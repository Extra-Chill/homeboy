//! Read-only triage reports for component sets.
//!
//! The primitive resolves a scope (component/project/fleet/rig/path/workspace) to component
//! references, then overlays GitHub issue/PR state. It intentionally keeps the
//! GitHub calls read-only so `homeboy triage ...` is safe as a dashboard verb.
//! The separate `triage --watch --auto-merge` path is the explicit opt-in
//! exception for state-transition automation.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration as StdDuration, Instant};

use crate::core::defaults;
use crate::core::deploy::release_download::{detect_remote_url, parse_github_url, GitHubRepo};
use crate::core::error::{Error, Result};
use crate::core::git::gh_probe_succeeds;
use crate::core::observation::{
    NewRunRecord, NewTriageItemRecord, ObservationStore, RunListFilter, RunStatus,
    TriageItemRecord, TriagePullRequestSignals,
};
use crate::core::scope::{self, Scope, ScopeComponentRef, ScopeKind, ScopeOutput};

pub use crate::core::scope::Scope as TriageTarget;

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

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum TriageCommandOutput {
    Report(TriageOutput),
    Watch(TriageWatchOutput),
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

pub fn watch(options: TriageWatchOptions) -> Result<TriageWatchOutput> {
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

struct TriageObservation {
    store: ObservationStore,
    run_id: String,
    store_path: String,
    previous_run_id: Option<String>,
    previous_run_at: Option<String>,
}

impl TriageObservation {
    fn start(target: &TriageTarget, options: &TriageOptions) -> Option<Self> {
        let store = ObservationStore::open_initialized().ok()?;
        let component_id = triage_observation_component_id(target);
        let metadata = triage_observation_metadata(target, options);
        let previous_run = store
            .latest_run(RunListFilter {
                kind: Some("triage".to_string()),
                component_id: Some(component_id.clone()),
                status: None,
                rig_id: None,
                limit: Some(1),
            })
            .ok()
            .flatten()
            .filter(|run| run.metadata_json == metadata);
        let store_path = store
            .status()
            .map(|status| status.path)
            .unwrap_or_else(|_| "<unavailable>".to_string());
        let cwd = std::env::current_dir().ok();
        let run = store
            .start_run(
                NewRunRecord::builder("triage")
                    .component_id(component_id)
                    .command(triage_command(target))
                    .optional_cwd_path(cwd.as_deref())
                    .current_homeboy_version()
                    .optional_rig_id(match target {
                        TriageTarget::Rig(id) => Some(id.clone()),
                        _ => None,
                    })
                    .metadata(metadata)
                    .build(),
            )
            .ok()?;

        Some(Self {
            store,
            run_id: run.id,
            store_path,
            previous_run_id: previous_run.as_ref().map(|run| run.id.clone()),
            previous_run_at: previous_run.map(|run| run.started_at),
        })
    }

    fn finish(self, output: &TriageOutput) -> Option<TriageObservationOutput> {
        let items = triage_observation_items(&self.run_id, output);
        let item_count = items.len();
        let previous_items = self
            .previous_run_id
            .as_deref()
            .and_then(|run_id| self.store.list_triage_items_for_run(run_id).ok());
        let comparison = self
            .previous_run_id
            .as_ref()
            .zip(previous_items.as_ref())
            .map(|(previous_run_id, previous_items)| {
                compare_triage_observations(previous_run_id, previous_items, &items)
            });
        let record_result = self.store.record_triage_items(&items);
        let status = if record_result.is_ok() {
            RunStatus::Pass
        } else {
            RunStatus::Error
        };
        let _ = self.store.finish_run(
            &self.run_id,
            status,
            Some(serde_json::json!({
                "summary": output.summary,
                "item_count": item_count,
                "recorded": record_result.is_ok(),
            })),
        );

        if record_result.is_err() {
            return None;
        }

        Some(TriageObservationOutput {
            run_id: self.run_id,
            item_count,
            store_path: self.store_path,
            previous_run_at: self.previous_run_at,
            comparison,
        })
    }
}

fn triage_observation_metadata(target: &TriageTarget, options: &TriageOptions) -> Value {
    serde_json::json!({
        "target": {
            "kind": target.kind_name(),
            "id": target.id(),
        },
        "options": {
            "include_issues": options.include_issues,
            "include_prs": options.include_prs,
            "mine": options.mine,
            "assigned": options.assigned,
            "labels": options.labels,
            "needs_review": options.needs_review,
            "failing_checks": options.failing_checks,
            "drilldown": options.drilldown,
            "issue_numbers": options.issue_numbers,
            "stale_days": options.stale_days,
            "limit": options.limit,
        }
    })
}

type TriageObservationItemKey = (String, String, String, String, u64);

fn compare_triage_observations(
    previous_run_id: &str,
    previous_items: &[TriageItemRecord],
    current_items: &[NewTriageItemRecord],
) -> TriageObservationComparison {
    let previous_by_key: BTreeMap<_, _> = previous_items
        .iter()
        .map(|item| (triage_record_key(item), item))
        .collect();
    let current_by_key: BTreeMap<_, _> = current_items
        .iter()
        .map(|item| (triage_new_item_key(item), item))
        .collect();

    let new_items = current_by_key
        .iter()
        .filter(|(key, _)| !previous_by_key.contains_key(*key))
        .map(|(_, item)| triage_new_item_ref(item))
        .collect();
    let resolved_items = previous_by_key
        .iter()
        .filter(|(key, _)| !current_by_key.contains_key(*key))
        .map(|(_, item)| triage_record_item_ref(item))
        .collect();
    let changed_items = current_by_key
        .iter()
        .filter_map(|(key, current)| {
            let previous = previous_by_key.get(key)?;
            let changed_fields = triage_changed_fields(previous, current);
            if changed_fields.is_empty() {
                return None;
            }
            Some(TriageObservationChangedItem {
                item: triage_new_item_ref(current),
                changed_fields,
            })
        })
        .collect();

    TriageObservationComparison {
        previous_run_id: previous_run_id.to_string(),
        previous_item_count: previous_items.len(),
        new_items,
        resolved_items,
        changed_items,
    }
}

fn triage_record_key(item: &TriageItemRecord) -> TriageObservationItemKey {
    (
        item.provider.clone(),
        item.repo_owner.clone(),
        item.repo_name.clone(),
        item.item_type.clone(),
        item.number,
    )
}

fn triage_new_item_key(item: &NewTriageItemRecord) -> TriageObservationItemKey {
    (
        item.provider.clone(),
        item.repo_owner.clone(),
        item.repo_name.clone(),
        item.item_type.clone(),
        item.number,
    )
}

fn triage_record_item_ref(item: &TriageItemRecord) -> TriageObservationItemRef {
    TriageObservationItemRef {
        repo: format!("{}/{}", item.repo_owner, item.repo_name),
        item_type: item.item_type.clone(),
        number: item.number,
        title: item.title.clone(),
        url: item.url.clone(),
    }
}

fn triage_new_item_ref(item: &NewTriageItemRecord) -> TriageObservationItemRef {
    TriageObservationItemRef {
        repo: format!("{}/{}", item.repo_owner, item.repo_name),
        item_type: item.item_type.clone(),
        number: item.number,
        title: item.title.clone(),
        url: item.url.clone(),
    }
}

fn triage_changed_fields(
    previous: &TriageItemRecord,
    current: &NewTriageItemRecord,
) -> Vec<String> {
    let mut fields = Vec::new();
    push_if_changed(&mut fields, "state", &previous.state, &current.state);
    push_if_changed(&mut fields, "title", &previous.title, &current.title);
    push_if_changed(&mut fields, "url", &previous.url, &current.url);
    push_if_changed(
        &mut fields,
        "checks",
        &previous.signals.checks,
        &current.signals.checks,
    );
    push_if_changed(
        &mut fields,
        "review_decision",
        &previous.signals.review_decision,
        &current.signals.review_decision,
    );
    push_if_changed_unless_unknown(
        &mut fields,
        "merge_state",
        &previous.signals.merge_state,
        &current.signals.merge_state,
    );
    push_if_changed(
        &mut fields,
        "next_action",
        &previous.signals.next_action,
        &current.signals.next_action,
    );
    push_if_changed(
        &mut fields,
        "comments_count",
        &previous.signals.comments_count,
        &current.signals.comments_count,
    );
    push_if_changed(
        &mut fields,
        "reviews_count",
        &previous.signals.reviews_count,
        &current.signals.reviews_count,
    );
    push_if_changed(
        &mut fields,
        "last_comment_at",
        &previous.signals.last_comment_at,
        &current.signals.last_comment_at,
    );
    push_if_changed(
        &mut fields,
        "last_review_at",
        &previous.signals.last_review_at,
        &current.signals.last_review_at,
    );
    push_if_changed(
        &mut fields,
        "updated_at",
        &previous.updated_at,
        &current.updated_at,
    );
    fields
}

fn push_if_changed<T: PartialEq>(fields: &mut Vec<String>, field: &str, previous: &T, current: &T) {
    if previous != current {
        fields.push(field.to_string());
    }
}

fn push_if_changed_unless_unknown(
    fields: &mut Vec<String>,
    field: &str,
    previous: &Option<String>,
    current: &Option<String>,
) {
    if previous == current
        || previous.as_deref() == Some("UNKNOWN")
        || current.as_deref() == Some("UNKNOWN")
    {
        return;
    }
    fields.push(field.to_string());
}

fn triage_observation_component_id(target: &TriageTarget) -> String {
    format!("{}:{}", target.kind_name(), target.id())
}

fn triage_observation_items(run_id: &str, output: &TriageOutput) -> Vec<NewTriageItemRecord> {
    let mut records = Vec::new();
    for component in &output.components {
        if let Some(issues) = &component.issues {
            for issue in &issues.items {
                records.push(NewTriageItemRecord {
                    run_id: run_id.to_string(),
                    provider: component.repo.provider.to_string(),
                    repo_owner: component.repo.owner.clone(),
                    repo_name: component.repo.name.clone(),
                    item_type: "issue".to_string(),
                    number: issue.number,
                    state: issue.state.clone(),
                    title: issue.title.clone(),
                    url: issue.url.clone(),
                    signals: TriagePullRequestSignals {
                        comments_count: issue.comments_count.and_then(usize_to_i64),
                        last_comment_at: issue.last_comment_at.clone(),
                        next_action: if issue.stale {
                            Some("stale_issue".to_string())
                        } else {
                            None
                        },
                        ..TriagePullRequestSignals::default()
                    },
                    updated_at: issue.updated_at.clone(),
                    metadata_json: serde_json::json!({
                        "component_id": component.component_id,
                        "labels": issue.labels,
                        "assignees": issue.assignees,
                        "linked_prs": issue.linked_prs,
                    }),
                });
            }
        }
        if let Some(prs) = &component.pull_requests {
            for pr in &prs.items {
                records.push(NewTriageItemRecord {
                    run_id: run_id.to_string(),
                    provider: component.repo.provider.to_string(),
                    repo_owner: component.repo.owner.clone(),
                    repo_name: component.repo.name.clone(),
                    item_type: "pull_request".to_string(),
                    number: pr.number,
                    state: pr.state.clone(),
                    title: pr.title.clone(),
                    url: pr.url.clone(),
                    signals: pr.signals.clone(),
                    updated_at: pr.updated_at.clone(),
                    metadata_json: serde_json::json!({
                        "component_id": component.component_id,
                        "draft": pr.draft,
                        "labels": pr.labels,
                        "assignees": pr.assignees,
                        "author": pr.author,
                        "check_failures": pr.check_failures,
                    }),
                });
            }
        }
    }
    records
}

fn usize_to_i64(value: usize) -> Option<i64> {
    i64::try_from(value).ok()
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

fn run_gh(args: &[String]) -> std::result::Result<String, String> {
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

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn summarize_checks(checks: &[Value]) -> Option<String> {
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
    "approved_but_pending_checks",
    "clean_and_ready",
    "stale_pr",
];

fn pr_action_severity(kind: &str) -> &'static str {
    match kind {
        "draft_with_failing_checks" | "checks_failed" | "approved_but_dirty" => "high",
        "needs_rebase" | "review_required" | "approved_but_pending_checks" | "clean_and_ready" => {
            "medium"
        }
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
mod tests {
    use super::*;

    #[test]
    fn parse_stale_days_accepts_plain_or_d_suffix() {
        assert_eq!(parse_stale_days("14").unwrap(), 14);
        assert_eq!(parse_stale_days("14d").unwrap(), 14);
        assert!(parse_stale_days("0d").is_err());
        assert!(parse_stale_days("two-weeks").is_err());
    }

    #[test]
    fn dedupe_refs_by_repo_merges_sources_and_usage() {
        let mut project_ref = ComponentRef::new(
            "intelligence".to_string(),
            "/tmp/intelligence".to_string(),
            Some("https://github.com/Automattic/intelligence.git".to_string()),
            None,
            "project:intelligence-chubes4".to_string(),
        );
        project_ref.usage.insert("intelligence-chubes4".to_string());

        let mut rig_ref = ComponentRef::new(
            "intelligence-dev".to_string(),
            "/tmp/intelligence-dev".to_string(),
            Some("git@github.com:Automattic/intelligence.git".to_string()),
            None,
            "rig:intelligence-chubes4".to_string(),
        );
        rig_ref.usage.insert("intelligence-chubes4".to_string());

        let component_ref = ComponentRef::new(
            "standalone".to_string(),
            "/tmp/standalone".to_string(),
            Some("https://github.com/Extra-Chill/standalone.git".to_string()),
            None,
            "component:standalone".to_string(),
        );

        let refs = dedupe_refs_by_repo(vec![project_ref, rig_ref, component_ref]);

        assert_eq!(refs.len(), 2);
        let intelligence = refs
            .iter()
            .find(|component_ref| component_ref.component_id == "intelligence")
            .expect("first ref for the repo should be retained");
        assert_eq!(
            intelligence.sources.iter().cloned().collect::<Vec<_>>(),
            vec![
                "project:intelligence-chubes4".to_string(),
                "rig:intelligence-chubes4".to_string(),
            ]
        );
        assert_eq!(
            intelligence.usage.iter().cloned().collect::<Vec<_>>(),
            vec!["intelligence-chubes4".to_string()]
        );
    }

    #[test]
    fn dedupe_refs_by_repo_keeps_unresolved_entries_separate() {
        let resolved = ComponentRef::new(
            "data-machine".to_string(),
            "/tmp/data-machine".to_string(),
            Some("https://github.com/Extra-Chill/data-machine.git".to_string()),
            None,
            "component:data-machine".to_string(),
        );
        let unresolved = ComponentRef::new(
            "local-only".to_string(),
            "".to_string(),
            None,
            None,
            "component:local-only".to_string(),
        );

        let refs = dedupe_refs_by_repo(vec![unresolved, resolved]);

        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.component_id == "data-machine"));
        assert!(refs.iter().any(|r| r.component_id == "local-only"));
    }

    #[test]
    fn unresolved_summary_is_visible_when_targets_fail_to_resolve() {
        let unresolved = vec![TriageUnresolved {
            component_id: "missing".to_string(),
            local_path: "/tmp/missing".to_string(),
            reason: "local path does not exist".to_string(),
            sources: vec!["workspace".to_string()],
        }];

        assert_eq!(
            summarize_unresolved(&unresolved).as_deref(),
            Some("1 unresolved component target(s): missing (/tmp/missing) - local path does not exist;")
        );
    }

    #[test]
    fn parse_issues_marks_stale_and_extracts_labels() {
        let raw = r#"[
            {
              "number": 7,
              "title": "Fix auth",
              "url": "https://github.com/o/r/issues/7",
              "state": "OPEN",
              "labels": [{"name":"P1"}],
              "assignees": [{"login":"chubes4"}],
              "updatedAt": "2026-01-01T00:00:00Z"
            }
        ]"#;
        let cutoff = Some(
            DateTime::parse_from_rfc3339("2026-02-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        let items = parse_issues(raw, cutoff).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].labels, vec!["P1"]);
        assert_eq!(items[0].assignees, vec!["chubes4"]);
        assert!(items[0].stale);
        assert!(items[0].linked_prs.is_empty());
    }

    #[test]
    fn parse_issue_accepts_single_issue_view_payload() {
        let raw = r#"{
          "number": 8,
          "title": "Closed bug",
          "url": "https://github.com/o/r/issues/8",
              "state": "CLOSED",
              "labels": [],
              "assignees": [],
              "comments": [
                {"createdAt":"2026-04-02T00:00:00Z","updatedAt":"2026-04-03T00:00:00Z"},
                {"createdAt":"2026-04-04T00:00:00Z","updatedAt":null}
              ],
              "updatedAt": "2026-04-01T00:00:00Z"
        }"#;

        let item = parse_issue(raw, None).unwrap();

        assert_eq!(item.number, 8);
        assert_eq!(item.state, "CLOSED");
        assert_eq!(item.comments_count, Some(2));
        assert_eq!(
            item.last_comment_at.as_deref(),
            Some("2026-04-04T00:00:00Z")
        );
        assert!(item.linked_prs.is_empty());
    }

    #[test]
    fn issue_bucket_counts_only_open_targeted_issues() {
        let bucket = issue_bucket(vec![
            TriageIssueItem {
                number: 1,
                title: "Open".to_string(),
                url: "https://github.com/o/r/issues/1".to_string(),
                state: "OPEN".to_string(),
                labels: vec![],
                assignees: vec![],
                updated_at: None,
                comments_count: None,
                last_comment_at: None,
                stale: false,
                linked_prs: Vec::new(),
            },
            TriageIssueItem {
                number: 2,
                title: "Closed".to_string(),
                url: "https://github.com/o/r/issues/2".to_string(),
                state: "CLOSED".to_string(),
                labels: vec![],
                assignees: vec![],
                updated_at: None,
                comments_count: None,
                last_comment_at: None,
                stale: false,
                linked_prs: Vec::new(),
            },
        ]);

        assert_eq!(bucket.open, 1);
        assert_eq!(bucket.items.len(), 2);
    }

    #[test]
    fn issue_actions_ignore_closed_targeted_issues() {
        let issues = TriageIssueBucket {
            open: 0,
            items: vec![TriageIssueItem {
                number: 1,
                title: "Closed".to_string(),
                url: "https://github.com/o/r/issues/1".to_string(),
                state: "CLOSED".to_string(),
                labels: vec!["P1".to_string()],
                assignees: vec![],
                updated_at: None,
                comments_count: None,
                last_comment_at: None,
                stale: true,
                linked_prs: Vec::new(),
            }],
        };

        let actions = build_actions(Some(&issues), None, &default_priority_labels_vec());

        assert!(actions.is_empty());
    }

    #[test]
    fn parse_linked_prs_extracts_merge_timestamp() {
        let raw = r#"[
            {
              "number": 12,
              "title": "Fix auth",
              "url": "https://github.com/o/r/pull/12",
              "state": "MERGED",
              "mergedAt": "2026-04-03T00:00:00Z"
            },
            {
              "number": 13,
              "title": "Follow-up",
              "url": "https://github.com/o/r/pull/13",
              "state": "OPEN",
              "mergedAt": null
            }
        ]"#;

        let items = parse_linked_prs(raw).unwrap();

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].number, 12);
        assert_eq!(items[0].merged_at.as_deref(), Some("2026-04-03T00:00:00Z"));
        assert!(items[1].merged_at.is_none());
    }

    #[test]
    fn parse_issue_numbers_allows_hash_prefix_and_comments() {
        let parsed = parse_issue_numbers("# first comment\n1531\n#1538\n\n1501\n").unwrap();

        assert_eq!(parsed, vec![1531, 1538, 1501]);
        assert!(parse_issue_numbers("1531\nabc\n").is_err());
    }

    #[test]
    fn summarize_checks_prefers_failures_over_pending() {
        let checks: Vec<Value> = serde_json::from_str(
            r#"[
                {"status":"IN_PROGRESS","conclusion":null},
                {"status":"COMPLETED","conclusion":"FAILURE"}
            ]"#,
        )
        .unwrap();
        assert_eq!(summarize_checks(&checks).as_deref(), Some("FAILURE"));
    }

    #[test]
    fn summarize_checks_reports_pending_and_success() {
        let pending: Vec<Value> =
            serde_json::from_str(r#"[{"status":"IN_PROGRESS","conclusion":null}]"#).unwrap();
        assert_eq!(summarize_checks(&pending).as_deref(), Some("PENDING"));

        let success: Vec<Value> =
            serde_json::from_str(r#"[{"status":"COMPLETED","conclusion":"SUCCESS"}]"#).unwrap();
        assert_eq!(summarize_checks(&success).as_deref(), Some("SUCCESS"));
    }

    #[test]
    fn parse_prs_omits_empty_optional_fields() {
        let raw = r#"[
            {
              "number": 9,
              "title": "Docs",
              "url": "https://github.com/o/r/pull/9",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "",
              "mergeStateStatus": "",
              "statusCheckRollup": [],
              "labels": [],
              "assignees": [],
              "author": {"login":"chubes4"},
              "comments": [{"createdAt":"2026-04-27T00:00:00Z","updatedAt":null}],
              "reviews": [{"submittedAt":"2026-04-28T00:00:00Z"}],
              "updatedAt": "2026-04-26T00:00:00Z"
            }
        ]"#;
        let items = parse_prs(raw, None, false).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].author.as_deref(), Some("chubes4"));
        assert!(items[0].signals.review_decision.is_none());
        assert!(items[0].signals.merge_state.is_none());
        assert!(items[0].check_failures.is_empty());
        assert!(items[0].signals.next_action.is_none());
        assert_eq!(items[0].signals.comments_count, Some(1));
        assert_eq!(items[0].signals.reviews_count, Some(1));
        assert_eq!(
            items[0].signals.last_comment_at.as_deref(),
            Some("2026-04-27T00:00:00Z")
        );
        assert_eq!(
            items[0].signals.last_review_at.as_deref(),
            Some("2026-04-28T00:00:00Z")
        );
    }

    #[test]
    fn parse_prs_adds_compact_check_failure_drilldown_only_when_requested() {
        let raw = r#"[
            {
              "number": 10,
              "title": "Fix tests",
              "url": "https://github.com/o/r/pull/10",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": null,
              "mergeStateStatus": "DIRTY",
              "statusCheckRollup": [
                {
                  "__typename": "CheckRun",
                  "name": "test / unit",
                  "workflowName": "CI",
                  "status": "COMPLETED",
                  "conclusion": "FAILURE",
                  "detailsUrl": "https://github.com/o/r/actions/runs/1/job/2"
                },
                {
                  "__typename": "StatusContext",
                  "context": "lint",
                  "status": "COMPLETED",
                  "conclusion": "SUCCESS",
                  "targetUrl": "https://example.test/lint"
                },
                {
                  "__typename": "CheckRun",
                  "workflowName": "CI",
                  "status": "COMPLETED",
                  "conclusion": "TIMED_OUT",
                  "detailsUrl": ""
                }
              ],
              "labels": [],
              "assignees": [],
              "author": {"login":"chubes4"},
              "updatedAt": "2026-04-26T00:00:00Z"
            }
        ]"#;

        let without_drilldown = parse_prs(raw, None, false).unwrap();
        assert_eq!(
            without_drilldown[0].signals.checks.as_deref(),
            Some("FAILURE")
        );
        assert!(without_drilldown[0].check_failures.is_empty());

        let with_drilldown = parse_prs(raw, None, true).unwrap();
        assert_eq!(with_drilldown[0].check_failures.len(), 2);
        assert_eq!(
            with_drilldown[0].check_failures[0].workflow.as_deref(),
            Some("CI")
        );
        assert_eq!(with_drilldown[0].check_failures[0].name, "test / unit");
        assert_eq!(
            with_drilldown[0].check_failures[0].url.as_deref(),
            Some("https://github.com/o/r/actions/runs/1/job/2")
        );
        assert_eq!(with_drilldown[0].check_failures[1].name, "unknown check");
        assert!(with_drilldown[0].check_failures[1].url.is_none());
    }

    #[test]
    fn parse_prs_derives_next_action_labels() {
        let raw = r#"[
            {
              "number": 1,
              "title": "Broken checks",
              "url": "https://github.com/o/r/pull/1",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "",
              "mergeStateStatus": "CLEAN",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"FAILURE"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"chubes4"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 2,
              "title": "Approved dirty",
              "url": "https://github.com/o/r/pull/2",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "APPROVED",
              "mergeStateStatus": "DIRTY",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"chubes4"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 3,
              "title": "Ready",
              "url": "https://github.com/o/r/pull/3",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "APPROVED",
              "mergeStateStatus": "CLEAN",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"chubes4"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 4,
              "title": "Needs eyes",
              "url": "https://github.com/o/r/pull/4",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "REVIEW_REQUIRED",
              "mergeStateStatus": "CLEAN",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"chubes4"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 5,
              "title": "Pending",
              "url": "https://github.com/o/r/pull/5",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "APPROVED",
              "mergeStateStatus": "CLEAN",
              "statusCheckRollup": [{"status":"IN_PROGRESS","conclusion":null}],
              "labels": [],
              "assignees": [],
              "author": {"login":"chubes4"},
              "updatedAt": "2026-04-26T00:00:00Z"
            }
        ]"#;

        let items = parse_prs(raw, None, false).unwrap();
        let actions: Vec<_> = items
            .iter()
            .map(|item| item.signals.next_action.as_deref().unwrap())
            .collect();
        assert_eq!(
            actions,
            vec![
                "checks_failed",
                "approved_but_dirty",
                "clean_and_ready",
                "review_required",
                "approved_but_pending_checks",
            ]
        );
    }

    #[test]
    fn parse_prs_marks_behind_and_dirty_as_needs_rebase() {
        let raw = r#"[
            {
              "number": 1,
              "title": "Behind",
              "url": "https://github.com/o/r/pull/1",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "",
              "mergeStateStatus": "BEHIND",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"chubes4"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 2,
              "title": "Dirty",
              "url": "https://github.com/o/r/pull/2",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "",
              "mergeStateStatus": "DIRTY",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"chubes4"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 3,
              "title": "Unstable",
              "url": "https://github.com/o/r/pull/3",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "",
              "mergeStateStatus": "UNSTABLE",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"chubes4"},
              "updatedAt": "2026-04-26T00:00:00Z"
            }
        ]"#;

        let items = parse_prs(raw, None, false).unwrap();
        assert_eq!(
            items[0].signals.next_action.as_deref(),
            Some("needs_rebase")
        );
        assert_eq!(
            items[1].signals.next_action.as_deref(),
            Some("needs_rebase")
        );
        assert!(items[2].signals.next_action.is_none());

        let actions = build_actions(
            None,
            Some(&TriagePrBucket {
                open: items.len(),
                items,
            }),
            &[],
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, "needs_rebase");
        assert_eq!(actions[0].severity, "medium");
        assert_eq!(actions[0].label, "2 PRs need rebase");
    }

    #[test]
    fn build_actions_prioritizes_pr_next_actions() {
        let prs = TriagePrBucket {
            open: 4,
            items: vec![
                triage_pr_with_action("clean_and_ready"),
                triage_pr_with_action("checks_failed"),
                triage_pr_with_action("review_required"),
                triage_pr_with_action("checks_failed"),
            ],
        };

        let priority_labels = default_priority_labels_vec();
        let actions = build_actions(None, Some(&prs), &priority_labels);
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].kind, "checks_failed");
        assert_eq!(actions[0].severity, "high");
        assert_eq!(actions[0].label, "2 PRs have failed checks");
        assert_eq!(actions[1].kind, "review_required");
        assert_eq!(actions[2].kind, "clean_and_ready");
    }

    #[test]
    fn priority_actions_use_default_labels_when_unconfigured() {
        let component_ref = ComponentRef::new(
            "data-machine".to_string(),
            "/tmp/data-machine".to_string(),
            None,
            Some("https://github.com/Extra-Chill/data-machine.git".to_string()),
            "component:data-machine".to_string(),
        );
        let labels = resolve_priority_labels(&component_ref, None);
        let issues = issues_with_labels(vec![vec!["bug"], vec!["polish"]]);

        let actions = build_actions(Some(&issues), None, &labels);

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, "priority_issues");
        assert_eq!(actions[0].label, "1 priority issue");
    }

    #[test]
    fn component_priority_labels_override_global_labels() {
        let component_ref = ComponentRef::new(
            "data-machine".to_string(),
            "/tmp/data-machine".to_string(),
            None,
            Some("https://github.com/Extra-Chill/data-machine.git".to_string()),
            "component:data-machine".to_string(),
        )
        .with_priority_labels(Some(vec!["urgent".to_string()]));
        let global = vec!["bug".to_string()];
        let labels = resolve_priority_labels(&component_ref, Some(&global));
        let issues = issues_with_labels(vec![vec!["bug"], vec!["urgent"]]);

        let actions = build_actions(Some(&issues), None, &labels);

        assert_eq!(labels, vec!["urgent".to_string()]);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].label, "1 priority issue");
    }

    #[test]
    fn global_priority_labels_apply_when_component_and_fleet_unset() {
        let component_ref = ComponentRef::new(
            "data-machine".to_string(),
            "/tmp/data-machine".to_string(),
            None,
            Some("https://github.com/Extra-Chill/data-machine.git".to_string()),
            "component:data-machine".to_string(),
        );
        let global = vec!["critical".to_string()];
        let labels = resolve_priority_labels(&component_ref, Some(&global));
        let issues = issues_with_labels(vec![vec!["bug"], vec!["critical"]]);

        let actions = build_actions(Some(&issues), None, &labels);

        assert_eq!(labels, vec!["critical".to_string()]);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].label, "1 priority issue");
    }

    #[test]
    fn fleet_priority_labels_apply_to_fleet_components() {
        crate::test_support::with_isolated_home(|home| {
            let component_dir = home.path().join(".config/homeboy/components");
            let project_dir = home.path().join(".config/homeboy/projects/site");
            let fleet_dir = home.path().join(".config/homeboy/fleets");
            std::fs::create_dir_all(&component_dir).unwrap();
            std::fs::create_dir_all(&project_dir).unwrap();
            std::fs::create_dir_all(&fleet_dir).unwrap();
            std::fs::write(
                component_dir.join("data-machine.json"),
                r#"{
                    "local_path": "/tmp/data-machine",
                    "remote_url": "https://github.com/Extra-Chill/data-machine.git"
                }"#,
            )
            .unwrap();
            std::fs::write(
                project_dir.join("site.json"),
                r#"{
                    "components": [
                        {"id": "data-machine", "local_path": "/tmp/data-machine"}
                    ]
                }"#,
            )
            .unwrap();
            std::fs::write(
                fleet_dir.join("growth.json"),
                r#"{
                    "project_ids": ["site"],
                    "priority_labels": ["release-blocker"]
                }"#,
            )
            .unwrap();

            let refs = resolve_target_components(&TriageTarget::Fleet("growth".into())).unwrap();

            assert_eq!(refs.len(), 1);
            assert_eq!(
                refs[0].priority_labels,
                Some(vec!["release-blocker".to_string()])
            );
        });
    }

    #[test]
    fn summarize_counts_component_actions() {
        let component = TriageComponentReport {
            component_id: "data-machine".to_string(),
            local_path: "/tmp/data-machine".to_string(),
            sources: vec!["component:data-machine".to_string()],
            usage: vec![],
            repo: TriageRepo {
                provider: "github",
                owner: "Extra-Chill".to_string(),
                name: "data-machine".to_string(),
                url: "https://github.com/Extra-Chill/data-machine".to_string(),
                source_repo: None,
                triage_remote_url: None,
            },
            issues: Some(TriageIssueBucket {
                open: 2,
                items: vec![
                    TriageIssueItem {
                        number: 1,
                        title: "Bug".to_string(),
                        url: "https://github.com/o/r/issues/1".to_string(),
                        state: "OPEN".to_string(),
                        labels: vec!["P1".to_string()],
                        assignees: vec![],
                        updated_at: None,
                        comments_count: None,
                        last_comment_at: None,
                        stale: false,
                        linked_prs: Vec::new(),
                    },
                    TriageIssueItem {
                        number: 3,
                        title: "Needs triage".to_string(),
                        url: "https://github.com/o/r/issues/3".to_string(),
                        state: "OPEN".to_string(),
                        labels: vec![],
                        assignees: vec![],
                        updated_at: None,
                        comments_count: None,
                        last_comment_at: None,
                        stale: false,
                        linked_prs: Vec::new(),
                    },
                ],
            }),
            pull_requests: Some(TriagePrBucket {
                open: 1,
                items: vec![TriagePrItem {
                    number: 2,
                    title: "Fix".to_string(),
                    url: "https://github.com/o/r/pull/2".to_string(),
                    state: "OPEN".to_string(),
                    draft: false,
                    signals: TriagePullRequestSignals {
                        checks: Some("FAILURE".to_string()),
                        review_decision: Some("REVIEW_REQUIRED".to_string()),
                        next_action: Some("checks_failed".to_string()),
                        ..TriagePullRequestSignals::default()
                    },
                    check_failures: Vec::new(),
                    labels: vec![],
                    assignees: vec![],
                    author: None,
                    updated_at: None,
                    stale: false,
                }],
            }),
            actions: vec![TriageAction {
                kind: "checks_failed".to_string(),
                severity: "high".to_string(),
                label: "1 PR has failed checks".to_string(),
            }],
            error: None,
        };

        let summary = summarize(&[component], &[]);
        assert_eq!(summary.components, 1);
        assert_eq!(summary.open_issues, 2);
        assert_eq!(summary.open_prs, 1);
        assert_eq!(summary.needs_review, 1);
        assert_eq!(summary.failing_checks, 1);
        assert_eq!(summary.actions, 1);
    }

    #[test]
    fn compare_triage_observations_reports_new_resolved_and_changed_items() {
        let previous = vec![
            stored_triage_item(1, "Old issue", None),
            stored_triage_item(2, "Resolved issue", None),
            stored_triage_item(3, "Changed PR", Some("review_required")),
        ];
        let current = vec![
            new_triage_item("current-run", 1, "Old issue", None),
            new_triage_item("current-run", 3, "Changed PR", Some("checks_failed")),
            new_triage_item("current-run", 4, "New issue", None),
        ];

        let comparison = compare_triage_observations("previous-run", &previous, &current);

        assert_eq!(comparison.previous_run_id, "previous-run");
        assert_eq!(comparison.previous_item_count, 3);
        assert_eq!(comparison.new_items.len(), 1);
        assert_eq!(comparison.new_items[0].number, 4);
        assert_eq!(comparison.resolved_items.len(), 1);
        assert_eq!(comparison.resolved_items[0].number, 2);
        assert_eq!(comparison.changed_items.len(), 1);
        assert_eq!(comparison.changed_items[0].item.number, 3);
        assert_eq!(
            comparison.changed_items[0].changed_fields,
            vec!["next_action"]
        );
    }

    #[test]
    fn triage_observation_metadata_distinguishes_personal_and_firehose_runs() {
        let personal = TriageOptions {
            mine: true,
            ..TriageOptions::default()
        };
        let firehose = TriageOptions {
            mine: false,
            ..TriageOptions::default()
        };

        assert_ne!(
            triage_observation_metadata(&TriageTarget::Workspace, &personal),
            triage_observation_metadata(&TriageTarget::Workspace, &firehose)
        );
    }

    #[test]
    fn compare_triage_observations_ignores_unknown_merge_state_flaps() {
        let mut previous = stored_triage_item(1, "Flappy PR", Some("checks_failed"));
        previous.signals.merge_state = Some("UNKNOWN".to_string());
        let mut current = new_triage_item("current-run", 1, "Flappy PR", Some("checks_failed"));
        current.signals.merge_state = Some("DIRTY".to_string());

        let comparison = compare_triage_observations("previous-run", &[previous], &[current]);

        assert!(comparison.changed_items.is_empty());
    }

    fn triage_pr_with_action(action: &str) -> TriagePrItem {
        TriagePrItem {
            number: 1,
            title: "PR".to_string(),
            url: "https://github.com/o/r/pull/1".to_string(),
            state: "OPEN".to_string(),
            draft: false,
            signals: TriagePullRequestSignals {
                next_action: Some(action.to_string()),
                ..TriagePullRequestSignals::default()
            },
            check_failures: Vec::new(),
            labels: vec![],
            assignees: vec![],
            author: None,
            updated_at: None,
            stale: false,
        }
    }

    fn stored_triage_item(number: u64, title: &str, next_action: Option<&str>) -> TriageItemRecord {
        TriageItemRecord {
            id: format!("item-{number}"),
            run_id: "previous-run".to_string(),
            provider: "github".to_string(),
            repo_owner: "Extra-Chill".to_string(),
            repo_name: "homeboy".to_string(),
            item_type: "pull_request".to_string(),
            number,
            state: "OPEN".to_string(),
            title: title.to_string(),
            url: format!("https://github.com/Extra-Chill/homeboy/pull/{number}"),
            signals: TriagePullRequestSignals {
                next_action: next_action.map(str::to_string),
                ..TriagePullRequestSignals::default()
            },
            updated_at: None,
            metadata_json: serde_json::json!({}),
            observed_at: "2026-05-08T12:00:00Z".to_string(),
        }
    }

    fn new_triage_item(
        run_id: &str,
        number: u64,
        title: &str,
        next_action: Option<&str>,
    ) -> NewTriageItemRecord {
        NewTriageItemRecord {
            run_id: run_id.to_string(),
            provider: "github".to_string(),
            repo_owner: "Extra-Chill".to_string(),
            repo_name: "homeboy".to_string(),
            item_type: "pull_request".to_string(),
            number,
            state: "OPEN".to_string(),
            title: title.to_string(),
            url: format!("https://github.com/Extra-Chill/homeboy/pull/{number}"),
            signals: TriagePullRequestSignals {
                next_action: next_action.map(str::to_string),
                ..TriagePullRequestSignals::default()
            },
            updated_at: None,
            metadata_json: serde_json::json!({}),
        }
    }

    fn default_priority_labels_vec() -> Vec<String> {
        DEFAULT_PRIORITY_LABELS
            .iter()
            .map(|label| label.to_string())
            .collect()
    }

    fn issues_with_labels(labels: Vec<Vec<&str>>) -> TriageIssueBucket {
        TriageIssueBucket {
            open: labels.len(),
            items: labels
                .into_iter()
                .enumerate()
                .map(|(index, labels)| TriageIssueItem {
                    number: index as u64 + 1,
                    title: format!("Issue {}", index + 1),
                    url: format!("https://github.com/o/r/issues/{}", index + 1),
                    state: "OPEN".to_string(),
                    labels: labels.into_iter().map(str::to_string).collect(),
                    assignees: vec![],
                    updated_at: None,
                    comments_count: None,
                    last_comment_at: None,
                    stale: false,
                    linked_prs: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn resolve_repo_prefers_triage_remote_without_losing_source_repo() {
        let component_ref = ComponentRef::new(
            "playground".to_string(),
            "/tmp/playground".to_string(),
            Some("https://github.com/chubes4/wordpress-playground.git".to_string()),
            Some("https://github.com/WordPress/wordpress-playground.git".to_string()),
            "component:playground".to_string(),
        );

        let resolved = resolve_repo(&component_ref).unwrap();

        assert_eq!(resolved.repo.owner, "WordPress");
        assert_eq!(resolved.repo.repo, "wordpress-playground");
        assert_eq!(
            resolved.triage_remote_url.as_deref(),
            Some("https://github.com/WordPress/wordpress-playground.git")
        );
        let source = resolved.source_repo.expect("source repo differs");
        assert_eq!(source.owner, "chubes4");
        assert_eq!(source.repo, "wordpress-playground");
    }

    #[test]
    fn resolve_repo_allows_triage_remote_without_git_source_remote() {
        let component_ref = ComponentRef::new(
            "playground".to_string(),
            "/tmp/not-a-git-repo".to_string(),
            None,
            Some("https://github.com/WordPress/wordpress-playground.git".to_string()),
            "rig:studio".to_string(),
        );

        let resolved = resolve_repo(&component_ref).unwrap();

        assert_eq!(resolved.repo.owner, "WordPress");
        assert_eq!(resolved.repo.repo, "wordpress-playground");
        assert!(resolved.source_repo.is_none());
    }

    #[test]
    fn resolve_repo_uses_parent_repo_for_fork_without_triage_remote() {
        let component_ref = ComponentRef::new(
            "playground".to_string(),
            "/tmp/playground".to_string(),
            Some("https://github.com/chubes4/wordpress-playground.git".to_string()),
            None,
            "component:playground".to_string(),
        );

        let resolved = resolve_repo_with_parent_resolver(&component_ref, |repo| {
            assert_eq!(repo.owner, "chubes4");
            assert_eq!(repo.repo, "wordpress-playground");
            Ok(Some(GitHubRepo {
                host: "github.com".to_string(),
                owner: "WordPress".to_string(),
                repo: "wordpress-playground".to_string(),
            }))
        })
        .unwrap();

        assert_eq!(resolved.repo.owner, "WordPress");
        assert_eq!(resolved.repo.repo, "wordpress-playground");
        assert!(resolved.triage_remote_url.is_none());
        let source = resolved.source_repo.expect("source repo is fork");
        assert_eq!(source.owner, "chubes4");
        assert_eq!(source.repo, "wordpress-playground");
    }

    #[test]
    fn parse_github_parent_repo_returns_parent_for_fork() {
        let parent = parse_github_parent_repo(
            r#"{
                "isFork": true,
                "parent": {
                    "name": "wordpress-playground",
                    "owner": { "login": "WordPress" }
                }
            }"#,
        )
        .unwrap()
        .expect("fork parent");

        assert_eq!(parent.owner, "WordPress");
        assert_eq!(parent.repo, "wordpress-playground");
    }

    #[test]
    fn parse_github_parent_repo_ignores_non_forks() {
        let parent = parse_github_parent_repo(
            r#"{
                "isFork": false,
                "parent": null
            }"#,
        )
        .unwrap();

        assert!(parent.is_none());
    }

    #[test]
    fn fetch_component_report_surfaces_source_repo_when_triage_differs() {
        let component_ref = ComponentRef::new(
            "playground".to_string(),
            "/tmp/playground".to_string(),
            Some("https://github.com/chubes4/wordpress-playground.git".to_string()),
            Some("https://github.com/WordPress/wordpress-playground.git".to_string()),
            "rig:studio".to_string(),
        );
        let resolved = resolve_repo(&component_ref).unwrap();

        let report = fetch_component_report(
            &component_ref,
            resolved,
            &TriageOptions {
                include_issues: false,
                include_prs: false,
                ..Default::default()
            },
            None,
        );

        assert_eq!(report.repo.owner, "WordPress");
        assert_eq!(report.repo.name, "wordpress-playground");
        assert_eq!(
            report.repo.triage_remote_url.as_deref(),
            Some("https://github.com/WordPress/wordpress-playground.git")
        );
        assert_eq!(
            report.repo.source_repo,
            Some(TriageRepoRef {
                owner: "chubes4".to_string(),
                name: "wordpress-playground".to_string(),
                url: "https://github.com/chubes4/wordpress-playground".to_string(),
            })
        );
    }

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

    #[test]
    fn component_target_threads_registered_triage_remote_override() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("playground");
            std::fs::create_dir_all(&checkout).unwrap();
            let component_dir = home.path().join(".config/homeboy/components");
            std::fs::create_dir_all(&component_dir).unwrap();
            std::fs::write(
                component_dir.join("playground.json"),
                format!(
                    r#"{{
                    "local_path": "{}",
                    "remote_url": "https://github.com/chubes4/wordpress-playground.git",
                    "triage_remote_url": "https://github.com/WordPress/wordpress-playground.git"
                }}"#,
                    checkout.display()
                ),
            )
            .unwrap();

            let refs =
                resolve_target_components(&TriageTarget::Component("playground".into())).unwrap();

            assert_eq!(refs.len(), 1);
            assert_eq!(
                refs[0].triage_remote_url.as_deref(),
                Some("https://github.com/WordPress/wordpress-playground.git")
            );
            assert_eq!(
                resolve_repo(&refs[0]).unwrap().repo.owner,
                "WordPress".to_string()
            );
        });
    }

    #[test]
    fn rig_target_threads_rig_component_triage_remote_override() {
        crate::test_support::with_isolated_home(|home| {
            let rig_dir = home.path().join(".config/homeboy/rigs");
            std::fs::create_dir_all(&rig_dir).unwrap();
            std::fs::write(
                rig_dir.join("studio.json"),
                r#"{
                    "id": "studio",
                    "components": {
                        "playground": {
                            "path": "/tmp/playground",
                            "remote_url": "https://github.com/chubes4/wordpress-playground.git",
                            "triage_remote_url": "https://github.com/WordPress/wordpress-playground.git"
                        }
                    }
                }"#,
            )
            .unwrap();

            let refs = resolve_target_components(&TriageTarget::Rig("studio".into())).unwrap();

            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0].component_id, "playground");
            assert_eq!(
                refs[0].triage_remote_url.as_deref(),
                Some("https://github.com/WordPress/wordpress-playground.git")
            );
            assert_eq!(
                resolve_repo(&refs[0]).unwrap().repo.owner,
                "WordPress".to_string()
            );
        });
    }

    #[test]
    fn path_target_synthesizes_component_from_git_origin() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("ad-hoc-checkout");
            std::fs::create_dir_all(&checkout).unwrap();
            let status = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());
            let status = std::process::Command::new("git")
                .args([
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/Extra-Chill/homeboy.git",
                ])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());

            let target = TriageTarget::Path {
                path: checkout.to_string_lossy().into_owned(),
                component_id: None,
            };
            let refs = resolve_target_components(&target).unwrap();
            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0].component_id, "ad-hoc-checkout");
            assert_eq!(
                refs[0].remote_url.as_deref(),
                Some("https://github.com/Extra-Chill/homeboy.git")
            );
            let repo = resolve_repo(&refs[0]).unwrap().repo;
            assert_eq!(repo.owner, "Extra-Chill");
            assert_eq!(repo.repo, "homeboy");
        });
    }

    #[test]
    fn path_target_uses_explicit_component_id_when_provided() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("checkout-dir");
            std::fs::create_dir_all(&checkout).unwrap();
            let status = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());
            let status = std::process::Command::new("git")
                .args([
                    "remote",
                    "add",
                    "origin",
                    "git@github.com:Extra-Chill/homeboy.git",
                ])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());

            let target = TriageTarget::Path {
                path: checkout.to_string_lossy().into_owned(),
                component_id: Some("homeboy".into()),
            };
            let refs = resolve_target_components(&target).unwrap();
            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0].component_id, "homeboy");
            let repo = resolve_repo(&refs[0]).unwrap().repo;
            assert_eq!(repo.owner, "Extra-Chill");
            assert_eq!(repo.repo, "homeboy");
        });
    }

    #[test]
    fn path_target_surfaces_remote_url_is_not_github_for_non_github_origin() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("non-github");
            std::fs::create_dir_all(&checkout).unwrap();
            let status = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());
            let status = std::process::Command::new("git")
                .args(["remote", "add", "origin", "https://gitlab.com/foo/bar.git"])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());

            let target = TriageTarget::Path {
                path: checkout.to_string_lossy().into_owned(),
                component_id: None,
            };
            let refs = resolve_target_components(&target).unwrap();
            let err = resolve_repo(&refs[0]).unwrap_err();
            assert_eq!(err, "remote_url_is_not_github");
        });
    }

    #[test]
    fn path_target_rejects_missing_directory() {
        let target = TriageTarget::Path {
            path: "/definitely/does/not/exist/triage-path-test".into(),
            component_id: None,
        };
        let err = resolve_target_components(&target).unwrap_err();
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
    }

    #[test]
    fn path_target_rejects_non_git_directory() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("not-a-git-repo");
            std::fs::create_dir_all(&checkout).unwrap();

            let target = TriageTarget::Path {
                path: checkout.to_string_lossy().into_owned(),
                component_id: None,
            };
            let err = resolve_target_components(&target).unwrap_err();
            assert_eq!(err.code.as_str(), "validation.invalid_argument");
        });
    }
}
