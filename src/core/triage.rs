//! Read-only triage reports for component sets.
//!
//! The primitive resolves a target (component/project/fleet/rig) to component
//! references, then overlays GitHub issue/PR state. It intentionally keeps the
//! GitHub calls read-only so `homeboy triage ...` is safe as a dashboard verb.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use crate::component;
use crate::deploy::release_download::{detect_remote_url, parse_github_url, GitHubRepo};
use crate::error::{Error, Result};
use crate::git::gh_probe_succeeds;
use crate::{fleet, project, rig};

#[derive(Debug, Clone)]
pub enum TriageTarget {
    Component(String),
    Project(String),
    Fleet(String),
    Rig(String),
    Workspace,
}

impl TriageTarget {
    fn kind(&self) -> &'static str {
        match self {
            TriageTarget::Component(_) => "component",
            TriageTarget::Project(_) => "project",
            TriageTarget::Fleet(_) => "fleet",
            TriageTarget::Rig(_) => "rig",
            TriageTarget::Workspace => "workspace",
        }
    }

    fn id(&self) -> &str {
        match self {
            TriageTarget::Component(id)
            | TriageTarget::Project(id)
            | TriageTarget::Fleet(id)
            | TriageTarget::Rig(id) => id,
            TriageTarget::Workspace => "workspace",
        }
    }

    fn command(&self) -> &'static str {
        match self {
            TriageTarget::Component(_) => "triage.component",
            TriageTarget::Project(_) => "triage.project",
            TriageTarget::Fleet(_) => "triage.fleet",
            TriageTarget::Rig(_) => "triage.rig",
            TriageTarget::Workspace => "triage.workspace",
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
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
    triage_remote_url: Option<String>,
    sources: BTreeSet<String>,
    usage: BTreeSet<String>,
}

impl ComponentRef {
    fn new(
        component_id: String,
        local_path: String,
        remote_url: Option<String>,
        triage_remote_url: Option<String>,
        source: String,
    ) -> Self {
        let mut sources = BTreeSet::new();
        sources.insert(source);
        Self {
            component_id,
            local_path,
            remote_url,
            triage_remote_url,
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
                comp.triage_remote_url,
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
                    let remote_url = comp.as_ref().and_then(|c| c.remote_url.clone());
                    let triage_remote_url = comp.as_ref().and_then(|c| c.triage_remote_url.clone());
                    ComponentRef::new(
                        attachment.id.clone(),
                        if attachment.local_path.is_empty() {
                            comp.as_ref()
                                .map(|c| c.local_path.clone())
                                .unwrap_or_default()
                        } else {
                            attachment.local_path
                        },
                        remote_url,
                        triage_remote_url,
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
                    component_spec.triage_remote_url.clone(),
                    format!("rig:{rig_id}"),
                );
                component_ref.usage.insert(rig_id.to_string());
                refs.push(component_ref);
            }
            refs.sort_by(|a, b| a.component_id.cmp(&b.component_id));
            Ok(refs)
        }
        TriageTarget::Workspace => resolve_workspace_components(),
    }
}

fn resolve_workspace_components() -> Result<Vec<ComponentRef>> {
    let mut refs = BTreeMap::new();

    for proj in project::list()? {
        for attachment in proj.components {
            let comp = component::load(&attachment.id).ok();
            let remote_url = comp.as_ref().and_then(|c| c.remote_url.clone());
            let triage_remote_url = comp.as_ref().and_then(|c| c.triage_remote_url.clone());
            let mut component_ref = ComponentRef::new(
                attachment.id.clone(),
                if attachment.local_path.is_empty() {
                    comp.as_ref()
                        .map(|c| c.local_path.clone())
                        .unwrap_or_default()
                } else {
                    attachment.local_path
                },
                remote_url,
                triage_remote_url,
                format!("project:{}", proj.id),
            );
            component_ref.usage.insert(proj.id.clone());
            merge_component_ref(&mut refs, component_ref);
        }
    }

    for spec in rig::list()? {
        for (component_id, component_spec) in spec.components.iter() {
            let mut component_ref = ComponentRef::new(
                component_id.clone(),
                rig::expand::expand_vars(&spec, &component_spec.path),
                component_spec.remote_url.clone(),
                component_spec.triage_remote_url.clone(),
                format!("rig:{}", spec.id),
            );
            component_ref.usage.insert(spec.id.clone());
            merge_component_ref(&mut refs, component_ref);
        }
    }

    for comp in component::list()? {
        let source = format!("component:{}", comp.id);
        merge_component_ref(
            &mut refs,
            ComponentRef::new(
                comp.id,
                comp.local_path,
                comp.remote_url,
                comp.triage_remote_url,
                source,
            ),
        );
    }

    Ok(dedupe_refs_by_repo(refs.into_values().collect()))
}

