//! Reclaim the local runner artifact download cache under
//! `<artifact-root>/runner`.
//!
//! # What actually lives here
//!
//! Exactly one writer produces this tree:
//! `homeboy_lab_runner::evidence::download::download_remote_artifact`. Both of
//! its transports (daemon relay and direct SSH) default their output path to
//!
//! ```text
//! <artifact-root>/runner/<runner-id>/<run-id>/<file-name>
//! ```
//!
//! when the caller passes no explicit `--output`. Every reachable caller —
//! `homeboy runs artifact get`, `homeboy runs artifacts <run-id> --pull`,
//! `lab apply`, evidence mirroring, and the HTTP artifact endpoint — funnels
//! through it. So this directory is not scratch: it is the operator-visible
//! download cache, and `runs artifact get` hands the resulting path back to the
//! caller as *the* location of their bytes.
//!
//! # Why the previous implementation was unsafe
//!
//! Before #10564 this function was an unconditional `fs::remove_dir_all` of the
//! whole root. Its only checks were path-containment ones: `--run-id` requires
//! `--runner`, each filter must be a single normal path component, and the root
//! must be a real directory. Those prove the deletion stays *inside* the cache.
//! They prove nothing about whether the bytes are dead. A bare `homeboy cleanup
//! --apply` swept every category, so artifacts an operator pulled seconds
//! earlier for review were removed by an unrelated sweep.
//!
//! # The predicate
//!
//! A cache directory is reclaimable only when all of the following hold. Each
//! is necessary; none is sufficient alone.
//!
//! 0. **Recorded intent.** The writer tags each cache directory it creates with
//!    a [`crate::runner_download_cache::RUNNER_DOWNLOAD_MARKER_FILE`] sidecar
//!    saying *why* the bytes were fetched (#10585). Only an explicit
//!    `internal_fetch` is reclaimable. An `operator_pull` tag, an unreadable
//!    tag, and — critically — **no tag at all** all retain, so every cache
//!    directory written before intent tagging existed is retained rather than
//!    swept. Age proves bytes are old; only the tag can prove they are
//!    homeboy's rather than the operator's, and the two are independent.
//! 1. **Ownership by name shape.** The candidate must be a real directory at
//!    exactly `<artifact-root>/runner/<a>/<b>` — the only shape the single
//!    writer above emits — with no symlink at either level. Anything else under
//!    the root (a loose file, a symlink, a bare `<a>` with no run caches) is
//!    reported and retained forever by this category. Following the
//!    [`super::orphaned_artifact_bytes`] precedent, ownership is proven from
//!    the writer's layout, never from a database join: bytes here are written
//!    before (and often entirely without) any local `artifacts` row, so row
//!    absence is the normal state of a download that is *succeeding*.
//! 2. **Age floor.** The newest modification time anywhere in the candidate's
//!    subtree must be at least [`RUNNER_DOWNLOAD_MIN_AGE`] old. The floor is
//!    the shared [`crate::cleanup::RUNNER_MIN_AGE_HOURS`] and is deliberately
//!    not operator-overridable — lowering it is a widening of a delete
//!    predicate. Taking the *newest* mtime is what makes a fresh pull beside a
//!    stale one survive: one new byte re-arms the whole cache directory.
//! 3. **Liveness veto.** The observation store is consulted in the *retain*
//!    direction only. If it positively reports a non-terminal run matching the
//!    `<run-id>` component, the candidate is retained. A missing row never
//!    authorizes removal — the age floor is the only authorization — so this
//!    cannot become the row-join data-loss bug that [`super::orphaned_artifact_bytes`]
//!    documents.
//! 4. **Fail closed everywhere.** An unreadable mtime, a future-dated mtime, an
//!    unwalkable subtree, a path that does not resolve inside the artifact
//!    root, an observation store that cannot be opened, or a running-run scan
//!    that hit its bound all *retain*. No uncertainty releases bytes.
//!
//! Removal is per cache directory, never whole-root, so a stale cache and a
//! fresh one under the same runner are decided independently.
//!
//! # Size is advisory and never gates removal
//!
//! Sizes are measured best effort purely for reporting. A failed measurement
//! sets `size_measured: false` and changes the verdict in neither direction,
//! per the precedent in [`super::orphaned_artifact_bytes`] and the
//! `bounded_directory_size` regression it describes.
//!
//! # Narrowing filters are not a substitute for the predicate
//!
//! `--runner` and `--run-id` restrict which candidates are *considered*. They
//! never bypass a check. An operator naming a run id is asking "clean this
//! one", not "skip the safety check".
//!
//! # Historical: non-canonical depth
//!
//! `RemoteArtifactToken::parse` percent-decodes its components *after* its only
//! containment check, so a runner id or run id containing an encoded `/` used
//! to produce a deeper tree than `<a>/<b>`, and a remote-supplied `filename`
//! was joined unsanitized (#10586). Such a tree was still age-gated, but its
//! depth-2 component was not a real run id, so the liveness veto could not key
//! on it. The writer now rejects any decoded id that is not a single path
//! component and sanitizes the file name
//! ([`crate::runner_download_cache::resolve_runner_download_target`]), so no
//! new non-canonical tree can appear. Pre-existing ones are untagged, and
//! untagged retains, so this category will never remove one.

