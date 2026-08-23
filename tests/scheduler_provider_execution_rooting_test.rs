//! Pins the scheduler's durable provider-execution records onto one store.
//!
//! ## What went wrong, concretely
//!
//! `AgentTaskScheduler` carries `lifecycle_store: Option<..>`, defaulting to
//! `None` and populated only by the `with_lifecycle_store` builder. Three
//! durable writes consulted it:
//!
//! ```text
//! reserve_provider_execution                claims the execution slot
//!   ... the provider actually runs, for minutes ...
//! record_provider_execution_terminal_with_model   records the outcome
//! record_provider_execution_cleanup_elapsed       records teardown cost
//! ```
//!
//! Each wrote `match self.lifecycle_store.as_ref() { Some(store) => .., None =>
//! <ambient> }`. The `None` arm was not defensive: `agent_task_dispatch_service`
//! sets `with_run_id` and never sets a store, so every `agent-task dispatch`
//! took it -- three independent reads of the environment around one provider
//! execution.
//!
//! A reservation and its terminal record are an exactly-once pair. Resolving
//! separately means a repoint between them reserves in one installation and
//! records the terminal state in another, leaving the reservation held forever
//! and the next dispatch seeing an execution already reserved.
//!
//! The fix is `durable_lifecycle_store()`, which prefers an injected store and
//! otherwise resolves at most once into a `OnceLock`.
//!
//! ## What this test pins
//!
//! That engine.rs performs at most one ambient lifecycle resolution, and that
//! it is the one inside the accessor. This reads source rather than running the
//! scheduler because the property is structural: it is about how many times the
//! environment may be consulted, not about what any single run returns.

use std::path::PathBuf;

fn engine_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/homeboy-agents/src/agent_task_scheduler/engine.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn scheduler_resolves_at_most_one_lifecycle_store() {
    let source = engine_source();
    let resolutions = source.matches("from_current_environment(").count();
    assert!(
        resolutions <= 1,
        "engine.rs performs {resolutions} ambient lifecycle resolutions; the \
         reservation, terminal record, and cleanup record are one exactly-once \
         sequence and must share one store. Route the new call through \
         durable_lifecycle_store() instead (#7505)."
    );
}

#[test]
fn the_one_resolution_lives_in_the_shared_accessor() {
    let source = engine_source();
    let accessor = source
        .split_once("fn durable_lifecycle_store(")
        .map(|(_, rest)| rest)
        .expect("engine.rs still defines durable_lifecycle_store");
    let body_end = accessor
        .find("\n    }\n")
        .expect("durable_lifecycle_store has a terminated body");
    assert!(
        accessor[..body_end].contains("from_current_environment("),
        "the single ambient resolution moved out of durable_lifecycle_store; \
         that accessor is what makes the three durable provider-execution \
         writes name one installation (#7505)."
    );
}

#[test]
fn the_durable_writes_do_not_reintroduce_a_fallback() {
    let source = engine_source();
    for wrapper in [
        "agent_task_lifecycle::reserve_provider_execution(",
        "agent_task_lifecycle::record_provider_execution_terminal_with_model(",
        "agent_task_lifecycle::record_provider_execution_cleanup_elapsed(",
    ] {
        assert!(
            !source.contains(wrapper),
            "engine.rs calls the free-function form `{wrapper}`, which resolves \
             its own root. That is the fallback arm this change removed; use \
             durable_lifecycle_store() (#7505)."
        );
    }
}
