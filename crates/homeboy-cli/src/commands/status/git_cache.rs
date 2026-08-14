//! Git-state caching and probing for the `status` command.
//!
//! `StatusGitCache` memoizes per-component git work (tag fetches, upstream
//! drift, release-state baselines, default-branch resolution) so a single
//! status run touches each repo's git plumbing once. The free functions below
//! back the cache and the merged-not-released / remote-version probes.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use homeboy::core::component;
use homeboy::core::git;
use homeboy_deploy::{self as deploy, ReleaseState};
use homeboy_release::release::version;

use super::types::{StatusTimer, UnreleasedMerge, UpstreamDrift};

pub(super) struct ProjectRemoteVersionProbe {
    pub(super) result: deploy::RemoteVersionProbeResult,
    pub(super) failure: Option<String>,
}

#[derive(Default)]
pub(super) struct StatusGitCache {
    refresh: bool,
    pub(super) upstream_drift: HashMap<String, Option<UpstreamDrift>>,
    fetched_tags: HashSet<String>,
    release_states: HashMap<String, Option<ReleaseState>>,
    baselines: HashMap<String, Option<git::BaselineInfo>>,
    origin_branches: HashMap<String, Option<String>>,
    pub(super) degraded_components: HashSet<String>,
    pub(super) degraded_component_phases: HashMap<String, HashSet<&'static str>>,
}

impl StatusGitCache {
    pub(super) fn with_refresh(refresh: bool) -> Self {
        Self {
            refresh,
            ..Self::default()
        }
    }
}

impl StatusGitCache {
    pub(super) fn fetch_origin_tags_for(&mut self, path: &str, timer: &StatusTimer) {
        let cache_key = upstream_drift_cache_key(path, timer);
        if self.refresh && self.fetched_tags.insert(cache_key) {
            fetch_origin_tags(path, timer);
        }
    }

    pub(super) fn fetch_upstream_drift_for(
        &mut self,
        component: &component::Component,
        timer: &StatusTimer,
    ) -> Option<UpstreamDrift> {
        let path = &component.local_path;
        let cache_key = component_cache_key(component);
        if !self.upstream_drift.contains_key(&cache_key) {
            self.fetch_origin_tags_for(path, timer);
            let drift = get_upstream_drift(component, timer);
            if drift.is_none() {
                self.mark_degraded(component, "inspect_upstream_and_unreleased");
            }
            self.upstream_drift.insert(cache_key.clone(), drift);
        }

        let drift = self.upstream_drift.get(&cache_key)?;

        drift.as_ref().map(|cached| {
            let mut drift = cached.clone();
            drift.component_id = component.id.clone();
            drift
        })
    }

    pub(super) fn release_state_for(
        &mut self,
        component: &component::Component,
        timer: &StatusTimer,
    ) -> Option<&ReleaseState> {
        let cache_key = component_cache_key(component);
        if !self.release_states.contains_key(&cache_key) {
            let state = self
                .baseline_for(component, timer)
                .and_then(|baseline| release_state_for_baseline(component, baseline, timer));
            if state.is_none() {
                self.mark_degraded(component, "inspect_release_state");
            }
            self.release_states.insert(cache_key.clone(), state);
        }

        self.release_states.get(&cache_key).and_then(Option::as_ref)
    }

    fn baseline_for(
        &mut self,
        component: &component::Component,
        timer: &StatusTimer,
    ) -> Option<&git::BaselineInfo> {
        let cache_key = component_cache_key(component);
        if !self.baselines.contains_key(&cache_key) {
            self.fetch_origin_tags_for(&component.local_path, timer);
            // Baseline discovery currently uses synchronous local Git helpers.
            // Never enter that sequence after the shared status deadline.
            if timer.expired() {
                self.mark_degraded(component, "inspect_release_state");
                self.baselines.insert(cache_key.clone(), None);
                return None;
            }
            let current_version = version::read_component_version(component)
                .ok()
                .map(|info| info.version);
            let tag_prefix = homeboy_release::release::component_tag_prefix(component)
                .ok()
                .flatten();
            let baseline = detect_baseline_with_deadline(
                &component.local_path,
                current_version.as_deref(),
                tag_prefix.as_deref(),
                timer,
            )
            .ok();
            if baseline.is_none() {
                self.mark_degraded(component, "inspect_release_state");
            }
            self.baselines.insert(cache_key.clone(), baseline);
        }

        self.baselines.get(&cache_key).and_then(Option::as_ref)
    }

    fn mark_degraded(&mut self, component: &component::Component, phase: &'static str) {
        self.degraded_components.insert(component.id.clone());
        self.degraded_component_phases
            .entry(component.id.clone())
            .or_default()
            .insert(phase);
    }

    fn default_origin_branch_for(&mut self, path: &str, timer: &StatusTimer) -> Option<&str> {
        let cache_key = upstream_drift_cache_key(path, timer);
        if !self.origin_branches.contains_key(&cache_key) {
            self.origin_branches
                .insert(cache_key.clone(), default_origin_branch(path, timer));
        }

        self.origin_branches
            .get(&cache_key)
            .and_then(Option::as_deref)
    }

