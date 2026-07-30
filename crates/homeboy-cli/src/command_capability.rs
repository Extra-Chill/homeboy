//! Startup capability classification for runtime selection.
//!
//! This intentionally uses only argv so it can run before extension discovery,
//! configuration hydration, or any mutation coordination.

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
        [command, rest @ ..]
            if matches!(command.as_str(), "activity" | "project" | "rig" | "server")
                && matches!(rest.first().map(String::as_str), None | Some("list")) =>
        {
            CommandCapability::ReadOnly
        }
        [command, subcommand, ..] if command == "runner" && subcommand == "status" => {
            CommandCapability::ReadOnly
        }
        [command, subcommand, ..]
            if matches!(
                (command.as_str(), subcommand.as_str()),
                ("runs", "list") | ("daemon", "status")
            ) =>
        {
            CommandCapability::ReadOnly
        }
        _ => CommandCapability::Mutation,
    }
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
            args(&["homeboy", "project", "list"]),
            args(&["homeboy", "rig", "list"]),
            args(&["homeboy", "server", "list"]),
            args(&["homeboy", "runner", "status"]),
            args(&["homeboy", "runner", "status", "--full"]),
            args(&["homeboy", "runs", "list", "--limit", "10"]),
            args(&["homeboy", "daemon", "status"]),
            args(&["homeboy", "status", "--all"]),
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
}
