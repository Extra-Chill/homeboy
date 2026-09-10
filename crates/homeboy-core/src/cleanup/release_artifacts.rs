//! Bounded retention for the durable release artifact store.
//!
//! # The gap this closes
//!
//! `homeboy release` copies every published asset into
//! `<artifact_root>/release/<repo>/<version>/` so a retry, a repair command, or
//! a deploy can reach the exact bytes that were published without rebuilding
//! them. Nothing ever removed those copies. Not a `homeboy cleanup` category,
//! not a retention manifest key, not a specialist prune verb — the store had no
//! count bound, no byte bound, and no age bound of any kind.
//!
//! Every other large managed store is bounded. Runtime runs cap at 100 entries
//! and 1 GiB, the shared Cargo store at 20 GiB, controller runtimes at 2 GiB and
//! 30 days, abandoned test homes at 2 GiB. Release artifacts alone grew without
//! a ceiling, and on one host reached **6.1 GB** — **6.0 GB of it a single
//! repository holding 14 near-identical builds at ~435 MB each**, all published
//! inside a nine-day window while upstream had already moved several minor
//! versions past every one of them (#14223).
//!
//! `orphaned-artifact-bytes` cannot reach this store and never will: these
//! directories are referenced by durable release records, so that category
//! correctly inventories **zero** candidates against them. They are live, not
//! orphaned. Bounding them therefore needs its own policy rather than a wider
//! orphan sweep.
//!
//! # Why deleting them is safe
//!
//! A durable release artifact is a copy of an asset already published to a
//! GitHub Release under an immutable tag. The remote copy is the source of
//! truth; the local one is a cache that exists to avoid a rebuild. Losing an old
//! entry costs a download, not a release.
//!
//! # Why both a count bound and a byte bound
//!
//! Per-release payloads differ by two orders of magnitude across repositories —
//! ~435 MB for one and ~4.3 MB for another *in total across every retained
//! version*. A bare count is a poor bound at that spread: the count that keeps
//! the small repository's whole history is the count that lets the large one
//! consume gigabytes. Both bounds apply per repository and the stricter one
//! wins, so a repository is bounded by whichever limit its own payload size
//! makes binding.
//!
//! # The safety contract
//!
//! 1. **The newest release is never pruned.** Whatever the budgets say, rank 0
//!    for a repository always carries a retention reason. A bound can never
//!    empty a repository's directory.
//! 2. **A young entry is never pruned.** [`RELEASE_ARTIFACT_MIN_AGE_HOURS`]
//!    covers the window in which a release is still being written, so a sweep
//!    racing a publication retains rather than deletes.
//! 3. **Retention is monotone in age.** Once a repository's byte budget is
//!    exhausted, every *older* entry is eligible too. Pruning a newer entry
//!    while keeping an older one is unreachable.
//! 4. **Eligibility is derived, never asserted.** See below.
//!
//! # Eligibility is one decision, not two
//!
//! [`ReleaseArtifactVersion::eligible`] is computed as
//! `retention_reasons.is_empty()` and is never assigned any other way. A
//! populated reason forces `eligible: false` structurally, so the reported
//! `candidate_count` and `estimated_bytes` cannot describe reclaim the apply
//! path will not perform. This is deliberate: #14222 is a live bug in which
//! `eligible` and `retention_reasons` disagree for controller runtimes and
//! cleanup advertises bytes it never frees. That shape is not reproduced here.
//!
//! # Hardlink-aware accounting
//!
//! Each version directory holds its payload under two names — a numbered
//! durable copy and a canonical upload name — because GitHub derives an asset
//! name from its local filename (see `stage_canonical_upload_path`). Those two
//! names are the *same inode*: staging hardlinks first and only copies if the
//! link fails. Summing `st_size` across directory entries therefore reports
//! roughly **twice** the disk a removal would actually free.
//!
//! [`measure_release_version`] deduplicates by `(device, inode)` within a
//! version directory, so `size_bytes` is what removing that directory really
//! returns. The naive total is retained as `logical_bytes` and the difference as
//! `hardlink_duplicate_bytes`, so an operator can see the correction rather than
//! having to trust it.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::Result;