use std::time::{Duration, SystemTime};

use super::persisted_cleanup::{path_is_within_root, symlink_metadata_if_exists};
use super::run_lookup::run_matches_label;
use super::*;
// The cache layout and its intent marker are owned by the writer's module, not
// restated here: the reclaimer and the writer must never drift onto two
// different directories or two different marker names.
use crate::runner_download_cache::{
    read_download_ownership, RUNNER_DOWNLOAD_DIR, RUNNER_DOWNLOAD_MARKER_FILE,
};

/// Age floor before a cached runner artifact download is reclaimable.
///
/// Shares [`crate::cleanup::RUNNER_MIN_AGE_HOURS`] with the other runner-scoped
/// cleanup categories and is intentionally not exposed as a flag or a
/// configuration key: this cache is the operator's copy of bytes they asked for,
/// so shortening the window is a data-loss lever, not a retention preference.
pub const RUNNER_DOWNLOAD_MIN_AGE: Duration =
    Duration::from_secs(crate::cleanup::RUNNER_MIN_AGE_HOURS * 3_600);

/// Candidate entries inspected when a caller does not resolve a budget from
/// [`crate::cleanup::CleanupPolicy`].
const DEFAULT_INSPECTION_LIMIT: usize = 1_000;

/// Maximum running-run rows read to build the liveness veto.
///
/// Exceeding it means an unseen running run could own any candidate, so the
/// sweep degrades to "retain everything" rather than to "veto nothing".
const RUNNING_RUN_SCAN_LIMIT: usize = 1_000;

/// Maximum directory depth walked inside one cache directory. A tree deeper
/// than this cannot be proven old, so it is retained.
const MAX_SCAN_DEPTH: usize = 64;

#[derive(Debug, Clone)]
pub struct RunnerDownloadCleanupOptions {
    pub apply: bool,
    /// Narrowing filter: only cache directories under this runner id.
    pub runner: Option<String>,
    /// Narrowing filter: only this run cache. Requires `runner`.
    pub run_id: Option<String>,
    /// Maximum candidate entries inspected in one invocation. Resolved by
    /// callers from [`crate::cleanup::CleanupPolicy::scan_limit`], which fails
    /// closed to zero rather than widening to `usize::MAX`.
    pub limit: usize,
    /// Whether the caller has already established that the observation store
    /// can be opened.
    ///
    /// `false` skips the liveness open entirely and goes straight to the
    /// fail-closed verdict this sweep would have reached anyway. That changes
    /// no outcome — an unopenable store already vetoes every candidate — but it
    /// avoids one more `create_dir_all` plus journal-file creation against a
    /// filesystem that has no inode left to give, which is the exact condition
    /// a degraded sweep is running under (#11127, #10603).
    ///
    /// Defaults to `true`: a caller that has not probed must not be silently
    /// downgraded into retaining everything.
    pub store_available: bool,
}

impl Default for RunnerDownloadCleanupOptions {
    fn default() -> Self {
        Self {
            apply: false,
            runner: None,
            run_id: None,
            limit: DEFAULT_INSPECTION_LIMIT,
            store_available: true,
        }
    }
}

