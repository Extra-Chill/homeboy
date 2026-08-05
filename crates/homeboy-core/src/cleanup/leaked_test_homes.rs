//! Reclaim isolated test homes abandoned by killed test processes.
//!
//! # The blind spot this closes
//!
//! `test_support::with_isolated_home` builds its home from a `TempDir`.
//! `TempDir` reclaims on `Drop`, and `Drop` provably cannot run when a process
//! is *killed* rather than unwound — OOM kill, harness timeout, `SIGKILL` from
//! a supervisor. On a RAM-constrained host that is routine, not exotic.
//!
//! What leaks is expensive: a home that materialized a controller runtime keeps
//! a private copy of the debug binary, so one abandoned entry is hundreds of
//! megabytes. Observed on one host: 1232 entries, 9.2 GB, while
//! `homeboy cleanup` truthfully reported **zero** reclaimable runtime-temp
//! bytes (#11073).
//!
//! It reported zero because it was looking somewhere else. `engine::temp` scans
//! `HOMEBOY_RUNTIME_TMPDIR` (or `~/.local/share/homeboy/runtime/tmp`);
//! `tempfile` resolves `TMPDIR`. When an operator points `TMPDIR` at a
//! dedicated volume — as that host does, via a systemd drop-in — those are
//! different directories and the leak is in the one nobody scanned. A category
//! that reports `0` because it looked in the wrong place is worse than one that
//! reports "not inspected", so this module reports **every root it considered**,
//! including the ones it could not read and why. A zero is always attributable
//! to a directory rather than to an assumption.
//!
//! # Why this is a separate category, not a wider `runtime-tmp`
//!
//! `runtime-tmp` owns Homeboy's own run scratch, and its entries carry pin
//! files, invocation leases, and metadata that prove ownership. These entries
//! have none of that; their only ownership evidence is the name. Folding a
//! name-shape predicate into a metadata-backed category would weaken the
//! stronger one, so the two stay separate and each keeps its own proof.
//!
//! # The safety contract
//!
//! A live test's home must never be deleted out from under it. Three
//! independent guards, in order:
//!
//! 1. **Name shape.** Only directories whose name starts with
//!    [`TEST_TEMPDIR_PREFIX`], directly under a scanned root, are ever
//!    considered. The scan never recurses into subdirectories looking for
//!    candidates, never follows a symlink, and never touches a non-directory.
//! 2. **Owner liveness.** The creating process stamps its PID into the name
//!    (see [`owned_test_tempdir_prefix`]). An entry whose owner is this process
//!    or any running process is classified [`LeakedTestHomeVerdict::OwnerAlive`]
//!    and is **unreachable by every reclaim path here** — no age, no byte
//!    budget, and no operator flag promotes it. PID reuse fails safe: a recycled
//!    PID makes an abandoned entry look alive, so it simply survives to a later
//!    pass.
//! 3. **Age floor.** Liveness alone would suffice for a process that has already
//!    exited, but a process can create its home microseconds before it becomes
//!    observable, and a failed metadata read must not be allowed to widen a
//!    delete predicate. An entry must be both abandoned *and* stale.
//!
//! # Why a byte budget as well as an age floor
//!
//! An age floor alone cannot keep up. These entries arrive at hundreds of
//! megabytes each at an unbounded rate, so a multi-day window is a promise to
//! fill the disk before the window closes (#11073). The budget promotes the
//! *oldest abandoned* entries — and only entries already proven abandoned by
//! guard 2 — until the retained footprint fits. It relaxes the age floor. It
//! never relaxes liveness, and it never touches an entry whose owner cannot be
//! determined, because "no owner recorded" is not proof of death.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::Result;

/// Filename prefix stamped on every temp directory a Homeboy test process owns.
///
/// This is the marker the whole module rests on, which is why it lives in
/// production code rather than in `test_support`: the process that *created*
/// these directories is gone by the time anything wants to reclaim them, so the
/// naming contract has to be readable by a binary with no test code compiled
/// into it at all.
pub const TEST_TEMPDIR_PREFIX: &str = "hb-test-";

/// Environment variable `tempfile` resolves its root from.
const TEMPDIR_ENV: &str = "TMPDIR";

/// Fixed roots probed in addition to `$TMPDIR`.
///
/// These mirror the candidate list the test-side tempdir allocator walks, so the
/// reaper cannot look in a different set of places than the creator wrote to.
/// That divergence is the entire bug this module exists for.
const FIXED_TEMP_ROOTS: &[&str] = &["/tmp", "/var/tmp", "/dev/shm"];

/// Entries described individually in one report.
///
/// Counts and byte totals always cover every inspected entry; only the
/// per-entry detail list is bounded, so a 1232-entry leak stays summarizable
/// without emitting 1232 records.
const MAX_REPORTED_ENTRIES: usize = 50;

