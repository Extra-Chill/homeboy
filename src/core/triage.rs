//! Read-only triage reports for component sets.
//!
//! The primitive resolves a target (component/project/fleet/rig) to component
//! references, then overlays GitHub issue/PR state. It intentionally keeps the
//! GitHub calls read-only so `homeboy triage ...` is safe as a dashboard verb.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::component;
use crate::deploy::release_download::{detect_remote_url, parse_github_url, GitHubRepo};
use crate::error::{Error, Result};
use crate::{fleet, project, rig};

#[derive(Debug, Clone)]
pub enum TriageTarget {
    Component(String),
    Project(String),
    Fleet(String),
    Rig(String),
}

impl TriageTarget {
    fn kind(&self) -> &'static str {
        match self {
            TriageTarget::Component(_) => "component",
            TriageTarget::Project(_) => "project",
            TriageTarget::Fleet(_) => "fleet",
            TriageTarget::Rig(_) => "rig",
        }
    }

    fn id(&self) -> &str {
        match self {
            TriageTarget::Component(id)
            | TriageTarget::Project(id)
            | TriageTarget::Fleet(id)
            | TriageTarget::Rig(id) => id,
        }
    }

    fn command(&self) -> &'static str {
        match self {
            TriageTarget::Component(_) => "triage.component",
            TriageTarget::Project(_) => "triage.project",
            TriageTarget::Fleet(_) => "triage.fleet",
            TriageTarget::Rig(_) => "triage.rig",
        }
    }
}

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
    pub stale_days: Option<i64>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageOutput {
    pub command: &'static str,
    pub target: TriageTargetOutput,
    pub summary: TriageSummary,
    pub components: Vec<TriageComponentReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<TriageUnresolved>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageTargetOutput {
    pub kind: &'static str,
    pub id: String,
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
}

#[derive(Debug, Clone, Serialize)]
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub check_failures: Vec<TriageCheckFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_state: Option<String>,
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

#[derive(Debug, Clone)]
struct ComponentRef {
    component_id: String,
    local_path: String,
    remote_url: Option<String>,
    sources: BTreeSet<String>,
    usage: BTreeSet<String>,
}

impl ComponentRef {
    fn new(
        component_id: String,
        local_path: String,
        remote_url: Option<String>,
        source: String,
    ) -> Self {
        let mut sources = BTreeSet::new();
        sources.insert(source);
        Self {
            component_id,
            local_path,
            remote_url,
            sources,
            usage: BTreeSet::new(),
        }
    }
}

pub fn run(target: TriageTarget, options: TriageOptions) -> Result<TriageOutput> {
    let refs = resolve_target_components(&target)?;
    let mut components = Vec::new();
    let mut unresolved = Vec::new();

    for component_ref in refs {
        match resolve_repo(&component_ref) {
            Ok(repo) => components.push(fetch_component_report(&component_ref, repo, &options)),
            Err(reason) => unresolved.push(TriageUnresolved {
                component_id: component_ref.component_id,
                local_path: component_ref.local_path,
                reason,
                sources: component_ref.sources.into_iter().collect(),
            }),
        }
    }

    let summary = summarize(&components, &unresolved);
    Ok(TriageOutput {
        command: target.command(),
        target: TriageTargetOutput {
            kind: target.kind(),
            id: target.id().to_string(),
        },
        summary,
        components,
        unresolved,
    })
}

fn resolve_target_components(target: &TriageTarget) -> Result<Vec<ComponentRef>> {
    match target {
        TriageTarget::Component(component_id) => {
            let comp = component::load(component_id)?;
            Ok(vec![ComponentRef::new(
                comp.id,
                comp.local_path,
                comp.remote_url,
                format!("component:{component_id}"),
            )])
        }
        TriageTarget::Project(project_id) => {
            let proj = project::load(project_id)?;
            Ok(proj
                .components
                .into_iter()
                .map(|attachment| {
                    let comp = component::load(&attachment.id).ok();
                    ComponentRef::new(
                        attachment.id.clone(),
                        if attachment.local_path.is_empty() {
                            comp.as_ref()
                                .map(|c| c.local_path.clone())
                                .unwrap_or_default()
                        } else {
                            attachment.local_path
                        },
                        comp.and_then(|c| c.remote_url),
                        format!("project:{project_id}"),
                    )
                })
                .collect())
        }
        TriageTarget::Fleet(fleet_id) => resolve_fleet_components(fleet_id),
        TriageTarget::Rig(rig_id) => {
            let spec = rig::load(rig_id)?;
            let mut refs = Vec::new();
            for (component_id, component_spec) in spec.components.iter() {
                let path = rig::expand::expand_vars(&spec, &component_spec.path);
                let mut component_ref = ComponentRef::new(
                    component_id.clone(),
                    path,
                    component_spec.remote_url.clone(),
                    format!("rig:{rig_id}"),
                );
                component_ref.usage.insert(rig_id.to_string());
                refs.push(component_ref);
            }
            refs.sort_by(|a, b| a.component_id.cmp(&b.component_id));
            Ok(refs)
        }
    }
}

fn resolve_fleet_components(fleet_id: &str) -> Result<Vec<ComponentRef>> {
    let fl = fleet::load(fleet_id)?;
    let mut refs: BTreeMap<String, ComponentRef> = BTreeMap::new();

    for project_id in &fl.project_ids {
        let Ok(proj) = project::load(project_id) else {
            continue;
        };
        for attachment in proj.components {
            let comp = component::load(&attachment.id).ok();
            let entry = refs.entry(attachment.id.clone()).or_insert_with(|| {
                ComponentRef::new(
                    attachment.id.clone(),
                    if attachment.local_path.is_empty() {
                        comp.as_ref()
                            .map(|c| c.local_path.clone())
                            .unwrap_or_default()
                    } else {
                        attachment.local_path.clone()
                    },
                    comp.as_ref().and_then(|c| c.remote_url.clone()),
                    format!("fleet:{fleet_id}"),
                )
            });
            entry.sources.insert(format!("project:{project_id}"));
            entry.usage.insert(project_id.clone());
            if entry.remote_url.is_none() {
                entry.remote_url = comp.and_then(|c| c.remote_url);
            }
            if entry.local_path.is_empty() && !attachment.local_path.is_empty() {
                entry.local_path = attachment.local_path;
            }
        }
    }

    Ok(refs.into_values().collect())
}

fn resolve_repo(component_ref: &ComponentRef) -> std::result::Result<GitHubRepo, String> {
    let remote_url = component_ref
        .remote_url
        .clone()
        .or_else(|| detect_remote_url(Path::new(&component_ref.local_path)))
        .ok_or_else(|| "missing_remote_url_and_no_git_origin".to_string())?;

    parse_github_url(&remote_url).ok_or_else(|| "remote_url_is_not_github".to_string())
}

fn fetch_component_report(
    component_ref: &ComponentRef,
    repo: GitHubRepo,
    options: &TriageOptions,
) -> TriageComponentReport {
    let repo_output = TriageRepo {
        provider: "github",
        owner: repo.owner.clone(),
        name: repo.repo.clone(),
        url: format!("https://github.com/{}/{}", repo.owner, repo.repo),
    };
    let stale_cutoff = options
        .stale_days
        .map(|days| Utc::now() - Duration::days(days));

    let mut error = None;
    let issues = if options.include_issues {
        match fetch_issues(&repo, options, stale_cutoff) {
            Ok(items) => Some(TriageIssueBucket {
                open: items.len(),
                items,
            }),
            Err(e) => {
                error = Some(e);
                Some(TriageIssueBucket {
                    open: 0,
                    items: Vec::new(),
                })
            }
        }
    } else {
        None
    };

    let pull_requests = if options.include_prs {
        match fetch_prs(&repo, options, stale_cutoff) {
            Ok(items) => Some(TriagePrBucket {
                open: items.len(),
                items,
            }),
            Err(e) => {
                error = Some(match error {
                    Some(existing) => format!("{existing}; {e}"),
                    None => e,
                });
                Some(TriagePrBucket {
                    open: 0,
                    items: Vec::new(),
                })
            }
        }
    } else {
        None
    };

    let actions = build_actions(issues.as_ref(), pull_requests.as_ref());

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

fn fetch_issues(
    repo: &GitHubRepo,
    options: &TriageOptions,
    stale_cutoff: Option<DateTime<Utc>>,
) -> std::result::Result<Vec<TriageIssueItem>, String> {
    ensure_gh_ready()?;
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
        "number,title,url,state,labels,assignees,updatedAt".to_string(),
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

fn fetch_prs(
    repo: &GitHubRepo,
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
        "number,title,url,state,isDraft,reviewDecision,mergeStateStatus,statusCheckRollup,labels,assignees,author,updatedAt".to_string(),
    ];
    if options.mine {
        args.push("--author".to_string());
        args.push("@me".to_string());
    }
    for label in &options.labels {
        args.push("--label".to_string());
        args.push(label.clone());
    }

    let raw = run_gh(&args)?;
    let mut items = parse_prs(&raw, stale_cutoff, options.drilldown)?;
    if options.needs_review {
        items.retain(|item| item.review_decision.as_deref() == Some("REVIEW_REQUIRED"));
    }
    if options.failing_checks {
        items.retain(|item| item.checks.as_deref() == Some("FAILURE"));
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

fn gh_probe_succeeds(args: &[&str]) -> bool {
    Command::new("gh")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<String>,
}

fn parse_issues(
    raw: &str,
    stale_cutoff: Option<DateTime<Utc>>,
) -> std::result::Result<Vec<TriageIssueItem>, String> {
    let parsed: Vec<RawIssue> = serde_json::from_str(raw.trim()).map_err(|e| e.to_string())?;
    Ok(parsed
        .into_iter()
        .map(|item| {
            let stale = is_stale(item.updated_at.as_deref(), stale_cutoff);
            TriageIssueItem {
                number: item.number,
                title: item.title,
                url: item.url,
                state: item.state,
                labels: item.labels.into_iter().filter_map(|l| l.name).collect(),
                assignees: item.assignees.into_iter().filter_map(|a| a.login).collect(),
                updated_at: item.updated_at,
                stale,
            }
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
            TriagePrItem {
                number: item.number,
                title: item.title,
                url: item.url,
                state: item.state,
                draft: item.is_draft,
                review_decision: non_empty(item.review_decision),
                checks: summarize_checks(&item.status_check_rollup),
                check_failures: if include_drilldown {
                    summarize_check_failures(&item.status_check_rollup)
                } else {
                    Vec::new()
                },
                merge_state: non_empty(item.merge_state_status),
                labels: item.labels.into_iter().filter_map(|l| l.name).collect(),
                assignees: item.assignees.into_iter().filter_map(|a| a.login).collect(),
                author: item.author.and_then(|a| a.login),
                updated_at: item.updated_at,
                stale,
            }
        })
        .collect())
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
) -> Vec<TriageAction> {
    let mut actions = Vec::new();
    if let Some(prs) = pull_requests {
        let failing = prs
            .items
            .iter()
            .filter(|pr| pr.checks.as_deref() == Some("FAILURE"))
            .count();
        if failing > 0 {
            actions.push(TriageAction {
                kind: "failing_checks".to_string(),
                severity: "high".to_string(),
                label: pluralize(failing, "PR has failing checks", "PRs have failing checks"),
            });
        }
        let needs_review = prs
            .items
            .iter()
            .filter(|pr| pr.review_decision.as_deref() == Some("REVIEW_REQUIRED"))
            .count();
        if needs_review > 0 {
            actions.push(TriageAction {
                kind: "review_required".to_string(),
                severity: "medium".to_string(),
                label: pluralize(needs_review, "PR needs review", "PRs need review"),
            });
        }
        let stale = prs.items.iter().filter(|pr| pr.stale).count();
        if stale > 0 {
            actions.push(TriageAction {
                kind: "stale_prs".to_string(),
                severity: "low".to_string(),
                label: pluralize(stale, "stale PR", "stale PRs"),
            });
        }
    }
    if let Some(issues) = issues {
        let urgent = issues
            .items
            .iter()
            .filter(|issue| {
                issue
                    .labels
                    .iter()
                    .any(|label| matches!(label.as_str(), "security" | "P0" | "P1" | "bug"))
            })
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
            .filter(|issue| issue.labels.is_empty() && issue.assignees.is_empty())
            .count();
        if untriaged > 0 {
            actions.push(TriageAction {
                kind: "untriaged_issues".to_string(),
                severity: "low".to_string(),
                label: pluralize(untriaged, "untriaged issue", "untriaged issues"),
            });
        }
        let stale = issues.items.iter().filter(|issue| issue.stale).count();
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
            summary.stale += issues.items.iter().filter(|item| item.stale).count();
        }
        if let Some(prs) = &component.pull_requests {
            summary.open_prs += prs.open;
            summary.needs_review += prs
                .items
                .iter()
                .filter(|item| item.review_decision.as_deref() == Some("REVIEW_REQUIRED"))
                .count();
            summary.failing_checks += prs
                .items
                .iter()
                .filter(|item| item.checks.as_deref() == Some("FAILURE"))
                .count();
            summary.stale += prs.items.iter().filter(|item| item.stale).count();
        }
        summary.actions += component.actions.len();
    }
    summary
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

#[allow(dead_code)]
fn _pathbuf(path: &str) -> PathBuf {
    PathBuf::from(path)
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
              "updatedAt": "2026-04-26T00:00:00Z"
            }
        ]"#;
        let items = parse_prs(raw, None, false).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].author.as_deref(), Some("chubes4"));
        assert!(items[0].review_decision.is_none());
        assert!(items[0].merge_state.is_none());
        assert!(items[0].check_failures.is_empty());
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
        assert_eq!(without_drilldown[0].checks.as_deref(), Some("FAILURE"));
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
                        stale: false,
                    },
                    TriageIssueItem {
                        number: 3,
                        title: "Needs triage".to_string(),
                        url: "https://github.com/o/r/issues/3".to_string(),
                        state: "OPEN".to_string(),
                        labels: vec![],
                        assignees: vec![],
                        updated_at: None,
                        stale: false,
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
                    review_decision: Some("REVIEW_REQUIRED".to_string()),
                    checks: Some("FAILURE".to_string()),
                    check_failures: Vec::new(),
                    merge_state: None,
                    labels: vec![],
                    assignees: vec![],
                    author: None,
                    updated_at: None,
                    stale: false,
                }],
            }),
            actions: vec![TriageAction {
                kind: "failing_checks".to_string(),
                severity: "high".to_string(),
                label: "1 PR has failing checks".to_string(),
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
}
