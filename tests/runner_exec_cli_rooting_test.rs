//! Pins `runner exec` onto a single observation store per invocation.
//!
//! ## What went wrong, concretely
//!
//! `exec_with_hydration` is the whole of `homeboy runner exec`. Against a
//! `--run-id` it performed eleven durable writes, in this order:
//!
//! ```text
//! ensure_generic_runner_exec_run          creates the run row
//! record_runner_exec_artifact_declarations
//! record_runner_exec_execution_record     (planned)
//!   ... the command actually runs ...
//! record_runner_exec_execution_record     (observed)
//! record_runner_exec_terminal_checkpoint
//! record_runner_exec_declaration_promotion  x3
//! record_runner_exec_artifact_refs
//! project_terminal_runner_result / finish_runner_exec_direct
//! ```
//!
//! Every one of those opened its own ambient `ObservationStore`. The first call
//! *creates* the row that all ten later calls address. So the run this command
//! created and the run it finished were the same row only by coincidence of
//! environment — nothing in the code made them the same installation.
//!
//! That coincidence holds right up until something moves `HOME` mid-invocation,
//! which is exactly what a hermetic test context does: it allocates temp roots
//! and deliberately leaves `HOME` alone. Under it the creating write and the
//! finishing write can land in different databases, and the later writes then
//! silently address a row that does not exist — every one of these helpers
//! returns `Ok` on a missing run rather than failing.
//!
//! ## What this test pins
//!
//! One invocation resolves at most one lifecycle store, and every lifecycle
//! write goes through it.
//!
//! ## Why the store is an `Option`
//!
//! `PathRoots` resolution is fallible. An invocation without `--run-id` writes
//! nothing durable, so resolving unconditionally would have made `runner exec`
//! fail in environments where it previously worked. The store is therefore
//! resolved only when there is a run to record against, which is also the exact
//! condition the two write blocks already tested.

use std::path::PathBuf;

fn exec_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/homeboy-cli/src/commands/runner/exec.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Every lifecycle write in `runner exec` names the invocation's own store.
#[test]
fn runner_exec_writes_through_the_invocation_store() {
    let source = exec_source();

    let ambient = source
        .lines()
        .filter(|line| line.contains("agent_task_lifecycle::"))
        .filter(|line| {
            // A call, not a type path or an import.
            line.contains('(') && !line.contains("_in_store(")
        })
        .filter(|line| !line.contains("AgentTaskLifecycleStore"))
        .map(str::trim)
        .collect::<Vec<_>>();

    assert!(
        ambient.is_empty(),
        "runner exec performs {} ambient lifecycle write(s): {ambient:#?}\n\n\
         The first durable write in this command creates the run row that every \
         later write addresses. An ambient opener here means the row created and \
         the row finished are the same only by coincidence of environment, and \
         these helpers return Ok on a missing run rather than failing (#7505). \
         Use the `_in_store` counterpart with `lifecycle_store`.",
        ambient.len()
    );
}

/// One invocation is one unit of work, so it resolves one store.
#[test]
fn runner_exec_resolves_at_most_one_lifecycle_store() {
    let source = exec_source();

    let resolutions = source
        .matches("AgentTaskLifecycleStore::from_current_environment")
        .count();
    assert_eq!(
        resolutions, 1,
        "runner exec resolves {resolutions} lifecycle stores. Resolving per \
         write is what this change removed; doing it again with an explicit \
         argument would be the same defect wearing a parameter (#7505)."
    );
}

/// The resolution stays gated on there being a run to record against.
#[test]
fn runner_exec_resolves_no_store_without_a_run_id() {
    let source = exec_source();

    assert!(
        source.contains("let lifecycle_store = validated_run_id"),
        "runner exec no longer gates lifecycle-store resolution on \
         `validated_run_id`. `PathRoots` resolution is fallible and an \
         invocation without `--run-id` writes nothing durable, so resolving \
         unconditionally makes commands fail that never needed a home (#7505)."
    );
}
