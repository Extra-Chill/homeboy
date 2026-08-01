//! Guards for the Lab help-visibility path table (#11141).
//!
//! `LAB_VISIBLE_COMMAND_PATHS` decides which subcommands advertise the Lab
//! placement/runner flags in `--help`. That is a second source of truth for "is
//! this path Lab-portable", parallel to each command's `lab_contract()`, and
//! until now NOTHING asserted the two agreed — the table was a `matches!` arm
//! chain, so it could not even be enumerated.
//!
//! Drift between the two is not cosmetic. A path missing from the table hides
//! `--placement`/`--runner` on a command that honours them, so the capability
//! is undiscoverable; a path present in the table with no contract advertises
//! flags that silently do nothing. The comparable class of bug in the safety
//! registry (#10313) was a declared path spelling clap does not expose, which
//! is likewise unobservable here without a test.

use super::{lab_cli_arguments_are_visible_for_path, LAB_VISIBLE_COMMAND_PATHS};
use crate::cli_surface::{current_command_surface, Cli, CommandSurfaceEntry};
use clap::{CommandFactory, FromArgMatches};

/// A parseable invocation at each Lab-visible path.
///
/// Help visibility is per-PATH; a Lab contract is resolved per-INVOCATION.
/// Several commands only carry a contract for a particular argument shape
/// (`retry --run`, `controller from-spec --resume`, `refactor --all`,
/// `fuzz list --remote-discovery`), which is precisely why the flags are
/// advertised on the path: the operator needs them discoverable in order to
/// reach that shape. So the claim under test is "there exists an invocation at
/// this path with a Lab contract", and these are those invocations.
///
/// Required arguments below are the ones clap actually demands; a path whose
/// required arguments change fails here loudly rather than skipping silently.
const REPRESENTATIVE_INVOCATIONS: &[&[&str]] = &[
    &["agent-task", "cook", "--to-worktree", "repo@slug"],
    &["agent-task", "run-plan", "--plan", "-"],
    &["agent-task", "run", "run-1"],
    &["agent-task", "run-next"],
    &["agent-task", "status", "run-1"],
    &["agent-task", "list"],
    &["agent-task", "active"],
    &["agent-task", "latest"],
    &["agent-task", "logs", "run-1"],
    &["agent-task", "artifacts", "run-1"],
    &["agent-task", "evidence", "run-1"],
    &["agent-task", "review", "run-1"],
    // Only `--run` executes; the bare form is local retry bookkeeping.
    &["agent-task", "retry", "run-1", "--run"],
    &[
        "agent-task",
        "promote",
        "--to-worktree",
        "repo@slug",
        "candidate.patch",
    ],
    &["agent-task", "providers"],
    &["agent-task", "fanout", "submit-batch", "--input", "{}"],
    &["agent-task", "fanout", "status", "batch-1"],
    &["agent-task", "fanout", "artifacts", "batch-1"],
    &[
        "agent-task",
        "fanout",
        "cook-batch",
        "--repo",
        "owner/repo",
        "https://example.test/issues/1",
    ],
    &["agent-task", "fanout", "run-plan", "--input", "{}"],
    &["agent-task", "auth", "status"],
    // #9375-adjacent: only `--resume` materializes, and only that shape has a
    // portable contract.
    &["agent-task", "controller", "from-spec", "--resume", "spec"],
    &[
        "agent-task",
        "controller",
        "run-from-spec",
        "--max-actions",
        "1",
        "spec",
    ],
    &["agent-task", "controller", "materialize", "spec"],
    &["agent-task", "controller", "resume", "loop-1"],
    &["bench"],
    &["bench", "matrix"],
    &["fuzz"],
    &["fuzz", "run"],
    &["fuzz", "run-campaign"],
    // Listing is local metadata discovery until a runner is queried.
    &["fuzz", "list", "--remote-discovery"],
    &["fuzz", "plan"],
    &["fuzz", "doctor", "--extension", "rust"],
    &["review"],
    &["review", "audit"],
    &["review", "lint"],
    &["review", "test"],
    &["trace"],
    // A bare `refactor` is not a hot resource command; `--all` is the shape
    // that offloads.
    &["refactor", "--all"],
    &["rig", "check", "target"],
    &["rig", "run", "--profile", "profile", "rig-1"],
    &["runtime", "refresh", "--source", "source", "runtime-1"],
    &["worktree", "cleanup"],
    &["extension", "update"],
    &["extension", "refresh", "source"],
    &[
        "extension",
        "dev-run",
        "--source",
        "source",
        "--runner",
        "runner-1",
        "extension-1",
        "command",
    ],
    &["extension", "show", "extension-1"],
    &["tunnel", "preview-consumer", "run", "--config", "config"],
    // `--server` is `required_unless_present = "runner_local"`, so it does not
    // appear in the usage line but clap still demands one of the pair.
    &[
        "tunnel",
        "service",
        "expose",
        "--server",
        "server-1",
        "--remote-host",
        "host",
        "--remote-port",
        "8080",
        "--auth-mode",
        "ssh-only",
        "service-1",
    ],
    &[
        "tunnel",
        "service",
        "start",
        "--command",
        "sleep",
        "service-1",
    ],
];

