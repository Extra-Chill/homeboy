//! `homeboy status` — component/project release-state overview.
//!
//! This module is split into focused submodules:
//! - [`types`] — CLI args and serialized output shapes.
//! - [`git_cache`] — per-component git caching and probes.
//! - [`context_paths`] — registered-context detection for the default view.
//! - [`dashboard_table`] — human-readable dashboard rendering.
//!
//! The orchestration entry points (`run`, dashboard/summary builders) live
//! here and compose those pieces.

use homeboy::core::component;
use homeboy::core::context;
use homeboy_release::deploy::ReleaseStateStatus;
use homeboy_release::release::version;
use homeboy::core::scope::{self, Scope};
use std::collections::HashMap;

use super::CmdResult;

mod context_paths;
mod dashboard_table;
mod git_cache;
mod types;

use context_paths::unregistered_cwd_status_output;
use dashboard_table::log_dashboard_table;
use git_cache::{fetch_project_remote_versions, log_unreleased_merges, StatusGitCache};

pub use types::{
    ProjectComponentDashboardStatus, ProjectDashboardOutput, ProjectDashboardSummary,
    ProjectStatusRow, StatusArgs, StatusOutput, StatusResult, StatusTiming,
    UnregisteredContextStatusOutput, UnreleasedMerge, UpstreamDrift,
};
use types::{StatusTimer, READY_TO_DEPLOY_NOTE, UNRELEASED_MERGES_NOTE};

pub fn run(args: StatusArgs, _global: &super::GlobalArgs) -> CmdResult<StatusResult> {
    if args.path.is_some() {
        return run_path_status(&args);
    }

    // Project dashboard mode: `homeboy status <project-id>`
    if let Some(ref project_id) = args.project {
        return run_project_dashboard(project_id, &args);
    }

    if !args.full && !args.all {
        if let Some(output) = unregistered_cwd_status_output() {
            return Ok((StatusResult::UnregisteredContext(output), 0));
        }
    }

    if args.full {
        let mut report = context::build_report(args.all, "status")?;
        report.command = "status".to_string();
        return Ok((StatusResult::Full(report), 0));
    }

    let mut timer = StatusTimer::new(args.timings);

    timer.begin("resolve_context");
    let (context_output, _) = context::run(None)?;
    timer.finish("resolve_context");

    let relevant_ids: std::collections::HashSet<String> = context_output
        .matched_components
        .iter()
        .chain(context_output.contained_components.iter())
        .cloned()
        .collect();

    if relevant_ids.is_empty() && !args.all {
        return Ok((
            StatusResult::UnregisteredContext(UnregisteredContextStatusOutput {
                command: "status",
                status: "unregistered_context",
                cwd: context_output.cwd,
                git_root: context_output.git_root,
                suggestion: context_output.suggestion.unwrap_or_else(|| {
                    "Repo not attached. Prefer: `homeboy project components attach-path <project-id> <path>`"
                        .to_string()
                }),
                action: "Run `homeboy status --all` to inspect every configured component, or attach this checkout to a project/component first.",
            }),
            0,
        ));
    }

    timer.begin("load_component_inventory");
    let all_components = component::inventory().unwrap_or_default();
    timer.finish("load_component_inventory");

    let show_all = args.all || relevant_ids.is_empty();

    let components: Vec<component::Component> = if show_all {
        all_components
    } else {
        all_components
            .into_iter()
            .filter(|c| relevant_ids.contains(&c.id))
            .collect()
    };

    summarize_components(components, &args, timer)
}

fn summarize_components(
    components: Vec<component::Component>,
    args: &StatusArgs,
    mut timer: StatusTimer,
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
    let mut git_cache = StatusGitCache::default();

    let has_filter =
        args.uncommitted || args.needs_release || args.ready || args.docs_only || args.unreleased;
    let include_upstream_drift = !has_filter;
    let include_unreleased_merges = !has_filter || args.unreleased;

    if include_upstream_drift || include_unreleased_merges {
        timer.begin("inspect_upstream_and_unreleased");
        for comp in &components {
            if include_upstream_drift {
                if let Some(drift) = git_cache.fetch_upstream_drift_for(comp) {
                    if drift.is_behind() {
                        behind_upstream.push(comp.id.clone());
                    }
                    upstream_drift.push(drift);
                }
            } else if include_unreleased_merges {
                git_cache.fetch_origin_tags_for(&comp.local_path);
            }

            // Detect merged-but-unreleased work per component (issue #4996). This is
            // measured against origin/<default-branch> (refreshed above), so a stale
            // local checkout does not hide unreleased merges.
            if include_unreleased_merges {
                if let Some(merge) = git_cache.detect_unreleased_merges_for(comp) {
                    unreleased_merges.push(merge);
                }
            }
        }
        timer.finish("inspect_upstream_and_unreleased");
    }

    timer.begin("inspect_release_state");
    for comp in &components {
        let status = git_cache
            .release_state_for(comp)
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
    timer.finish("inspect_release_state");

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
            timings: timer.into_timings(),
            clean,
        }),
        0,
    ))
}

