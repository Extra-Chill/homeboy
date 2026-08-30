//! Sha-keyed cache of base-branch test measurements.
//!
//! # Why keyed by sha
//!
//! The expensive half of a differential verdict is measuring the base branch.
//! Keying that measurement by the base revision's sha means it is paid **once
//! per base-branch movement** rather than once per branch: every branch cut
//! from the same `main` reuses one measurement, and a `main` that has moved is
//! a *miss* rather than a stale answer. Without this the whole feature is just
//! automating the thing that was already too slow.
//!
//! # Why the scope is part of the key
//!
//! `cargo test -p homeboy-core --lib` and a whole-workspace run are different
//! measurements of different things, and comparing one against the other
//! produces confident nonsense. The scope string is therefore part of the key,
//! and it is compared **verbatim**: a different argument order is a different
//! key. That costs an occasional avoidable miss and buys the guarantee that a
//! hit is always a like-for-like comparison. A miss is cheap; a wrong hit is a
//! false verdict.
//!
//! # Why every mismatch degrades to a miss
//!
//! [`BaselineCache::load`] returns `None` for an absent file, unreadable JSON,
//! an unknown schema, or any recorded identity that disagrees with the key.
//! None of those are errors the caller can act on, and all of them mean the
//! same thing: there is nothing here that can honestly be compared. The verdict
//! layer turns that into [`super::DifferentialVerdict::NoBaseline`], which
//! blocks — so a corrupted cache degrades to "prove it yourself", never to a
//! clean verdict.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use homeboy_core::error::{Error, Result};
use homeboy_core::paths;

use super::{BaselineEvidence, TestMeasurement};

/// Named store below the Homeboy data root.
pub const BASELINE_CACHE_STORE: &str = "test-differential-baselines";

/// Bumped whenever the on-disk record shape changes. An unknown schema is a
/// miss, not an error.
pub const BASELINE_CACHE_SCHEMA: u32 = 1;

/// Scope string used when a run selected no explicit scope.
pub const WHOLE_SUITE_SCOPE: &str = "whole-suite";

/// Longest sanitized scope prefix kept in a cache file name, before the hash
/// suffix that actually guarantees uniqueness.
const SCOPE_NAME_PREFIX_LIMIT: usize = 48;

/// Default cache root: `<homeboy data>/test-differential-baselines`.
pub fn default_root() -> Result<PathBuf> {
    paths::homeboy_data_store(BASELINE_CACHE_STORE)
}

/// Canonical scope key for a test invocation.
///
/// Trims and drops empty arguments, then joins verbatim. Order is preserved
/// deliberately — see the module docs on why a miss beats a wrong hit.
pub fn scope_key(args: &[String]) -> String {
    let joined = args
        .iter()
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        WHOLE_SUITE_SCOPE.to_string()
    } else {
        joined
    }
}

/// The identity a cached measurement must match to be usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineCacheKey {
    pub component_id: String,
    /// The base ref as the caller named it, e.g. `origin/main`.
    pub reference: String,
    /// The revision `reference` resolves to.
    pub revision: String,
    /// Canonical scope string from [`scope_key`].
    pub scope: String,
}

impl BaselineCacheKey {
    pub fn new(
        component_id: impl Into<String>,
        reference: impl Into<String>,
        revision: impl Into<String>,
        scope: impl Into<String>,
    ) -> Self {
        Self {
            component_id: component_id.into().trim().to_string(),
            reference: reference.into().trim().to_string(),
            revision: revision.into().trim().to_string(),
            scope: scope.into().trim().to_string(),
        }
    }

    /// Filesystem-safe, collision-resistant name for this scope.
    ///
    /// A readable prefix keeps the cache inspectable by hand; the hash suffix
    /// is what makes distinct scopes distinct after sanitization has collapsed
    /// punctuation.
    pub fn scope_fingerprint(&self) -> String {
        let sanitized = paths::sanitize_path_segment(&self.scope);
        let prefix: String = sanitized.chars().take(SCOPE_NAME_PREFIX_LIMIT).collect();
        format!("{prefix}-{:016x}", fnv1a64(self.scope.as_bytes()))
    }

    /// `<component>/<revision>/<scope>.json`, relative to the cache root.
    pub fn relative_path(&self) -> PathBuf {
        PathBuf::from(paths::sanitize_path_segment(&self.component_id))
            .join(paths::sanitize_path_segment(&self.revision))
            .join(format!("{}.json", self.scope_fingerprint()))
    }

    /// Directory holding every revision recorded for this component.
    fn component_dir(&self) -> PathBuf {
        PathBuf::from(paths::sanitize_path_segment(&self.component_id))
    }
}