/// Whether the liveness veto could be evaluated at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerDownloadLiveness {
    /// The observation store answered a bounded running-run query. Candidates
    /// matching a non-terminal run are retained; the rest are decided by the
    /// age floor.
    ObservationStore,
    /// The store could not be opened, the query failed, or the running-run scan
    /// hit its bound. Every candidate is retained.
    Unavailable,
    /// No cache root exists, so there was nothing to decide and the store was
    /// never opened. Reported instead of `Unavailable` so an empty plan does
    /// not look like a degraded one.
    NotConsulted,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerDownloadCleanupOutcome {
    pub dry_run: bool,
    /// The scope this invocation inspected: the cache root, narrowed by any
    /// `--runner` / `--run-id` filter.
    pub root: PathBuf,
    pub min_age_seconds: u64,
    pub liveness: RunnerDownloadLiveness,
    pub inspected_count: usize,
    pub planned_count: usize,
    pub removed_count: usize,
    pub skipped_count: usize,
    /// Files inside planned (dry run) or removed (apply) cache directories.
    pub file_count: usize,
    /// Sub-directories inside planned or removed cache directories, excluding
    /// the cache directories themselves.
    pub directory_count: usize,
    pub planned_size_bytes: u64,
    pub removed_size_bytes: u64,
    /// True when the sweep stopped at `limit` with candidates still unread.
    pub truncated: bool,
    pub rows: Vec<RunnerDownloadCleanupRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerDownloadCleanupRow {
    /// Path relative to `<artifact-root>/runner`.
    pub path: String,
    pub runner_id: String,
    /// Empty for entries that are not the canonical `<runner>/<run>` shape.
    pub run_id: String,
    /// What the intent marker says: `operator_pull`, `internal_fetch`,
    /// `unrecorded` (no marker), or `unreadable`. Only `internal_fetch` is
    /// reclaimable. Empty for entries that were never read for one.
    pub intent: String,
    pub file_count: usize,
    pub directory_count: usize,
    /// Age of the *newest* byte in the subtree. Zero when it is unknown, in
    /// which case the row is always a skip.
    pub age_seconds: u64,
    pub size_bytes: u64,
    /// Size is reported for operators, never used to decide removal. `false`
    /// means the measurement failed and `size_bytes` is a floor, not a fact.
    pub size_measured: bool,
    pub action: String,
    pub reason: String,
}

/// Plan (and optionally apply) runner artifact download cache cleanup.
///
/// # Errors
///
/// Returns a validation error when `run_id` is given without `runner`, when a
/// filter is not a single normal path component, or when the cache root exists
/// but is not a real directory. A single unreadable candidate is reported as a
/// retained row rather than failing the sweep.
pub fn cleanup_runner_downloads(
    options: RunnerDownloadCleanupOptions,
) -> Result<RunnerDownloadCleanupOutcome> {
    // Filter validation runs before any filesystem or store access so a
    // traversal attempt is rejected identically whether or not a cache exists.
    let filters = CleanupFilters::resolve(&options)?;
    let artifact_root = crate::artifacts::root()?;
    sweep(
        &artifact_root,
        &options,
        &filters,
        RUNNER_DOWNLOAD_MIN_AGE,
        SystemTime::now(),
    )
}

/// Validated narrowing filters. Both are single normal path components, so they
/// can only ever select *within* the cache root.
#[derive(Debug)]
struct CleanupFilters {
    runner: Option<String>,
    run_id: Option<String>,
}

impl CleanupFilters {
    fn resolve(options: &RunnerDownloadCleanupOptions) -> Result<Self> {
        if options.run_id.is_some() && options.runner.is_none() {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "--run-id requires --runner so cleanup stays inside one runner cache",
                options.run_id.clone(),
                None,
            ));
        }
        Ok(Self {
            runner: cleanup_path_component("runner", options.runner.as_deref())?,
            run_id: cleanup_path_component("run_id", options.run_id.as_deref())?,
        })
    }

    fn selects_runner(&self, name: &str) -> bool {
        self.runner.as_deref().is_none_or(|runner| runner == name)
    }

    fn selects_run(&self, name: &str) -> bool {
        self.run_id.as_deref().is_none_or(|run_id| run_id == name)
    }

    /// The scope reported to the operator: the cache root narrowed by whatever
    /// they typed.
    fn scope(&self, root: &Path) -> PathBuf {
        let mut scope = root.to_path_buf();
        if let Some(runner) = &self.runner {
            scope = scope.join(runner);
        }
        if let Some(run_id) = &self.run_id {
            scope = scope.join(run_id);
        }
        scope
    }
}

