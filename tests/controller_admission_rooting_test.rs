//! Pins ambient controller-runtime admission out of production code.
//!
//! `tests/nextest_shard_parallelism_test.rs` names two conditions for restoring
//! `rust_nextest_shard_threads = 0`. This file guards the first one: the
//! controller-runtime admission store must stop being machine-global for
//! production callers.
//!
//! ## What went wrong, concretely
//!
//! Run `32294476539` failed `status_preserves_existing_terminal_runtime_evidence`
//! on an `assert_eq!(before, after)` whose sides differed in one field:
//!
//! ```text
//! before  metadata.controller_admission.owner = { request_id: "run_store-contract", .. }
//! after   metadata.controller_admission.owner = Null
//! ```
//!
//! `run_store-contract` is a different test in the same file. The mechanism is
//! not shared memory and not a shared environment — it is a shared *directory*.
//! That test builds a `HermeticTestContext`, which allocates temp roots and
//! deliberately does **not** mutate `HOME`. Production code beneath it then
//! resolved the admission lease ambiently, out of the real `$HOME`, while the
//! record it was writing lived in the hermetic root. One operation, two homes.
//!
//! Process-per-test isolation is irrelevant to that: a fresh process is not a
//! fresh disk, and nothing was reading process-global memory in the first place.
//!
//! ## What this test pins
//!
//! The ambient admission entry points in `homeboy_core::controller_runtime` are
//! `#[cfg(test)]`. Production code physically cannot call them, so a regression
//! is a build failure in the PR that introduces it rather than a red shard in
//! somebody else's.
//!
//! If an ambient admission entry point is intentionally restored to production,
//! delete or invert this test in the same commit, so the intent is recorded
//! once rather than rediscovered from a flake.

const CONTROLLER_RUNTIME_SOURCE: &str =
    include_str!("../crates/homeboy-core/src/controller_runtime.rs");

/// Entry points that resolve the admission root from process-global state.
const AMBIENT_ADMISSION_ENTRY_POINTS: &[&str] = &[
    "pub fn admit_current()",
    "pub fn admit_current_for(",
    "pub fn admit_current_for_with_cancellation_check(",
    "pub fn admission_status(",
    "pub fn cancel_admission(",
    "pub fn pin_current_queued(",
];

#[test]
fn ambient_admission_entry_points_are_test_only() {
    for entry_point in AMBIENT_ADMISSION_ENTRY_POINTS {
        let at = CONTROLLER_RUNTIME_SOURCE
            .find(entry_point)
            .unwrap_or_else(|| {
                panic!(
                "`{entry_point}` no longer exists in controller_runtime.rs. If it was renamed, \
                 update this list; if it was deleted outright, that is strictly better than the \
                 gate and this entry should be removed."
            )
            });

        let preceding = &CONTROLLER_RUNTIME_SOURCE[..at];
        assert!(
            preceding.trim_end().ends_with("#[cfg(test)]"),
            "`{entry_point}` must stay `#[cfg(test)]`. It resolves the controller-runtime \
             admission root from process-global state, which is what let one test observe \
             another's `controller_admission.owner` through the shared on-disk store (run \
             32294476539). Production callers take an explicit root via the `_in_root` / `_at` \
             siblings. See this file's module docs and #7505."
        );
    }
}

#[test]
fn rooted_admission_siblings_exist_for_every_gated_entry_point() {
    // The gate above is only tenable because a rooted form exists for each one.
    // If a sibling disappears, the gate stops being a design boundary and starts
    // being an obstacle, which is the point at which someone deletes it.
    for sibling in [
        "pub fn admit_current_for_with_cancellation_check_in_root(",
        "pub fn admission_status_at(",
        "pub fn cancel_admission_at(",
        "pub fn pin_current_queued_in_root(",
        "pub fn runtime_root_in(",
    ] {
        assert!(
            CONTROLLER_RUNTIME_SOURCE.contains(sibling),
            "rooted sibling `{sibling}` is missing; production callers have nowhere to go (#7505)"
        );
    }
}
