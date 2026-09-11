//! One resolved retention policy for every Homeboy cleanup entry point.
//!
//! Cleanup is reachable through an aggregate planner (`homeboy cleanup
//! --include <category>`) and through a set of category specialists (`homeboy
//! runs artifact cleanup-persisted`, `homeboy runs artifact cleanup-downloads`,
//! `homeboy self cleanup-runtime-tmp`, `homeboy runner workspace prune`,
//! `homeboy runner cache-prune`, `homeboy runtime controller-prune`). Every one
//! of those is a delete path, so every one of them needs the *same* answer to
//! "how old is old enough" and "how much may a single invocation touch".
//!
//! Before this module each specialist carried its own `default_value_t`
//! literal. Those literals happened to equal the shipped configuration
//! defaults, which made the drift invisible: the moment an operator widened
//! `retention.runtime_tmp_days` to 30, `homeboy self cleanup-runtime-tmp
//! --apply` still deleted at 7 days. Configured retention was silently ignored
//! by the specialist while the aggregate honored it. This module is the single
//! place those windows become concrete numbers, following the precedent
//! [`crate::controller_runtime::resolve_cleanup_options`] set for controller
//! runtimes in #10288.
//!
//! # Safety contract
//!
//! * **Fail closed.** Every conversion that can lose range resolves to the
//!   *smaller* budget, never to an unbounded one. A nonsensical limit means
//!   zero removals, not `usize::MAX` removals.
//! * **Advisory signals are never deletion proof.** This module resolves ages,
//!   counts, and byte budgets only. Ownership proof (name shape, metadata
//!   schema, parsed identifiers, process liveness) stays with each category,
//!   which is the only place that can prove it. A failed size or liveness
//!   measurement must change a verdict in neither direction; see
//!   [`crate::observation::runs_service::orphaned_artifact_bytes`] for the
//!   worked example.
//! * **Windows widen only by explicit operator input.** `None` in
//!   [`CleanupPolicyOverrides`] means "use the configured value"; it never
//!   means "use whatever this call site felt like".

use std::time::Duration;

use serde::Serialize;

use crate::defaults::{self, RetentionConfig};
use crate::{Error, Result};

const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_HOUR: u64 = 3_600;

/// Age floor before an inactive runner-side Lab workspace or a managed runner
/// binary slot is eligible for removal.
///
/// This is a fixed floor rather than a configuration key on purpose. Both
/// resources live on a *remote* host whose clock, job table, and in-flight
/// uploads the controller cannot fully observe, so the floor exists to cover
/// the window in which a resource looks orphaned but is still being written.
/// Lowering it is a widening of a delete predicate, so it stays an explicit
/// per-invocation operator argument (`--min-age-hours`) instead of a
/// configuration default that would silently apply to every future sweep.
pub const RUNNER_MIN_AGE_HOURS: u64 = 24;

/// Workspaces inspected per runner per aggregate pass.
///
/// This is a *page size*, not a delete budget, which is why the aggregate
/// `--limit` (a record budget for row-driven categories) is deliberately not
/// wired to it. Remote workspace pruning is paginated with a cursor and repeated
/// passes; bounding the page keeps a single SSH round trip below ARG_MAX and
/// below the page wall-clock budget.
pub const RUNNER_WORKSPACE_PAGE_LIMIT: usize = 25;

/// Pagination passes an aggregate apply performs per runner.
pub const RUNNER_WORKSPACE_APPLY_PASSES: usize = 10;

/// Pagination passes an aggregate dry run performs per runner. A preview only
/// needs enough evidence to describe the plan.
pub const RUNNER_WORKSPACE_DRY_RUN_PASSES: usize = 1;

