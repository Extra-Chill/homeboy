use clap::Args;
use homeboy::core::component;
use homeboy::core::context;
use homeboy::core::deploy::{self, DeployConfig, ReleaseStateStatus};
use homeboy::core::git;
use homeboy::core::release::version;
use homeboy::core::scope::{self, Scope};
use serde::Serialize;

use super::CmdResult;

#[derive(Args)]
pub struct StatusArgs {
    /// Project ID — show version dashboard for a project's components
    pub project: Option<String>,

    /// Inspect this checkout path instead of the registered component path
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,

    /// Show the full workspace/context report (the old init behavior)
    #[arg(long)]
    pub full: bool,

    /// Show only components with uncommitted changes
    #[arg(long)]
    pub uncommitted: bool,

    /// Show only components that need a release
    #[arg(long)]
    pub needs_release: bool,

    /// Show only components ready to deploy
    #[arg(long)]
    pub ready: bool,

    /// Show only components with docs-only changes
    #[arg(long)]
    pub docs_only: bool,

    /// Show all components regardless of current directory context
    #[arg(long, short = 'a')]
    pub all: bool,

    /// Show only outdated components (local != remote)
    #[arg(long)]
    pub outdated: bool,
}

/// Per-component upstream drift info.
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamDrift {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
    /// Latest tag on origin (e.g. "v0.8.0").
    /// Differs from local version when the local checkout is stale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_origin_tag: Option<String>,
}

impl UpstreamDrift {
    fn is_behind(&self) -> bool {
        self.behind.unwrap_or(0) > 0
    }
}

/// Clarifying note emitted alongside the git-state-only `ready_to_deploy` list.
///
/// `ready_to_deploy` reflects local git/workspace state ("has a clean release
/// tag that *could* be deployed"), NOT whether the deploy target is actually
/// behind. Acting on it blindly re-deploys components that may already be live.
/// For a target-accurate diff (installed version vs latest release tag) run
/// `homeboy status <project>`, which reports `current` / `outdated` per
/// component. See issue #4588.
const READY_TO_DEPLOY_NOTE: &str = "ready_to_deploy is git-state-only (components with a clean release tag that *could* be deployed); it does NOT mean the deploy target is behind. Run `homeboy status <project>` for a target-accurate diff (installed version vs latest release tag).";

#[derive(Debug, Serialize)]
pub struct StatusOutput {
    pub command: &'static str,
    pub total: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncommitted: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub needs_release: Vec<String>,
    /// Components in a clean release state (no uncommitted changes, no commits
    /// since the last version tag).
    ///
    /// IMPORTANT: this is git/workspace state only. It means "has a release tag
    /// that *could* be deployed", NOT "the deployed target is behind the latest
    /// release". For a target-accurate deploy diff, use `homeboy status
    /// <project>` (compares installed-on-target versions against release tags).
    /// See `ready_to_deploy_note` and issue #4588.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ready_to_deploy: Vec<String>,
    /// Clarifying note, emitted only when `ready_to_deploy` is non-empty, so
    /// operators don't mistake the git-state list for a real deploy backlog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_to_deploy_note: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub docs_only: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub behind_upstream: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub upstream_drift: Vec<UpstreamDrift>,
    pub clean: usize,
}

/// A single row in the project status dashboard.
#[derive(Debug, Serialize)]
pub struct ProjectStatusRow {
    pub component_id: String,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    /// Latest tag on origin. When this differs from local_version, the local
    /// checkout is stale and needs a pull.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_version: Option<String>,
    pub unreleased_commits: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead_upstream: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind_upstream: Option<u32>,
    pub status: ProjectComponentDashboardStatus,
}

/// Status indicator for the project dashboard.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectComponentDashboardStatus {
    /// Local and remote versions match, no unreleased commits
    Current,
    /// Local and remote versions match, but a newer origin tag exists
    PinnedCurrent,
    /// Local version differs from remote (needs deploy)
    Outdated,
    /// Releasable code commits since the current version baseline
    NeedsRelease,
    /// Only docs changes since last tag
    DocsOnly,
    /// Uncommitted changes in working directory
    Uncommitted,
    /// Local branch is behind upstream (needs pull)
    BehindUpstream,
    /// Cannot determine status
    Unknown,
}