/// The sweep with its age floor and clock supplied explicitly. Both are fixed
/// on every production path; tests move them so the age comparison, the
/// fail-closed branches, and the removal branch are all reachable without
/// rewriting filesystem timestamps.
fn sweep(
    artifact_root: &Path,
    options: &RunnerDownloadCleanupOptions,
    filters: &CleanupFilters,
    min_age: Duration,
    now: SystemTime,
) -> Result<RunnerDownloadCleanupOutcome> {
    // A caller that already knows the store will not open gets the same
    // fail-closed veto without paying for another failing open.
    let store_available = options.store_available;
    sweep_with(artifact_root, options, filters, min_age, now, move || {
        if store_available {
            LivenessVeto::read()
        } else {
            LivenessVeto::unavailable()
        }
    })
}

/// The sweep with its liveness source injected.
///
/// The source is a closure, not a value, so an absent cache root costs no
/// observation-store open on the very common "nothing to clean" path. Tests
/// supply a fixed veto to reach the fail-closed and non-terminal branches
/// without staging a live run.
fn sweep_with<F>(
    artifact_root: &Path,
    options: &RunnerDownloadCleanupOptions,
    filters: &CleanupFilters,
    min_age: Duration,
    now: SystemTime,
    read_liveness: F,
) -> Result<RunnerDownloadCleanupOutcome>
where
    F: FnOnce() -> LivenessVeto,
{
    let root = artifact_root.join(RUNNER_DOWNLOAD_DIR);
    let mut outcome = RunnerDownloadCleanupOutcome {
        dry_run: !options.apply,
        root: filters.scope(&root),
        min_age_seconds: min_age.as_secs(),
        liveness: RunnerDownloadLiveness::NotConsulted,
        inspected_count: 0,
        planned_count: 0,
        removed_count: 0,
        skipped_count: 0,
        file_count: 0,
        directory_count: 0,
        planned_size_bytes: 0,
        removed_size_bytes: 0,
        truncated: false,
        rows: Vec::new(),
    };

    let Some(root_metadata) = symlink_metadata_if_exists(&root)? else {
        // Nothing cached. Reported as an empty plan with no store access, so
        // the common case costs nothing.
        outcome.liveness = RunnerDownloadLiveness::NotConsulted;
        return Ok(outcome);
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(Error::validation_invalid_argument(
            "artifact_root",
            format!(
                "runner artifact cache root must be a real directory: {}",
                root.display()
            ),
            Some(root.display().to_string()),
            None,
        ));
    }

    let liveness = read_liveness();
    outcome.liveness = liveness.source();

    for runner_name in sorted_child_names(&root)? {
        if !filters.selects_runner(&runner_name) {
            continue;
        }
        let runner_path = root.join(&runner_name);
        let Some(runner_metadata) = symlink_metadata_if_exists(&runner_path)? else {
            continue;
        };
        if !runner_metadata.is_dir() || runner_metadata.file_type().is_symlink() {
            // Not the shape the writer emits. Reported so the bytes stay
            // visible, and never removed by this category.
            if !record_unowned(&mut outcome, options.limit, &runner_name, &runner_name) {
                return Ok(outcome);
            }
            continue;
        }

        // An unreadable runner directory is skipped, not fatal: one bad
        // directory must not fail the whole cleanup inventory, and skipping can
        // only under-reap.
        let Ok(run_names) = sorted_child_names(&runner_path) else {
            if !record_unowned(&mut outcome, options.limit, &runner_name, &runner_name) {
                return Ok(outcome);
            }
            continue;
        };
        for run_name in run_names {
            if !filters.selects_run(&run_name) {
                continue;
            }
            let run_path = runner_path.join(&run_name);
            let relative = format!("{runner_name}/{run_name}");
            let Some(run_metadata) = symlink_metadata_if_exists(&run_path)? else {
                continue;
            };
            if !run_metadata.is_dir() || run_metadata.file_type().is_symlink() {
                if !record_unowned(&mut outcome, options.limit, &relative, &runner_name) {
                    return Ok(outcome);
                }
                continue;
            }
            if outcome.inspected_count >= options.limit {
                outcome.truncated = true;
                return Ok(outcome);
            }
            outcome.inspected_count += 1;
            sweep_candidate(
                &mut outcome,
                &liveness,
                artifact_root,
                Candidate {
                    path: &run_path,
                    metadata: &run_metadata,
                    relative: &relative,
                    runner_id: &runner_name,
                    run_id: &run_name,
                },
                min_age,
                now,
            );
        }
    }

    Ok(outcome)
}

