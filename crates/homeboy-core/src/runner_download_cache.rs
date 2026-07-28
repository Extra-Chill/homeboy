//! The `<artifact-root>/runner` download cache: its layout, the containment
//! rules that keep a remote from writing outside it, and the intent marker that
//! records *why* each cache directory exists.
//!
//! Two subsystems in two crates share this tree and must not disagree about it:
//!
//! - the **writer**,
//!   `homeboy_lab_runner::evidence::download::download_remote_artifact`, which
//!   resolves an output path and tags the cache directory it created;
//! - the **reclaimer**,
//!   [`crate::observation::runs_service::cleanup_runner_downloads`], which reads
//!   that tag to decide whether bytes are homeboy's to reclaim.
//!
//! Before this module the writer joined three remote-influenced strings with no
//! validation and recorded nothing about intent, so the two halves of the
//! contract were both missing. Both live here now, in `homeboy-core`, because
//! `homeboy-lab-runner` depends on core and not the other way round.
//!
//! # Containment (#10586)
//!
//! `RemoteArtifactToken::parse` percent-*decodes* its components after its only
//! containment check (a `/` split that rejects a fourth segment), so a token of
//! `runner-artifact://lab/%2E%2E%2F%2E%2E%2Fetc/passwd` parses "cleanly" into a
//! `run_id` of `../../etc`. A containment check on the encoded form is no check
//! at all — order matters — so [`resolve_runner_download_target`] validates the
//! *decoded* strings, and the file name (which is remote-supplied outright, via
//! the daemon's `filename` field or a `Content-Disposition` header) is reduced
//! to a single sanitized component before it is joined.
//!
//! Three independent barriers, all fail-closed:
//!
//! 1. `runner_id` and `run_id` must each be exactly one normal path component.
//!    They are identifiers, never paths, so this rejects rather than sanitizes:
//!    silently rewriting an id would put bytes somewhere the operator cannot
//!    predict.
//! 2. The file name is *sanitized*, not rejected — a remote may legitimately
//!    return an odd name — down to one component of `[A-Za-z0-9._-]`, with
//!    leading and trailing `.`/`_` trimmed, falling back to the artifact id and
//!    then to [`FALLBACK_FILE_NAME`]. Trimming leading dots is also what makes
//!    it impossible for a remote to overwrite the marker file below.
//! 3. The joined result is re-proved against the cache root with the shared
//!    [`crate::paths::resolve_contained_local_path`] helper, and neither cache
//!    level may be a symlink. Barriers 1 and 2 already make barrier 3
//!    unreachable; it is kept because a future edit to either is one keystroke
//!    away from re-opening the hole.
//!
//! # Intent (#10585)
//!
//! `cleanup --include runner-downloads` can prove a cache directory is old and
//! unclaimed. It cannot prove the operator is *done* with it, because nothing
//! recorded why the bytes were fetched. [`record_download_intent`] writes a
//! [`RUNNER_DOWNLOAD_MARKER_FILE`] sidecar *inside* the cache directory holding
//! the strongest claim made on it, and the cleanup predicate reads it through
//! [`read_download_ownership`].
//!
//! The tag only ever *relaxes*, and only on an explicit `internal_fetch`:
//!
//! | marker state | verdict |
//! | --- | --- |
//! | absent | retain — untagged bytes are operator-owned (covers every cache written before this change) |
//! | unreadable or unparseable | retain |
//! | `operator_pull` | retain |
//! | `internal_fetch` | eligible; the age floor and liveness veto still decide |
//!
//! Operator ownership is sticky: once a cache directory has served an operator
//! pull it never downgrades back to `internal_fetch`, because the operator is
//! holding those bytes regardless of what else fetches into the same directory.
//! And a marker write that fails *removes* the marker rather than leaving a
//! stale one, so the failure mode is "untagged" (retain), never "internal"
//! (reclaimable).

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths;

/// Top-level artifact-root directory owned by the runner download cache.
pub const RUNNER_DOWNLOAD_DIR: &str = "runner";