/// Output for the project status dashboard.
#[derive(Debug, Serialize)]
pub struct ProjectDashboardOutput {
    pub command: &'static str,
    pub project_id: String,
    pub total: usize,
    pub components: Vec<ProjectStatusRow>,
    pub summary: ProjectDashboardSummary,
}

/// Summary counts for the project dashboard.
#[derive(Debug, Serialize)]
pub struct ProjectDashboardSummary {
    pub current: usize,
    pub pinned_current: usize,
    pub outdated: usize,
    pub needs_release: usize,
    pub docs_only: usize,
    pub uncommitted: usize,
    pub behind_upstream: usize,
    pub unknown: usize,
}

pub enum StatusResult {
    Summary(StatusOutput),
    Full(homeboy::core::context::report::ContextReport),
    Dashboard(ProjectDashboardOutput),
}

impl serde::Serialize for StatusResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            StatusResult::Summary(output) => output.serialize(serializer),
            StatusResult::Full(output) => output.serialize(serializer),
            StatusResult::Dashboard(output) => output.serialize(serializer),
        }
    }
}

pub fn run(args: StatusArgs, _global: &super::GlobalArgs) -> CmdResult<StatusResult> {
    if args.path.is_some() {
        return run_path_status(&args);
    }

    // Project dashboard mode: `homeboy status <project-id>`
    if let Some(ref project_id) = args.project {
        return run_project_dashboard(project_id, &args);
    }

    if args.full {
        let mut report = context::build_report(args.all, "status")?;
        report.command = "status".to_string();
        return Ok((StatusResult::Full(report), 0));
    }

    let (context_output, _) = context::run(None)?;

    let relevant_ids: std::collections::HashSet<String> = context_output
        .matched_components
        .iter()
        .chain(context_output.contained_components.iter())
        .cloned()
        .collect();

    let all_components = component::inventory().unwrap_or_default();

    let show_all = args.all || relevant_ids.is_empty();

    let components: Vec<component::Component> = if show_all {
        all_components
    } else {
        all_components
            .into_iter()
            .filter(|c| relevant_ids.contains(&c.id))
            .collect()
    };

    summarize_components(components, &args)
}

fn summarize_components(
    components: Vec<component::Component>,
    args: &StatusArgs,
) -> CmdResult<StatusResult> {
    let total = components.len();

    let mut uncommitted = Vec::new();
    let mut needs_release = Vec::new();
    let mut ready_to_deploy = Vec::new();
    let mut docs_only = Vec::new();
    let mut behind_upstream = Vec::new();
    let mut upstream_drift = Vec::new();
    let mut clean: usize = 0;

    // Fetch from origin and detect upstream drift for each component
    for comp in &components {
        if let Some(drift) = fetch_upstream_drift_for(&comp.local_path, &comp.id) {
            if drift.is_behind() {
                behind_upstream.push(comp.id.clone());
            }
            upstream_drift.push(drift);
        }
    }

    for comp in &components {
        let status = deploy::calculate_release_state(comp)
            .map(|state| state.status())
            .unwrap_or(ReleaseStateStatus::Unknown);

        match status {
            ReleaseStateStatus::Uncommitted => uncommitted.push(comp.id.clone()),
            ReleaseStateStatus::NeedsRelease => needs_release.push(comp.id.clone()),
            ReleaseStateStatus::DocsOnly => docs_only.push(comp.id.clone()),
            ReleaseStateStatus::Clean => ready_to_deploy.push(comp.id.clone()),
            ReleaseStateStatus::Unknown => clean += 1,
        }
    }

    // Apply filters if any are set
    let has_filter = args.uncommitted || args.needs_release || args.ready || args.docs_only;

    if has_filter {
        if !args.uncommitted {
            uncommitted.clear();
        }
        if !args.needs_release {
            needs_release.clear();
        }
        if !args.ready {
            ready_to_deploy.clear();
        }
        if !args.docs_only {
            docs_only.clear();
        }
    }

    let ready_to_deploy_note = if ready_to_deploy.is_empty() {
        None
    } else {
        Some(READY_TO_DEPLOY_NOTE)
    };

    Ok((
        StatusResult::Summary(StatusOutput {
            command: "status",
            total,
            uncommitted,
            needs_release,
            ready_to_deploy,
            ready_to_deploy_note,
            docs_only,
            behind_upstream,
            upstream_drift,
            clean,
        }),
        0,
    ))
}