/// Path override mode: inspect one checkout without requiring registry membership.
fn run_path_status(args: &StatusArgs) -> CmdResult<StatusResult> {
    let path = args.path.as_deref();
    let mut timer = StatusTimer::new(args.timings);
    timer.begin("resolve_path_component");
    let component = component::resolve_effective(args.project.as_deref(), path, None)?;
    timer.finish("resolve_path_component");

    if args.full {
        let mut report = context::build_report_for_component(args.all, "status", component, path)?;
        report.command = "status".to_string();
        return Ok((StatusResult::Full(report), 0));
    }

    summarize_components(vec![component], args, timer)
}

/// Project dashboard: show version drift across all components in a project.
///
/// Combines local version, remote (deployed) version, release state, upstream
/// drift, and unreleased commit count into a single view per component.
fn run_project_dashboard(project_id: &str, args: &StatusArgs) -> CmdResult<StatusResult> {
    let mut timer = StatusTimer::new(args.timings);

    timer.begin("resolve_project_components");
    let components = scope::resolve_scope_component_records(&Scope::Project(project_id.into()))?;
    timer.finish("resolve_project_components");

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
    timer.begin("read_local_versions");
    let local_versions: std::collections::HashMap<String, String> = components
        .iter()
        .filter_map(|c| version::get_component_version(c).map(|v| (c.id.clone(), v)))
        .collect();
    timer.finish("read_local_versions");

    // Gather remote versions via deploy check mode (handles SSH internally)
    timer.begin("fetch_remote_versions");
    let remote_probe = fetch_project_remote_versions(project_id, &components);
    let remote_versions = remote_probe.versions;
    let remote_diagnostics: HashMap<String, String> = remote_probe
        .failures
        .into_iter()
        .map(|failure| (failure.component_id, failure.diagnostic))
        .collect();
    timer.finish("fetch_remote_versions");

    let mut git_cache = StatusGitCache::default();

    // Fetch upstream drift for all components
    timer.begin("inspect_upstream_drift");
    let upstream_drift_map: std::collections::HashMap<String, UpstreamDrift> = components
        .iter()
        .filter_map(|c| {
            git_cache
                .fetch_upstream_drift_for(c)
                .map(|d| (c.id.clone(), d))
        })
        .collect();
    timer.finish("inspect_upstream_drift");

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
        bundled: 0,
        retired: 0,
        unknown: 0,
        degraded: 0,
    };

    timer.begin("build_dashboard_rows");
    for comp in &components {
        // Bundled/retired components are no longer standalone deploy targets.
        // Surface their lifecycle status for visibility, but do not run the
        // version/release-state machinery that would flag false `outdated`
        // drift (issue #3489).
        if !comp.is_active_lifecycle() {
            let dashboard_status = match comp.lifecycle {
                component::ComponentLifecycle::Bundled => {
                    summary.bundled += 1;
                    ProjectComponentDashboardStatus::Bundled
                }
                _ => {
                    summary.retired += 1;
                    ProjectComponentDashboardStatus::Retired
                }
            };
            rows.push(ProjectStatusRow {
                component_id: comp.id.clone(),
                local_version: local_versions.get(&comp.id).cloned(),
                remote_version: None,
                remote_version_diagnostic: None,
                origin_version: None,
                unreleased_commits: 0,
                ahead_upstream: None,
                behind_upstream: None,
                status: dashboard_status,
            });
            continue;
        }

        let local_ver = local_versions.get(&comp.id).cloned();
        let remote_ver = remote_versions.get(&comp.id).cloned();
        let drift = upstream_drift_map.get(&comp.id);

        let release_state = git_cache.release_state_for(comp).cloned();
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
        let dashboard_status = if remote_diagnostics.contains_key(&comp.id) {
            ProjectComponentDashboardStatus::Degraded
        } else {
            match release_status {
            ReleaseStateStatus::Uncommitted => ProjectComponentDashboardStatus::Uncommitted,
            ReleaseStateStatus::NeedsRelease => ProjectComponentDashboardStatus::NeedsRelease,
            ReleaseStateStatus::DocsOnly => ProjectComponentDashboardStatus::DocsOnly,
            ReleaseStateStatus::Clean => {
                // Check source freshness first. Deployment health is evaluated
                // independently, so a newer target is not marked outdated when
                // this configured checkout is stale.
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
            }
        };

        match &dashboard_status {
            ProjectComponentDashboardStatus::Current => summary.current += 1,
            ProjectComponentDashboardStatus::PinnedCurrent => summary.pinned_current += 1,
            ProjectComponentDashboardStatus::Outdated => summary.outdated += 1,
            ProjectComponentDashboardStatus::NeedsRelease => summary.needs_release += 1,
            ProjectComponentDashboardStatus::DocsOnly => summary.docs_only += 1,
            ProjectComponentDashboardStatus::Uncommitted => summary.uncommitted += 1,
            ProjectComponentDashboardStatus::BehindUpstream => summary.behind_upstream += 1,
            // Lifecycle statuses are assigned on the early-return path above and
            // never reach this active-component branch.
            ProjectComponentDashboardStatus::Bundled => summary.bundled += 1,
            ProjectComponentDashboardStatus::Retired => summary.retired += 1,
            ProjectComponentDashboardStatus::Unknown => summary.unknown += 1,
            ProjectComponentDashboardStatus::Degraded => summary.degraded += 1,
        }

        rows.push(ProjectStatusRow {
            component_id: comp.id.clone(),
            local_version: local_ver,
            remote_version: remote_ver,
            remote_version_diagnostic: remote_diagnostics.get(&comp.id).cloned(),
            origin_version: drift.and_then(|d| d.latest_origin_tag.clone()),
            unreleased_commits,
            ahead_upstream: drift.and_then(|d| d.ahead),
            behind_upstream: drift.and_then(|d| d.behind),
            status: dashboard_status,
        });
    }
    timer.finish("build_dashboard_rows");

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
            timings: timer.into_timings(),
        }),
        0,
    ))
}