/// Intent marker written inside each `<runner-id>/<run-id>` cache directory.
///
/// Inside rather than beside, for two reasons: `remove_dir_all` on the cache
/// directory takes the marker with it (a sibling would be orphaned and then
/// reported forever as an unowned loose file), and a dotfile does not collide
/// with the remote-supplied artifact names sharing the directory.
pub const RUNNER_DOWNLOAD_MARKER_FILE: &str = ".homeboy-download.json";

/// Schema tag persisted in the marker. Readers accept unknown fields so an
/// older homeboy can still read a newer marker; the type is additive-only.
pub const RUNNER_DOWNLOAD_MARKER_SCHEMA: &str = "homeboy.runner-download-marker.v1";

/// Name used when neither the remote file name nor the artifact id survives
/// sanitization.
pub const FALLBACK_FILE_NAME: &str = "artifact";

/// Upper bound on artifact ids retained in one marker. The list is provenance
/// for an operator, not an index, so the oldest entries are dropped rather than
/// letting a long-lived cache directory grow the marker without limit.
const MAX_TRACKED_ARTIFACT_IDS: usize = 64;

/// Why a runner artifact was fetched into the download cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerDownloadIntent {
    /// An operator asked for these bytes and the path was reported back to
    /// them. Never reclaimable by the `runner-downloads` category.
    OperatorPull,
    /// Homeboy fetched these bytes for its own use (applying a change artifact,
    /// mirroring runner evidence) and will not read them again.
    InternalFetch,
}

impl RunnerDownloadIntent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OperatorPull => "operator_pull",
            Self::InternalFetch => "internal_fetch",
        }
    }

    /// Combine two claims on the same cache directory. Operator ownership wins
    /// and is never downgraded: an internal fetch landing in a directory the
    /// operator already pulled into does not make those bytes reclaimable.
    fn strongest(self, other: Self) -> Self {
        if self == Self::OperatorPull || other == Self::OperatorPull {
            Self::OperatorPull
        } else {
            Self::InternalFetch
        }
    }
}

/// Persisted intent sidecar.
///
/// Additive only. This file is written by one homeboy build and read by
/// another (and crosses the homeboy/homeboy-extensions boundary, which ships
/// independently), so it deliberately does **not** deny unknown fields and
/// every field except `intent` has a default. `intent` has none: a marker that
/// cannot state an intent is treated as unreadable, which retains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerDownloadMarker {
    #[serde(default = "default_marker_schema")]
    pub schema: String,
    pub intent: RunnerDownloadIntent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_fetched_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fetched_at: Option<String>,
    /// Artifact ids fetched into this cache directory, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<String>,
}

fn default_marker_schema() -> String {
    RUNNER_DOWNLOAD_MARKER_SCHEMA.to_string()
}

/// What the marker says about one cache directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerDownloadOwnership {
    /// No marker. Either the cache predates intent tagging or the writer could
    /// not record one; both mean "assume the operator owns these bytes".
    Unrecorded,
    /// A marker exists but could not be read, is not a regular file, or does
    /// not parse. Uncertainty retains.
    Unreadable,
    Tagged(RunnerDownloadIntent),
}

impl RunnerDownloadOwnership {
    /// The single question the cleanup predicate asks. Only an explicit
    /// `internal_fetch` releases anything.
    pub fn is_reclaimable(self) -> bool {
        matches!(self, Self::Tagged(RunnerDownloadIntent::InternalFetch))
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unrecorded => "unrecorded",
            Self::Unreadable => "unreadable",
            Self::Tagged(intent) => intent.as_str(),
        }
    }

    /// Why a non-reclaimable cache directory is being kept. `None` for the
    /// reclaimable case, which has no retain reason to report.
    pub fn retain_reason(self) -> Option<&'static str> {
        match self {
            Self::Unrecorded => Some(
                "download intent is unrecorded, so the cache is treated as operator-owned (fail closed)",
            ),
            Self::Unreadable => Some(
                "download intent marker is unreadable, so the cache is treated as operator-owned (fail closed)",
            ),
            Self::Tagged(RunnerDownloadIntent::OperatorPull) => {
                Some("an operator pulled these bytes; only the operator releases them")
            }
            Self::Tagged(RunnerDownloadIntent::InternalFetch) => None,
        }
    }
}