/// One cached base-branch measurement, as stored on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedBaselineRecord {
    pub schema: u32,
    pub component_id: String,
    pub reference: String,
    pub revision: String,
    pub scope: String,
    /// RFC 3339 timestamp of when this measurement was recorded.
    pub recorded_at: String,
    pub measurement: TestMeasurement,
}

impl CachedBaselineRecord {
    /// Whether this record can honestly answer `key`.
    ///
    /// Component, revision, and scope are all compared: a file found at the
    /// right path but carrying a different identity has been corrupted or
    /// hand-edited, and trusting the path over the contents would turn that
    /// into a silent wrong answer. The *reference* is deliberately not
    /// compared — `main` and `origin/main` resolving to the same sha are the
    /// same measurement.
    pub fn matches(&self, key: &BaselineCacheKey) -> bool {
        self.schema == BASELINE_CACHE_SCHEMA
            && self.component_id == key.component_id
            && self.revision == key.revision
            && self.scope == key.scope
    }

    pub fn into_evidence(self) -> BaselineEvidence {
        BaselineEvidence {
            reference: self.reference,
            revision: self.revision,
            recorded_at: self.recorded_at,
            measurement: self.measurement,
        }
    }
}

/// A directory of cached base-branch measurements.
#[derive(Debug, Clone)]
pub struct BaselineCache {
    root: PathBuf,
}

impl BaselineCache {
    /// Open a cache rooted at an explicit directory.
    ///
    /// Tests use this rather than the default root so they never depend on the
    /// ambient home directory or on any relationship between the cache and the
    /// process temp directory.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Open the cache at the default location under the Homeboy data root.
    pub fn open() -> Result<Self> {
        Ok(Self::at(default_root()?))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, key: &BaselineCacheKey) -> PathBuf {
        self.root.join(key.relative_path())
    }

    /// The cached measurement for `key`, or `None` for any reason at all.
    pub fn load(&self, key: &BaselineCacheKey) -> Option<BaselineEvidence> {
        self.load_record(key)
            .map(CachedBaselineRecord::into_evidence)
    }

    /// The cached record for `key`, retaining the storage-level fields.
    pub fn load_record(&self, key: &BaselineCacheKey) -> Option<CachedBaselineRecord> {
        let raw = std::fs::read_to_string(self.path_for(key)).ok()?;
        let record: CachedBaselineRecord = serde_json::from_str(&raw).ok()?;
        record.matches(key).then_some(record)
    }

    /// Record a measurement of the base branch.
    pub fn store(
        &self,
        key: &BaselineCacheKey,
        measurement: &TestMeasurement,
        recorded_at: impl Into<String>,
    ) -> Result<PathBuf> {
        let record = CachedBaselineRecord {
            schema: BASELINE_CACHE_SCHEMA,
            component_id: key.component_id.clone(),
            reference: key.reference.clone(),
            revision: key.revision.clone(),
            scope: key.scope.clone(),
            recorded_at: recorded_at.into(),
            measurement: measurement.clone().normalized(),
        };

        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(
                    format!(
                        "Failed to create differential baseline cache directory {}: {error}",
                        parent.display()
                    ),
                    Some("test.differential.cache.store".to_string()),
                )
            })?;
        }

        let body = serde_json::to_string_pretty(&record).map_err(|error| {
            Error::internal_io(
                format!("Failed to serialize differential baseline record: {error}"),
                Some("test.differential.cache.store".to_string()),
            )
        })?;
        std::fs::write(&path, format!("{body}\n")).map_err(|error| {
            Error::internal_io(
                format!(
                    "Failed to write differential baseline record {}: {error}",
                    path.display()
                ),
                Some("test.differential.cache.store".to_string()),
            )
        })?;

        Ok(path)
    }

    /// Drop every recorded revision for this component except `key`'s.
    ///
    /// The base branch moves constantly, so without this the cache grows one
    /// directory per observed sha forever. Returns how many revision
    /// directories were removed.
    pub fn prune_superseded(&self, key: &BaselineCacheKey) -> Result<usize> {
        let component_dir = self.root.join(key.component_dir());
        let keep = paths::sanitize_path_segment(&key.revision);

        let entries = match std::fs::read_dir(&component_dir) {
            Ok(entries) => entries,
            // Nothing recorded yet is not a failure to prune.
            Err(_) => return Ok(0),
        };

        let mut removed = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name == keep {
                continue;
            }
            std::fs::remove_dir_all(&path).map_err(|error| {
                Error::internal_io(
                    format!(
                        "Failed to prune superseded differential baseline {}: {error}",
                        path.display()
                    ),
                    Some("test.differential.cache.prune".to_string()),
                )
            })?;
            removed += 1;
        }

        Ok(removed)
    }
}

/// FNV-1a, 64-bit. Chosen because it is three lines and needs no dependency:
/// this hash only has to separate scope strings inside one directory, and it is
/// never a security or integrity boundary — the record's own fields are
/// re-checked on load.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}