/// Store directory, below the artifact root, that holds durable release copies.
pub const RELEASE_ARTIFACT_STORE: &str = "release";

/// Age floor before a published release directory is eligible for removal.
///
/// A fixed floor rather than a configuration key, matching the precedent set by
/// [`super::RUNNER_MIN_AGE_HOURS`] and
/// [`super::LEAKED_TEST_HOME_MIN_AGE_HOURS`]: it exists to cover the window in
/// which a release is mid-publication and its directory is still being written,
/// which is a property of how long a publish takes rather than of an operator's
/// retention taste. Lowering it would widen a delete predicate for every future
/// sweep at once.
///
/// The newest-entry rule already protects an in-flight release of a *new*
/// version. This floor additionally covers a republication that rewrites an
/// older version's directory.
pub const RELEASE_ARTIFACT_MIN_AGE_HOURS: u64 = 1;

/// Versions described individually in one report.
///
/// Counts and byte totals always cover every inspected version; only the
/// per-version detail list is bounded.
const MAX_REPORTED_VERSIONS: usize = 50;

const REASON_LATEST: &str = "newest published release for this repository is never pruned";
const REASON_TOO_YOUNG: &str = "younger than the in-flight release age floor";

/// One published version directory under one repository.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseArtifactVersion {
    pub repo: String,
    pub version: String,
    pub path: PathBuf,
    /// Disk a removal actually returns: allocated blocks, counting each inode
    /// once even when several directory entries share it.
    pub size_bytes: u64,
    /// Naive sum of `st_size` across every file entry, hardlinked or not.
    /// Reported so the correction below is visible rather than implicit.
    pub logical_bytes: u64,
    /// Bytes `logical_bytes` double-counts because two names share one inode.
    pub hardlink_duplicate_bytes: u64,
    pub age_seconds: u64,
    /// Position among this repository's versions, newest first. `0` is the
    /// current release and is never pruned.
    pub rank: usize,
    /// Every reason this version is retained. Empty means eligible.
    pub retention_reasons: Vec<String>,
    /// Always `retention_reasons.is_empty()`. Never assigned independently.
    pub eligible: bool,
    pub removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removal_error: Option<String>,
}

/// Per-repository rollup, so an operator sees which repository holds the bytes.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseArtifactRepo {
    pub repo: String,
    pub path: PathBuf,
    pub version_count: usize,
    pub total_size_bytes: u64,
    pub retained_size_bytes: u64,
    pub candidate_count: usize,
    pub estimated_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ReleaseArtifactCleanupOptions {
    pub apply: bool,
    /// Versions retained per repository before the count bound binds.
    pub max_count_per_repo: usize,
    /// Ceiling on retained release bytes per repository.
    pub max_bytes_per_repo: u64,
    /// Age floor covering an in-flight publication.
    pub min_age: Duration,
    /// Maximum version directories inspected in one pass.
    pub limit: usize,
    /// Release store root. `None` resolves `<artifact_root>/release`.
    pub root: Option<PathBuf>,
}

