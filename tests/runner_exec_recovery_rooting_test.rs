//! Pins the runner-exec recovery reconciliation onto a single observation store.
//!
//! ## What went wrong, concretely
//!
//! `reconcile_terminal_runner_exec_runs_with_owner` takes `roots: &PathRoots`
//! and opens its candidate reader through them:
//!
//! ```text
//! let store = ObservationStore::open_initialized_in_roots(roots)?;
//! ```
//!
//! It then selected candidates from that store and wrote its results through
//! five calls that each opened their own *ambient* store:
//!
//! ```text
//! record_runner_exec_terminal_checkpoint(..)     // 1 ambient open
//! record_runner_exec_declaration_promotion(..)   // 3 ambient opens
//! record_runner_exec_artifact_refs(..)           // 1 ambient open
//! ```
//!
//! So a pass that *read* through injected roots *wrote* through whatever the
//! environment happened to name. Under a `HermeticTestContext` — which
//! allocates temp roots and deliberately does not mutate `HOME` — the read side
//! saw the hermetic root and the write side saw the real one. The reconciler
//! would select a run, promote its artifacts, and record the outcome against a
//! row in a different database than the one it had just decided from.
//!
//! This is the same split-root shape as `ActiveRigRunLease::drop` (#13011):
//! injection applied to one half of an operation is not injection, it is a
//! guarantee that the two halves can disagree.
//!
//! ## What this test pins
//!
//! Inside `recovery.rs`, the three write helpers are reached only through their
//! `_in_store` counterparts. The ambient names remain exported — other callers
//! are not yet rooted and deleting them would be a bottom-up migration — but
//! they must not reappear in the one caller that already holds roots.
//!
//! A source-level assertion is the right instrument here. The defect is not
//! observable from behaviour under a single root: both stores resolve, both
//! writes succeed, and the split only shows up as a lost update when the two
//! roots differ. What is being pinned is the *provenance* of the store, which
//! is a property of the call, not of its result.

use std::path::PathBuf;

fn recovery_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/homeboy-lab-runner/src/execution/recovery.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The reconciler holds `roots`, so every observation write it performs has to
/// name the store built from those roots.
#[test]
fn recovery_reconciliation_writes_through_the_injected_store() {
    let source = recovery_source();

    for helper in [
        "record_runner_exec_terminal_checkpoint",
        "record_runner_exec_declaration_promotion",
        "record_runner_exec_artifact_refs",
    ] {
        let ambient = format!("{helper}(");
        assert!(
            !source.contains(&ambient),
            "recovery.rs calls the ambient `{helper}`. This function already \
             holds `roots: &PathRoots` and opens its reader through them, so an \
             ambient write here reads one installation and writes another \
             (#7505). Use `{helper}_in_store` with the `lifecycle_store` built \
             from the same roots."
        );
        assert!(
            source.contains(&format!("{helper}_in_store(")),
            "recovery.rs no longer calls `{helper}_in_store`. If this write was \
             removed on purpose, drop it from this list; if it was re-routed \
             through an ambient opener, that is the split-root regression this \
             test exists to catch (#7505)."
        );
    }
}

/// One reconciliation pass is one unit of work, so it builds one lifecycle
/// store — not one per write.
#[test]
fn recovery_reconciliation_builds_exactly_one_lifecycle_store() {
    let source = recovery_source();

    let constructions = source.matches("AgentTaskLifecycleStore::new(").count();
    assert_eq!(
        constructions, 1,
        "recovery.rs constructs {constructions} lifecycle stores. The \
         reconciler is one unit of work over one set of roots; constructing a \
         store per write would restore the per-call resolution this change \
         removed, just with an explicit argument (#7505)."
    );

    assert!(
        !source.contains("AgentTaskLifecycleStore::from_current_environment"),
        "recovery.rs resolves a lifecycle store from the environment. It is \
         handed `roots` by its caller; resolving again here would reintroduce \
         an ambient root below an injected one (#7505)."
    );
}
