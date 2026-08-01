//! Cleanup that still runs when the observation store cannot be opened.
//!
//! # The deadlock
//!
//! A workspace mount reached ZERO free inodes with ~34 GB still free. Every
//! byte-only probe read `ok`, every write failed with a hard `ENOSPC`, and the
//! recovery tool could not start: `ObservationStore::open_initialized` is a
//! `create_dir_all`, a connection open that needs an inode for the WAL journal,
//! the migration ladder, and an unfinished-publication reconciliation pass. At
//! zero free inodes **the only tool that can free space cannot start** (#10603).
//!
//! #10603's fix added inode accounting to
//! [`crate::observation::disk_budget`]. That measurement is correct, and it is
//! wired to nothing: its only two call sites are report-only. A measurement
//! that is never consulted by a decision does not change an outcome (#11127).
//!
//! # Which categories survive a closed database
//!
//! Three cleanup categories prove ownership by *name shape* rather than by
//! joining against the `artifacts` table. Their docstrings are the primary
//! source; what matters here is how each behaves when the store is gone, and
//! the three are **not** equivalent:
//!
//! * [`crate::observation::runs_service::cleanup_orphaned_artifact_bytes`] —
//!   genuinely store-free. Its removal decision is a pure function of (name
//!   shape, entry type, age, symlink-freedom, containment), and it documents
//!   that "the database is deliberately not consulted at all" because the paths
//!   it reaps never get a row. This is the category that actually reclaims
//!   bytes in a degraded sweep.
//! * `crate::engine::temp::cleanup_runtime_tmp` — store-free *in effect*. It
//!   opens the store in one place, `inspection_owner_protection`, to check
//!   whether a named run is still `running`, and that check fails **open**
//!   (`.ok().flatten().is_some_and(...)` — an unopenable store yields `false`,
//!   so no protection is applied and the entry stays removable). Its real
//!   liveness signals — the pin file's owner PID and the invocation lease — are
//!   filesystem-local and unaffected.
//! * [`crate::observation::runs_service::cleanup_runner_downloads`] — store
//!   *tolerant*, not store-independent. Its liveness veto fails **closed** by
//!   design: `LivenessVeto::read` returning `running: None` vetoes every
//!   candidate, because these bytes are the operator's copy of something they
//!   asked Homeboy to fetch and a wrong delete is not reversible. So it runs
//!   safely with the database closed and reclaims exactly nothing. It is
//!   reported here for visibility, flagged
//!   [`DegradedCleanupCategory::reclaims_without_store`] `= false`, rather than
//!   quietly omitted or dishonestly counted.
//!
//! # Why this is not just "let each category fail"
//!
//! The aggregate already isolates a failing category, so a degraded sweep was
//! never a hard error. But every database-backed category independently
//! attempts its own `open_initialized`, and each attempt is another
//! `create_dir_all` plus journal-file creation against a filesystem that has
//! nothing left to give. Probing once and gating on the answer replaces N
//! failing writes with one, and turns N generic failures into one named
//! degraded mode the operator can act on.

use serde::Serialize;
use serde_json::{json, Value};

use crate::engine::temp::{self, RuntimeTempCleanupOptions};
use crate::observation::runs_service::{
    self, OrphanedArtifactBytesCleanupOptions, RunnerDownloadCleanupOptions,
};
use crate::Error;

/// Cleanup categories that can plan and apply with the observation store shut.
///
/// Ordered by how much they can actually reclaim in that state, so an operator
/// reading the list top-down reaches the useful ones first.
pub const STORE_INDEPENDENT_CLEANUP_CATEGORIES: &[&str] =
    &["orphaned-artifact-bytes", "runtime-tmp", "runner-downloads"];

/// Whether the observation store can be opened at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StoreAvailability {
    Available,
    Unavailable {
        /// The failing error's code, so a consumer can branch without parsing
        /// prose.
        code: String,
        reason: String,
        /// True when the open failed specifically because the filesystem has
        /// no capacity. This is the state that justifies degrading rather than
        /// reporting a plain failure.
        storage_exhausted: bool,
    },
}

impl StoreAvailability {
    pub fn is_available(&self) -> bool {
        matches!(self, StoreAvailability::Available)
    }