/// Record an entry under the cache root that is not the canonical
/// `<runner>/<run>` shape. Returns `false` when the inspection budget is
/// exhausted, in which case the caller must stop.
fn record_unowned(
    outcome: &mut RunnerDownloadCleanupOutcome,
    limit: usize,
    relative: &str,
    runner_id: &str,
) -> bool {
    if outcome.inspected_count >= limit {
        outcome.truncated = true;
        return false;
    }
    outcome.inspected_count += 1;
    outcome.skip(
        relative,
        runner_id,
        "",
        "",
        0,
        "not the canonical <runner-id>/<run-id> cache directory this category owns",
    );
    true
}

/// One canonical `<runner-id>/<run-id>` cache directory under inspection.
struct Candidate<'a> {
    path: &'a Path,
    metadata: &'a fs::Metadata,
    /// `<runner-id>/<run-id>`, relative to the cache root.
    relative: &'a str,
    runner_id: &'a str,
    run_id: &'a str,
}

fn sweep_candidate(
    outcome: &mut RunnerDownloadCleanupOutcome,
    liveness: &LivenessVeto,
    artifact_root: &Path,
    candidate: Candidate<'_>,
    min_age: Duration,
    now: SystemTime,
) {
    let Candidate {
        path,
        metadata,
        relative,
        runner_id,
        run_id,
    } = candidate;
    if !path_is_within_root(path, artifact_root) {
        outcome.skip(
            relative,
            runner_id,
            run_id,
            "",
            0,
            "path does not resolve inside the artifact root",
        );
        return;
    }

    // Read before the age scan so every row can report it, including the rows
    // the age floor decides.
    let ownership = read_download_ownership(path);

    let scan = scan_subtree_from(path, metadata);

    // The age of the *newest* byte anywhere in the cache directory. `None`
    // means some part of the subtree could not be read or is future-dated, so
    // the candidate is retained.
    let Some(age) = scan.newest_age(now) else {
        outcome.skip(
            relative,
            runner_id,
            run_id,
            ownership.as_str(),
            0,
            "modification time is unreadable, in the future, or the subtree could not be fully walked",
        );
        return;
    };
    if age < min_age {
        // The data-loss case this predicate exists for: an artifact pulled
        // moments ago is younger than the floor, so it is never a candidate.
        outcome.skip(
            relative,
            runner_id,
            run_id,
            ownership.as_str(),
            age.as_secs(),
            "newer than the runner download age floor",
        );
        return;
    }
    // Age proves the bytes are old. Only the writer's tag can prove they are
    // homeboy's rather than the operator's (#10585), and an absent tag is
    // deliberately read as operator-owned.
    if let Some(reason) = ownership.retain_reason() {
        outcome.skip(
            relative,
            runner_id,
            run_id,
            ownership.as_str(),
            age.as_secs(),
            reason,
        );
        return;
    }
    if liveness.vetoes(run_id) {
        outcome.skip(
            relative,
            runner_id,
            run_id,
            ownership.as_str(),
            age.as_secs(),
            liveness.veto_reason(),
        );
        return;
    }

    let mut row = RunnerDownloadCleanupRow {
        path: relative.to_string(),
        runner_id: runner_id.to_string(),
        run_id: run_id.to_string(),
        intent: ownership.as_str().to_string(),
        file_count: scan.file_count,
        directory_count: scan.directory_count,
        age_seconds: age.as_secs(),
        size_bytes: scan.size_bytes,
        size_measured: scan.size_measured,
        action: "remove".to_string(),
        reason:
            "internal fetch past the age floor with no non-terminal owning run; not operator-owned"
                .to_string(),
    };
    outcome.planned_count += 1;
    outcome.planned_size_bytes += scan.size_bytes;
    outcome.file_count += scan.file_count;
    outcome.directory_count += scan.directory_count;

    if outcome.dry_run {
        outcome.rows.push(row);
        return;
    }

    match fs::remove_dir_all(path) {
        Ok(()) => {
            outcome.removed_count += 1;
            outcome.removed_size_bytes += scan.size_bytes;
            row.action = "removed".to_string();
            outcome.rows.push(row);
            prune_empty_runner_directory(path);
        }
        Err(error) => {
            // One unremovable cache must not abort the sweep or fail the
            // command; it is reported so the bytes stay visible.
            outcome.planned_count -= 1;
            outcome.planned_size_bytes -= scan.size_bytes;
            outcome.file_count -= scan.file_count;
            outcome.directory_count -= scan.directory_count;
            outcome.skipped_count += 1;
            row.action = "skip".to_string();
            row.reason = format!("removal failed: {error}");
            outcome.rows.push(row);
        }
    }
}

