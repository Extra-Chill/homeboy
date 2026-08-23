//! Pins one promotion onto one observation store.
//!
//! ## What went wrong, concretely
//!
//! The promotion flow threaded `observation_store: Option<&ObservationStore>`
//! through seven functions. The `_in_observation_store` entry points passed
//! `Some(..)`; the ambient entry points passed `None`, and three interior sites
//! then re-resolved the environment on their own:
//!
//! ```text
//! retain_committed_changes_artifact         opened its own ambient store
//! verified_controller_artifact_projection   ambient arm of a match
//! verified_controller_artifact_projection_path  ambient arm of a match
//! ```
//!
//! A promotion reads the run, retains the committed delta, and projects the
//! verified artifact as one unit of work about one run. Resolving separately
//! meant a promotion could check a run in one installation and retain its
//! artifact in another -- and `retain_committed_changes_artifact` returns
//! `Ok(None)` when the run is absent, so the failure mode is a silently skipped
//! retention rather than an error.
//!
//! Now the five ambient entry points resolve once each and pass the store down,
//! and the parameter is a plain reference, so there is no `None` for an
//! interior site to interpret.

use std::path::PathBuf;

fn promote_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/homeboy-agents/src/agent_task_promotion/promote.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn the_promotion_store_is_not_optional() {
    let source = promote_source();
    assert!(
        !source.contains("Option<&homeboy_core::observation::ObservationStore>"),
        "the promotion flow carries an optional observation store again. An \
         optional store is what let interior sites re-resolve the environment \
         mid-promotion; resolve once at the entry point and pass a plain \
         reference (#7505)."
    );
}

#[test]
fn interior_promotion_sites_do_not_reopen_the_environment() {
    let source = promote_source();
    // Every legitimate resolution is a `let` binding at a public entry point.
    // Any other shape is an interior site resolving for itself.
    let total = source
        .matches("ObservationStore::open_initialized()")
        .count();
    let at_entry_points = source
        .matches("let observation_store = homeboy_core::observation::ObservationStore::open_initialized()?;")
        .count();
    assert_eq!(
        total, at_entry_points,
        "promote.rs opens an ambient observation store somewhere other than an \
         entry-point binding: {total} total, {at_entry_points} at entry points. \
         A promotion is one unit of work and resolves its store once (#7505)."
    );
}