    pub(super) fn detect_unreleased_merges_for(
        &mut self,
        comp: &component::Component,
        timer: &StatusTimer,
    ) -> Option<UnreleasedMerge> {
        let path = &comp.local_path;

        let origin_branch = self.default_origin_branch_for(path, timer)?.to_string();
        let baseline = self.baseline_for(comp, timer)?;
        let baseline_ref = baseline.reference.as_deref()?;

        let range = format!("{}..{}", baseline_ref, origin_branch);
        let count_output = git::run_git_with_env_timeout(
            std::path::Path::new(path),
            &["rev-list", "--count", "--no-merges", &range],
            "status git unreleased merges",
            &[],
            local_probe_timeout(timer)?,
        )
        .ok()?;

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
}

pub(super) fn upstream_drift_cache_key(path: &str, timer: &StatusTimer) -> String {
    timer
        .remaining()
        .and_then(|_| local_probe_timeout(timer))
        .and_then(|timeout| git::get_git_root_with_timeout(path, timeout).ok())
        .unwrap_or_else(|| path.to_string())
}

pub(super) fn component_cache_key(component: &component::Component) -> String {
    format!("{}\0{}", component.id, component.local_path)
}

/// Per-component `git fetch` bound so an unresponsive remote can't stall
/// `homeboy status` indefinitely in a many-component workspace (#7378). The
/// fetch is best-effort — a timeout or missing remote falls back to local tag
/// data, exactly like the previous unbounded best-effort fetch.
const STATUS_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const STATUS_LOCAL_GIT_TIMEOUT: Duration = Duration::from_secs(5);

fn local_probe_timeout(timer: &StatusTimer) -> Option<Duration> {
    timer
        .remaining()
        .map(|remaining| remaining.min(STATUS_LOCAL_GIT_TIMEOUT))
}

fn fetch_origin_tags(path: &str, timer: &StatusTimer) {
    // Best-effort, timeout-bounded fetch — silently proceeds on no remote,
    // network issue, or timeout, using whatever local tags are already present.
    let Some(remaining) = timer.remaining() else {
        return;
    };
    let _ = git::run_git_with_env_timeout(
        std::path::Path::new(path),
        &["fetch", "--tags", "--quiet"],
        "status fetch origin tags",
        &[],
        remaining.min(STATUS_FETCH_TIMEOUT),
    );
}

fn get_upstream_drift(
    component: &component::Component,
    timer: &StatusTimer,
) -> Option<UpstreamDrift> {
    let path = &component.local_path;
    let snapshot = git::get_repo_snapshot_with_timeout(path, local_probe_timeout(timer)?).ok()?;

    // After fetching tags, find the latest tag across ALL refs (not just HEAD).
    // `git describe --tags --abbrev=0` only returns tags reachable from HEAD,
    // which misses newer tags when the local checkout is behind.
    let tag_prefix = homeboy_release::release::component_tag_prefix(component)
        .ok()
        .flatten();
    let latest_origin_tag = git::get_latest_tag_any_with_prefix_with_timeout(
        path,
        tag_prefix.as_deref(),
        local_probe_timeout(timer)?,
    )
    .ok()
    .flatten();

    Some(UpstreamDrift {
        component_id: String::new(), // caller sets component_id after
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        latest_origin_tag,
    })
}

fn detect_baseline_with_deadline(
    path: &str,
    current_version: Option<&str>,
    tag_prefix: Option<&str>,
    timer: &StatusTimer,
) -> Result<git::BaselineInfo, ()> {
    let Some(tag) = latest_merged_release_tag(path, tag_prefix, timer)? else {
        return Ok(git::BaselineInfo {
            latest_tag: None,
            source: Some(git::BaselineSource::LastNCommits),
            reference: None,
            warning: Some(
                "No release tag found; using repository history as baseline.".to_string(),
            ),
        });
    };
    let tag_version = git::extract_version_from_tag(&tag);
    if current_version
        .zip(tag_version.as_deref())
        .is_some_and(|(current, tag)| current != tag)
    {
        // Version-commit fallback is intentionally bounded as well. The tag is
        // still a safe baseline when no matching release commit is found.
        let current = current_version.ok_or(())?;
        let log = git::run_git_with_env_timeout(
            std::path::Path::new(path),
            &["log", "-200", "--format=%h|%s"],
            "status git version baseline",
            &[],
            local_probe_timeout(timer).ok_or(())?,
        )
        .map_err(|_| ())?;
        if let Some(reference) = log.lines().find_map(|line| {
            let (hash, subject) = line.split_once('|')?;
            subject.contains(current).then(|| hash.to_string())
        }) {
            return Ok(git::BaselineInfo {
                latest_tag: Some(tag),
                source: Some(git::BaselineSource::VersionCommit),
                reference: Some(reference),
                warning: Some(
                    "Version differs from latest tag; using matching version commit.".to_string(),
                ),
            });
        }
    }
    Ok(git::BaselineInfo {
        latest_tag: Some(tag.clone()),
        source: Some(git::BaselineSource::Tag),
        reference: Some(tag),
        warning: None,
    })
}

fn latest_merged_release_tag(
    path: &str,
    tag_prefix: Option<&str>,
    timer: &StatusTimer,
) -> Result<Option<String>, ()> {
    let tags = git::run_git_with_env_timeout(
        std::path::Path::new(path),
        &["tag", "--merged", "HEAD", "--sort=-v:refname", "--list"],
        "status git release baseline tag",
        &[],
        local_probe_timeout(timer).ok_or(())?,
    )
    .map_err(|_| ())?;
    Ok(tags
        .lines()
        .map(str::trim)
        .filter(|tag| tag_prefix.is_none_or(|prefix| tag.strip_prefix(prefix).is_some()))
        .filter_map(|tag| {
            let version = git::extract_version_from_tag(tag)?;
            semver::Version::parse(&version)
                .ok()
                .map(|version| (tag, version))
        })
        .max_by(|(_, left), (_, right)| left.cmp(right))
        .map(|(tag, _)| tag.to_string()))
}

fn release_state_for_baseline(
    component: &component::Component,
    baseline: &git::BaselineInfo,
    timer: &StatusTimer,
) -> Option<ReleaseState> {
    let range = baseline
        .reference
        .as_deref()
        .map(|reference| format!("{reference}..HEAD"))
        .unwrap_or_else(|| "HEAD".to_string());
    let commits = git::run_git_with_env_timeout(
        std::path::Path::new(&component.local_path),
        &["log", "--no-merges", &range, "--format=%h|%s"],
        "status git release commits",
        &[],
        local_probe_timeout(timer)?,
    )
    .ok()?;
    let mut total = 0;
    let mut docs_only = 0;
    for line in commits.lines() {
        let (hash, subject) = line.split_once('|')?;
        total += 1;
        if subject.to_ascii_lowercase().starts_with("docs") {
            docs_only += 1;
            continue;
        }
        let files = git::run_git_with_env_timeout(
            std::path::Path::new(&component.local_path),
            &["diff-tree", "--no-commit-id", "--name-only", "-r", hash],
            "status git commit files",
            &[],
            local_probe_timeout(timer)?,
        )
        .ok()?;
        if files
            .lines()
            .all(|file| file.ends_with(".md") || file.contains("/docs/"))
        {
            docs_only += 1;
        }
    }
    let status = git::run_git_with_env_timeout(
        std::path::Path::new(&component.local_path),
        &["status", "--porcelain=v1", "--untracked-files=normal"],
        "status git release worktree",
        &[],
        local_probe_timeout(timer)?,
    )
    .ok()?;
    Some(ReleaseState {
        commits_since_version: total,
        code_commits: total - docs_only,
        docs_only_commits: docs_only,
        has_uncommitted_changes: !status.is_empty(),
        baseline_ref: baseline.reference.clone(),
        baseline_warning: baseline.warning.clone(),
    })
}

/// Log merged-but-unreleased components to stderr for human-readable output.
///
/// Mirrors the dashboard table's terminal-only behavior so JSON consumers are
/// unaffected. Keeps the merged-not-released signal visible in `homeboy status`
/// without a project argument (issue #4996).
pub(super) fn log_unreleased_merges(merges: &[UnreleasedMerge]) {
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
pub(super) fn default_origin_branch(path: &str, timer: &StatusTimer) -> Option<String> {
    if let Ok(symbolic) = git::run_git_with_env_timeout(
        std::path::Path::new(path),
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
        "status git origin head",
        &[],
        local_probe_timeout(timer)?,
    ) {
        let symbolic = symbolic.trim();
        if !symbolic.is_empty() {
            return Some(symbolic.to_string());
        }
    }

    ["origin/main", "origin/trunk", "origin/master"]
        .iter()
        .find(|branch| {
            git::run_git_with_env_timeout(
                std::path::Path::new(path),
                &["rev-parse", "--verify", "--quiet", branch],
                "status git origin branch",
                &[],
                local_probe_timeout(timer).unwrap_or(Duration::ZERO),
            )
            .is_ok()
        })
        .map(|branch| (*branch).to_string())
}

/// Fetch remote (deployed) versions for all components in a project.
///
/// Uses deploy check mode internally, which handles SSH resolution.
/// Returns empty map on failure (e.g., no server configured, SSH unavailable).
pub(super) fn fetch_project_remote_versions(
    project_id: &str,
    components: &[component::Component],
    timer: &StatusTimer,
) -> ProjectRemoteVersionProbe {
    match deploy::fetch_project_remote_versions_with_deadline(
        project_id,
        components,
        timer.deadline(),
    ) {
        Ok(result) => ProjectRemoteVersionProbe {
            result,
            failure: None,
        },
        Err(error) => {
            homeboy::log_status!(
                "status",
                "Warning: could not fetch remote versions for project '{}' — showing local data only",
                project_id
            );
            ProjectRemoteVersionProbe {
                result: deploy::RemoteVersionProbeResult::default(),
                failure: Some(error.to_string()),
            }
        }
    }
}