/// Drop the `<runner-id>` directory once its last run cache is gone.
///
/// Non-recursive on purpose: `remove_dir` succeeds only on an empty directory,
/// so this can never remove a sibling cache that survived the predicate. A
/// failure means "not empty" and is the expected outcome most of the time.
fn prune_empty_runner_directory(run_path: &Path) {
    if let Some(runner_path) = run_path.parent() {
        let _ = fs::remove_dir(runner_path);
    }
}

impl RunnerDownloadCleanupOutcome {
    /// Record a retained candidate. Size is not measured for skipped entries:
    /// it would report a number that no decision consumed.
    fn skip(
        &mut self,
        path: &str,
        runner_id: &str,
        run_id: &str,
        intent: &str,
        age_seconds: u64,
        reason: &str,
    ) {
        self.skipped_count += 1;
        self.rows.push(RunnerDownloadCleanupRow {
            path: path.to_string(),
            runner_id: runner_id.to_string(),
            run_id: run_id.to_string(),
            intent: intent.to_string(),
            file_count: 0,
            directory_count: 0,
            age_seconds,
            size_bytes: 0,
            size_measured: false,
            action: "skip".to_string(),
            reason: reason.to_string(),
        });
    }
}

/// Non-terminal runs read once, used only to *retain* candidates.
///
/// The database is never consulted in the release direction. A cache directory
/// with no matching run row is not thereby proven dead — runner-side run ids
/// frequently have no local row at all — so absence changes nothing and the age
/// floor remains the only authorization to delete.
struct LivenessVeto {
    /// `None` means the store could not be consulted, which vetoes everything.
    running: Option<Vec<RunRecord>>,
}

impl LivenessVeto {
    /// The veto a caller reaches when liveness cannot be established: retain
    /// everything. Identical to what [`LivenessVeto::read`] returns on a failed
    /// open, reachable without attempting one.
    fn unavailable() -> Self {
        Self { running: None }
    }

    fn read() -> Self {
        let Ok(store) = ObservationStore::open_initialized() else {
            return Self { running: None };
        };
        // One row over the bound is read so truncation is detectable rather
        // than silently dropping vetoes.
        let probe = i64::try_from(RUNNING_RUN_SCAN_LIMIT.saturating_add(1)).unwrap_or(i64::MAX);
        let Ok(running) = store.list_runs(RunListFilter {
            status: Some(RunStatus::Running.as_str().to_string()),
            limit: Some(probe),
            ..RunListFilter::default()
        }) else {
            return Self { running: None };
        };
        if running.len() > RUNNING_RUN_SCAN_LIMIT {
            // An unseen running run could own any candidate, so degrade to
            // "retain everything" rather than to "veto nothing".
            return Self { running: None };
        }
        Self {
            running: Some(running),
        }
    }

    fn source(&self) -> RunnerDownloadLiveness {
        if self.running.is_some() {
            RunnerDownloadLiveness::ObservationStore
        } else {
            RunnerDownloadLiveness::Unavailable
        }
    }

    /// `true` when the candidate must be retained: either a non-terminal run
    /// claims it, or liveness could not be evaluated at all.
    fn vetoes(&self, run_id: &str) -> bool {
        let Some(running) = &self.running else {
            return true;
        };
        running
            .iter()
            .any(|run| run.id == run_id || run_matches_label(run, run_id))
    }

    fn veto_reason(&self) -> &'static str {
        if self.running.is_some() {
            "a non-terminal run still claims this cache directory"
        } else {
            "observation store liveness is unavailable; retained (fail closed)"
        }
    }
}