fn deployed_version_dashboard_status(
    local_ver: &Option<String>,
    remote_ver: &Option<String>,
    origin_tag: Option<&str>,
) -> ProjectComponentDashboardStatus {
    match homeboy_release::deploy::compare_deployed_versions(
        local_ver.as_deref(),
        remote_ver.as_deref(),
    ) {
        homeboy_release::deploy::ComponentStatus::NeedsUpdate => {
            ProjectComponentDashboardStatus::Outdated
        }
        homeboy_release::deploy::ComponentStatus::UpToDate
            if local_ver.as_deref().is_some_and(|local| {
                origin_tag_is_newer_than_local(origin_tag, local)
            }) =>
        {
            ProjectComponentDashboardStatus::PinnedCurrent
        }
        homeboy_release::deploy::ComponentStatus::Unknown => {
            ProjectComponentDashboardStatus::Unknown
        }
        homeboy_release::deploy::ComponentStatus::UpToDate
        | homeboy_release::deploy::ComponentStatus::BehindRemote => {
            ProjectComponentDashboardStatus::Current
        }
        homeboy_release::deploy::ComponentStatus::BehindUpstream
        | homeboy_release::deploy::ComponentStatus::SourceStale => {
            unreachable!("version comparison only returns version statuses")
        }
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

#[cfg(test)]
mod tests {
    use super::git_cache::{component_cache_key, default_origin_branch, upstream_drift_cache_key};
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use crate::commands::GlobalArgs;
    use clap::Parser;
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    use tempfile::TempDir;

    static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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
            timings: false,
        }
    }

    fn default_status_args() -> StatusArgs {
        StatusArgs {
            project: None,
            path: None,
            full: false,
            uncommitted: false,
            needs_release: false,
            ready: false,
            docs_only: false,
            all: false,
            outdated: false,
            unreleased: false,
            timings: false,
        }
    }

    fn make_git_repo(name: &str) -> (TempDir, std::path::PathBuf) {
        crate::test_support::shared_git_repo_fixture(name)
    }

    fn make_committed_git_repo(name: &str) -> (TempDir, std::path::PathBuf) {
        crate::test_support::shared_committed_git_repo_fixture(name)
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
            timings: Vec::new(),
            clean: 0,
        }
    }

    #[test]
    fn parser_accepts_status_timings() {
        let cli = Cli::try_parse_from(["homeboy", "status", "--timings"])
            .expect("status --timings parses");

        match cli.command {
            Commands::Status(args) => assert!(args.timings),
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn status_timings_are_omitted_unless_present() {
        let output = empty_status_output();
        let json = serde_json::to_value(&output).expect("serialize status output");
        assert!(json.get("timings").is_none());

        let output = StatusOutput {
            timings: vec![StatusTiming {
                phase: "inspect_release_state",
                elapsed_ms: 12,
            }],
            ..empty_status_output()
        };
        let json = serde_json::to_value(&output).expect("serialize status output");

        assert_eq!(
            json.get("timings")
                .and_then(|value| value.as_array())
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(json["timings"][0]["phase"], "inspect_release_state");
        assert_eq!(json["timings"][0]["elapsed_ms"], 12);
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
    fn deployed_version_status_keeps_newer_remote_current() {
        let status = deployed_version_dashboard_status(
            &Some("0.12.2".to_string()),
            &Some("0.12.15".to_string()),
            Some("v0.12.15"),
        );

        assert!(matches!(status, ProjectComponentDashboardStatus::Current));
    }

    #[test]
    fn deployed_version_status_marks_unknown_versions_unknown() {
        let status = deployed_version_dashboard_status(
            &Some("not-a-version".to_string()),
            &Some("1.0.0".to_string()),
            None,
        );

        assert!(matches!(status, ProjectComponentDashboardStatus::Unknown));
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
    fn default_status_from_unregistered_cwd_returns_actionable_context_without_global_scan() {
        let _guard = CWD_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let original_cwd = env::current_dir().expect("current dir");
        let dir = TempDir::new().expect("tempdir");
        env::set_current_dir(dir.path()).expect("set unregistered cwd");

        let started = Instant::now();
        let result = run(default_status_args(), &GlobalArgs {});
        let elapsed = started.elapsed();

        env::set_current_dir(original_cwd).expect("restore cwd");
        let (result, code) = result.expect("status succeeds from unregistered cwd");

        assert_eq!(code, 0);
        assert!(
            elapsed.as_secs() < 2,
            "unregistered status should fast-return, elapsed={elapsed:?}"
        );
        match result {
            StatusResult::UnregisteredContext(output) => {
                assert_eq!(output.status, "unregistered_context");
                assert_eq!(
                    PathBuf::from(&output.cwd).canonicalize().ok(),
                    dir.path().canonicalize().ok()
                );
                assert!(output.suggestion.contains("attach"));
                assert!(output.action.contains("homeboy status --all"));
            }
            _ => panic!("expected unregistered context output"),
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
        let (_dir, repo) = make_committed_git_repo("with-origin");
        // Build a fake "origin" remote by cloning into a bare repo and wiring it up.
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
        let (_dir, repo) = make_committed_git_repo("fallback-origin");
        // No origin/HEAD symbolic ref; only a conventional remote-tracking ref.
        run_git(&repo, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);

        let resolved = default_origin_branch(&repo.to_string_lossy());
        assert_eq!(resolved.as_deref(), Some("origin/trunk"));
    }

    #[test]
    fn default_origin_branch_none_without_remote_refs() {
        let (_dir, repo) = make_committed_git_repo("no-origin");

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

    #[test]
    fn upstream_drift_cache_is_component_scoped_in_shared_repos() {
        let (_dir, repo) = make_git_repo("monorepo");
        let component_dir = repo.join("components/demo");
        fs::create_dir_all(&component_dir).expect("component dir");
        let component = component::Component {
            id: "actual-component".to_string(),
            local_path: component_dir.to_string_lossy().to_string(),
            ..Default::default()
        };

        let repo_key = upstream_drift_cache_key(&repo.to_string_lossy());
        let component_key = upstream_drift_cache_key(&component_dir.to_string_lossy());
        assert_eq!(component_key, repo_key);

        let scoped_cache_key = component_cache_key(&component);
        assert_ne!(scoped_cache_key, repo_key);

        let mut git_cache = StatusGitCache::default();
        git_cache.upstream_drift.insert(
            scoped_cache_key,
            Some(UpstreamDrift {
                component_id: "cached-component".to_string(),
                ahead: Some(2),
                behind: Some(1),
                latest_origin_tag: Some("v1.2.3".to_string()),
            }),
        );

        let drift = git_cache
            .fetch_upstream_drift_for(&component)
            .expect("cached drift");

        assert_eq!(drift.component_id, "actual-component");
        assert_eq!(drift.ahead, Some(2));
        assert_eq!(drift.behind, Some(1));
        assert_eq!(drift.latest_origin_tag.as_deref(), Some("v1.2.3"));
    }
}