impl Default for ReleaseArtifactCleanupOptions {
    fn default() -> Self {
        Self {
            apply: false,
            max_count_per_repo: usize::MAX,
            max_bytes_per_repo: u64::MAX,
            min_age: Duration::from_secs(RELEASE_ARTIFACT_MIN_AGE_HOURS * 3_600),
            limit: 1_000,
            root: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseArtifactCleanupOutput {
    pub command: &'static str,
    pub dry_run: bool,
    pub root: PathBuf,
    /// False when the store root could not be read. A store that was not
    /// inspected is reported as such rather than contributing a silent zero.
    pub root_inspected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub max_count_per_repo: usize,
    pub max_bytes_per_repo: u64,
    pub min_age_seconds: u64,
    pub repos: Vec<ReleaseArtifactRepo>,
    pub inspected_count: usize,
    /// Genuinely removable versions only. Never the inspected total.
    pub candidate_count: usize,
    pub removed_count: usize,
    pub skipped_count: usize,
    /// Hardlink-corrected bytes of the candidates above, and of nothing else.
    pub estimated_bytes: u64,
    pub reclaimed_bytes: u64,
    pub retained_size_bytes: u64,
    pub total_size_bytes: u64,
    /// True when the inspection limit stopped the scan early, so the totals are
    /// a floor rather than the whole store.
    pub truncated: bool,
    /// True when per-version detail was capped. Counts and totals are unaffected.
    pub entries_truncated: bool,
    pub versions: Vec<ReleaseArtifactVersion>,
}

/// Plan, and optionally apply, reclamation of superseded release artifacts.
///
/// Never consults the observation store: a version directory's identity is its
/// path and its retention is decided by rank, age, and size, all of which are
/// filesystem-local.
///
/// # Errors
///
/// Returns an error only when the artifact root itself cannot be resolved. An
/// unreadable store root is reported on the output rather than failing the pass,
/// so one bad directory cannot hide the bytes in the others.
pub fn cleanup_release_artifacts(
    options: ReleaseArtifactCleanupOptions,
) -> Result<ReleaseArtifactCleanupOutput> {
    let root = match options.root.clone() {
        Some(root) => root,
        None => crate::paths::artifact_root()?.join(RELEASE_ARTIFACT_STORE),
    };
    let now = SystemTime::now();

    let mut versions: Vec<ReleaseArtifactVersion> = Vec::new();
    let mut repo_paths: Vec<(String, PathBuf)> = Vec::new();
    let mut truncated = false;
    let mut root_inspected = false;
    let mut skip_reason = None;

    match fs::read_dir(&root) {
        Ok(read) => {
            root_inspected = true;
            let mut repos: Vec<(String, PathBuf)> = read
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    let name = path.file_name()?.to_str()?.to_string();
                    is_plain_directory(&path).then_some((name, path))
                })
                .collect();
            // Deterministic order so a truncated pass inspects the same
            // repositories on every run rather than whatever the directory
            // iterator yielded.
            repos.sort_by(|left, right| left.0.cmp(&right.0));

            for (repo, repo_path) in repos {
                if truncated {
                    break;
                }
                repo_paths.push((repo.clone(), repo_path.clone()));
                let Ok(read) = fs::read_dir(&repo_path) else {
                    continue;
                };
                for entry in read.flatten() {
                    if versions.len() >= options.limit {
                        truncated = true;
                        break;
                    }
                    let path = entry.path();
                    if !is_plain_directory(&path) {
                        continue;
                    }
                    let Some(version) = path.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    let usage = measure_release_version(&path);
                    versions.push(ReleaseArtifactVersion {
                        repo: repo.clone(),
                        version: version.to_string(),
                        path,
                        size_bytes: usage.allocated_bytes,
                        logical_bytes: usage.logical_bytes,
                        hardlink_duplicate_bytes: usage.hardlink_duplicate_bytes,
                        age_seconds: usage.age_seconds(now),
                        // Placeholder until `classify` runs. Chosen as the most
                        // conservative value so a classification that never ran
                        // cannot delete anything.
                        rank: 0,
                        retention_reasons: vec![REASON_LATEST.to_string()],
                        eligible: false,
                        removed: false,
                        removal_error: None,
                    });
                }
            }
        }
        Err(error) => skip_reason = Some(error.to_string()),
    }

    classify(&mut versions, &options);

    let mut removed_count = 0usize;
    let mut reclaimed_bytes = 0u64;
    for version in &mut versions {
        if !version.eligible || !options.apply {
            continue;
        }
        match fs::remove_dir_all(&version.path) {
            Ok(()) => {
                version.removed = true;
                removed_count += 1;
                reclaimed_bytes = reclaimed_bytes.saturating_add(version.size_bytes);
            }
            Err(error) => version.removal_error = Some(error.to_string()),
        }
    }

    let repos = summarize_repos(&repo_paths, &versions);
    let inspected_count = versions.len();
    let candidate_count = versions.iter().filter(|version| version.eligible).count();
    let estimated_bytes = versions
        .iter()
        .filter(|version| version.eligible)
        .fold(0u64, |total, version| {
            total.saturating_add(version.size_bytes)
        });
    let total_size_bytes = versions.iter().fold(0u64, |total, version| {
        total.saturating_add(version.size_bytes)
    });
    let retained_size_bytes = versions
        .iter()
        .filter(|version| !version.removed)
        .fold(0u64, |total, version| {
            total.saturating_add(version.size_bytes)
        });

    // Largest first: an operator triaging disk pressure wants the 435 MB build
    // named, not whichever version the directory iterator happened to yield.
    versions.sort_by(|left, right| {
        right
            .size_bytes
            .cmp(&left.size_bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    let entries_truncated = versions.len() > MAX_REPORTED_VERSIONS;
    versions.truncate(MAX_REPORTED_VERSIONS);

    Ok(ReleaseArtifactCleanupOutput {
        command: "cleanup.release_artifacts",
        dry_run: !options.apply,
        root,
        root_inspected,
        skip_reason,
        max_count_per_repo: options.max_count_per_repo,
        max_bytes_per_repo: options.max_bytes_per_repo,
        min_age_seconds: options.min_age.as_secs(),
        repos,
        inspected_count,
        candidate_count,
        removed_count,
        skipped_count: inspected_count.saturating_sub(candidate_count),
        estimated_bytes,
        reclaimed_bytes,
        retained_size_bytes,
        total_size_bytes,
        truncated,
        entries_truncated,
        versions,
    })
}

fn summarize_repos(
    repo_paths: &[(String, PathBuf)],
    versions: &[ReleaseArtifactVersion],
) -> Vec<ReleaseArtifactRepo> {
    repo_paths
        .iter()
        .map(|(repo, path)| {
            let owned = versions.iter().filter(|version| &version.repo == repo);
            let mut summary = ReleaseArtifactRepo {
                repo: repo.clone(),
                path: path.clone(),
                version_count: 0,
                total_size_bytes: 0,
                retained_size_bytes: 0,
                candidate_count: 0,
                estimated_bytes: 0,
            };
            for version in owned {
                summary.version_count += 1;
                summary.total_size_bytes =
                    summary.total_size_bytes.saturating_add(version.size_bytes);
                if version.eligible {
                    summary.candidate_count += 1;
                    summary.estimated_bytes =
                        summary.estimated_bytes.saturating_add(version.size_bytes);
                } else {
                    summary.retained_size_bytes = summary
                        .retained_size_bytes
                        .saturating_add(version.size_bytes);
                }
            }
            summary
        })
        .collect()
}

/// Assign every version a rank and its retention reasons, then derive
/// eligibility from those reasons and nothing else.
///
/// Grouping is per repository because both budgets are per repository: one
/// repository's build size must not decide another's retention.
fn classify(versions: &mut [ReleaseArtifactVersion], options: &ReleaseArtifactCleanupOptions) {
    let mut repos: Vec<String> = versions
        .iter()
        .map(|version| version.repo.clone())
        .collect();
    repos.sort();
    repos.dedup();

    for repo in repos {
        let mut indexes: Vec<usize> = versions
            .iter()
            .enumerate()
            .filter(|(_, version)| version.repo == repo)
            .map(|(index, _)| index)
            .collect();
        // Newest first. Ties break on version name so ordering — and therefore
        // which entry is protected as "newest" — is deterministic.
        indexes.sort_by(|left, right| {
            let left = &versions[*left];
            let right = &versions[*right];
            left.age_seconds
                .cmp(&right.age_seconds)
                .then_with(|| right.version.cmp(&left.version))
        });

        let mut retained_bytes = 0u64;
        let mut budget_exhausted = false;
        for (rank, index) in indexes.into_iter().enumerate() {
            let version = &mut versions[index];
            version.rank = rank;

            let projected = retained_bytes.saturating_add(version.size_bytes);
            // Latched: once a repository's budget is gone it stays gone for
            // every older entry, so retention can never keep an older version
            // while pruning a newer one.
            let within_bytes = !budget_exhausted && projected <= options.max_bytes_per_repo;
            if !within_bytes {
                budget_exhausted = true;
            }

            let mut reasons = Vec::new();
            if rank == 0 {
                reasons.push(REASON_LATEST.to_string());
            } else if rank < options.max_count_per_repo && within_bytes {
                reasons.push(format!(
                    "within the newest-{} and {}-byte per-repository retention budget",
                    options.max_count_per_repo, options.max_bytes_per_repo
                ));
            }
            if Duration::from_secs(version.age_seconds) < options.min_age {
                reasons.push(REASON_TOO_YOUNG.to_string());
            }

            // The whole eligibility model, in one line. `eligible` has no other
            // assignment anywhere in this module, so a populated reason cannot
            // coexist with a promise to reclaim (#14222).
            version.eligible = reasons.is_empty();
            version.retention_reasons = reasons;

            if !version.eligible {
                retained_bytes = projected;
            }
        }
    }
}

/// Hardlink-corrected footprint of one version directory.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReleaseVersionUsage {
    /// Disk a removal returns. Each inode counted once.
    pub allocated_bytes: u64,
    /// Naive `st_size` sum across every file entry.
    pub logical_bytes: u64,
    /// The double-count `logical_bytes` carries and `allocated_bytes` does not.
    pub hardlink_duplicate_bytes: u64,
    pub newest_modified: Option<SystemTime>,
}

impl ReleaseVersionUsage {
    fn age_seconds(&self, now: SystemTime) -> u64 {
        self.newest_modified
            .and_then(|modified| now.duration_since(modified).ok())
            .map(|elapsed| elapsed.as_secs())
            // A metadata read that fails resolves to age zero, which reads as
            // "too young to touch". Failing the other way would let an
            // unreadable timestamp widen a delete predicate.
            .unwrap_or(0)
    }
}

/// Measure one version directory, counting each inode once.
///
/// The inode set is scoped to this directory, which is the correct scope for
/// the question being asked: "how much does removing *this* directory return".
#[must_use]
pub fn measure_release_version(path: &Path) -> ReleaseVersionUsage {
    let mut seen = HashSet::new();
    let mut usage = ReleaseVersionUsage::default();
    accumulate(path, &mut seen, &mut usage);
    usage
}

fn accumulate(path: &Path, seen: &mut HashSet<(u64, u64)>, usage: &mut ReleaseVersionUsage) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if let Ok(modified) = metadata.modified() {
        usage.newest_modified = Some(match usage.newest_modified {
            Some(current) => current.max(modified),
            None => modified,
        });
    }

    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        // Directory blocks are reclaimed with the tree, so they count toward
        // allocated bytes. They are not payload, so they stay out of the
        // logical total, which exists to be compared against the naive sum an
        // uncorrected implementation would report.
        usage.allocated_bytes = usage
            .allocated_bytes
            .saturating_add(allocated_bytes(&metadata));
        let Ok(read) = fs::read_dir(path) else {
            return;
        };
        for entry in read.flatten() {
            accumulate(&entry.path(), seen, usage);
        }
        return;
    }

    usage.logical_bytes = usage.logical_bytes.saturating_add(metadata.len());
    if first_link_to_inode(&metadata, seen) {
        usage.allocated_bytes = usage
            .allocated_bytes
            .saturating_add(allocated_bytes(&metadata));
    } else {
        usage.hardlink_duplicate_bytes = usage
            .hardlink_duplicate_bytes
            .saturating_add(metadata.len());
    }
}

/// True when this entry is the first name seen for its inode.
///
/// Only multiply-linked entries are tracked. A file with `nlink == 1` cannot be
/// a duplicate of anything, so keeping it out of the set holds the set to the
/// size of the hardlinked population rather than the whole tree.
#[cfg(unix)]
fn first_link_to_inode(metadata: &fs::Metadata, seen: &mut HashSet<(u64, u64)>) -> bool {
    use std::os::unix::fs::MetadataExt;

    if metadata.nlink() <= 1 {
        return true;
    }
    seen.insert((metadata.dev(), metadata.ino()))
}

/// Without inode identity a duplicate cannot be recognized, so every entry
/// counts. This over-reports reclaimable bytes on a hardlinked layout rather
/// than under-reporting retained ones.
#[cfg(not(unix))]
fn first_link_to_inode(_metadata: &fs::Metadata, _seen: &mut HashSet<(u64, u64)>) -> bool {
    true
}

#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;

    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

fn is_plain_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests;
