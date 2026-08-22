//! Startup capability classification for runtime selection.
//!
//! This intentionally uses only argv so it can run before extension discovery,
//! configuration hydration, or any mutation coordination.
//!
//! ## Why this is not `command_safety_manifest_from(...).find_path(argv).mutates`
//!
//! #11141 proposed replacing this classifier with a lookup into the command
//! safety manifest, on the theory that it duplicates `CommandSpec.safety`. The
//! ordering objection this module states does NOT block that: the manifest
//! derives from `COMMAND_SPECS` (static consts) plus `Cli::command()` (clap
//! derive metadata), and `registered_command` is a linear scan over a static
//! array — neither reads config nor discovers extensions. Verified 2026-08-01.
//!
//! The conversion is blocked by something else. The two tables answer different
//! questions:
//!
//! - `CommandSafetyEntry.mutates` is ONE bool per command PATH, feeding a
//!   documentation/audit projection that deliberately under-declares —
//!   `command_safety_manifest_audit` is `report_only: true`, and a path with no
//!   registry declaration resolves to `read_only()` by default.
//! - `CommandCapability` is a per-INVOCATION runtime gate that must fail closed,
//!   because `Mutation` is what authorizes scheduling runner-exec recovery
//!   children (before a mutating command can evict their source evidence) and
//!   the deferred-workload worker restart.
//!
//! A per-path lookup cannot express the argument-conditional rows here at all:
//! `status` is read-only until `--refresh`, and `project`/`rig`/`server` are
//! read-only only when bare or `list`. Worse, the registry's silence is not
//! evidence of read-only-ness. Every one of these resolves `mutates: false`
//! today while this classifier correctly answers `Mutation`:
//!
//! - `status --refresh` — its descriptor is a bare `command_spec`,
//!   so the manifest has no `--refresh` distinction and no subcommand table
//! - `daemon start` — its descriptor likewise declares nothing
//! - `runtime promotion-takeover` — `RUNTIME_SUBCOMMAND_SAFETY` declares only
//!   `refresh`
//! - `agent-task retry <run> --run` — `AGENT_TASK_SUBCOMMAND_SAFETY` declares no
//!   `retry` row
//!
//! Converting would silently reclassify all four as `ReadOnly` and skip their
//! startup recovery. `classifies_actions_as_mutations_by_default` below pins
//! exactly that. The `_ => Mutation` fail-closed default the issue asks for is
//! already in place.
//!
//! The duplication is real; the fix is not a per-path lookup. It would need the
//! registry to carry a mutation-inducing-flag axis, and every silent path to be
//! declared rather than defaulted.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCapability {
    /// The command may inspect local state but cannot change a controller runtime.
    ReadOnly,
    /// The command can change durable state or invoke an operation that can.
    Mutation,
}

/// Position of the bare `--` that divides Homeboy's arguments from the ones it
/// forwards verbatim, if the invocation has one.
///
/// This is the single separator primitive. Only the FIRST bare `--` divides:
/// any later one is part of the forwarded command (`homeboy ssh host -- sh -c
/// -- x`) and belongs to whoever receives it. Every question about the boundary
/// — "which arguments may Homeboy read" ([`homeboy_owned_args`]) and "where
/// does the passthrough begin" (`mark_explicit_passthrough`) — resolves through
/// this one answer rather than through a private rescan (#11755).
pub(crate) fn argv_separator_index(args: &[String]) -> Option<usize> {
    args.iter().position(|arg| arg == "--")
}

/// The arguments Homeboy itself owns: everything before the first bare `--`.
///
/// Everything after it is forwarded verbatim to a remote or child command —
/// `homeboy ssh <target> -- df -h /` — so it must never influence how Homeboy
/// classifies or routes its own invocation. Reading through the separator let a
/// remote `-h` short-circuit this function to `ReadOnly` on an `ssh` invocation
/// that is otherwise `Mutation`, and made the startup help fast path claim an
/// invocation clap would go on to parse successfully (#11577).
///
/// This is the only sanctioned way to inspect argv for a flag Homeboy owns.
/// `owned_args_guard_test` fails the build when a new raw argv flag scan
/// appears outside it, because the two sites above were written months apart
/// and nothing but review stood between them and a third.
pub(crate) fn homeboy_owned_args(args: &[String]) -> &[String] {
    match argv_separator_index(args) {
        Some(separator) => &args[..separator],
        None => args,
    }
}