/// The resolved, contained destination for one downloaded runner artifact.
#[derive(Debug, Clone)]
pub struct RunnerDownloadTarget {
    /// `<artifact-root>/runner/<runner-id>/<run-id>`.
    pub cache_dir: PathBuf,
    /// `<cache_dir>/<file_name>`.
    pub file_path: PathBuf,
    /// The name actually written, after sanitization. Callers report this
    /// rather than the remote-declared name so reported metadata can never
    /// disagree with the bytes on disk.
    pub file_name: String,
}

/// The cache root, `<artifact-root>/runner`.
pub fn runner_download_root(artifact_root: &Path) -> PathBuf {
    artifact_root.join(RUNNER_DOWNLOAD_DIR)
}

/// Resolve where a downloaded runner artifact may be written.
///
/// # Errors
///
/// Returns a validation error when `runner_id` or `run_id` is not a single
/// normal path component (after percent-decoding — that is the whole point),
/// when the joined path escapes the cache root, or when either cache level
/// already exists as a symlink. It never falls back to a raw or partially
/// validated path.
pub fn resolve_runner_download_target(
    artifact_root: &Path,
    runner_id: &str,
    run_id: &str,
    remote_file_name: Option<&str>,
    artifact_id: &str,
) -> Result<RunnerDownloadTarget> {
    let root = runner_download_root(artifact_root);
    let runner_component = require_single_component("runner_id", runner_id)?;
    let run_component = require_single_component("run_id", run_id)?;
    let file_name = remote_file_name
        .and_then(sanitize_download_file_name)
        .or_else(|| sanitize_download_file_name(artifact_id))
        .unwrap_or_else(|| FALLBACK_FILE_NAME.to_string());

    // Belt and braces: the three components are already proven to be single
    // normal components, so this cannot fail today. It is the check that keeps
    // failing closed if one of them is ever loosened.
    let relative = Path::new(&runner_component)
        .join(&run_component)
        .join(&file_name);
    let file_path = paths::resolve_contained_local_path(&root, &relative, "artifact_output_path")?;

    let cache_dir = file_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| contained_path_error(&file_path))?;
    if !paths::local_path_is_contained(&root, &cache_dir) {
        return Err(contained_path_error(&cache_dir));
    }

    // A symlink at either level would defeat the lexical proof above. Neither
    // level is ever created by anything but this writer, so refusing is safe.
    reject_symlinked_cache_level(&root.join(&runner_component))?;
    reject_symlinked_cache_level(&cache_dir)?;

    Ok(RunnerDownloadTarget {
        cache_dir,
        file_path,
        file_name,
    })
}

/// Reduce a remote-supplied name to a single safe path component.
///
/// Returns `None` when nothing usable survives, so callers can fall back
/// deliberately instead of inheriting an empty or `..`-shaped name. Leading and
/// trailing `.`/`_` are trimmed, which also guarantees the result can never be
/// [`RUNNER_DOWNLOAD_MARKER_FILE`].
pub fn sanitize_download_file_name(value: &str) -> Option<String> {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches(['.', '_']);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// [`sanitize_download_file_name`] with the shared [`FALLBACK_FILE_NAME`]
/// applied. Used by any caller deriving a file name from an artifact id.
pub fn sanitize_artifact_file_name(value: &str) -> String {
    sanitize_download_file_name(value).unwrap_or_else(|| FALLBACK_FILE_NAME.to_string())
}

/// Read the intent marker for one cache directory.
///
/// Every failure mode collapses to [`RunnerDownloadOwnership::Unreadable`],
/// which retains. This function does not error: a cleanup sweep must not abort
/// because one marker is corrupt.
pub fn read_download_ownership(cache_dir: &Path) -> RunnerDownloadOwnership {
    let marker_path = cache_dir.join(RUNNER_DOWNLOAD_MARKER_FILE);
    match fs::symlink_metadata(&marker_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RunnerDownloadOwnership::Unrecorded
        }
        Err(_) => return RunnerDownloadOwnership::Unreadable,
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return RunnerDownloadOwnership::Unreadable
        }
        Ok(_) => {}
    }
    let Ok(raw) = fs::read_to_string(&marker_path) else {
        return RunnerDownloadOwnership::Unreadable;
    };
    match serde_json::from_str::<RunnerDownloadMarker>(&raw) {
        Ok(marker) => RunnerDownloadOwnership::Tagged(marker.intent),
        Err(_) => RunnerDownloadOwnership::Unreadable,
    }
}