    /// True only when the store is unavailable *and* the cause is exhausted
    /// storage.
    ///
    /// Deliberately narrower than `!is_available()`: a corrupt database, a
    /// permissions problem, or a migration failure must not silently reroute an
    /// operator's `--apply` into a partial sweep.
    pub fn is_storage_exhausted(&self) -> bool {
        matches!(
            self,
            StoreAvailability::Unavailable {
                storage_exhausted: true,
                ..
            }
        )
    }

    /// Operator-facing explanation for a category skipped in degraded mode.
    pub fn skip_reason(&self) -> Option<String> {
        match self {
            StoreAvailability::Available => None,
            StoreAvailability::Unavailable { reason, .. } => Some(format!(
                "observation store unavailable, so database-backed cleanup cannot plan: {reason}. \
                 Store-independent categories still run: {}.",
                STORE_INDEPENDENT_CLEANUP_CATEGORIES.join(", ")
            )),
        }
    }

    /// Classify an observation-store open failure.
    pub fn from_open_error(error: &Error) -> Self {
        StoreAvailability::Unavailable {
            code: error.code.as_str().to_string(),
            reason: error.message.clone(),
            storage_exhausted: error.is_storage_exhausted(),
        }
    }
}

/// Probe the observation store once, for a whole sweep.
///
/// This is an open attempt rather than a capacity inference on purpose: the
/// question a caller needs answered is "can the database be used", and the only
/// honest answer comes from trying. Capacity is one reason it can fail; a
/// corrupt file or a failed migration are others, and they must not be
/// misreported as a full disk.
pub fn observation_store_availability() -> StoreAvailability {
    match crate::observation::ObservationStore::open_initialized() {
        Ok(_) => StoreAvailability::Available,
        Err(error) => StoreAvailability::from_open_error(&error),
    }
}

#[derive(Debug, Clone)]
pub struct DegradedCleanupOptions {
    pub apply: bool,
    /// Maximum candidate entries inspected per category. Callers resolve this
    /// from [`super::CleanupPolicy::scan_limit`], which fails closed to zero.
    pub limit: usize,
    pub runtime_tmp_days: u64,
    pub runtime_tmp_managed_days: Option<u64>,
}