pub fn classify(args: &[String]) -> CommandCapability {
    let args = args.get(1..).unwrap_or_default();
    let args = homeboy_owned_args(args);

    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return CommandCapability::ReadOnly;
    }

    if args
        .windows(2)
        .any(|args| args == ["agent-task", "gate-feedback"])
    {
        return CommandCapability::ReadOnly;
    }

    match args {
        [flag] if flag == "--version" || flag == "-V" => CommandCapability::ReadOnly,
        [command, subcommand, ..]
            if command == "self"
                && matches!(
                    subcommand.as_str(),
                    "identity" | "inspect" | "status" | "upgrade-admission"
                ) =>
        {
            CommandCapability::ReadOnly
        }
        [command, rest @ ..]
            if command == "status" && !rest.iter().any(|arg| arg == "--refresh") =>
        {
            CommandCapability::ReadOnly
        }
        [command, ..] if command == "activity" => CommandCapability::ReadOnly,
        [command, rest @ ..]
            if matches!(command.as_str(), "project" | "rig" | "server")
                && matches!(rest.first().map(String::as_str), None | Some("list")) =>
        {
            CommandCapability::ReadOnly
        }
        [command, subcommand, ..] if command == "runner" && subcommand == "status" => {
            CommandCapability::ReadOnly
        }
        [command, subcommand, rest @ ..]
            if command == "runner"
                && subcommand == "doctor"
                && !rest.iter().any(|arg| arg == "--repair") =>
        {
            CommandCapability::ReadOnly
        }
        [command, subcommand, ..]
            if matches!(
                (command.as_str(), subcommand.as_str()),
                ("runs", "list")
                    | ("daemon", "status")
                    | ("agent-task", "status")
                    | ("agent-task", "evidence")
                    | ("agent-task", "active")
            ) =>
        {
            CommandCapability::ReadOnly
        }
        _ => CommandCapability::Mutation,
    }
}