/// Record why bytes were fetched into `cache_dir`, merging with any claim
/// already recorded there.
///
/// Best effort by design, and asymmetrically so: the only outcomes are "the
/// marker now states the strongest claim" or "there is no marker", and the
/// second retains. A download must not fail because bookkeeping failed, and
/// bookkeeping must never leave behind a tag that is weaker than the truth.
pub fn record_download_intent(cache_dir: &Path, intent: RunnerDownloadIntent, artifact_id: &str) {
    let marker_path = cache_dir.join(RUNNER_DOWNLOAD_MARKER_FILE);
    let now = chrono::Utc::now().to_rfc3339();
    let existing = fs::read_to_string(&marker_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<RunnerDownloadMarker>(&raw).ok());

    let mut marker = match existing {
        Some(previous) => RunnerDownloadMarker {
            schema: RUNNER_DOWNLOAD_MARKER_SCHEMA.to_string(),
            intent: previous.intent.strongest(intent),
            first_fetched_at: previous.first_fetched_at.or_else(|| Some(now.clone())),
            last_fetched_at: Some(now.clone()),
            artifact_ids: previous.artifact_ids,
        },
        None => RunnerDownloadMarker {
            schema: RUNNER_DOWNLOAD_MARKER_SCHEMA.to_string(),
            intent,
            first_fetched_at: Some(now.clone()),
            last_fetched_at: Some(now),
            artifact_ids: Vec::new(),
        },
    };
    if !artifact_id.is_empty() && !marker.artifact_ids.iter().any(|id| id == artifact_id) {
        marker.artifact_ids.push(artifact_id.to_string());
    }
    if marker.artifact_ids.len() > MAX_TRACKED_ARTIFACT_IDS {
        let overflow = marker.artifact_ids.len() - MAX_TRACKED_ARTIFACT_IDS;
        marker.artifact_ids.drain(0..overflow);
    }

    let Ok(serialized) = serde_json::to_string_pretty(&marker) else {
        let _ = fs::remove_file(&marker_path);
        return;
    };
    if fs::write(&marker_path, serialized).is_err() {
        // Fail closed. A half-written or stale marker could claim
        // `internal_fetch` for a directory an operator now owns.
        let _ = fs::remove_file(&marker_path);
    }
}

fn require_single_component(field: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    let path = Path::new(trimmed);
    let single = !trimmed.is_empty()
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.contains('\0')
        && !path.is_absolute()
        && path.components().count() == 1
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !single {
        return Err(Error::validation_invalid_argument(
            field,
            format!(
                "{field} must be a single path component after decoding; \
                 runner artifact downloads never write outside <artifact-root>/{RUNNER_DOWNLOAD_DIR}"
            ),
            Some(value.to_string()),
            None,
        ));
    }
    Ok(trimmed.to_string())
}

fn reject_symlinked_cache_level(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(Error::validation_invalid_argument(
                "artifact_output_path",
                format!(
                    "runner artifact cache path is a symlink and is refused: {}",
                    path.display()
                ),
                Some(path.display().to_string()),
                None,
            ))
        }
        _ => Ok(()),
    }
}

fn contained_path_error(path: &Path) -> Error {
    Error::validation_invalid_argument(
        "artifact_output_path",
        format!(
            "runner artifact download path escapes <artifact-root>/{RUNNER_DOWNLOAD_DIR}: {}",
            path.display()
        ),
        Some(path.display().to_string()),
        None,
    )
}

#[cfg(test)]
mod tests;
