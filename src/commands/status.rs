use clap::Args;
use homeboy::core::component;
use homeboy::core::context;
use homeboy::core::deploy::{self, DeployConfig, ReleaseStateStatus};
use homeboy::core::git;
use homeboy::core::release::version;
use homeboy::core::scope::{self, Scope};
use serde::Serialize;

use super::CmdResult;

mod dashboard_table;
use dashboard_table::log_dashboard_table;

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

    /// Show only components carrying merged-but-unreleased work (commits on
    /// origin/<default-branch> that are past the latest release tag).
    #[arg(long)]
    pub unreleased: bool,
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

/// A component carrying code that is merged to its default branch on origin but
/// not yet in a release tag — the "merged-not-released" state.
///
/// This is the complement of `ready_to_deploy`/`UpstreamDrift`: instead of
/// asking "is the local checkout ahead of the latest tag" (which depends on a
/// fresh local checkout and a local-vs-tag diff), it asks the higher-stakes
/// inverse — "is there work merged on `origin/<default-branch>` that no release
/// tag covers yet, so the code does NOT exist on prod?"
///
/// The count is measured against `origin/<default-branch>` (refreshed by the
/// tag/branch fetch that already runs for upstream drift), so it is robust to a
/// stale local HEAD. See issue #4996.
#[derive(Debug, Clone, Serialize)]
pub struct UnreleasedMerge {
    pub component_id: String,
    /// The latest release tag the unreleased work is measured against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_tag: Option<String>,
    /// Count of commits on `origin/<default-branch>` past `latest_tag`
    /// (merge commits excluded, matching release-state counting).
    pub commits_since_tag: u32,
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

/// Clarifying note emitted alongside `unreleased_merges`.
///
/// `unreleased_merges` flags components whose `origin/<default-branch>` carries
/// commits past the latest release tag — i.e. work that is merged but not in any
/// release, so the code does NOT exist on prod. This is the merged→released axis
/// of the merged→released→deployed chain; for the released→deployed axis
/// (installed version vs latest tag) run `homeboy status <project>`. See #4996.
const UNRELEASED_MERGES_NOTE: &str = "unreleased_merges flags components with commits merged to origin/<default-branch> that are past the latest release tag (merged but NOT released — the code is not on prod yet). A merged PR here is NOT 'shipped'. Cut a release, then run `homeboy status <project>` to confirm installed-vs-tag (released-but-not-deployed).";

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
    /// Components carrying code merged to `origin/<default-branch>` but not yet
    /// covered by a release tag ("merged-not-released"). Closes the false
    /// "shipped" read where a merged PR is mistaken for live code. Additive and
    /// independent of `ready_to_deploy` (issue #4996).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unreleased_merges: Vec<UnreleasedMerge>,
    /// Clarifying note, emitted only when `unreleased_merges` is non-empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreleased_merges_note: Option<&'static str>,
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
    let mut unreleased_merges = Vec::new();
    let mut clean: usize = 0;

    let has_filter =
        args.uncommitted || args.needs_release || args.ready || args.docs_only || args.unreleased;
    let include_upstream_drift = !has_filter;
    let include_unreleased_merges = !has_filter || args.unreleased;

    if include_upstream_drift || include_unreleased_merges {
        for comp in &components {
            fetch_origin_tags(&comp.local_path);

            if include_upstream_drift {
                if let Some(drift) = get_upstream_drift_for(&comp.local_path, &comp.id) {
                    if drift.is_behind() {
                        behind_upstream.push(comp.id.clone());
                    }
                    upstream_drift.push(drift);
                }
            }

            // Detect merged-but-unreleased work per component (issue #4996). This is
            // measured against origin/<default-branch> (refreshed above), so a stale
            // local checkout does not hide unreleased merges.
            if include_unreleased_merges {
                if let Some(merge) = detect_unreleased_merges_for(comp) {
                    unreleased_merges.push(merge);
                }
            }
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
        if !args.unreleased {
            unreleased_merges.clear();
        }
    }

    let ready_to_deploy_note = if ready_to_deploy.is_empty() {
        None
    } else {
        Some(READY_TO_DEPLOY_NOTE)
    };

    let unreleased_merges_note = if unreleased_merges.is_empty() {
        None
    } else {
        Some(UNRELEASED_MERGES_NOTE)
    };

    log_unreleased_merges(&unreleased_merges);

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
            unreleased_merges,
            unreleased_merges_note,
            clean,
        }),
        0,
    ))
}