/// Whether this command owns runner-exec recovery.
///
/// Recovery may contact a previously selected runner, so only `runner exec`
/// may start it. Other mutations must not turn routine commands into an
/// unrelated background-recovery admission path.
pub(crate) fn requires_startup_reconciliation(args: &[String]) -> bool {
    homeboy_owned_args(args.get(1..).unwrap_or_default())
        .windows(2)
        .any(|args| args == ["runner", "exec"])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn classifies_runtime_safe_diagnostics_without_parsing_runtime_state() {
        for command in [
            args(&["homeboy", "--version"]),
            args(&["homeboy", "self", "identity"]),
            args(&["homeboy", "self", "status"]),
            args(&[
                "homeboy",
                "self",
                "upgrade-admission",
                "--legacy-identity",
                "homeboy 0.338.0+legacy",
            ]),
            args(&["homeboy", "status"]),
            args(&[
                "homeboy",
                "runner",
                "doctor",
                "lab",
                "--scope",
                "lab-offload",
            ]),
            args(&["homeboy", "agent-task", "retry", "--help"]),
            args(&[
                "homeboy",
                "agent-task",
                "gate-feedback",
                "--promotion",
                "{}",
                "--source-task",
                "{}",
            ]),
        ] {
            assert_eq!(classify(&command), CommandCapability::ReadOnly);
        }
    }

    #[test]
    fn classifies_inspection_commands_as_read_only_during_active_controller_work() {
        for command in [
            args(&["homeboy", "activity"]),
            args(&["homeboy", "activity", "list"]),
            args(&["homeboy", "activity", "list", "--limit", "1"]),
            args(&["homeboy", "activity", "show", "run-1"]),
            args(&["homeboy", "activity", "watch", "run-1"]),
            args(&["homeboy", "project", "list"]),
            args(&["homeboy", "rig", "list"]),
            args(&["homeboy", "server", "list"]),
            args(&["homeboy", "runner", "status"]),
            args(&["homeboy", "runner", "status", "--full"]),
            args(&["homeboy", "runs", "list", "--limit", "10"]),
            args(&["homeboy", "daemon", "status"]),
            args(&["homeboy", "status", "--all"]),
            args(&["homeboy", "agent-task", "status", "run-1", "--full"]),
            args(&["homeboy", "agent-task", "evidence", "run-1"]),
            args(&["homeboy", "agent-task", "active", "--full"]),
        ] {
            assert_eq!(
                classify(&command),
                CommandCapability::ReadOnly,
                "{command:?}"
            );
        }
    }

    #[test]
    fn classifies_actions_as_mutations_by_default() {
        for command in [
            args(&["homeboy", "upgrade"]),
            args(&["homeboy", "agent-task", "retry", "run-1", "--run"]),
            args(&["homeboy", "runtime", "promotion-takeover"]),
            args(&["homeboy", "status", "--refresh"]),
            args(&["homeboy", "runs", "reconcile"]),
            args(&["homeboy", "daemon", "start"]),
        ] {
            assert_eq!(classify(&command), CommandCapability::Mutation);
        }
    }

    #[test]
    fn only_runner_exec_owns_startup_recovery() {
        let review = args(&[
            "homeboy",
            "agent-task",
            "review",
            "run-1",
            "--to-worktree",
            "repo@local",
        ]);
        assert_eq!(classify(&review), CommandCapability::Mutation);
        assert!(!requires_startup_reconciliation(&review));
        assert!(!requires_startup_reconciliation(&args(&[
            "homeboy",
            "agent-task",
            "retry",
            "run-1",
            "--run",
        ])));
        assert!(requires_startup_reconciliation(&args(&[
            "homeboy", "runner", "exec", "lab", "--", "true",
        ])));
    }

    #[test]
    fn other_mutations_do_not_own_runner_recovery() {
        for command in [
            args(&[
                "homeboy",
                "daemon",
                "supervise",
                "--addr",
                "127.0.0.1:0",
                "--startup-token",
                "token",
            ]),
            args(&[
                "homeboy",
                "daemon",
                "serve",
                "--addr",
                "127.0.0.1:0",
                "--startup-token",
                "token",
            ]),
        ] {
            assert_eq!(classify(&command), CommandCapability::Mutation);
            assert!(!requires_startup_reconciliation(&command));
        }

        for command in [
            args(&["homeboy", "daemon", "start"]),
            args(&["homeboy", "daemon", "ensure-running"]),
        ] {
            assert!(!requires_startup_reconciliation(&command));
        }
    }

    #[test]
    fn a_forwarded_remote_argument_cannot_reclassify_the_invocation() {
        // `homeboy ssh <target> --timeout 30 -- df -h /` is a mutation that
        // happens to forward `-h`. Reading through the separator classified it
        // ReadOnly and skipped the mutation path entirely (#11577).
        let ssh_with_remote_help = args(&[
            "homeboy",
            "ssh",
            "chubes-net",
            "--timeout",
            "30",
            "--",
            "df",
            "-h",
            "/",
        ]);
        assert_eq!(classify(&ssh_with_remote_help), CommandCapability::Mutation);

        // Homeboy's own help is still help, before or after other flags.
        assert_eq!(
            classify(&args(&["homeboy", "ssh", "--help"])),
            CommandCapability::ReadOnly
        );

        // A remote command whose own flags mimic Homeboy's read-only surface
        // must not borrow that classification either.
        assert_eq!(
            classify(&args(&["homeboy", "ssh", "host", "--", "runner", "status"])),
            CommandCapability::Mutation
        );
    }

    #[test]
    fn homeboy_owned_args_stops_at_the_first_separator() {
        let forwarded = args(&["ssh", "host", "--", "df", "-h"]);
        assert_eq!(homeboy_owned_args(&forwarded), &args(&["ssh", "host"])[..]);

        // No separator means every argument is Homeboy's.
        let all = args(&["runner", "status"]);
        assert_eq!(homeboy_owned_args(&all), &all[..]);

        // Only the first separator delimits; later ones belong to the remote.
        let nested = args(&["ssh", "host", "--", "sh", "-c", "--", "x"]);
        assert_eq!(homeboy_owned_args(&nested), &args(&["ssh", "host"])[..]);
    }

    #[test]
    fn the_separator_primitive_names_only_the_first_boundary() {
        assert_eq!(argv_separator_index(&args(&["runner", "status"])), None);
        assert_eq!(
            argv_separator_index(&args(&["ssh", "host", "--", "df"])),
            Some(2)
        );

        // A nested separator is the forwarded command's own argument.
        let nested = args(&["ssh", "host", "--", "sh", "-c", "--", "x"]);
        assert_eq!(argv_separator_index(&nested), Some(2));

        // The two views agree by construction: the owned prefix ends exactly
        // where the primitive says the boundary is.
        for argv in [
            args(&["runner", "status"]),
            args(&["ssh", "host", "--", "df"]),
            nested,
        ] {
            let owned = homeboy_owned_args(&argv);
            assert_eq!(
                owned.len(),
                argv_separator_index(&argv).unwrap_or(argv.len())
            );
        }
    }
}

#[cfg(test)]
#[path = "owned_args_guard_test.rs"]
mod owned_args_guard_test;