/// Path override mode: inspect one checkout without requiring registry membership.
fn run_path_status(args: &StatusArgs) -> CmdResult<StatusResult> {
    let path = args.path.as_deref();
    let component = component::resolve_effective(args.project.as_deref(), path, None)?;

    if args.full {
        let mut report = context::build_report_for_component(args.all, "status", component, path)?;
        report.command = "status".to_string();
        return Ok((StatusResult::Full(report), 0));
    }

    summarize_components(vec![component], args)
}

/// Project dashboard: show version drift across all components in a project.
///
/// Combines local version, remote (deployed) version, release state, upstream
/// drift, and unreleased commit count into a single view per component.
fn run_project_dashboard(project_id: &str, args: &StatusArgs) -> CmdResult<StatusResult> {
    let components = scope::resolve_scope_component_records(&Scope::Project(project_id.into()))?;

    if components.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "project",
            format!("Project '{}' has no components attached", project_id),
            Some(project_id.to_string()),
            Some(vec![
                "Attach components with: homeboy project set <project> --json '{\"components\":[{\"id\":\"...\",\"local_path\":\"...\"}]}'".to_string(),
            ]),
        ));
    }

    // Gather local versions
    let local_versions: std::collections::HashMap<String, String> = components
        .iter()
        .filter_map(|c| version::get_component_version(c).map(|v| (c.id.clone(), v)))
        .collect();

    // Gather remote versions via deploy check mode (handles SSH internally)
    let remote_versions = fetch_project_remote_versions(project_id);

    // Fetch upstream drift for all components
    let upstream_drift_map: std::collections::HashMap<String, UpstreamDrift> = components
        .iter()
        .filter_map(|c| fetch_upstream_drift_for(&c.local_path, &c.id).map(|d| (c.id.clone(), d)))
        .collect();

    // Build per-component rows
    let mut rows: Vec<ProjectStatusRow> = Vec::new();
    let mut summary = ProjectDashboardSummary {
        current: 0,
        pinned_current: 0,
        outdated: 0,
        needs_release: 0,
        docs_only: 0,
        uncommitted: 0,
        behind_upstream: 0,
        unknown: 0,
    };

    for comp in &components {
        let local_ver = local_versions.get(&comp.id).cloned();
        let remote_ver = remote_versions.get(&comp.id).cloned();
        let drift = upstream_drift_map.get(&comp.id);

        let release_state = deploy::calculate_release_state(comp);
        let release_status = release_state
            .as_ref()
            .map(|s| s.status())
            .unwrap_or(ReleaseStateStatus::Unknown);

        let unreleased_commits = release_state
            .as_ref()
            .map(|s| s.commits_since_version)
            .unwrap_or(0);

        // Determine dashboard status.
        // Priority: uncommitted > needs_release > docs_only > behind_upstream > outdated > current > unknown
        let dashboard_status = match release_status {
            ReleaseStateStatus::Uncommitted => ProjectComponentDashboardStatus::Uncommitted,
            ReleaseStateStatus::NeedsRelease => ProjectComponentDashboardStatus::NeedsRelease,
            ReleaseStateStatus::DocsOnly => ProjectComponentDashboardStatus::DocsOnly,
            ReleaseStateStatus::Clean => {
                // Check upstream drift first
                if let Some(d) = drift {
                    if d.is_behind() {
                        ProjectComponentDashboardStatus::BehindUpstream
                    } else {
                        deployed_version_dashboard_status(
                            &local_ver,
                            &remote_ver,
                            d.latest_origin_tag.as_deref(),
                        )
                    }
                } else {
                    deployed_version_dashboard_status(&local_ver, &remote_ver, None)
                }
            }
            ReleaseStateStatus::Unknown => ProjectComponentDashboardStatus::Unknown,
        };

        match &dashboard_status {
            ProjectComponentDashboardStatus::Current => summary.current += 1,
            ProjectComponentDashboardStatus::PinnedCurrent => summary.pinned_current += 1,
            ProjectComponentDashboardStatus::Outdated => summary.outdated += 1,
            ProjectComponentDashboardStatus::NeedsRelease => summary.needs_release += 1,
            ProjectComponentDashboardStatus::DocsOnly => summary.docs_only += 1,
            ProjectComponentDashboardStatus::Uncommitted => summary.uncommitted += 1,
            ProjectComponentDashboardStatus::BehindUpstream => summary.behind_upstream += 1,
            ProjectComponentDashboardStatus::Unknown => summary.unknown += 1,
        }

        rows.push(ProjectStatusRow {
            component_id: comp.id.clone(),
            local_version: local_ver,
            remote_version: remote_ver,
            origin_version: drift.and_then(|d| d.latest_origin_tag.clone()),
            unreleased_commits,
            ahead_upstream: drift.and_then(|d| d.ahead),
            behind_upstream: drift.and_then(|d| d.behind),
            status: dashboard_status,
        });
    }

    // Apply filters
    if args.outdated {
        rows.retain(|r| {
            matches!(
                r.status,
                ProjectComponentDashboardStatus::Outdated
                    | ProjectComponentDashboardStatus::PinnedCurrent
            )
        });
    }
    if args.needs_release {
        rows.retain(|r| matches!(r.status, ProjectComponentDashboardStatus::NeedsRelease));
    }
    if args.uncommitted {
        rows.retain(|r| matches!(r.status, ProjectComponentDashboardStatus::Uncommitted));
    }
    if args.docs_only {
        rows.retain(|r| matches!(r.status, ProjectComponentDashboardStatus::DocsOnly));
    }
    if args.ready {
        rows.retain(|r| matches!(r.status, ProjectComponentDashboardStatus::Current));
    }

    // Log the table to stderr for human-readable output
    log_dashboard_table(&rows);

    let total = rows.len();

    Ok((
        StatusResult::Dashboard(ProjectDashboardOutput {
            command: "status",
            project_id: project_id.to_string(),
            total,
            components: rows,
            summary,
        }),
        0,
    ))
}