fn status_has_filter(args: &StatusArgs) -> bool {
    args.uncommitted || args.needs_release || args.ready || args.docs_only || args.unreleased
}

fn status_includes_upstream_drift(args: &StatusArgs) -> bool {
    !status_has_filter(args)
}

fn status_includes_unreleased_merges(args: &StatusArgs) -> bool {
    !status_has_filter(args) || args.unreleased
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
    fetch_origin_tags(path);

    get_upstream_drift(path)
}

fn fetch_origin_tags(path: &str) {
    // Best-effort fetch — silently proceeds if no remote or network issue.
    let _ = homeboy::core::engine::command::run_in_optional(
        path,
        "git",
        &["fetch", "--tags", "--quiet"],
    );
}

fn get_upstream_drift(path: &str) -> Option<UpstreamDrift> {
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

fn get_upstream_drift_for(path: &str, id: &str) -> Option<UpstreamDrift> {
    let mut drift = get_upstream_drift(path)?;
    drift.component_id = id.to_string();
    Some(drift)
}

/// Detect merged-but-unreleased work for a component (issue #4996).
///
/// Reuses existing primitives rather than introducing new ones:
/// - `version::read_component_version` + `git::detect_baseline_with_version`
///   resolve the same release baseline the local release-state uses (which also
///   runs the best-effort tag fetch).
/// - The default origin branch is resolved with the same precedence the deploy
///   planner uses (`origin/HEAD` symbolic ref, then main/trunk/master).
/// - Commits past the baseline are counted with `git rev-list --count
///   --no-merges <baseline>..origin/<branch>`, mirroring the `--no-merges`
///   counting in `get_commits_since_tag`.
///
/// Unlike `ready_to_deploy` (local HEAD vs tag), this measures
/// `origin/<default-branch>` vs the latest tag, so a stale local checkout cannot
/// mask unreleased merges. Returns `None` when there is no unreleased work, the
/// path is not a git repo, or the origin branch cannot be resolved.
fn detect_unreleased_merges_for(comp: &component::Component) -> Option<UnreleasedMerge> {
    let path = &comp.local_path;

    let origin_branch = default_origin_branch(path)?;

    let current_version = version::read_component_version(comp)
        .ok()
        .map(|info| info.version);

    let baseline = git::detect_baseline_with_version(path, current_version.as_deref()).ok()?;
    let baseline_ref = baseline.reference.as_deref()?;

    let range = format!("{}..{}", baseline_ref, origin_branch);
    let count_output = homeboy::core::engine::command::run_in_optional(
        path,
        "git",
        &["rev-list", "--count", "--no-merges", &range],
    )?;

    let commits_since_tag: u32 = count_output.trim().parse().ok()?;
    if commits_since_tag == 0 {
        return None;
    }

    Some(UnreleasedMerge {
        component_id: comp.id.clone(),
        latest_tag: baseline.latest_tag.clone(),
        commits_since_tag,
    })
}

/// Log merged-but-unreleased components to stderr for human-readable output.
///
/// Mirrors the dashboard table's terminal-only behavior so JSON consumers are
/// unaffected. Keeps the merged-not-released signal visible in `homeboy status`
/// without a project argument (issue #4996).
fn log_unreleased_merges(merges: &[UnreleasedMerge]) {
    if merges.is_empty() || !std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        return;
    }

    eprintln!(
        "⚠️  {} component(s) carry merged-but-unreleased work (merged to origin, NOT in any release — code is not on prod yet):",
        merges.len()
    );
    for merge in merges {
        let tag = merge.latest_tag.as_deref().unwrap_or("(no tag)");
        eprintln!(
            "    {} — {} commit(s) past {}",
            merge.component_id, merge.commits_since_tag, tag
        );
    }
    eprintln!("    Cut a release, then `homeboy status <project>` to confirm installed-vs-tag.");
}