/// Tempdir prefix that records the creating process, e.g. `hb-test-4127-`.
///
/// Ownership is a fact, not something to infer from a clock. Stamping the PID
/// into the name lets a reaper ask "is the process that made this still alive?"
/// instead of "has enough time passed that it probably is not?".
#[must_use]
pub fn owned_test_tempdir_prefix() -> String {
    format!("{TEST_TEMPDIR_PREFIX}{}-", std::process::id())
}

/// Recover the owning PID from a name produced by [`owned_test_tempdir_prefix`].
///
/// Returns `None` for any name that does not carry one — notably directories
/// written by a binary that predates the PID prefix, and any name whose second
/// segment is not a number. `None` means *unknown*, never *unowned*, and every
/// caller here treats it that way.
#[must_use]
pub fn test_tempdir_owner_pid(name: &str) -> Option<u32> {
    name.strip_prefix(TEST_TEMPDIR_PREFIX)?
        .split_once('-')
        .and_then(|(pid, _)| pid.parse::<u32>().ok())
}

/// Every directory this process would consider a temp root.
///
/// `$TMPDIR` first — that is what `tempfile` honors, and honoring it is the
/// difference between scanning the leak and scanning an empty directory — then
/// the conventional fixed roots. The Homeboy runtime temp root is deliberately
/// absent: it is `runtime-tmp`'s scope, and its entries prove ownership through
/// pin files rather than through a name.
#[must_use]
pub fn effective_temp_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(value) = std::env::var_os(TEMPDIR_ENV) {
        push_unique_root(&mut roots, PathBuf::from(value));
    }
    for fixed in FIXED_TEMP_ROOTS {
        push_unique_root(&mut roots, PathBuf::from(fixed));
    }
    roots
}

fn push_unique_root(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if path.as_os_str().is_empty() || roots.contains(&path) {
        return;
    }
    roots.push(path);
}

/// Why one entry was or was not reclaimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeakedTestHomeVerdict {
    /// The creating process is still running. Never reapable — this verdict is
    /// what makes deleting a live test's home unreachable.
    OwnerAlive,
    /// The creating process is gone and the entry is past the age floor.
    AbandonedAndAged,
    /// The creating process is gone and the entry is younger than the age
    /// floor, but the retained footprint exceeds its byte budget and this is
    /// among the oldest abandoned entries.
    AbandonedOverBudget,
    /// The creating process is gone but the entry is still inside the age floor
    /// and the budget did not need it.
    RetainedTooYoung,
    /// No owning PID is recorded, so death cannot be proven, and the entry is
    /// still inside the age floor. Never promoted by the byte budget.
    RetainedUnknownOwner,
}

impl LeakedTestHomeVerdict {
    /// The single place that decides whether a verdict permits deletion.
    #[must_use]
    pub fn is_reapable(self) -> bool {
        matches!(self, Self::AbandonedAndAged | Self::AbandonedOverBudget)
    }
}

/// One inspected temp root, reported whether or not it yielded anything.
#[derive(Debug, Clone, Serialize)]
pub struct LeakedTestHomeRoot {
    pub path: PathBuf,
    /// False when the root could not be read or was never reached. A root that
    /// was not inspected is reported as such rather than contributing a silent
    /// zero.
    pub inspected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub matched_count: usize,
    pub matched_size_bytes: u64,
}

