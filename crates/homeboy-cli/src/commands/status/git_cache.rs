//! Git-state caching and probing for the `status` command.
//!
//! `StatusGitCache` memoizes per-component git work (tag fetches, upstream
//! drift, release-state baselines, default-branch resolution) so a single
//! status run touches each repo's git plumbing once. The free functions below
//! back the cache and the merged-not-released / remote-version probes.

use std::collections::{HashMap, HashSet};

use homeboy::core::component;
use homeboy_release::deploy::{self, ReleaseState};
use homeboy::core::git;
use homeboy_release::release::version;

use super::types::{UnreleasedMerge, UpstreamDrift};

#[derive(Default)]
pub(super) struct StatusGitCache {
    pub(super) upstream_drift: HashMap<String, Option<UpstreamDrift>>,
    fetched_tags: HashSet<String>,
    release_states: HashMap<String, Option<ReleaseState>>,
    baselines: HashMap<String, Option<git::BaselineInfo>>,
    origin_branches: HashMap<String, Option<String>>,
}

impl StatusGitCache {
    pub(super) fn fetch_origin_tags_for(&mut self, path: &str) {
        let cache_key = upstream_drift_cache_key(path);
        if self.fetched_tags.insert(cache_key) {
            fetch_origin_tags(path);
        }
    }

    pub(super) fn fetch_upstream_drift_for(
        &mut self,
        component: &component::Component,
    ) -> Option<UpstreamDrift> {
        let path = &component.local_path;
        let cache_key = component_cache_key(component);
        if !self.upstream_drift.contains_key(&cache_key) {
            self.fetch_origin_tags_for(path);
            self.upstream_drift
                .insert(cache_key.clone(), get_upstream_drift(component));
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
    ) -> Option<&ReleaseState> {
        let cache_key = component_cache_key(component);
        if !self.release_states.contains_key(&cache_key) {
            let state = self.baseline_for(component).and_then(|baseline| {
                deploy::calculate_release_state_from_baseline(component, baseline)
            });
            self.release_states.insert(cache_key.clone(), state);
        }

        self.release_states.get(&cache_key).and_then(Option::as_ref)
    }

    fn baseline_for(&mut self, component: &component::Component) -> Option<&git::BaselineInfo> {
        let cache_key = component_cache_key(component);
        if !self.baselines.contains_key(&cache_key) {
            self.fetch_origin_tags_for(&component.local_path);
            let current_version = version::read_component_version(component)
                .ok()
                .map(|info| info.version);
            let tag_prefix = homeboy_release::release::component_tag_prefix(component)
                .ok()
                .flatten();
            let baseline = git::detect_baseline_with_version_and_tag_prefix_from_fetched_tags(
                &component.local_path,
                current_version.as_deref(),
                tag_prefix.as_deref(),
            )
            .ok();
            self.baselines.insert(cache_key.clone(), baseline);
        }

        self.baselines.get(&cache_key).and_then(Option::as_ref)
    }

    fn default_origin_branch_for(&mut self, path: &str) -> Option<&str> {
        let cache_key = upstream_drift_cache_key(path);
        if !self.origin_branches.contains_key(&cache_key) {
            self.origin_branches
                .insert(cache_key.clone(), default_origin_branch(path));
        }

        self.origin_branches
            .get(&cache_key)
            .and_then(Option::as_deref)
    }

    pub(super) fn detect_unreleased_merges_for(
        &mut self,
        comp: &component::Component,
    ) -> Option<UnreleasedMerge> {
        let path = &comp.local_path;

        let origin_branch = self.default_origin_branch_for(path)?.to_string();
        let baseline = self.baseline_for(comp)?;
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
}

pub(super) fn upstream_drift_cache_key(path: &str) -> String {
    git::get_git_root(path).unwrap_or_else(|_| path.to_string())
}

pub(super) fn component_cache_key(component: &component::Component) -> String {
    format!("{}\0{}", component.id, component.local_path)
}

fn fetch_origin_tags(path: &str) {
    // Best-effort fetch — silently proceeds if no remote or network issue.
    let _ = homeboy::core::engine::command::run_in_optional(
        path,
        "git",
        &["fetch", "--tags", "--quiet"],
    );
}

fn get_upstream_drift(component: &component::Component) -> Option<UpstreamDrift> {
    let path = &component.local_path;
    let snapshot = git::get_repo_snapshot(path).ok()?;

    // After fetching tags, find the latest tag across ALL refs (not just HEAD).
    // `git describe --tags --abbrev=0` only returns tags reachable from HEAD,
    // which misses newer tags when the local checkout is behind.
    let tag_prefix = homeboy_release::release::component_tag_prefix(component)
        .ok()
        .flatten();
    let latest_origin_tag = git::get_latest_tag_any_with_prefix(path, tag_prefix.as_deref())
        .ok()
        .flatten();

    Some(UpstreamDrift {
        component_id: String::new(), // caller sets component_id after
        ahead: snapshot.ahead,
        behind: snapshot.behind,
        latest_origin_tag,
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
pub(super) fn default_origin_branch(path: &str) -> Option<String> {
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
pub(super) fn fetch_project_remote_versions(
    project_id: &str,
    components: &[component::Component],
) -> deploy::RemoteVersionProbeResult {
    match deploy::fetch_project_remote_versions(project_id, components) {
        Ok(result) => result,
        Err(_) => {
            homeboy::log_status!(
                "status",
                "Warning: could not fetch remote versions for project '{}' — showing local data only",
                project_id
            );
            deploy::RemoteVersionProbeResult::default()
        }
    }
}
