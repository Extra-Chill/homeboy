//! Pins the globally-propagated clap argument surface.
//!
//! Lives beside `cli_surface/mod.rs` rather than inside its `mod tests` block
//! because that file sits 7 lines under the audit's 1500-line `god_file`
//! threshold; anything added inline trips it.

use super::Cli;
use clap::CommandFactory;

/// The globally-propagated flag set is a wire protocol, not just documentation:
/// `homeboy-lab-runner` negotiates the lease-less recovery contract by parsing
/// a remote binary's rendered `Options:` block
/// (`negotiate_leaseless_recovery_contract`), and every `global = true`
/// argument is rendered into that block for every subcommand. Pin the set so
/// any addition or removal is a deliberate, reviewed protocol change.
///
/// This also anchors the invariant that motivated deleting the fieldless
/// `GlobalArgs` handler parameter: process-wide CLI state is carried by these
/// clap arguments on `Cli`, never by a separate struct threaded through
/// command handlers.
#[test]
fn root_global_flag_surface_is_pinned() {
    let mut longs: Vec<String> = Cli::command()
        .get_arguments()
        .filter(|arg| arg.is_global_set())
        .filter_map(|arg| arg.get_long())
        // `help`/`version` are clap-generated; this pins Homeboy-declared
        // globals only.
        .filter(|long| !matches!(*long, "help" | "version"))
        .map(str::to_string)
        .collect();
    longs.sort();

    assert_eq!(
        longs.iter().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "allow-dirty-lab-workspace",
            "artifact-root",
            "detach-after-handoff",
            "lab-env-json",
            "notification-route",
            "notification-transport",
            "output",
            "placement",
            "preserve-workspace-on-failure",
            "runner",
            "runner-env",
            "runner-workspace-root",
            "skip-deps-hydration",
            "wait",
        ],
        "the globally-propagated flag surface changed; update remote \
         capability negotiation and docs before accepting this",
    );
}
