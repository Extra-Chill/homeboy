//! Pins a locked registry mutation onto the root its lock was taken against.
//!
//! ## What went wrong, concretely
//!
//! `generation_store` addresses every file through one runner-sessions
//! directory, but reached it through seven separate fallible helpers, each of
//! which read the environment for itself. A locked mutation therefore looked
//! like this:
//!
//! ```text
//! with_registry_lock(runner_id, || {          <- resolves, locks
//!     let operation = replacement_operation_path(runner_id)?;   <- resolves again
//!     let evidence  = rejected_replacement_path(runner_id)?;    <- and again
//!     write_durable_json(&evidence, ..)?;
//!     write_durable_json(&operation, ..)?;
//! })
//! ```
//!
//! The lock exists to make that read-modify-write atomic, and that guarantee
//! holds only if the lock file and the files it guards are in the same
//! installation. Four independent reads do not promise that. A repoint between
//! the lock and the write leaves the lock guarding one home while the mutation
//! lands in another -- which is the one thing a lock is supposed to prevent.
//!
//! `with_rooted_registry_lock` resolves the config root once, takes the lock
//! against it, and hands it to the operation, so the fence and the mutation
//! address one installation by construction.

use std::path::PathBuf;

fn generation_store_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/homeboy-lab-runner/src/generation_store.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn the_rooted_lock_hands_its_root_to_the_operation() {
    let source = generation_store_source();
    let accessor = source
        .split_once("fn with_rooted_registry_lock")
        .map(|(_, rest)| rest)
        .expect("generation_store still defines with_rooted_registry_lock");
    let body_end = accessor
        .find("\n}\n")
        .expect("with_rooted_registry_lock has a terminated body");
    let body = &accessor[..body_end];
    assert!(
        body.contains("path_in_root(&config_root, runner_id)"),
        "with_rooted_registry_lock no longer takes its lock against the root it \
         passes down. The lock file and the files it guards have to be resolved \
         from one root or the lock guarantees nothing (#7505)."
    );
}

#[test]
fn rooted_operations_do_not_reresolve_inside_the_lock() {
    let source = generation_store_source();
    for (operation, ambient) in [
        (
            "retire_rejected_state_loss_replacement",
            "rejected_replacement_path(runner_id)",
        ),
        (
            "record_unleased_candidate_reconciliation_replay",
            "superseded_replacement_path(runner_id)",
        ),
        ("retire_replacement", "pending_replacement_path(runner_id)"),
    ] {
        let start = source
            .find(&format!("fn {operation}"))
            .unwrap_or_else(|| panic!("{operation} still exists"));
        let body = &source[start..];
        let end = body.find("\n}\n").expect("terminated body");
        assert!(
            !body[..end].contains(ambient),
            "{operation} resolves `{ambient}` inside a lock it already holds a \
             root for. Use the `_in_root` form with the lock's config_root \
             (#7505)."
        );
    }
}
