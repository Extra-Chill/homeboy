//! Data types for the `status` command: CLI args, serialized output shapes,
//! dashboard rows/summaries, and the phase timer.

use serde::Serialize;
use std::time::Instant;

use clap::Args;

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

    /// Emit status phase progress to stderr and include phase timings in JSON
    #[arg(long)]
    pub timings: bool,

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
    pub(super) fn is_behind(&self) -> bool {
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

#[derive(Debug, Clone, Serialize)]
pub struct StatusTiming {
    pub phase: &'static str,
    pub elapsed_ms: u128,
}

/// Clarifying note emitted alongside the git-state-only `ready_to_deploy` list.
///
/// `ready_to_deploy` reflects local git/workspace state ("has a clean release
/// tag that *could* be deployed"), NOT whether the deploy target is actually
/// behind. Acting on it blindly re-deploys components that may already be live.
/// For a target-accurate diff (installed version vs latest release tag) run
/// `homeboy status <project>`, which reports `current` / `outdated` per
/// component. See issue #4588.
pub(super) const READY_TO_DEPLOY_NOTE: &str = "ready_to_deploy is git-state-only (components with a clean release tag that *could* be deployed); it does NOT mean the deploy target is behind. Run `homeboy status <project>` for a target-accurate diff (installed version vs latest release tag).";

/// Clarifying note emitted alongside `unreleased_merges`.
///
/// `unreleased_merges` flags components whose `origin/<default-branch>` carries
/// commits past the latest release tag — i.e. work that is merged but not in any
/// release, so the code does NOT exist on prod. This is the merged→released axis
/// of the merged→released→deployed chain; for the released→deployed axis
/// (installed version vs latest tag) run `homeboy status <project>`. See #4996.
pub(super) const UNRELEASED_MERGES_NOTE: &str = "unreleased_merges flags components with commits merged to origin/<default-branch> that are past the latest release tag (merged but NOT released — the code is not on prod yet). A merged PR here is NOT 'shipped'. Cut a release, then run `homeboy status <project>` to confirm installed-vs-tag (released-but-not-deployed).";

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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub timings: Vec<StatusTiming>,
    pub clean: usize,
}

#[derive(Debug, Serialize)]
pub struct UnregisteredContextStatusOutput {
    pub command: &'static str,
    pub status: &'static str,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_root: Option<String>,
    pub suggestion: String,
    pub action: &'static str,
}

/// A single row in the project status dashboard.
#[derive(Debug, Serialize)]
pub struct ProjectStatusRow {
    pub component_id: String,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    /// Actionable evidence when the bounded remote version probe failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_version_diagnostic: Option<String>,
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
    /// Absorbed into another component; not independently deployable. Tracked
    /// for visibility but excluded from deploy/outdated obligations.
    Bundled,
    /// Sunset; no longer a deploy target. Excluded from outdated obligations.
    Retired,
    /// Cannot determine status
    Unknown,
    /// A bounded remote probe failed; local and git data remain available.
    Degraded,
}

/// Output for the project status dashboard.
#[derive(Debug, Serialize)]
pub struct ProjectDashboardOutput {
    pub command: &'static str,
    pub project_id: String,
    pub total: usize,
    pub components: Vec<ProjectStatusRow>,
    pub summary: ProjectDashboardSummary,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub timings: Vec<StatusTiming>,
}

pub(super) struct StatusTimer {
    enabled: bool,
    phase_started: Instant,
    timings: Vec<StatusTiming>,
}

impl StatusTimer {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            phase_started: Instant::now(),
            timings: Vec::new(),
        }
    }

    pub(super) fn begin(&mut self, phase: &'static str) {
        if self.enabled {
            eprintln!("[status] {phase}...");
        }
        self.phase_started = Instant::now();
    }

    pub(super) fn finish(&mut self, phase: &'static str) {
        if !self.enabled {
            return;
        }

        let elapsed_ms = self.phase_started.elapsed().as_millis();
        eprintln!("[status] {phase} completed in {elapsed_ms}ms");
        self.timings.push(StatusTiming { phase, elapsed_ms });
    }

    pub(super) fn into_timings(self) -> Vec<StatusTiming> {
        self.timings
    }
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
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bundled: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retired: usize,
    pub unknown: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub degraded: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

pub enum StatusResult {
    Summary(StatusOutput),
    UnregisteredContext(UnregisteredContextStatusOutput),
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
            StatusResult::UnregisteredContext(output) => output.serialize(serializer),
            StatusResult::Full(output) => output.serialize(serializer),
            StatusResult::Dashboard(output) => output.serialize(serializer),
        }
    }
}