fn deployed_version_dashboard_status(
    local_ver: &Option<String>,
    remote_ver: &Option<String>,
    origin_tag: Option<&str>,
) -> ProjectComponentDashboardStatus {
    match (local_ver, remote_ver) {
        (Some(local), Some(remote)) if local != remote => ProjectComponentDashboardStatus::Outdated,
        (Some(_), None) => ProjectComponentDashboardStatus::Outdated,
        (Some(local), Some(remote))
            if local == remote && origin_tag_is_newer_than_local(origin_tag, local) =>
        {
            ProjectComponentDashboardStatus::PinnedCurrent
        }
        _ => ProjectComponentDashboardStatus::Current,
    }
}

fn origin_tag_is_newer_than_local(origin_tag: Option<&str>, local: &str) -> bool {
    let Some(origin) = origin_tag else {
        return false;
    };
    let origin = origin.trim_start_matches('v');
    let local = local.trim_start_matches('v');
    if origin == local {
        return false;
    }

    semver::Version::parse(origin)
        .ok()
        .zip(semver::Version::parse(local).ok())
        .is_some_and(|(origin, local)| origin > local)
}

/// Fetch from origin and compute upstream drift for a component.
///
/// Returns `None` if the path is not a git repo or has no upstream configured.
fn fetch_upstream_drift(path: &str) -> Option<UpstreamDrift> {
    // Best-effort fetch — silently proceeds if no remote or network issue.
    let _ = homeboy::core::engine::command::run_in_optional(
        path,
        "git",
        &["fetch", "--tags", "--quiet"],
    );

    let snapshot = git::get_repo_snapshot(path).ok()?;

    // After fetching tags, find the latest tag across ALL refs (not just HEAD).
    // `git describe --tags --abbrev=0` only returns tags reachable from HEAD,
    // which misses newer tags when the local checkout is behind.
    let latest_origin_tag = get_latest_tag_overall(path);

    Some(UpstreamDrift {
        component_id: String::new(), // caller sets component_id after
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        latest_origin_tag,
    })
}

