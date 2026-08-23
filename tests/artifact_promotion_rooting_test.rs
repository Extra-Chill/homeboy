//! Pins directory-child promotion's idempotence guard onto the store it guards.
//!
//! ## What went wrong, concretely
//!
//! `promote_runner_exec_artifact_dirs` opened one store for its own artifact
//! writes, and then made three lifecycle calls that each opened another:
//!
//! ```text
//! let store = ObservationStore::open_initialized()?;        // store A
//! checkpoint_runner_exec_directory_tree(..)                 // store B
//! for child in children {
//!     if runner_exec_directory_child_is_promoted(..)? {     // store C  <- the guard
//!         continue;
//!     }
//!     record_runner_exec_directory_child(&store, ..)?;      // store A  <- the artifact
//!     record_runner_exec_directory_child_promotion(..)?;    // store D  <- the checkpoint
//! }
//! ```
//!
//! The loop is a resume: it exists so an interrupted directory promotion can be
//! re-run without duplicating children. The guard at C decides that, and the
//! state it reads is written at D. Two different opens.
//!
//! Under one root they agree and the resume works. Under two they do not, and
//! the failure is silent in both directions:
//!
//! * guard reads a root where nothing is promoted -> every child is promoted
//!   again, duplicating artifacts under a fresh id
//! * guard reads a root where all children are promoted -> the loop skips
//!   everything and the directory promotes nothing
//!
//! Neither raises. The artifact itself went to store A, so a duplicate looks
//! like a successful promotion and a skip looks like an idempotent no-op.
//!
//! ## What this test pins
//!
//! The guard, the checkpoint it reads, and the artifact write all name the
//! caller's store.
//!
//! ## Why source-level
//!
//! The defect is not reachable from behaviour under a single root, which is the
//! only root a test process has unless it goes out of its way. What is pinned is
//! the *provenance* of the store — a property of the call, not of its result.

use std::path::PathBuf;

fn promotion_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/homeboy-lab-runner/src/execution/artifact_promotion.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    // Tests below this marker deliberately drive the ambient wrappers to
    // simulate a crashed promotion; only production code is pinned here.
    source
        .split_once("\n#[cfg(test)]\nmod tests {")
        .map(|(production, _)| production.to_string())
        .unwrap_or(source)
}

/// The guard and the write it guards share the caller's store.
#[test]
fn directory_child_promotion_guard_shares_the_promotion_store() {
    let source = promotion_source();

    for helper in [
        "checkpoint_runner_exec_directory_tree",
        "runner_exec_directory_child_is_promoted",
        "record_runner_exec_directory_child_promotion",
    ] {
        assert!(
            !source.contains(&format!("{helper}(")),
            "artifact_promotion.rs calls the ambient `{helper}`. This loop is a \
             resume: the guard decides whether a child is promoted again, and \
             the state it reads is written by its sibling call. An ambient \
             opener splits the two, which duplicates children or skips them \
             all, and neither raises (#7505). Use `{helper}_in_store`."
        );
        assert!(
            source.contains(&format!("{helper}_in_store(")),
            "artifact_promotion.rs no longer calls `{helper}_in_store` (#7505)."
        );
    }
}

/// Promotion opens no store of its own; it is handed one.
#[test]
fn promotion_opens_no_ambient_store() {
    let source = promotion_source();

    let opens = source.matches("ObservationStore::open_initialized()").count();
    assert_eq!(
        opens, 0,
        "artifact_promotion.rs opens {opens} ambient store(s). Both production \
         callers — `runner exec` and recovery reconciliation — already hold a \
         lifecycle store, so opening one here reintroduces a second root \
         beneath an injected one (#7505)."
    );
}

/// Every promotion entry point offers a rooted counterpart.
#[test]
fn every_promotion_entry_point_has_a_rooted_counterpart() {
    let source = promotion_source();

    for entry in [
        "promote_runner_exec_artifacts",
        "promote_runner_exec_artifact_dirs",
        "promote_runner_exec_summaries",
    ] {
        assert!(
            source.contains(&format!("pub fn {entry}_in_store(")),
            "`{entry}` has no `_in_store` counterpart. The ambient name is kept \
             for callers that are not yet rooted, but a rooted caller must have \
             somewhere to go (#7505)."
        );
    }
}
