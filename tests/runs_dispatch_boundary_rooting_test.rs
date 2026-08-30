//! Pins `homeboy runs` onto one observation store per invocation.
//!
//! ## What went wrong, concretely
//!
//! `dispatch::run` already declared the boundary and opened one store:
//!
//! ```text
//! pub fn run(args: RunsArgs) -> CmdResult<RunsOutput> {
//!     let store = ObservationStore::open_initialized()?;
//!     match args.command { .. }
//! }
//! ```
//!
//! But several variants were dispatched without it and opened their own, so a
//! single `homeboy runs env` opened two stores: the boundary's, unused, and the
//! handler's. That is not a wrong answer today -- both resolve the same
//! environment inside one short process -- but it means the boundary was
//! declared and not enforced, and every handler that keeps its own open is one
//! more place that has to be revisited before the ambient entry point can go.
//!
//! Run detail, artifact, and resource handlers now take the store the dispatcher
//! already holds.
//!
//! ## What this test pins
//!
//! That those handlers keep taking an injected store. It deliberately does not
//! assert a total count for the module: `show_run` uses `open_readonly`, which
//! is a different mode chosen so a read cannot create or migrate a database,
//! and several sibling commands are still their own boundaries.

use std::path::PathBuf;

fn source(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/homeboy-cli/src/commands/runs")
        .join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn dispatched_handlers_do_not_reopen_the_store_they_were_given() {
    for (relative, names) in [
        (
            "handlers.rs",
            &[
                "resume_plan",
                "artifacts_from_args",
                "env",
                "artifact_command",
                "artifact_preview_handle",
                "artifact_get_inner",
                "artifact_get_handle",
            ][..],
        ),
        ("remote_artifact.rs", &["attach", "preview", "capture"]),
        ("resources.rs", &["runs_resources_in_store"]),
    ] {
        let source = source(relative);
        for name in names {
            let start = source
                .find(&format!("fn {name}("))
                .unwrap_or_else(|| panic!("`{name}` still exists in {relative}"));
            let rest = &source[start..];
            let end = rest.find("\n}\n").expect("terminated body");
            let body = &rest[..end];
            assert!(
                body.contains("store: &ObservationStore"),
                "`{name}` stopped taking the dispatcher's store (#7505)."
            );
            assert!(
                !body.contains("ObservationStore::open_initialized()"),
                "`{name}` takes an injected store and then opens another one. That \
             is the shape the compiler cannot catch: the signature looks rooted \
             while the body still resolves the environment for itself (#7505)."
            );
        }
    }
}

#[test]
fn the_dispatcher_hands_its_store_to_those_handlers() {
    let dispatch = source("dispatch.rs");
    for call in [
        "handlers::resume_plan(&store, &run_id)",
        "handlers::env(&store, &run_id)",
        "handlers::artifacts_from_args(&store, args)",
        "handlers::artifact_command(&store, args)",
        "resources::runs_resources_in_store(&store, roots.config(), args)",
    ] {
        assert!(
            dispatch.contains(call),
            "`dispatch::run` no longer passes its store to a handler that takes \
             one. The boundary is only real if the dispatcher actually hands the \
             store down (#7505). Expected:\n{call}"
        );
    }
}