/// Get the latest version tag in the repo regardless of what HEAD points to.
///
/// Unlike `get_latest_tag()` which uses `git describe` (reachable from HEAD),
/// this lists all tags and picks the one with the highest semver version.
fn get_latest_tag_overall(path: &str) -> Option<String> {
    let output = homeboy::core::engine::command::run_in_optional(
        path,
        "git",
        &["tag", "-l", "--sort=-v:refname"],
    )?;

    output
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Like `fetch_upstream_drift` but sets the component ID in the result.
fn fetch_upstream_drift_for(path: &str, id: &str) -> Option<UpstreamDrift> {
    let mut drift = fetch_upstream_drift(path)?;
    drift.component_id = id.to_string();
    Some(drift)
}

/// Fetch remote (deployed) versions for all components in a project.
///
/// Uses deploy check mode internally, which handles SSH resolution.
/// Returns empty map on failure (e.g., no server configured, SSH unavailable).
fn fetch_project_remote_versions(project_id: &str) -> std::collections::HashMap<String, String> {
    let config = DeployConfig {
        component_ids: vec![],
        all: true,
        outdated: false,
        behind_upstream: false,
        dry_run: false,
        check: true,
        force: false,
        skip_build: true,
        keep_deps: false,
        expected_version: None,
        no_pull: true,
        head: true,
        tagged: false,
    };

    match deploy::run(project_id, &config) {
        Ok(result) => result
            .results
            .into_iter()
            .filter_map(|r| r.remote_version.map(|v| (r.id, v)))
            .collect(),
        Err(_) => {
            homeboy::log_status!(
                "status",
                "Warning: could not fetch remote versions for project '{}' — showing local data only",
                project_id
            );
            std::collections::HashMap::new()
        }
    }
}

/// Log a human-readable table to stderr.
fn log_dashboard_table(rows: &[ProjectStatusRow]) {
    if rows.is_empty() || !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return;
    }

    // Calculate column widths
    let widths = DashboardColumnWidths {
        id: rows
            .iter()
            .map(|r| r.component_id.len())
            .max()
            .unwrap_or(9)
            .max(9),
        local: rows
            .iter()
            .map(|r| r.local_version.as_deref().unwrap_or("-").len())
            .max()
            .unwrap_or(5)
            .max(5),
        remote: rows
            .iter()
            .map(|r| r.remote_version.as_deref().unwrap_or("-").len())
            .max()
            .unwrap_or(6)
            .max(6),
        origin: rows
            .iter()
            .map(|r| r.origin_version.as_deref().unwrap_or("-").len())
            .max()
            .unwrap_or(6)
            .max(6),
    };

    log_dashboard_header(&widths);
    log_dashboard_separator(&widths);

    for row in rows {
        let local = row.local_version.as_deref().unwrap_or("-");
        let remote = row.remote_version.as_deref().unwrap_or("-");
        let origin = row.origin_version.as_deref().unwrap_or("-");
        let upstream = format_upstream(&row.ahead_upstream, &row.behind_upstream);
        let status_icon = match &row.status {
            ProjectComponentDashboardStatus::Current => "✅ current",
            ProjectComponentDashboardStatus::PinnedCurrent => "📌 pinned current",
            ProjectComponentDashboardStatus::Outdated => "⚠️  outdated",
            ProjectComponentDashboardStatus::NeedsRelease => "🔶 needs release",
            ProjectComponentDashboardStatus::DocsOnly => "📝 docs only",
            ProjectComponentDashboardStatus::Uncommitted => "🔴 uncommitted",
            ProjectComponentDashboardStatus::BehindUpstream => "⬇️  behind upstream",
            ProjectComponentDashboardStatus::Unknown => "❓ unknown",
        };

        eprintln!(
            "{:<id_w$}  {:<local_w$}  {:<remote_w$}  {:<origin_w$}  {:>10}  {:>8}  {}",
            row.component_id,
            local,
            remote,
            origin,
            row.unreleased_commits,
            upstream,
            status_icon,
            id_w = widths.id,
            local_w = widths.local,
            remote_w = widths.remote,
            origin_w = widths.origin,
        );
    }
}