/// One isolated test home found under a temp root.
#[derive(Debug, Clone, Serialize)]
pub struct LeakedTestHome {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub age_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    pub owner_alive: bool,
    pub verdict: LeakedTestHomeVerdict,
    pub reapable: bool,
    pub removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removal_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LeakedTestHomeCleanupOptions {
    pub apply: bool,
    /// An entry must be at least this stale before the age path reclaims it.
    pub min_age: Duration,
    /// Ceiling on retained leaked bytes. Exceeding it promotes the oldest
    /// *abandoned* entries past the age floor, never a live or unknown owner.
    pub max_total_bytes: u64,
    /// Maximum entries inspected across all roots in one pass.
    pub limit: usize,
    /// Roots to scan. Empty means [`effective_temp_roots`].
    pub roots: Vec<PathBuf>,
}

impl Default for LeakedTestHomeCleanupOptions {
    fn default() -> Self {
        Self {
            apply: false,
            min_age: Duration::from_secs(3_600),
            max_total_bytes: u64::MAX,
            limit: 1_000,
            roots: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LeakedTestHomeCleanupOutput {
    pub command: &'static str,
    pub dry_run: bool,
    pub min_age_seconds: u64,
    pub max_total_bytes: u64,
    /// Every root considered, inspected or not.
    pub roots: Vec<LeakedTestHomeRoot>,
    pub inspected_count: usize,
    pub planned_count: usize,
    pub removed_count: usize,
    pub skipped_count: usize,
    pub planned_size_bytes: u64,
    pub removed_size_bytes: u64,
    pub retained_size_bytes: u64,
    pub total_size_bytes: u64,
    /// True when the inspection limit stopped the scan before every entry was
    /// walked, so the totals are a floor rather than the whole leak.
    pub truncated: bool,
    /// True when per-entry detail was capped. Counts and totals are unaffected.
    pub entries_truncated: bool,
    pub entries: Vec<LeakedTestHome>,
}

/// Plan, and optionally apply, reclamation of abandoned isolated test homes.
///
/// Never consults the observation store: ownership here is proven by name shape
/// and process liveness, both of which are filesystem-local. That is what makes
/// this category usable in a degraded sweep, which is exactly the state a
/// disk-pressure incident leaves a host in.
///
/// # Errors
///
/// Currently infallible — an unreadable root is reported on that root rather
/// than failing the pass, because one bad directory must not hide the bytes in
/// the others. The `Result` is kept so a future proof step can fail loudly
/// without changing every call site.
pub fn cleanup_leaked_test_homes(
    options: LeakedTestHomeCleanupOptions,
) -> Result<LeakedTestHomeCleanupOutput> {
    let now = SystemTime::now();
    let roots = resolve_roots(&options.roots);

    let mut reported_roots: Vec<LeakedTestHomeRoot> = Vec::new();
    let mut entries: Vec<LeakedTestHome> = Vec::new();
    let mut truncated = false;

    for root in roots {
        let mut report = LeakedTestHomeRoot {
            path: root.clone(),
            inspected: false,
            skip_reason: None,
            matched_count: 0,
            matched_size_bytes: 0,
        };
        if truncated {
            report.skip_reason =
                Some("inspection limit reached before this root was scanned".to_string());
            reported_roots.push(report);
            continue;
        }
        let read = match fs::read_dir(&root) {
            Ok(read) => read,
            Err(error) => {
                report.skip_reason = Some(error.to_string());
                reported_roots.push(report);
                continue;
            }
        };
        report.inspected = true;
        for entry in read.flatten() {
            if entries.len() >= options.limit {
                truncated = true;
                break;
            }
            let Some(observed) = observe_entry(&entry.path(), now) else {
                continue;
            };
            report.matched_count += 1;
            report.matched_size_bytes = report
                .matched_size_bytes
                .saturating_add(observed.size_bytes);
            entries.push(observed);
        }
        reported_roots.push(report);
    }

    classify(&mut entries, options.min_age, options.max_total_bytes);

    let mut planned_count = 0usize;
    let mut planned_size_bytes = 0u64;
    let mut removed_count = 0usize;
    let mut removed_size_bytes = 0u64;
    let mut retained_size_bytes = 0u64;
    let mut total_size_bytes = 0u64;

    for entry in &mut entries {
        total_size_bytes = total_size_bytes.saturating_add(entry.size_bytes);
        if !entry.reapable {
            retained_size_bytes = retained_size_bytes.saturating_add(entry.size_bytes);
            continue;
        }
        planned_count += 1;
        planned_size_bytes = planned_size_bytes.saturating_add(entry.size_bytes);
        if !options.apply {
            continue;
        }
        match fs::remove_dir_all(&entry.path) {
            Ok(()) => {
                entry.removed = true;
                removed_count += 1;
                removed_size_bytes = removed_size_bytes.saturating_add(entry.size_bytes);
            }
            Err(error) => {
                entry.removal_error = Some(error.to_string());
                retained_size_bytes = retained_size_bytes.saturating_add(entry.size_bytes);
            }
        }
    }

    let inspected_count = entries.len();
    let skipped_count = inspected_count.saturating_sub(planned_count);

    // Largest first: an operator triaging disk pressure wants the 666 MB entry
    // named, not whichever one the directory iterator happened to yield first.
    entries.sort_by(|left, right| {
        right
            .size_bytes
            .cmp(&left.size_bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    let entries_truncated = entries.len() > MAX_REPORTED_ENTRIES;
    entries.truncate(MAX_REPORTED_ENTRIES);

    Ok(LeakedTestHomeCleanupOutput {
        command: "cleanup.leaked_test_homes",
        dry_run: !options.apply,
        min_age_seconds: options.min_age.as_secs(),
        max_total_bytes: options.max_total_bytes,
        roots: reported_roots,
        inspected_count,
        planned_count,
        removed_count,
        skipped_count,
        planned_size_bytes,
        removed_size_bytes,
        retained_size_bytes,
        total_size_bytes,
        truncated,
        entries_truncated,
        entries,
    })
}

fn resolve_roots(requested: &[PathBuf]) -> Vec<PathBuf> {
    if requested.is_empty() {
        return effective_temp_roots();
    }
    let mut roots = Vec::new();
    for root in requested {
        push_unique_root(&mut roots, root.clone());
    }
    roots
}

/// Classify one directory entry, or reject it as not ours.
///
/// Rejection is the common case and is deliberately strict: a symlink, a file,
/// or any name outside the prefix is not a Homeboy test home and this module has
/// nothing to say about it.
fn observe_entry(path: &Path, now: SystemTime) -> Option<LeakedTestHome> {
    let name = path.file_name()?.to_str()?;
    if !name.starts_with(TEST_TEMPDIR_PREFIX) {
        return None;
    }
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    // A metadata read that fails resolves to age zero, which reads as "too young
    // to touch". Failing the other way would let an unreadable timestamp widen a
    // delete predicate.
    let age = metadata
        .modified()
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .unwrap_or(Duration::ZERO);
    let owner_pid = test_tempdir_owner_pid(name);
    let owner_alive = owner_pid.is_some_and(owner_is_alive);
    Some(LeakedTestHome {
        path: path.to_path_buf(),
        size_bytes: entry_size_bytes(path),
        age_seconds: age.as_secs(),
        owner_pid,
        owner_alive,
        // Placeholder until `classify` runs. Chosen as the most conservative
        // verdict so a classification that never ran cannot delete anything.
        verdict: LeakedTestHomeVerdict::OwnerAlive,
        reapable: false,
        removed: false,
        removal_error: None,
    })
}

/// This process counts as alive without asking the OS: it is mid-run and still
/// owns anything it created.
fn owner_is_alive(pid: u32) -> bool {
    pid == std::process::id() || crate::process::pid_is_running(pid)
}

/// Recursive on-disk size, never following symlinks.
fn entry_size_bytes(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return metadata.len();
    }
    let Ok(read) = fs::read_dir(path) else {
        return metadata.len();
    };
    read.flatten().fold(metadata.len(), |total, entry| {
        total.saturating_add(entry_size_bytes(&entry.path()))
    })
}

/// Assign every entry a verdict, then relax the age floor for as few abandoned
/// entries as the byte budget requires.
fn classify(entries: &mut [LeakedTestHome], min_age: Duration, max_total_bytes: u64) {
    for entry in entries.iter_mut() {
        entry.verdict = age_verdict(entry, min_age);
        entry.reapable = entry.verdict.is_reapable();
    }
    promote_over_budget(entries, max_total_bytes);
}

/// The verdict an entry gets from liveness and age alone, before any budget.
fn age_verdict(entry: &LeakedTestHome, min_age: Duration) -> LeakedTestHomeVerdict {
    if entry.owner_alive {
        return LeakedTestHomeVerdict::OwnerAlive;
    }
    if Duration::from_secs(entry.age_seconds) >= min_age {
        return LeakedTestHomeVerdict::AbandonedAndAged;
    }
    match entry.owner_pid {
        Some(_) => LeakedTestHomeVerdict::RetainedTooYoung,
        None => LeakedTestHomeVerdict::RetainedUnknownOwner,
    }
}

/// Relax the age floor, and only the age floor, until the retained footprint
/// fits its budget.
///
/// Only [`LeakedTestHomeVerdict::RetainedTooYoung`] is promotable, which is the
/// whole safety argument in one line: that verdict is reachable only for an
/// entry whose owning PID was recorded *and* read as not running. A live owner
/// and an unrecorded owner are both structurally out of reach.
fn promote_over_budget(entries: &mut [LeakedTestHome], max_total_bytes: u64) {
    let retained = entries
        .iter()
        .filter(|entry| !entry.reapable)
        .fold(0u64, |total, entry| total.saturating_add(entry.size_bytes));
    if retained <= max_total_bytes {
        return;
    }
    let mut excess = retained - max_total_bytes;

    let mut promotable: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.verdict == LeakedTestHomeVerdict::RetainedTooYoung)
        .map(|(index, _)| index)
        .collect();
    // Oldest first, then largest: reclaim what has been dead longest before
    // reclaiming what merely happens to be big.
    promotable.sort_by(|left, right| {
        let left = &entries[*left];
        let right = &entries[*right];
        right
            .age_seconds
            .cmp(&left.age_seconds)
            .then_with(|| right.size_bytes.cmp(&left.size_bytes))
            .then_with(|| left.path.cmp(&right.path))
    });

    for index in promotable {
        if excess == 0 {
            return;
        }
        let entry = &mut entries[index];
        entry.verdict = LeakedTestHomeVerdict::AbandonedOverBudget;
        entry.reapable = true;
        excess = excess.saturating_sub(entry.size_bytes);
    }
}

#[cfg(test)]
mod tests;