/// Measurements taken over one cache directory's subtree.
#[derive(Debug)]
struct SubtreeScan {
    file_count: usize,
    directory_count: usize,
    size_bytes: u64,
    /// False when any entry's length could not be read. Advisory only: it
    /// never moves the verdict in either direction.
    size_measured: bool,
    newest_mtime: Option<SystemTime>,
    /// False when any entry's mtime was unreadable or the tree could not be
    /// fully walked. Forces retention.
    mtime_complete: bool,
}

impl SubtreeScan {
    fn observe(&mut self, metadata: &fs::Metadata) {
        match metadata.modified() {
            Ok(modified) => match self.newest_mtime {
                Some(newest) if newest >= modified => {}
                _ => self.newest_mtime = Some(modified),
            },
            Err(_) => self.mtime_complete = false,
        }
    }

    fn unreadable(&mut self) {
        self.mtime_complete = false;
        self.size_measured = false;
    }

    /// Age of the newest byte in the subtree, or `None` when it cannot be
    /// proven. `duration_since` errors on a future mtime, which is exactly the
    /// clock-skew case that must not be rounded down to "old enough".
    fn newest_age(&self, now: SystemTime) -> Option<Duration> {
        if !self.mtime_complete {
            return None;
        }
        now.duration_since(self.newest_mtime?).ok()
    }
}

impl Default for SubtreeScan {
    fn default() -> Self {
        Self {
            file_count: 0,
            directory_count: 0,
            size_bytes: 0,
            size_measured: true,
            newest_mtime: None,
            mtime_complete: true,
        }
    }
}

fn scan_subtree_from(path: &Path, metadata: &fs::Metadata) -> SubtreeScan {
    let mut scan = SubtreeScan::default();
    scan_subtree(path, metadata, MAX_SCAN_DEPTH, &mut scan, true);
    // `scan_subtree` counted the cache directory itself; report its contents.
    scan.directory_count = scan.directory_count.saturating_sub(1);
    scan
}

/// `skip_marker` excludes homeboy's own intent sidecar from the measurement,
/// and only at the cache directory's own level where the writer puts it. It is
/// bookkeeping, not artifact bytes: counting it would inflate every reported
/// file count and size by one file that the operator did not download, and
/// letting its mtime into the age would let a bookkeeping rewrite re-arm the
/// floor on its own. It is still removed with the directory.
fn scan_subtree(
    path: &Path,
    metadata: &fs::Metadata,
    depth: usize,
    scan: &mut SubtreeScan,
    skip_marker: bool,
) {
    scan.observe(metadata);
    if metadata.file_type().is_symlink() {
        // Never followed. A symlink contributes its own mtime and nothing else,
        // so a link into a live tree can neither be measured nor descended.
        scan.file_count += 1;
        return;
    }
    if !metadata.is_dir() {
        scan.file_count += 1;
        scan.size_bytes = scan.size_bytes.saturating_add(metadata.len());
        return;
    }
    scan.directory_count += 1;
    if depth == 0 {
        // Deeper than this sweep walks: the subtree cannot be proven old.
        scan.unreadable();
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        scan.unreadable();
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            scan.unreadable();
            continue;
        };
        if skip_marker
            && entry.file_name().as_os_str() == std::ffi::OsStr::new(RUNNER_DOWNLOAD_MARKER_FILE)
        {
            continue;
        }
        let child = entry.path();
        let Ok(child_metadata) = fs::symlink_metadata(&child) else {
            scan.unreadable();
            continue;
        };
        scan_subtree(&child, &child_metadata, depth - 1, scan, false);
    }
}

fn cleanup_path_component(name: &str, value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::validation_invalid_argument(
            name,
            format!("{name} must be a single path component"),
            Some(value.to_string()),
            None,
        ));
    }
    Ok(Some(value.to_string()))
}

/// Sorted child names, so a bounded sweep is deterministic across runs. A
/// missing directory is an empty listing, not an error.
fn sorted_child_names(path: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(format!(
                    "read runner artifact cache directory {}",
                    path.display()
                )),
            ))
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "read runner artifact cache directory {}",
                    path.display()
                )),
            )
        })?;
        names.push(entry.file_name().to_string_lossy().to_string());
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests;