struct DashboardColumnWidths {
    id: usize,
    local: usize,
    remote: usize,
    origin: usize,
}

fn log_dashboard_header(widths: &DashboardColumnWidths) {
    eprintln!(
        "{:<id_w$}  {:<local_w$}  {:<remote_w$}  {:<origin_w$}  {:>10}  {:>8}  Status",
        "Component",
        "Local",
        "Remote",
        "Origin",
        "Unreleased",
        "Upstream",
        id_w = widths.id,
        local_w = widths.local,
        remote_w = widths.remote,
        origin_w = widths.origin,
    );
}

fn log_dashboard_separator(widths: &DashboardColumnWidths) {
    eprintln!(
        "{:-<id_w$}  {:-<local_w$}  {:-<remote_w$}  {:-<origin_w$}  {:->10}  {:->8}  {:-<10}",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        id_w = widths.id,
        local_w = widths.local,
        remote_w = widths.remote,
        origin_w = widths.origin,
    );
}

/// Format upstream ahead/behind as a compact string like "↓3" or "↑1↓2" or "=".
fn format_upstream(ahead: &Option<u32>, behind: &Option<u32>) -> String {
    match (ahead, behind) {
        (Some(a), Some(b)) if *a > 0 && *b > 0 => format!("↑{}↓{}", a, b),
        (Some(a), Some(_)) if *a > 0 => format!("↑{}", a),
        (Some(_), Some(b)) if *b > 0 => format!("↓{}", b),
        (None, Some(b)) if *b > 0 => format!("↓{}", b),
        (Some(a), None) if *a > 0 => format!("↑{}", a),
        (Some(0), Some(0)) | (None, Some(0)) | (Some(0), None) => "=".to_string(),
        _ => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use crate::commands::GlobalArgs;
    use clap::Parser;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn status_args(project: Option<String>, path: String, full: bool) -> StatusArgs {
        StatusArgs {
            project,
            path: Some(path),
            full,
            uncommitted: false,
            needs_release: false,
            ready: false,
            docs_only: false,
            all: false,
            outdated: false,
        }
    }

    fn make_git_repo(name: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let repo = dir.path().join(name);
        fs::create_dir_all(&repo).expect("repo dir");
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .expect("git init");
        (dir, repo)
    }

    fn empty_status_output() -> StatusOutput {
        StatusOutput {
            command: "status",
            total: 0,
            uncommitted: Vec::new(),
            needs_release: Vec::new(),
            ready_to_deploy: Vec::new(),
            ready_to_deploy_note: None,
            docs_only: Vec::new(),
            behind_upstream: Vec::new(),
            upstream_drift: Vec::new(),
            clean: 0,
        }
    }

    #[test]
    fn ready_to_deploy_note_is_omitted_when_no_components_are_clean() {
        let output = empty_status_output();
        let json = serde_json::to_value(&output).expect("serialize status output");

        // ready_to_deploy is empty -> note must not leak into the JSON contract.
        assert!(json.get("ready_to_deploy").is_none());
        assert!(
            json.get("ready_to_deploy_note").is_none(),
            "note should be omitted when ready_to_deploy is empty"
        );
    }

    #[test]
    fn ready_to_deploy_note_clarifies_git_state_only_when_components_are_clean() {
        let output = StatusOutput {
            total: 1,
            ready_to_deploy: vec!["data-machine".to_string()],
            ready_to_deploy_note: Some(READY_TO_DEPLOY_NOTE),
            ..empty_status_output()
        };
        let json = serde_json::to_value(&output).expect("serialize status output");

        let note = json
            .get("ready_to_deploy_note")
            .and_then(|v| v.as_str())
            .expect("note present when ready_to_deploy is non-empty");

        // The note must steer operators away from treating git state as a
        // target-accurate deploy backlog (issue #4588).
        assert!(
            note.contains("git-state-only"),
            "note should flag the list as git-state-only"
        );
        assert!(
            note.contains("homeboy status <project>"),
            "note should point at the target-accurate project dashboard"
        );
    }

    #[test]
    fn deployed_version_status_marks_current_version_with_newer_origin_tag_as_pinned_current() {
        let status = deployed_version_dashboard_status(
            &Some("0.139.18".to_string()),
            &Some("0.139.18".to_string()),
            Some("v0.139.19"),
        );

        assert!(matches!(
            status,
            ProjectComponentDashboardStatus::PinnedCurrent
        ));
    }

    #[test]
    fn deployed_version_status_keeps_exact_origin_tag_current() {
        let status = deployed_version_dashboard_status(
            &Some("0.139.18".to_string()),
            &Some("0.139.18".to_string()),
            Some("v0.139.18"),
        );

        assert!(matches!(status, ProjectComponentDashboardStatus::Current));
    }

    #[test]
    fn parser_accepts_status_path_only() {
        let cli = Cli::try_parse_from(["homeboy", "status", "--path", "/tmp/example", "--full"])
            .expect("status --path parses");

        match cli.command {
            Commands::Status(args) => {
                assert_eq!(args.project, None);
                assert_eq!(args.path.as_deref(), Some("/tmp/example"));
                assert!(args.full);
            }
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn parser_accepts_status_id_with_path() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "status",
            "wordpress-playground",
            "--path",
            "/tmp/wp-playground",
            "--full",
        ])
        .expect("status <id> --path parses");

        match cli.command {
            Commands::Status(args) => {
                assert_eq!(args.project.as_deref(), Some("wordpress-playground"));
                assert_eq!(args.path.as_deref(), Some("/tmp/wp-playground"));
                assert!(args.full);
            }
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn status_path_only_inspects_one_synthetic_component() {
        let (_dir, repo) = make_git_repo("external-repo");
        let args = status_args(None, repo.to_string_lossy().to_string(), false);

        let (result, code) = run(args, &GlobalArgs {}).expect("status --path succeeds");

        assert_eq!(code, 0);
        match result {
            StatusResult::Summary(output) => {
                assert_eq!(output.total, 1);
                assert!(output.upstream_drift.is_empty());
            }
            _ => panic!("expected summary output"),
        }
    }

    #[test]
    fn status_id_with_path_full_uses_explicit_component_id() {
        let (_dir, repo) = make_git_repo("wp-playground-checkout");
        let args = status_args(
            Some("wordpress-playground".to_string()),
            repo.to_string_lossy().to_string(),
            true,
        );

        let (result, code) = run(args, &GlobalArgs {}).expect("status <id> --path --full succeeds");

        assert_eq!(code, 0);
        match result {
            StatusResult::Full(report) => {
                assert_eq!(report.context.cwd, repo.to_string_lossy());
                assert_eq!(report.components.len(), 1);
                assert_eq!(report.components[0].id, "wordpress-playground");
                assert_eq!(report.components[0].path, ".");
            }
            _ => panic!("expected full output"),
        }
    }
}
