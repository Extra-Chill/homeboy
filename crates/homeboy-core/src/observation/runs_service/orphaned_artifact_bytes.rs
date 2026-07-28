//! Reap crash-orphaned scratch bytes under the artifact root.
//!
//! Every other artifact-root cleanup surface is database-row driven, so bytes
//! that were written before their row existed are structurally invisible to
//! them. Two artifact-root path families are created *outside* any durable
//! journal and are only reclaimed by an in-process cleanup branch, which means
//! SIGKILL/OOM leaks them permanently:
//!
//! * `<root>/<run_id>/.artifact-<uuid>.staging` — the staging sibling written
//!   by `staged_artifact_path` before a file artifact is hard-linked into
//!   place.
//! * `<root>/_scratch/patch-<label>-<uuid>/` — the full working-tree baseline
//!   copy taken by daemon patch capture, reclaimed only by `impl Drop`.
//!
//! # Why this is not a generic orphan walk
//!
//! Issue #10284 proposes walking the artifact root and removing anything with
//! no matching `artifacts.path` row. That is not safe here, for two reasons
//! that are both load-bearing:
//!
//! 1. **The artifact root is a shared namespace, not an artifact table
//!    projection.** At least ten subsystems own top-level subtrees under it
//!    that are never registered as artifact rows at those paths — `runner/`,
//!    `runner-attach/`, `runner-exec-attach/`, `agent-task/`,
//!    `agent-task-loop-controller/`, `controller-scratch-recovery/`,
//!    `recovered-runner-artifacts/`, `executor-finalized/`,
//!    `preview-consumer/`, and `_scratch/`. A row-join reaper would classify
//!    all of them as orphans and delete live state.
//! 2. **Artifact bytes are created before their row exists, by design.** The
//!    window between `copy_artifact_file`/`copy_artifact_directory` and the
//!    INSERT is exactly when a row-join reaper would see an "orphan". Deleting
//!    there corrupts a publication that is actively succeeding.
//!
//! So the ownership proof used here is *name shape*, not row absence: both
//! reaped families are produced by a single private constructor whose name
//! format no other writer emits. Age is then required on top of that, to cover
//! the in-flight window. A missing row proves nothing about these paths — they
//! never get one — so the database is deliberately not consulted at all.
//!
//! # Size is advisory and never gates removal
//!
//! `bounded_directory_size` in the lab workspace pruner shells out to `du -sk`
//! and feeds `size.is_some()` into the liveness verdict
//! (`workspace/sync/mod.rs:2602-2604`, `:3457`). When `du` failed, size came
//! back `None`, liveness degraded to `"unknown"`, and prune silently became a
//! no-op. That couples an advisory measurement to a deletion decision, in the
//! direction that quietly disables the feature.
//!
//! Here the removal decision is a pure function of (name shape, entry type,
//! age, symlink-freedom, containment). Size is measured best-effort purely for
//! reporting: a failed measurement reports `size_measured: false` and still
//! removes, because it was never evidence of anything.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use uuid::Uuid;

use super::persisted_cleanup::{path_is_within_root, symlink_metadata_if_exists};
use crate::{Error, Result};

/// Minimum age before a name-owned scratch path is treated as crash residue.
///
/// This is the only liveness signal these paths have — nothing journals them,
/// so there is no lease to consult and no owning process to probe. The floor is
/// deliberately far larger than any plausible in-process window (a patch-capture
/// baseline copy of a large working tree, or a staging copy of a multi-gigabyte
/// artifact) and is intentionally not operator-overridable.
pub const ORPHANED_ARTIFACT_BYTES_MIN_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Top-level artifact-root directory owned by daemon patch capture.
const PATCH_CAPTURE_SCRATCH_DIR: &str = "_scratch";
/// Name prefix emitted by `patch_capture::create_scratch_dir`.
const PATCH_CAPTURE_SCRATCH_PREFIX: &str = "patch-";
/// Name affixes emitted by `store::artifacts::staged_artifact_path`.
const ARTIFACT_STAGING_PREFIX: &str = ".artifact-";
const ARTIFACT_STAGING_SUFFIX: &str = ".staging";