/// Age floor before an abandoned isolated test home is eligible for removal.
///
/// A fixed floor rather than a configuration key, for the same reason as
/// [`RUNNER_MIN_AGE_HOURS`]: the category's real ownership proof is process
/// liveness, and the floor exists only to cover the window in which a home has
/// been created but its owner is not yet observable. That window is a property
/// of process startup, not of an operator's retention taste, and lowering it
/// would widen a delete predicate for every future sweep at once.
///
/// One hour is far above any such window and far below the multi-day retention
/// that let 9.2 GB accumulate unreclaimed (#11073).
pub const LEAKED_TEST_HOME_MIN_AGE_HOURS: u64 = 1;

/// Ceiling on retained abandoned isolated test homes, in bytes.
///
/// A day count cannot bound this directory. Each abandoned home can carry a
/// private copy of a debug binary — hundreds of megabytes — and they arrive at
/// whatever rate the host kills test processes, so any age window is a promise
/// to fill the disk before the window closes. `runtime_run_max_bytes` already
/// set the precedent that high-churn reconstructable storage is bounded by
/// bytes; this is the same shape for the same reason.
///
/// Crossing it relaxes the age floor for the oldest *abandoned* entries only.
/// It never relaxes the liveness proof.
pub const LEAKED_TEST_HOME_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Age floor before a published release artifact directory is eligible.
///
/// A fixed floor for the same reason as [`RUNNER_MIN_AGE_HOURS`] and
/// [`LEAKED_TEST_HOME_MIN_AGE_HOURS`]: it covers the window in which a release
/// is mid-publication and its durable directory is still being written. That
/// window is a property of how long a publish takes, not of an operator's
/// retention taste.
///
/// Re-exported from [`crate::cleanup::release_artifacts`] so the manifest and
/// the category cannot name two different floors.
pub use crate::cleanup::release_artifacts::RELEASE_ARTIFACT_MIN_AGE_HOURS;

/// Stable schema identifier for the serialized policy snapshot.
pub const CLEANUP_POLICY_SCHEMA: &str = "homeboy/retention-manifest/v1";

/// Overrides an operator typed on one invocation.
///
/// `None` means "use the configured value". Call sites forward only what an
/// operator actually passed; they never substitute a literal of their own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupPolicyOverrides {
    /// Operator `--older-than-days` for terminal runs and their persisted
    /// artifacts.
    pub terminal_run_days: Option<i64>,
    /// Operator `--limit`: maximum records inspected by one invocation.
    pub limit: Option<i64>,
    /// Operator `--older-than-days` for runtime temp entries.
    pub runtime_tmp_days: Option<u64>,
    /// Operator age override for metadata-backed runtime temp entries only.
    pub runtime_tmp_managed_days: Option<u64>,
    /// Operator `--run-max-bytes` for retained failed runtime-run evidence.
    pub runtime_run_max_bytes: Option<u64>,
    /// Operator `--run-max-count` for retained failed runtime-run directories.
    pub runtime_run_max_count: Option<usize>,
    /// Operator `--release-max-count` for retained release versions per repo.
    pub release_artifact_max_count: Option<usize>,
    /// Operator `--release-max-bytes` for retained release bytes per repo.
    pub release_artifact_max_bytes: Option<u64>,
}

/// The effective retention policy for one cleanup invocation.
///
/// This type is both the resolved policy *and* the manifest reported in
/// cleanup output, so a report can never describe a window the deletion did not
/// apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CleanupPolicy {
    pub schema: &'static str,
    pub terminal_run_days: i64,
    pub runtime_tmp_days: u64,
    pub runtime_tmp_managed_days: u64,
    pub runtime_run_max_bytes: u64,
    pub runtime_run_max_count: usize,
    pub shared_store_days: u64,
    pub shared_store_max_bytes: u64,
    pub shared_store_lease_seconds: u64,
    pub shared_store_reserve_bytes: u64,
    pub shared_store_reserve_inodes: u64,
    pub controller_runtime_days: u64,
    pub controller_runtime_max_bytes: u64,
    pub release_artifact_max_count: usize,
    pub release_artifact_max_bytes: u64,
    pub release_artifact_min_age_hours: u64,
    pub runner_min_age_hours: u64,
    pub runner_workspace_page_limit: usize,
    pub leaked_test_home_min_age_hours: u64,
    pub leaked_test_home_max_total_bytes: u64,
    pub limit: i64,
    pub terminal_run_guard: bool,
}

