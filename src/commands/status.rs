use clap::Args;
use homeboy::component;
use homeboy::context;
use homeboy::deploy::{self, DeployConfig, ReleaseStateStatus};
use homeboy::project;
use homeboy::version;
use serde::Serialize;

use super::CmdResult;

#[derive(Args)]
pub struct StatusArgs {
    /// Project ID — show version dashboard for a project's components
    pub project: Option<String>,

    /// Show the full workspace/context report (the old init behavior)
    #[arg(long)]
    pub full: bool,

    /// Show only components with uncommitted changes
    #[arg(long)]
    pub uncommitted: bool,

    /// Show only components that need a version bump
    #[arg(long)]
    pub needs_bump: bool,

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

#[derive(Debug, Serialize)]
pub struct StatusOutput {
    pub command: &'static str,
    pub total: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncommitted: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub needs_bump: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ready_to_deploy: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub docs_only: Vec<String>,
    pub clean: usize,
}

/// A single row in the project status dashboard.
#[derive(Debug, Serialize)]
pub struct ProjectStatusRow {
    pub component_id: String,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    pub unreleased_commits: u32,
    pub status: ProjectComponentDashboardStatus,
}

/// Status indicator for the project dashboard.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectComponentDashboardStatus {
    /// Local and remote versions match, no unreleased commits
    Current,
    /// Local version differs from remote (needs deploy)
    Outdated,
    /// Unreleased commits since last tag (needs version bump)
    NeedsBump,
    /// Only docs changes since last tag
    DocsOnly,
    /// Uncommitted changes in working directory
    Uncommitted,
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
    pub outdated: usize,
    pub needs_bump: usize,
    pub docs_only: usize,
    pub uncommitted: usize,
    pub unknown: usize,
}

pub enum StatusResult {
    Summary(StatusOutput),
    Full(homeboy::context::report::ContextReport),
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

    let total = components.len();

    let mut uncommitted = Vec::new();
    let mut needs_bump = Vec::new();
    let mut ready_to_deploy = Vec::new();
    let mut docs_only = Vec::new();
    let mut clean: usize = 0;

    for comp in &components {
        let status = deploy::calculate_release_state(comp)
            .map(|state| state.status())
            .unwrap_or(ReleaseStateStatus::Unknown);

        match status {
            ReleaseStateStatus::Uncommitted => uncommitted.push(comp.id.clone()),
            ReleaseStateStatus::NeedsBump => needs_bump.push(comp.id.clone()),
            ReleaseStateStatus::DocsOnly => docs_only.push(comp.id.clone()),
            ReleaseStateStatus::Clean => ready_to_deploy.push(comp.id.clone()),
            ReleaseStateStatus::Unknown => clean += 1,
        }
    }

    // Apply filters if any are set
    let has_filter = args.uncommitted || args.needs_bump || args.ready || args.docs_only;

    if has_filter {
        if !args.uncommitted {
            uncommitted.clear();
        }
        if !args.needs_bump {
            needs_bump.clear();
        }
        if !args.ready {
            ready_to_deploy.clear();
        }
        if !args.docs_only {
            docs_only.clear();
        }
    }

    Ok((
        StatusResult::Summary(StatusOutput {
            command: "status",
            total,
            uncommitted,
            needs_bump,
            ready_to_deploy,
            docs_only,
            clean,
        }),
        0,
    ))
}