/// Owner of a reaped path, reported so operators can attribute the leak.
const OWNER_ARTIFACT_STAGING: &str = "artifact-staging";
const OWNER_PATCH_CAPTURE_SCRATCH: &str = "patch-capture-scratch";

#[derive(Debug, Clone)]
pub struct OrphanedArtifactBytesCleanupOptions {
    pub apply: bool,
    /// Maximum number of candidate paths inspected. Bounds a single sweep the
    /// same way the persisted-artifact cleanup limit does.
    pub limit: usize,
}

impl Default for OrphanedArtifactBytesCleanupOptions {
    fn default() -> Self {
        Self {
            apply: false,
            limit: 1000,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OrphanedArtifactBytesCleanupOutcome {
    pub dry_run: bool,
    pub artifact_root: PathBuf,
    pub min_age_seconds: u64,
    pub inspected_count: usize,
    pub planned_count: usize,
    pub removed_count: usize,
    pub skipped_count: usize,
    pub planned_size_bytes: u64,
    pub removed_size_bytes: u64,
    /// True when the sweep stopped at `limit` with candidates still unread.
    pub truncated: bool,
    pub rows: Vec<OrphanedArtifactBytesRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrphanedArtifactBytesRow {
    pub path: String,
    /// Which private constructor owns this name shape.
    pub owner: &'static str,
    #[serde(rename = "type")]
    pub entry_type: &'static str,
    pub age_seconds: u64,
    pub size_bytes: u64,
    /// Size is reported for operators, never used to decide removal. `false`
    /// means the measurement failed and `size_bytes` is a floor, not a fact.
    pub size_measured: bool,
    pub action: String,
    pub reason: String,
}

/// Reap crash-orphaned artifact-root scratch under the configured root.
pub fn cleanup_orphaned_artifact_bytes(
    options: OrphanedArtifactBytesCleanupOptions,
) -> Result<OrphanedArtifactBytesCleanupOutcome> {
    let artifact_root = crate::artifacts::root()?;
    cleanup_orphaned_artifact_bytes_in(&artifact_root, options)
}

/// Reap crash-orphaned scratch under a caller-provided artifact root.
pub fn cleanup_orphaned_artifact_bytes_in(
    artifact_root: &Path,
    options: OrphanedArtifactBytesCleanupOptions,
) -> Result<OrphanedArtifactBytesCleanupOutcome> {
    sweep(
        artifact_root,
        options,
        ORPHANED_ARTIFACT_BYTES_MIN_AGE,
        SystemTime::now(),
    )
}

/// The sweep with its age floor and clock supplied explicitly. Both are fixed
/// on every production path; tests move them so the age comparison, the
/// future-mtime fail-closed branch, and the removal branch are all reachable
/// without rewriting filesystem timestamps.
fn sweep(
    artifact_root: &Path,
    options: OrphanedArtifactBytesCleanupOptions,
    min_age: Duration,
    now: SystemTime,
) -> Result<OrphanedArtifactBytesCleanupOutcome> {
    let mut outcome = OrphanedArtifactBytesCleanupOutcome {
        dry_run: !options.apply,
        artifact_root: artifact_root.to_path_buf(),
        min_age_seconds: min_age.as_secs(),
        inspected_count: 0,
        planned_count: 0,
        removed_count: 0,
        skipped_count: 0,
        planned_size_bytes: 0,
        removed_size_bytes: 0,
        truncated: false,
        rows: Vec::new(),
    };

    for top_level in sorted_child_names(artifact_root)? {
        let container = artifact_root.join(&top_level);
        // Only descend one level. Deeper trees belong to subsystems that own
        // their own lifecycle; this sweep must never walk into them.
        let Some(container_metadata) = symlink_metadata_if_exists(&container)? else {
            continue;
        };
        if !container_metadata.is_dir() || container_metadata.file_type().is_symlink() {
            continue;
        }
        // An unreadable subtree is skipped, not fatal: one bad directory must
        // not fail the whole cleanup inventory. Skipping can only under-reap.
        let Ok(names) = sorted_child_names(&container) else {
            continue;
        };
        for name in names {
            let path = container.join(&name);
            let Some(metadata) = symlink_metadata_if_exists(&path)? else {
                continue;
            };
            let Some(candidate) = classify_candidate(&top_level, &name, &metadata) else {
                continue;
            };
            // Bound only on real candidates, so `truncated` means "candidates
            // remain", not "the root has many unrelated entries".
            if outcome.inspected_count >= options.limit {
                outcome.truncated = true;
                return Ok(outcome);
            }
            outcome.inspected_count += 1;
            sweep_candidate(
                &mut outcome,
                artifact_root,
                &path,
                candidate,
                &metadata,
                min_age,
                now,
            );
        }
    }

    Ok(outcome)
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    owner: &'static str,
    entry_type: &'static str,
}

/// Decide whether a depth-two artifact-root entry is one of the two name
/// families this sweep owns. Everything else returns `None` and is never
/// inspected, reported, or removed.
fn classify_candidate(
    container_name: &str,
    entry_name: &str,
    metadata: &fs::Metadata,
) -> Option<Candidate> {
    if metadata.file_type().is_symlink() {
        return None;
    }
    if metadata.is_file() && is_artifact_staging_name(entry_name) {
        return Some(Candidate {
            owner: OWNER_ARTIFACT_STAGING,
            entry_type: "file",
        });
    }
    if metadata.is_dir()
        && container_name == PATCH_CAPTURE_SCRATCH_DIR
        && is_patch_capture_scratch_name(entry_name)
    {
        return Some(Candidate {
            owner: OWNER_PATCH_CAPTURE_SCRATCH,
            entry_type: "directory",
        });
    }
    None
}

fn sweep_candidate(
    outcome: &mut OrphanedArtifactBytesCleanupOutcome,
    artifact_root: &Path,
    path: &Path,
    candidate: Candidate,
    metadata: &fs::Metadata,
    min_age: Duration,
    now: SystemTime,
) {
    // Fail closed on every uncertainty below: an unreadable or future-dated
    // mtime, or a path that does not resolve inside the artifact root, is
    // reported and kept rather than guessed to be residue.
    let Some(age) = entry_age(metadata, now) else {
        outcome.skip(
            path,
            candidate,
            0,
            "modification time is unreadable or in the future",
        );
        return;
    };
    if age < min_age {
        outcome.skip(
            path,
            candidate,
            age.as_secs(),
            "younger than the crash-residue age floor",
        );
        return;
    }
    if !path_is_within_root(path, artifact_root) {
        outcome.skip(
            path,
            candidate,
            age.as_secs(),
            "path does not resolve inside the artifact root",
        );
        return;
    }

    // Advisory only. A `None` here changes the report, never the verdict.
    let measured = best_effort_size(path, metadata);
    let size_bytes = measured.unwrap_or(0);
    outcome.planned_count += 1;
    outcome.planned_size_bytes += size_bytes;

    if outcome.dry_run {
        outcome.rows.push(row(
            path,
            candidate,
            age.as_secs(),
            size_bytes,
            measured.is_some(),
            "remove",
            "crash-orphaned scratch owned by a single private constructor",
        ));
        return;
    }

    match remove_candidate(path, candidate) {
        Ok(()) => {
            outcome.removed_count += 1;
            outcome.removed_size_bytes += size_bytes;
            outcome.rows.push(row(
                path,
                candidate,
                age.as_secs(),
                size_bytes,
                measured.is_some(),
                "removed",
                "crash-orphaned scratch owned by a single private constructor",
            ));
        }
        Err(error) => {
            // One unremovable path must not abort the sweep or fail the
            // command; it is reported so the leak stays visible.
            outcome.planned_count -= 1;
            outcome.planned_size_bytes -= size_bytes;
            outcome.skipped_count += 1;
            outcome.rows.push(row(
                path,
                candidate,
                age.as_secs(),
                size_bytes,
                measured.is_some(),
                "skip",
                &format!("removal failed: {error}"),
            ));
        }
    }
}

impl OrphanedArtifactBytesCleanupOutcome {
    /// Record a retained candidate. Size is not measured for skipped paths:
    /// it would report a number that no decision consumed.
    fn skip(&mut self, path: &Path, candidate: Candidate, age_seconds: u64, reason: &str) {
        self.skipped_count += 1;
        self.rows
            .push(row(path, candidate, age_seconds, 0, false, "skip", reason));
    }
}

fn remove_candidate(path: &Path, candidate: Candidate) -> io::Result<()> {
    if candidate.entry_type == "directory" {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn row(
    path: &Path,
    candidate: Candidate,
    age_seconds: u64,
    size_bytes: u64,
    size_measured: bool,
    action: &str,
    reason: &str,
) -> OrphanedArtifactBytesRow {
    OrphanedArtifactBytesRow {
        path: path.display().to_string(),
        owner: candidate.owner,
        entry_type: candidate.entry_type,
        age_seconds,
        size_bytes,
        size_measured,
        action: action.to_string(),
        reason: reason.to_string(),
    }
}

/// `.artifact-<uuid>.staging`, the exact shape `staged_artifact_path` emits.
/// The UUID must parse: a prefix match alone would also accept operator files.
fn is_artifact_staging_name(name: &str) -> bool {
    name.strip_prefix(ARTIFACT_STAGING_PREFIX)
        .and_then(|rest| rest.strip_suffix(ARTIFACT_STAGING_SUFFIX))
        .is_some_and(|id| Uuid::parse_str(id).is_ok())
}

/// `patch-<label>-<uuid>`, the exact shape `patch_capture::create_scratch_dir`
/// emits. The label is not pinned to today's `baseline`/`after` values, but the
/// trailing UUID is required, so a hand-made `_scratch/patch-notes` is ignored.
fn is_patch_capture_scratch_name(name: &str) -> bool {
    // A hyphenated UUID contains hyphens, so the label boundary is found by
    // fixed width from the end, not by splitting on the last separator.
    const HYPHENATED_UUID_LEN: usize = 36;
    let Some(rest) = name.strip_prefix(PATCH_CAPTURE_SCRATCH_PREFIX) else {
        return false;
    };
    if rest.len() <= HYPHENATED_UUID_LEN + 1 {
        return false;
    }
    let id_start = rest.len() - HYPHENATED_UUID_LEN;
    if !rest.is_char_boundary(id_start) || !rest.is_char_boundary(id_start - 1) {
        return false;
    }
    if rest.as_bytes()[id_start - 1] != b'-' {
        return false;
    }
    Uuid::parse_str(&rest[id_start..]).is_ok()
}

fn entry_age(metadata: &fs::Metadata, now: SystemTime) -> Option<Duration> {
    // `duration_since` errors on a future mtime, which is exactly the clock-skew
    // case that must not be rounded down to "old enough".
    now.duration_since(metadata.modified().ok()?).ok()
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
                Some(format!("read artifact root {}", path.display())),
            ))
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read artifact root {}", path.display())),
            )
        })?;
        names.push(entry.file_name().to_string_lossy().to_string());
    }
    names.sort();
    Ok(names)
}

/// Apparent size for reporting. Returns `None` on any read failure rather than
/// a partial total, so the report can say the number is untrustworthy instead
/// of quietly presenting a floor as a measurement. Never follows symlinks.
fn best_effort_size(path: &Path, metadata: &fs::Metadata) -> Option<u64> {
    const MAX_DEPTH: usize = 64;
    fn walk(path: &Path, metadata: &fs::Metadata, depth: usize) -> Option<u64> {
        if metadata.file_type().is_symlink() {
            return Some(0);
        }
        if !metadata.is_dir() {
            return Some(metadata.len());
        }
        if depth == 0 {
            return None;
        }
        let mut total = metadata.len();
        for entry in fs::read_dir(path).ok()? {
            let entry = entry.ok()?;
            let child = entry.path();
            let child_metadata = fs::symlink_metadata(&child).ok()?;
            total = total.checked_add(walk(&child, &child_metadata, depth - 1)?)?;
        }
        Some(total)
    }
    walk(path, metadata, MAX_DEPTH)
}

#[cfg(test)]
mod tests;
