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
//!   because `Mutation` is what authorizes `reconcile_terminal_runner_exec_runs`
//!   (recovering completed runner-exec evidence before a mutating command can
//!   evict it) and the deferred-workload worker restart.
//!
//! A per-path lookup cannot express the argument-conditional rows here at all:
//! `status` is read-only until `--refresh`, and `project`/`rig`/`server` are
//! read-only only when bare or `list`. Worse, the registry's silence is not
//! evidence of read-only-ness. Every one of these resolves `mutates: false`
//! today while this classifier correctly answers `Mutation`:
//!
//! - `status --refresh` — `ops_command_spec!(status)` is a bare `command_spec`,
//!   so the manifest has no `--refresh` distinction and no subcommand table
//! - `daemon start` — `ops_command_spec!(daemon)` likewise declares nothing
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

pub fn classify(args: &[String]) -> CommandCapability {
    let args = args.get(1..).unwrap_or_default();

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
        [command, subcommand]
            if command == "self" && matches!(subcommand.as_str(), "identity" | "status") =>
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

/// Whether startup may reconcile runner-exec state before this invocation.
///
/// This is intentionally narrower than command mutation: a review may
/// materialize a controller-local artifact, but runner recovery would make its
/// already-durable aggregate unavailable during a Lab outage.
pub fn requires_startup_reconciliation(args: &[String]) -> bool {
    classify(args) == CommandCapability::Mutation
        // The lifecycle command that launches these internal processes already
        // performs startup recovery. Repeating it here delays lease publication
        // and can make the supervisor kill an otherwise healthy cold start.
        && !args
            .get(1..)
            .unwrap_or_default()
            .windows(2)
            .any(|args| {
                matches!(args, [command, subcommand]
                    if command == "daemon" && matches!(subcommand.as_str(), "supervise" | "serve"))
            })
        && !args
            .get(1..)
            .unwrap_or_default()
            .windows(2)
            .any(|args| args == ["agent-task", "review"])
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
    fn review_keeps_its_local_materialization_capability_but_skips_runner_recovery() {
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
        assert!(requires_startup_reconciliation(&args(&[
            "homeboy",
            "agent-task",
            "retry",
            "run-1",
            "--run",
        ])));
    }

    #[test]
    fn internal_daemon_processes_publish_their_lease_without_repeating_recovery() {
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
            assert!(requires_startup_reconciliation(&command));
        }
    }
}