/// Resolve the default origin branch ref for a checkout.
///
/// Precedence matches the deploy planner: `origin/HEAD` symbolic ref first, then
/// the conventional `origin/main` / `origin/trunk` / `origin/master` fallbacks.
fn default_origin_branch(path: &str) -> Option<String> {
    if let Some(symbolic) = homeboy::core::engine::command::run_in_optional(
        path,
        "git",
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    ) {
        let symbolic = symbolic.trim();
        if !symbolic.is_empty() {
            return Some(symbolic.to_string());
        }
    }

    ["origin/main", "origin/trunk", "origin/master"]
        .iter()
        .find(|branch| {
            homeboy::core::engine::command::run_in_optional(
                path,
                "git",
                &["rev-parse", "--verify", "--quiet", branch],
            )
            .is_some()
        })
        .map(|branch| (*branch).to_string())
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
            unreleased: false,
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
            unreleased_merges: Vec::new(),
            unreleased_merges_note: None,
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
            ready_to_deploy: vec!["sample-plugin".to_string()],
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

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .expect("git command");
        assert!(status.success(), "git {:?} failed", args);
    }

    fn commit_empty(repo: &std::path::Path, message: &str) {
        run_git(repo, &["commit", "--allow-empty", "-q", "-m", message]);
    }

    #[test]
    fn parser_accepts_unreleased_filter() {
        let cli = Cli::try_parse_from(["homeboy", "status", "--unreleased"])
            .expect("status --unreleased parses");

        match cli.command {
            Commands::Status(args) => assert!(args.unreleased),
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn unfiltered_summary_includes_origin_dependent_sections() {
        let args = status_args(None, "/tmp/example".to_string(), false);

        assert!(status_includes_upstream_drift(&args));
        assert!(status_includes_unreleased_merges(&args));
    }

    #[test]
    fn local_release_state_filters_skip_origin_dependent_sections() {
        let mut args = status_args(None, "/tmp/example".to_string(), false);
        args.needs_release = true;

        assert!(!status_includes_upstream_drift(&args));
        assert!(!status_includes_unreleased_merges(&args));
    }

    #[test]
    fn unreleased_filter_keeps_unreleased_origin_work_without_drift() {
        let mut args = status_args(None, "/tmp/example".to_string(), false);
        args.unreleased = true;

        assert!(!status_includes_upstream_drift(&args));
        assert!(status_includes_unreleased_merges(&args));
    }

    #[test]
    fn unreleased_merges_note_is_omitted_when_empty() {
        let output = empty_status_output();
        let json = serde_json::to_value(&output).expect("serialize status output");

        assert!(
            json.get("unreleased_merges").is_none(),
            "empty unreleased_merges must not leak into the JSON contract"
        );
        assert!(
            json.get("unreleased_merges_note").is_none(),
            "note should be omitted when unreleased_merges is empty"
        );
    }

    #[test]
    fn unreleased_merges_note_present_when_merges_exist() {
        let output = StatusOutput {
            total: 1,
            unreleased_merges: vec![UnreleasedMerge {
                component_id: "extrachill-artist-platform".to_string(),
                latest_tag: Some("v1.11.0".to_string()),
                commits_since_tag: 3,
            }],
            unreleased_merges_note: Some(UNRELEASED_MERGES_NOTE),
            ..empty_status_output()
        };
        let json = serde_json::to_value(&output).expect("serialize status output");

        let note = json
            .get("unreleased_merges_note")
            .and_then(|v| v.as_str())
            .expect("note present when unreleased_merges is non-empty");

        // The note must steer operators away from reading a merged PR as shipped.
        assert!(
            note.contains("merged but NOT released"),
            "note should flag merged-not-released"
        );
        assert!(
            note.contains("not on prod yet"),
            "note should clarify the code is not live"
        );
    }

    #[test]
    fn default_origin_branch_resolves_origin_head_symbolic_ref() {
        let (_dir, repo) = make_git_repo("with-origin");
        // Build a fake "origin" remote by cloning into a bare repo and wiring it up.
        commit_empty(&repo, "feat: initial");
        // Create the origin/main remote-tracking ref directly so the resolver
        // has something to find without network access.
        run_git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        run_git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );

        let resolved = default_origin_branch(&repo.to_string_lossy());
        assert_eq!(resolved.as_deref(), Some("origin/main"));
    }

    #[test]
    fn default_origin_branch_falls_back_to_conventional_branches() {
        let (_dir, repo) = make_git_repo("fallback-origin");
        commit_empty(&repo, "feat: initial");
        // No origin/HEAD symbolic ref; only a conventional remote-tracking ref.
        run_git(&repo, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);

        let resolved = default_origin_branch(&repo.to_string_lossy());
        assert_eq!(resolved.as_deref(), Some("origin/trunk"));
    }

    #[test]
    fn default_origin_branch_none_without_remote_refs() {
        let (_dir, repo) = make_git_repo("no-origin");
        commit_empty(&repo, "feat: initial");

        assert!(default_origin_branch(&repo.to_string_lossy()).is_none());
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