impl CleanupPolicy {
    /// Record budget for one invocation, as a `usize`.
    ///
    /// Fails closed: a limit that cannot be represented resolves to zero
    /// inspected records, never to `usize::MAX`. Widening a delete budget
    /// because a conversion failed is exactly the fail-open shape cleanup must
    /// not have. [`crate::controller_runtime`] has always converted this way;
    /// the CLI aggregate used to widen to `usize::MAX` instead.
    #[must_use]
    pub fn scan_limit(self) -> usize {
        usize::try_from(self.limit).unwrap_or(0)
    }

    /// Age floor for runtime temp entries.
    #[must_use]
    pub fn runtime_tmp_min_age(self) -> Duration {
        Duration::from_secs(self.runtime_tmp_days.saturating_mul(SECONDS_PER_DAY))
    }

    /// Age floor for shared Cargo target stores.
    #[must_use]
    pub fn shared_store_min_age(self) -> Duration {
        Duration::from_secs(self.shared_store_days.saturating_mul(SECONDS_PER_DAY))
    }

    /// Lease TTL that keeps an in-use shared Cargo target store alive.
    #[must_use]
    pub fn shared_store_lease_ttl(self) -> Duration {
        Duration::from_secs(self.shared_store_lease_seconds)
    }

    /// Age floor for unreferenced controller runtime identities.
    #[must_use]
    pub fn controller_runtime_min_age(self) -> Duration {
        Duration::from_secs(self.controller_runtime_days.saturating_mul(SECONDS_PER_DAY))
    }

    /// Age floor covering an in-flight release publication.
    #[must_use]
    pub fn release_artifact_min_age(self) -> Duration {
        Duration::from_secs(
            self.release_artifact_min_age_hours
                .saturating_mul(SECONDS_PER_HOUR),
        )
    }

    /// Age floor for runner-side workspaces and managed binary slots.
    #[must_use]
    pub fn runner_min_age(self) -> Duration {
        Duration::from_secs(self.runner_min_age_hours.saturating_mul(SECONDS_PER_HOUR))
    }

    /// Age floor for abandoned isolated test homes.
    #[must_use]
    pub fn leaked_test_home_min_age(self) -> Duration {
        Duration::from_secs(
            self.leaked_test_home_min_age_hours
                .saturating_mul(SECONDS_PER_HOUR),
        )
    }

    /// Pagination passes for one remote workspace prune invocation.
    #[must_use]
    pub fn runner_workspace_passes(apply: bool) -> usize {
        if apply {
            RUNNER_WORKSPACE_APPLY_PASSES
        } else {
            RUNNER_WORKSPACE_DRY_RUN_PASSES
        }
    }
}

/// Resolve the effective cleanup policy from persisted configuration plus the
/// overrides an operator typed.
///
/// # Errors
///
/// Returns a validation error when the resolved window or budget is
/// nonsensical. Resolution validates the *effective* value, so a corrupt
/// configuration file is rejected on every entry point rather than only on the
/// ones that happened to re-check their own arguments.
pub fn resolve_cleanup_policy(overrides: CleanupPolicyOverrides) -> Result<CleanupPolicy> {
    cleanup_policy_from_retention(&defaults::load_config().retention, overrides)
}