/// Project dashboard: show version drift across all components in a project.
///
/// Combines local version, remote (deployed) version, release state, and
/// unreleased commit count into a single view per component.
fn run_project_dashboard(project_id: &str, args: &StatusArgs) -> CmdResult<StatusResult> {
    let proj = project::load(project_id)?;
    let components = project::resolve_project_components(&proj)?;

    if components.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
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

    // Build per-component rows
    let mut rows: Vec<ProjectStatusRow> = Vec::new();
    let mut summary = ProjectDashboardSummary {
        current: 0,
        outdated: 0,
        needs_bump: 0,
        docs_only: 0,
        uncommitted: 0,
        unknown: 0,
    };

    for comp in &components {
        let local_ver = local_versions.get(&comp.id).cloned();
        let remote_ver = remote_versions.get(&comp.id).cloned();

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
        // Priority: uncommitted > needs_bump > docs_only > outdated > current > unknown
        let dashboard_status = match release_status {
            ReleaseStateStatus::Uncommitted => ProjectComponentDashboardStatus::Uncommitted,
            ReleaseStateStatus::NeedsBump => ProjectComponentDashboardStatus::NeedsBump,
            ReleaseStateStatus::DocsOnly => ProjectComponentDashboardStatus::DocsOnly,
            ReleaseStateStatus::Clean => {
                // Clean release state — check if deployed version matches local
                match (&local_ver, &remote_ver) {
                    (Some(local), Some(remote)) if local != remote => {
                        ProjectComponentDashboardStatus::Outdated
                    }
                    (Some(_), None) => ProjectComponentDashboardStatus::Outdated,
                    _ => ProjectComponentDashboardStatus::Current,
                }
            }
            ReleaseStateStatus::Unknown => ProjectComponentDashboardStatus::Unknown,
        };

        match &dashboard_status {
            ProjectComponentDashboardStatus::Current => summary.current += 1,
            ProjectComponentDashboardStatus::Outdated => summary.outdated += 1,
            ProjectComponentDashboardStatus::NeedsBump => summary.needs_bump += 1,
            ProjectComponentDashboardStatus::DocsOnly => summary.docs_only += 1,
            ProjectComponentDashboardStatus::Uncommitted => summary.uncommitted += 1,
            ProjectComponentDashboardStatus::Unknown => summary.unknown += 1,
        }

        rows.push(ProjectStatusRow {
            component_id: comp.id.clone(),
            local_version: local_ver,
            remote_version: remote_ver,
            unreleased_commits,
            status: dashboard_status,
        });
    }

    // Apply filters
    if args.outdated {
        rows.retain(|r| matches!(r.status, ProjectComponentDashboardStatus::Outdated));
    }
    if args.needs_bump {
        rows.retain(|r| matches!(r.status, ProjectComponentDashboardStatus::NeedsBump));
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

/// Fetch remote (deployed) versions for all components in a project.
///
/// Uses deploy check mode internally, which handles SSH resolution.
/// Returns empty map on failure (e.g., no server configured, SSH unavailable).
fn fetch_project_remote_versions(project_id: &str) -> std::collections::HashMap<String, String> {
    let config = DeployConfig {
        component_ids: vec![],
        all: true,
        outdated: false,
        dry_run: false,
        check: true,
        force: false,
        skip_build: true,
        keep_deps: false,
        expected_version: None,
        no_pull: true,
        head: true,
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
    let id_width = rows
        .iter()
        .map(|r| r.component_id.len())
        .max()
        .unwrap_or(9)
        .max(9);
    let local_width = rows
        .iter()
        .map(|r| r.local_version.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(5)
        .max(5);
    let remote_width = rows
        .iter()
        .map(|r| r.remote_version.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(6)
        .max(6);

    // Header
    eprintln!(
        "{:<id_w$}  {:<local_w$}  {:<remote_w$}  {:>10}  Status",
        "Component",
        "Local",
        "Remote",
        "Unreleased",
        id_w = id_width,
        local_w = local_width,
        remote_w = remote_width,
    );
    eprintln!(
        "{:-<id_w$}  {:-<local_w$}  {:-<remote_w$}  {:->10}  {:-<10}",
        "",
        "",
        "",
        "",
        "",
        id_w = id_width,
        local_w = local_width,
        remote_w = remote_width,
    );

    for row in rows {
        let local = row.local_version.as_deref().unwrap_or("-");
        let remote = row.remote_version.as_deref().unwrap_or("-");
        let status_icon = match &row.status {
            ProjectComponentDashboardStatus::Current => "✅ current",
            ProjectComponentDashboardStatus::Outdated => "⚠️  outdated",
            ProjectComponentDashboardStatus::NeedsBump => "🔶 needs bump",
            ProjectComponentDashboardStatus::DocsOnly => "📝 docs only",
            ProjectComponentDashboardStatus::Uncommitted => "🔴 uncommitted",
            ProjectComponentDashboardStatus::Unknown => "❓ unknown",
        };

        eprintln!(
            "{:<id_w$}  {:<local_w$}  {:<remote_w$}  {:>10}  {}",
            row.component_id,
            local,
            remote,
            row.unreleased_commits,
            status_icon,
            id_w = id_width,
            local_w = local_width,
            remote_w = remote_width,
        );
    }
}