fn owned(path: &[&str]) -> Vec<String> {
    path.iter().map(|segment| segment.to_string()).collect()
}

/// The declared table is what `lab_cli_arguments_are_visible_for_path` answers
/// with. Without this the tests below could pass against a table the help
/// surface never consults.
#[test]
fn every_declared_path_is_visible_and_undeclared_paths_are_not() {
    for path in LAB_VISIBLE_COMMAND_PATHS {
        assert!(
            lab_cli_arguments_are_visible_for_path(&owned(path)),
            "declared Lab-visible path `{}` is not visible",
            path.join(" ")
        );
    }

    // The root has no path, and `contract manifest` is the non-portable
    // subcommand a previous refactor wrongly advertised these flags on.
    for path in [vec![], vec!["contract"], vec!["contract", "manifest"]] {
        assert!(
            !lab_cli_arguments_are_visible_for_path(&owned(&path)),
            "`{}` must not advertise Lab placement flags",
            path.join(" ")
        );
    }
}

/// Every Lab-visible path must resolve to a real node in the clap-derived
/// command surface.
///
/// This is the #10313 guard applied to help visibility: a declared path that
/// clap does not expose can never match, so the placement flags are hidden
/// forever on a command that supports them, with no error anywhere.
#[test]
fn every_lab_visible_path_resolves_to_a_clap_surface_node() {
    fn resolves(entries: &[CommandSurfaceEntry], path: &[&str]) -> bool {
        let Some((first, rest)) = path.split_first() else {
            return true;
        };

        entries
            .iter()
            .find(|entry| entry.name == *first)
            .is_some_and(|entry| resolves(&entry.subcommands, rest))
    }

    let surface = current_command_surface();

    for path in LAB_VISIBLE_COMMAND_PATHS {
        assert!(
            resolves(&surface.commands, path),
            "Lab-visible path `{}` is not a path in the clap command surface",
            path.join(" ")
        );
    }
}

/// The path table may only refine the registry, never contradict it.
///
/// `CommandSpec.lab_supported` is the declared answer to "does this command
/// family route to Lab at all". A Lab-visible path under a command the registry
/// does not declare Lab-supported is one table disagreeing with another — the
/// exact shape of #8025/#9375/#9428/#9763, where a private copy of one
/// workload's taxonomy drifted from the copy that governed behavior.
#[test]
fn lab_visible_paths_stay_within_the_registry_lab_supported_commands() {
    let declared = LAB_VISIBLE_COMMAND_PATHS
        .iter()
        .filter_map(|path| path.first().copied())
        .collect::<std::collections::BTreeSet<_>>();
    let registered = crate::command_contract::COMMAND_SPECS
        .iter()
        .filter(|spec| spec.lab_supported)
        .map(|spec| spec.name)
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        declared, registered,
        "the Lab help-visibility table and `CommandSpec.lab_supported` must describe the same command families"
    );
}

/// The agreement the issue says nothing asserts (#11141).
///
/// Help visibility is per-path; a Lab contract is per-invocation. The honest
/// claim is that SOME invocation at each Lab-visible path carries a Lab
/// contract — if none does, the flags are pure noise in `--help`.
#[test]
fn every_lab_visible_path_has_an_invocation_with_a_lab_contract() {
    assert_eq!(
        REPRESENTATIVE_INVOCATIONS.len(),
        LAB_VISIBLE_COMMAND_PATHS.len(),
        "every Lab-visible path needs a representative invocation"
    );

    for (path, invocation) in LAB_VISIBLE_COMMAND_PATHS
        .iter()
        .zip(REPRESENTATIVE_INVOCATIONS)
    {
        assert!(
            invocation.starts_with(path),
            "representative invocation `{}` is not an invocation of `{}`",
            invocation.join(" "),
            path.join(" ")
        );

        let argv = std::iter::once("homeboy")
            .chain(invocation.iter().copied())
            .collect::<Vec<_>>();
        let matches = Cli::command()
            .try_get_matches_from(&argv)
            .unwrap_or_else(|error| panic!("`{}` should parse: {error}", argv.join(" ")));
        let command = Cli::from_arg_matches(&matches)
            .expect("validated arguments should parse")
            .command;

        assert!(
            command.portability_contract().lab_command().is_some(),
            "`{}` advertises the Lab placement flags in --help but `{}` resolves no Lab contract",
            path.join(" "),
            argv.join(" ")
        );
    }
}