fn merge_component_ref(refs: &mut BTreeMap<String, ComponentRef>, component_ref: ComponentRef) {
    let entry = refs
        .entry(component_ref.component_id.clone())
        .or_insert_with(|| component_ref.clone());
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
            }
            Err(_) => unresolved.push(component_ref),
        }
    }

    let mut refs: Vec<ComponentRef> = resolved.into_values().collect();
    refs.extend(unresolved);
    refs.sort_by(|a, b| a.component_id.cmp(&b.component_id));
    refs
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
            let remote_url = comp.as_ref().and_then(|c| c.remote_url.clone());
            let triage_remote_url = comp.as_ref().and_then(|c| c.triage_remote_url.clone());
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
                    remote_url.clone(),
                    triage_remote_url.clone(),
                    format!("fleet:{fleet_id}"),
                )
            });
            entry.sources.insert(format!("project:{project_id}"));
            entry.usage.insert(project_id.clone());
            if entry.remote_url.is_none() {
                entry.remote_url = remote_url;
            }
            if entry.triage_remote_url.is_none() {
                entry.triage_remote_url = triage_remote_url;
            }
            if entry.local_path.is_empty() && !attachment.local_path.is_empty() {
                entry.local_path = attachment.local_path;
            }
        }
    }

    Ok(refs.into_values().collect())
}

#[derive(Debug, Clone)]
struct ResolvedRepo {
    repo: GitHubRepo,
    triage_remote_url: Option<String>,
    source_repo: Option<GitHubRepo>,
}

fn resolve_repo(component_ref: &ComponentRef) -> std::result::Result<ResolvedRepo, String> {
    let source_remote_url = component_ref
        .remote_url
        .clone()
        .or_else(|| detect_remote_url(Path::new(&component_ref.local_path)));

    let triage_remote_url = component_ref
        .triage_remote_url
        .clone()
        .or_else(|| source_remote_url.clone())
        .ok_or_else(|| "missing_remote_url_and_no_git_origin".to_string())?;
    let repo = parse_github_url(&triage_remote_url).ok_or_else(|| {
        if component_ref.triage_remote_url.is_some() {
            "triage_remote_url_is_not_github".to_string()
        } else {
            "remote_url_is_not_github".to_string()
        }
    })?;

    let source_repo = source_remote_url
        .and_then(|url| parse_github_url(&url))
        .filter(|source| source.owner != repo.owner || source.repo != repo.repo);

    Ok(ResolvedRepo {
        repo,
        triage_remote_url: component_ref.triage_remote_url.clone(),
        source_repo,
    })
}

fn fetch_component_report(
    component_ref: &ComponentRef,
    resolved: ResolvedRepo,
    options: &TriageOptions,
) -> TriageComponentReport {
    let repo = resolved.repo;
    let repo_output = TriageRepo {
        provider: "github",
        owner: repo.owner.clone(),
        name: repo.repo.clone(),
        url: format!("https://github.com/{}/{}", repo.owner, repo.repo),
        source_repo: resolved.source_repo.map(|source| TriageRepoRef {
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
            let mut pr = TriagePrItem {
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
                next_action: None,
            };
            pr.next_action = derive_pr_next_action(&pr);
            pr
        })
        .collect())
}

fn derive_pr_next_action(pr: &TriagePrItem) -> Option<String> {
    let checks = pr.checks.as_deref();
    let review = pr.review_decision.as_deref();
    let merge = pr.merge_state.as_deref();

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
) -> Vec<TriageAction> {
    let mut actions = Vec::new();
    if let Some(prs) = pull_requests {
        let mut action_counts = BTreeMap::<String, usize>::new();
        for pr in &prs.items {
            if let Some(next_action) = &pr.next_action {
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
        "draft_with_failing_checks" | "checks_failed" | "approved_but_dirty" | "needs_rebase" => {
            "high"
        }
        "review_required" | "approved_but_pending_checks" | "clean_and_ready" => "medium",
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
        assert!(items[0].next_action.is_none());
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
            .map(|item| item.next_action.as_deref().unwrap())
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

        let actions = build_actions(None, Some(&prs));
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].kind, "checks_failed");
        assert_eq!(actions[0].severity, "high");
        assert_eq!(actions[0].label, "2 PRs have failed checks");
        assert_eq!(actions[1].kind, "review_required");
        assert_eq!(actions[2].kind, "clean_and_ready");
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
                    next_action: Some("checks_failed".to_string()),
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

    fn triage_pr_with_action(action: &str) -> TriagePrItem {
        TriagePrItem {
            number: 1,
            title: "PR".to_string(),
            url: "https://github.com/o/r/pull/1".to_string(),
            state: "OPEN".to_string(),
            draft: false,
            review_decision: None,
            checks: None,
            check_failures: Vec::new(),
            merge_state: None,
            labels: vec![],
            assignees: vec![],
            author: None,
            updated_at: None,
            stale: false,
            next_action: Some(action.to_string()),
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
}