/// Resolve a cleanup policy against an explicit retention configuration.
///
/// # Errors
///
/// See [`resolve_cleanup_policy`].
pub fn cleanup_policy_from_retention(
    retention: &RetentionConfig,
    overrides: CleanupPolicyOverrides,
) -> Result<CleanupPolicy> {
    let terminal_run_days = overrides
        .terminal_run_days
        .unwrap_or(retention.terminal_run_days);
    let limit = overrides.limit.unwrap_or(retention.limit);
    if terminal_run_days < 0 {
        return Err(Error::validation_invalid_argument(
            "retention",
            "--older-than-days must be zero or greater",
            None,
            None,
        ));
    }
    if limit < 1 {
        return Err(Error::validation_invalid_argument(
            "retention",
            "--limit must be positive",
            None,
            None,
        ));
    }
    let runtime_tmp_days = overrides
        .runtime_tmp_days
        .unwrap_or(retention.runtime_tmp_days);
    Ok(CleanupPolicy {
        schema: CLEANUP_POLICY_SCHEMA,
        terminal_run_days,
        runtime_tmp_days,
        runtime_tmp_managed_days: overrides
            .runtime_tmp_managed_days
            .unwrap_or(runtime_tmp_days),
        runtime_run_max_bytes: overrides
            .runtime_run_max_bytes
            .unwrap_or(retention.runtime_run_max_bytes),
        runtime_run_max_count: overrides
            .runtime_run_max_count
            .unwrap_or(retention.runtime_run_max_count),
        shared_store_days: retention.shared_store_days,
        shared_store_max_bytes: retention.shared_store_max_bytes,
        shared_store_lease_seconds: retention.shared_store_lease_seconds,
        shared_store_reserve_bytes: retention.shared_store_reserve_bytes,
        shared_store_reserve_inodes: retention.shared_store_reserve_inodes,
        controller_runtime_days: retention.controller_runtime_days,
        controller_runtime_max_bytes: retention.controller_runtime_max_bytes,
        release_artifact_max_count: overrides
            .release_artifact_max_count
            .unwrap_or(retention.release_artifact_max_count),
        release_artifact_max_bytes: overrides
            .release_artifact_max_bytes
            .unwrap_or(retention.release_artifact_max_bytes),
        release_artifact_min_age_hours: RELEASE_ARTIFACT_MIN_AGE_HOURS,
        runner_min_age_hours: RUNNER_MIN_AGE_HOURS,
        runner_workspace_page_limit: RUNNER_WORKSPACE_PAGE_LIMIT,
        leaked_test_home_min_age_hours: LEAKED_TEST_HOME_MIN_AGE_HOURS,
        leaked_test_home_max_total_bytes: LEAKED_TEST_HOME_MAX_TOTAL_BYTES,
        limit,
        terminal_run_guard: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retention() -> RetentionConfig {
        RetentionConfig::default()
    }

    #[test]
    fn unset_overrides_resolve_to_configured_retention() {
        let configured = RetentionConfig {
            terminal_run_days: 90,
            runtime_tmp_days: 30,
            runtime_run_max_count: 5,
            limit: 7,
            ..retention()
        };

        let policy = cleanup_policy_from_retention(&configured, CleanupPolicyOverrides::default())
            .expect("resolve policy");

        // The regression this guards: specialists used to carry their own
        // `default_value_t` literals, so a widened configuration window was
        // honored by the aggregate and silently ignored by the specialist.
        assert_eq!(policy.terminal_run_days, 90);
        assert_eq!(policy.runtime_tmp_days, 30);
        assert_eq!(policy.runtime_tmp_managed_days, 30);
        assert_eq!(policy.runtime_run_max_count, 5);
        assert_eq!(policy.limit, 7);
    }

    #[test]
    fn managed_runtime_tmp_override_preserves_unmanaged_floor() {
        let configured = RetentionConfig {
            runtime_tmp_days: 30,
            ..retention()
        };

        let policy = cleanup_policy_from_retention(
            &configured,
            CleanupPolicyOverrides {
                runtime_tmp_managed_days: Some(0),
                ..CleanupPolicyOverrides::default()
            },
        )
        .expect("resolve policy");

        assert_eq!(policy.runtime_tmp_days, 30);
        assert_eq!(policy.runtime_tmp_managed_days, 0);
    }

    #[test]
    fn typed_overrides_take_precedence_over_configuration() {
        let configured = RetentionConfig {
            terminal_run_days: 90,
            runtime_tmp_days: 30,
            limit: 7,
            ..retention()
        };

        let policy = cleanup_policy_from_retention(
            &configured,
            CleanupPolicyOverrides {
                terminal_run_days: Some(1),
                limit: Some(2),
                runtime_tmp_days: Some(3),
                ..CleanupPolicyOverrides::default()
            },
        )
        .expect("resolve policy");

        assert_eq!(policy.terminal_run_days, 1);
        assert_eq!(policy.limit, 2);
        assert_eq!(policy.runtime_tmp_days, 3);
    }

    #[test]
    fn negative_window_is_rejected_on_every_entry_point() {
        let configured = RetentionConfig {
            terminal_run_days: -1,
            ..retention()
        };

        // A corrupt configuration is rejected even when the operator typed
        // nothing, so a specialist cannot inherit a nonsensical window that the
        // aggregate would have refused.
        assert!(
            cleanup_policy_from_retention(&configured, CleanupPolicyOverrides::default()).is_err()
        );
        assert!(cleanup_policy_from_retention(
            &retention(),
            CleanupPolicyOverrides {
                terminal_run_days: Some(-1),
                ..CleanupPolicyOverrides::default()
            },
        )
        .is_err());
    }

    #[test]
    fn nonpositive_limit_is_rejected() {
        assert!(cleanup_policy_from_retention(
            &RetentionConfig {
                limit: 0,
                ..retention()
            },
            CleanupPolicyOverrides::default(),
        )
        .is_err());
        assert!(cleanup_policy_from_retention(
            &retention(),
            CleanupPolicyOverrides {
                limit: Some(0),
                ..CleanupPolicyOverrides::default()
            },
        )
        .is_err());
    }

    #[test]
    fn zero_day_window_stays_expressible() {
        // `--older-than-days 0` is a supported operator request (reclaim
        // everything terminal now), so validation rejects only negatives.
        let policy = cleanup_policy_from_retention(
            &retention(),
            CleanupPolicyOverrides {
                terminal_run_days: Some(0),
                ..CleanupPolicyOverrides::default()
            },
        )
        .expect("resolve policy");
        assert_eq!(policy.terminal_run_days, 0);
    }

    #[test]
    fn scan_limit_fails_closed_instead_of_widening() {
        let policy = cleanup_policy_from_retention(
            &retention(),
            CleanupPolicyOverrides {
                limit: Some(i64::MAX),
                ..CleanupPolicyOverrides::default()
            },
        )
        .expect("resolve policy");

        // On every target where `i64::MAX` fits a `usize` this is the identity.
        // Where it does not, the budget collapses to zero removals rather than
        // widening to an unbounded delete.
        let expected = usize::try_from(i64::MAX).unwrap_or(0);
        assert_eq!(policy.scan_limit(), expected);
    }

    #[test]
    fn runner_age_floor_is_one_named_value() {
        let policy = cleanup_policy_from_retention(&retention(), CleanupPolicyOverrides::default())
            .expect("resolve policy");
        assert_eq!(policy.runner_min_age_hours, RUNNER_MIN_AGE_HOURS);
        assert_eq!(
            policy.runner_min_age(),
            Duration::from_secs(RUNNER_MIN_AGE_HOURS * SECONDS_PER_HOUR)
        );
    }

    #[test]
    fn derived_windows_match_configured_days() {
        let configured = RetentionConfig {
            runtime_tmp_days: 3,
            shared_store_days: 4,
            controller_runtime_days: 5,
            shared_store_lease_seconds: 61,
            ..retention()
        };
        let policy = cleanup_policy_from_retention(&configured, CleanupPolicyOverrides::default())
            .expect("resolve policy");

        assert_eq!(
            policy.runtime_tmp_min_age(),
            Duration::from_secs(3 * SECONDS_PER_DAY)
        );
        assert_eq!(
            policy.shared_store_min_age(),
            Duration::from_secs(4 * SECONDS_PER_DAY)
        );
        assert_eq!(
            policy.controller_runtime_min_age(),
            Duration::from_secs(5 * SECONDS_PER_DAY)
        );
        assert_eq!(policy.shared_store_lease_ttl(), Duration::from_secs(61));
    }

    /// The leaked-test-home floor is a fixed, named value like the runner floor,
    /// and its byte ceiling is finite. An infinite ceiling would silently
    /// restore the age-only bound that #11073 showed cannot keep up.
    #[test]
    fn the_leaked_test_home_bound_is_both_an_age_floor_and_a_finite_byte_ceiling() {
        let policy = cleanup_policy_from_retention(&retention(), CleanupPolicyOverrides::default())
            .expect("resolve policy");

        assert_eq!(
            policy.leaked_test_home_min_age_hours,
            LEAKED_TEST_HOME_MIN_AGE_HOURS
        );
        assert_eq!(
            policy.leaked_test_home_min_age(),
            Duration::from_secs(LEAKED_TEST_HOME_MIN_AGE_HOURS * SECONDS_PER_HOUR)
        );
        assert!(policy.leaked_test_home_min_age_hours > 0);
        assert_eq!(
            policy.leaked_test_home_max_total_bytes,
            LEAKED_TEST_HOME_MAX_TOTAL_BYTES
        );
        assert!(policy.leaked_test_home_max_total_bytes < u64::MAX);
    }

    /// #14223: the release artifact store had no count, byte, or age bound of
    /// any kind, and grew to 6.1 GB. Both budgets must be finite by default and
    /// both must be resolvable from configuration.
    #[test]
    fn the_release_artifact_bound_is_a_finite_count_and_a_finite_byte_ceiling() {
        let policy = cleanup_policy_from_retention(&retention(), CleanupPolicyOverrides::default())
            .expect("resolve policy");

        assert!(policy.release_artifact_max_count < usize::MAX);
        assert!(policy.release_artifact_max_bytes < u64::MAX);
        assert_eq!(
            policy.release_artifact_min_age_hours,
            RELEASE_ARTIFACT_MIN_AGE_HOURS
        );
        assert_eq!(
            policy.release_artifact_min_age(),
            Duration::from_secs(RELEASE_ARTIFACT_MIN_AGE_HOURS * SECONDS_PER_HOUR)
        );

        // The regression this module exists to prevent: a widened configuration
        // window honored by one entry point and ignored by another.
        let configured = RetentionConfig {
            release_artifact_max_count: 3,
            release_artifact_max_bytes: 4096,
            ..retention()
        };
        let policy = cleanup_policy_from_retention(&configured, CleanupPolicyOverrides::default())
            .expect("resolve policy");
        assert_eq!(policy.release_artifact_max_count, 3);
        assert_eq!(policy.release_artifact_max_bytes, 4096);

        let policy = cleanup_policy_from_retention(
            &configured,
            CleanupPolicyOverrides {
                release_artifact_max_count: Some(1),
                release_artifact_max_bytes: Some(2048),
                ..CleanupPolicyOverrides::default()
            },
        )
        .expect("resolve policy");
        assert_eq!(policy.release_artifact_max_count, 1);
        assert_eq!(policy.release_artifact_max_bytes, 2048);
    }

    #[test]
    fn workspace_passes_depend_only_on_mutation_intent() {
        assert_eq!(
            CleanupPolicy::runner_workspace_passes(true),
            RUNNER_WORKSPACE_APPLY_PASSES
        );
        assert_eq!(
            CleanupPolicy::runner_workspace_passes(false),
            RUNNER_WORKSPACE_DRY_RUN_PASSES
        );
    }
}