impl DegradedCleanupOptions {
    /// Degraded options resolved from the configured retention policy.
    pub fn from_policy(policy: &super::CleanupPolicy, apply: bool) -> Self {
        Self {
            apply,
            limit: policy.scan_limit(),
            runtime_tmp_days: policy.runtime_tmp_days,
            runtime_tmp_managed_days: Some(policy.runtime_tmp_managed_days),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DegradedCleanupCategory {
    pub category: &'static str,
    /// Whether this category can reclaim anything with the store closed.
    ///
    /// `false` for a category that runs safely but retains everything, so a
    /// zero here reads as "structurally cannot", not "found nothing".
    pub reclaims_without_store: bool,
    pub planned_bytes: u64,
    pub reclaimed_bytes: u64,
    pub planned_count: usize,
    pub removed_count: usize,
    /// The category's own outcome, or its error details when it failed.
    pub detail: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<Value>,
}

impl DegradedCleanupCategory {
    fn failed(category: &'static str, reclaims_without_store: bool, error: Error) -> Self {
        Self {
            category,
            reclaims_without_store,
            planned_bytes: 0,
            reclaimed_bytes: 0,
            planned_count: 0,
            removed_count: 0,
            detail: error.details.clone(),
            failure: Some(json!({
                "code": error.code.as_str(),
                "message": error.message,
            })),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DegradedCleanupOutcome {
    pub command: &'static str,
    pub dry_run: bool,
    /// Why the degraded path was taken.
    pub reason: String,
    pub store: StoreAvailability,
    pub planned_bytes: u64,
    pub reclaimed_bytes: u64,
    pub categories: Vec<DegradedCleanupCategory>,
    /// Categories deliberately not attempted because they cannot plan without
    /// the database.
    pub skipped_database_backed: bool,
    pub next_command: &'static str,
}

/// Run every store-independent cleanup category with the database closed.
///
/// Never returns `Err`. A degraded sweep is the last recovery path available,
/// so one failing category must not take the others down with it — the whole
/// point is to reclaim whatever can still be reclaimed.
pub fn degraded_cleanup(
    options: DegradedCleanupOptions,
    store: StoreAvailability,
    reason: impl Into<String>,
) -> DegradedCleanupOutcome {
    let mut categories = Vec::new();

    // The only category that genuinely reclaims with the store shut. It never
    // consults the database in either direction.
    categories.push(
        match runs_service::cleanup_orphaned_artifact_bytes(OrphanedArtifactBytesCleanupOptions {
            apply: options.apply,
            limit: options.limit,
        }) {
            Ok(outcome) => DegradedCleanupCategory {
                category: "orphaned-artifact-bytes",
                reclaims_without_store: true,
                planned_bytes: outcome.planned_size_bytes,
                reclaimed_bytes: outcome.removed_size_bytes,
                planned_count: outcome.planned_count,
                removed_count: outcome.removed_count,
                detail: serde_json::to_value(&outcome).unwrap_or(Value::Null),
                failure: None,
            },
            Err(error) => DegradedCleanupCategory::failed("orphaned-artifact-bytes", true, error),
        },
    );

    // Its store read is an advisory protection that fails open, and its real
    // liveness signals (owner PID, invocation lease) are filesystem-local.
    categories.push(
        match temp::cleanup_runtime_tmp_bounded(RuntimeTempCleanupOptions {
            apply: options.apply,
            older_than_days: options.runtime_tmp_days,
            managed_older_than_days: options.runtime_tmp_managed_days,
            prefix: None,
            limit: options.limit,
            run_max_bytes: u64::MAX,
            run_max_count: usize::MAX,
            cursor: None,
        }) {
            Ok(outcome) => DegradedCleanupCategory {
                category: "runtime-tmp",
                reclaims_without_store: true,
                planned_bytes: outcome.totals.planned_size_bytes,
                reclaimed_bytes: outcome.totals.removed_size_bytes,
                planned_count: outcome.planned_count,
                removed_count: outcome.removed_count,
                detail: serde_json::to_value(&outcome).unwrap_or(Value::Null),
                failure: None,
            },
            Err(error) => DegradedCleanupCategory::failed("runtime-tmp", true, error),
        },
    );

    // Runs safely, reclaims nothing: its liveness veto fails closed, so a
    // closed store retains every candidate. Reported so the retained bytes stay
    // visible instead of vanishing from the degraded report entirely.
    categories.push(
        match runs_service::cleanup_runner_downloads(RunnerDownloadCleanupOptions {
            apply: options.apply,
            runner: None,
            run_id: None,
            limit: options.limit,
            store_available: store.is_available(),
        }) {
            Ok(outcome) => DegradedCleanupCategory {
                category: "runner-downloads",
                reclaims_without_store: false,
                planned_bytes: outcome.planned_size_bytes,
                reclaimed_bytes: outcome.removed_size_bytes,
                planned_count: outcome.planned_count,
                removed_count: outcome.removed_count,
                detail: serde_json::to_value(&outcome).unwrap_or(Value::Null),
                failure: None,
            },
            Err(error) => DegradedCleanupCategory::failed("runner-downloads", false, error),
        },
    );

    DegradedCleanupOutcome {
        command: "cleanup.degraded",
        dry_run: !options.apply,
        reason: reason.into(),
        planned_bytes: categories.iter().map(|entry| entry.planned_bytes).sum(),
        reclaimed_bytes: categories.iter().map(|entry| entry.reclaimed_bytes).sum(),
        skipped_database_backed: !store.is_available(),
        store,
        categories,
        next_command: "homeboy cleanup --apply",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exhausted() -> StoreAvailability {
        StoreAvailability::from_open_error(&Error::storage_exhausted(
            "No space left on device (os error 28)",
            Some("open observation store".to_string()),
        ))
    }

    /// The deadlock signal. An open that failed for lack of capacity is the one
    /// condition that justifies rerouting to a partial, store-free sweep.
    #[test]
    fn an_open_that_failed_for_capacity_is_reported_as_storage_exhausted() {
        let availability = exhausted();

        assert!(!availability.is_available());
        assert!(availability.is_storage_exhausted());
        assert_eq!(
            availability,
            StoreAvailability::Unavailable {
                code: "storage.exhausted".to_string(),
                reason: "Filesystem capacity exhausted".to_string(),
                storage_exhausted: true,
            }
        );
    }

    /// A corrupt database or a failed migration must not be laundered into
    /// "the disk is full" — that would silently downgrade an operator's
    /// `--apply` into a partial sweep for an unrelated fault.
    #[test]
    fn a_non_capacity_open_failure_is_not_treated_as_exhaustion() {
        let availability = StoreAvailability::from_open_error(&Error::internal_unexpected(
            "SQLite observation store error: apply migration 12: disk image is malformed",
        ));

        assert!(!availability.is_available());
        assert!(!availability.is_storage_exhausted());
    }

    /// Pointing a zero-inode operator at plain `homeboy cleanup --apply` is the
    /// loop #10603 could not escape. The skip reason has to name the categories
    /// that still work.
    #[test]
    fn the_skip_reason_names_the_categories_that_still_run() {
        let reason = exhausted().skip_reason().expect("degraded skip reason");

        assert!(reason.contains("orphaned-artifact-bytes"), "{reason}");
        assert!(reason.contains("runtime-tmp"), "{reason}");
        assert!(
            reason.contains("database-backed cleanup cannot plan"),
            "{reason}"
        );
    }

    #[test]
    fn an_available_store_has_no_skip_reason() {
        assert!(StoreAvailability::Available.skip_reason().is_none());
        assert!(StoreAvailability::Available.is_available());
        assert!(!StoreAvailability::Available.is_storage_exhausted());
    }

    /// The load-bearing claim: a degraded sweep completes **without ever
    /// opening the observation store**.
    ///
    /// Proven directly rather than by inspection — the database file must not
    /// exist afterwards. Opening it is a `create_dir_all` plus a connection
    /// open that needs an inode for the WAL journal, which is precisely what
    /// cannot happen at zero free inodes. If some category quietly opened one,
    /// this fails.
    #[test]
    fn a_degraded_sweep_plans_every_category_without_opening_the_store() {
        crate::test_support::with_isolated_home(|_| {
            let database = crate::observation::store::database_path().expect("database path");
            assert!(!database.exists(), "fixture must start with no database");

            let outcome = degraded_cleanup(
                DegradedCleanupOptions {
                    apply: false,
                    limit: 100,
                    runtime_tmp_days: 7,
                    runtime_tmp_managed_days: None,
                },
                exhausted(),
                "test",
            );

            assert!(
                !database.exists(),
                "a degraded sweep must not create the observation store it cannot open"
            );
            assert!(outcome.skipped_database_backed);
            assert!(outcome.dry_run);
            let categories: Vec<_> = outcome
                .categories
                .iter()
                .map(|entry| entry.category)
                .collect();
            assert_eq!(categories, STORE_INDEPENDENT_CLEANUP_CATEGORIES);
            for entry in &outcome.categories {
                assert!(
                    entry.failure.is_none(),
                    "{} failed in degraded mode: {:?}",
                    entry.category,
                    entry.failure
                );
            }
        });
    }

    /// `runner-downloads` runs with the store shut but its liveness veto fails
    /// closed, so it reclaims nothing. Reporting its zero exactly like the
    /// others would misread a structural limit as an empty result, so the
    /// distinction is carried in the payload rather than left to a reader.
    #[test]
    fn only_the_categories_that_can_reclaim_without_the_store_claim_to() {
        crate::test_support::with_isolated_home(|_| {
            let outcome = degraded_cleanup(
                DegradedCleanupOptions {
                    apply: false,
                    limit: 100,
                    runtime_tmp_days: 7,
                    runtime_tmp_managed_days: None,
                },
                exhausted(),
                "test",
            );

            let reclaiming: Vec<_> = outcome
                .categories
                .iter()
                .filter(|entry| entry.reclaims_without_store)
                .map(|entry| entry.category)
                .collect();
            assert_eq!(reclaiming, ["orphaned-artifact-bytes", "runtime-tmp"]);
        });
    }

    /// A failing category must not take the rest of the sweep down with it.
    /// A degraded pass is the last recovery path available.
    #[test]
    fn a_failed_category_is_reported_without_losing_its_error_code() {
        let failed =
            DegradedCleanupCategory::failed("orphaned-artifact-bytes", true, exhausted_error());

        let failure = failed.failure.expect("failure detail");
        assert_eq!(failure["code"], "storage.exhausted");
        assert_eq!(failed.planned_bytes, 0);
        assert_eq!(failed.reclaimed_bytes, 0);
    }

    /// The list is a contract, not a comment: the CLI gate and the degraded
    /// fallback both render it to the operator.
    #[test]
    fn the_store_independent_category_list_matches_the_categories_that_run() {
        assert_eq!(
            STORE_INDEPENDENT_CLEANUP_CATEGORIES,
            ["orphaned-artifact-bytes", "runtime-tmp", "runner-downloads"]
        );
    }

    fn exhausted_error() -> Error {
        Error::storage_exhausted("No space left on device (os error 28)", None)
    }
}
