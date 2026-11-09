//! Registry of command paths removed from the CLI on purpose.
//!
//! Clap reports a removed command as a bare `unrecognized subcommand` error,
//! which tells a consumer nothing about whether the command was removed
//! deliberately or what to use instead. When parsing fails with an invalid
//! subcommand whose path is in [`REMOVED_COMMANDS`], Homeboy exits non-zero
//! with a typed `command.removed` error naming the path, its replacement, a
//! migration hint, and the release that removed it. Any other unknown command
//! keeps the normal clap error.

use homeboy::core::{Error, ErrorCode};
use serde_json::{json, Value};

use crate::commands::utils::response::{self as output, CommandIdentity, CommandResultEnvelope};

/// Exit code for a removed command: a usage error, like other argv rejections.
pub(crate) const REMOVED_COMMAND_EXIT_CODE: i32 = 2;

/// One command path removed from the CLI.
#[derive(Debug)]
pub(crate) struct RemovedCommand {
    /// Command path after `homeboy`, for example `["rig", "sync"]`.
    pub path: &'static [&'static str],
    /// The command to use instead, or `None` when nothing replaces it.
    pub replacement: Option<&'static str>,
    /// One-line migration hint.
    pub hint: &'static str,
    /// Homeboy version whose release removed the command.
    pub removed_in: &'static str,
}

const NO_REPLACEMENT_HINT: &str =
    "Homeboy no longer ships this command; drop the call or move the workflow out of Homeboy.";

/// Every removed command path. Add an entry when a release removes a command.
pub(crate) static REMOVED_COMMANDS: &[RemovedCommand] = &[
    RemovedCommand {
        path: &["rig", "sync"],
        replacement: None,
        hint: "Rig stack syncing was deleted with the stack capability; drop the `rig sync` step from scripts and fixtures.",
        removed_in: "0.418.9",
    },
    RemovedCommand {
        path: &["stack"],
        replacement: None,
        hint: NO_REPLACEMENT_HINT,
        removed_in: "0.418.9",
    },
    RemovedCommand {
        path: &["fleet"],
        replacement: None,
        hint: "Deploy each project explicitly with `homeboy deploy`.",
        removed_in: "0.418.9",
    },
    RemovedCommand {
        path: &["triage"],
        replacement: None,
        hint: NO_REPLACEMENT_HINT,
        removed_in: "0.418.9",
    },
    RemovedCommand {
        path: &["db"],
        replacement: None,
        hint: NO_REPLACEMENT_HINT,
        removed_in: "0.418.9",
    },
    RemovedCommand {
        path: &["logs"],
        replacement: None,
        hint: NO_REPLACEMENT_HINT,
        removed_in: "0.418.9",
    },
    RemovedCommand {
        path: &["api"],
        replacement: None,
        hint: NO_REPLACEMENT_HINT,
        removed_in: "0.418.9",
    },
    RemovedCommand {
        path: &["topology"],
        replacement: None,
        hint: NO_REPLACEMENT_HINT,
        removed_in: "0.418.9",
    },
    RemovedCommand {
        path: &["agent-task", "loop"],
        replacement: None,
        hint: NO_REPLACEMENT_HINT,
        removed_in: "0.420.1",
    },
    RemovedCommand {
        path: &["agent-task", "cook-continue"],
        replacement: Some("homeboy agent-task recover"),
        hint: "`agent-task recover` is the Cook recovery verb.",
        removed_in: "0.429.1",
    },
    RemovedCommand {
        path: &["agent-task", "latest"],
        replacement: Some("homeboy agent-task list --latest"),
        hint: "Walk recent Cooks with `agent-task list --latest`.",
        removed_in: "0.433.23",
    },
    RemovedCommand {
        path: &["activity"],
        replacement: None,
        hint: NO_REPLACEMENT_HINT,
        removed_in: "0.434.23",
    },
    RemovedCommand {
        path: &["file"],
        replacement: None,
        hint: NO_REPLACEMENT_HINT,
        removed_in: "0.437.4",
    },
    RemovedCommand {
        path: &["tunnel"],
        replacement: None,
        hint: NO_REPLACEMENT_HINT,
        removed_in: "0.438.0",
    },
];

impl RemovedCommand {
    fn display_path(&self) -> String {
        format!("homeboy {}", self.path.join(" "))
    }

    fn replacement_label(&self) -> &'static str {
        self.replacement.unwrap_or("no replacement")
    }

    fn identity(&self) -> CommandIdentity {
        match self.path {
            [command] => CommandIdentity::top_level(*command),
            [command, rest @ ..] => CommandIdentity::with_operation(*command, rest.join(" ")),
            [] => CommandIdentity::top_level("unknown"),
        }
    }

    /// The typed `command.removed` error.
    pub(crate) fn error(&self) -> Error {
        Error::new(
            ErrorCode::CommandRemoved,
            format!(
                "`{}` was removed in Homeboy {} ({})",
                self.display_path(),
                self.removed_in,
                self.replacement_label()
            ),
            json!({
                "removed_path": self.path.join(" "),
                "replacement": self.replacement,
                "hint": self.hint,
                "removed_in": self.removed_in,
            }),
        )
        .with_hint(self.hint)
    }

    /// Human-readable stderr rendering of the removal.
    pub(crate) fn human_message(&self) -> String {
        format!(
            "error: `{}` was removed in Homeboy {}\n\nreplacement: {}\nhint: {}",
            self.display_path(),
            self.removed_in,
            self.replacement_label(),
            self.hint
        )
    }

    /// The `homeboy/command-result` envelope for the removal.
    pub(crate) fn envelope(&self) -> CommandResultEnvelope<Value> {
        output::cli_response_for_json_result_for_identity(
            &Err(self.error()),
            REMOVED_COMMAND_EXIT_CODE,
            &self.identity(),
            None,
        )
    }

    /// Print the human message to stderr and the JSON envelope to stdout,
    /// returning the process exit code.
    pub(crate) fn emit(&self) -> i32 {
        eprintln!("{}", self.human_message());
        output::print_response(&self.envelope()).ok();
        REMOVED_COMMAND_EXIT_CODE
    }
}

/// Match a clap invalid-subcommand error against the registry.
///
/// The rejected token must be the last segment of a registered path, and the
/// whole path must appear contiguously in argv.
pub(crate) fn find_removed_command(
    error: &clap::Error,
    argv: &[String],
) -> Option<&'static RemovedCommand> {
    use clap::error::{ContextKind, ErrorKind};

    if error.kind() != ErrorKind::InvalidSubcommand {
        return None;
    }
    let rejected = error.context().find_map(|(kind, value)| {
        matches!(kind, ContextKind::InvalidSubcommand).then(|| value.to_string())
    })?;
    let args = argv.get(1..).unwrap_or_default();
    REMOVED_COMMANDS.iter().find(|removed| {
        removed.path.last() == Some(&rejected.as_str())
            && args.windows(removed.path.len()).any(|window| {
                window
                    .iter()
                    .map(String::as_str)
                    .eq(removed.path.iter().copied())
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::Cli;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    fn parse_error(args: &[String]) -> clap::Error {
        Cli::command_with_scoped_lab_args()
            .try_get_matches_from(args)
            .expect_err("argv must be rejected by the CLI")
    }

    #[test]
    fn removed_rig_sync_yields_command_removed_error() {
        let args = argv(&[
            "homeboy",
            "rig",
            "sync",
            "static-site-importer-fixture-matrix",
        ]);
        let removed = find_removed_command(&parse_error(&args), &args)
            .expect("rig sync must match the removed-command registry");
        assert_eq!(removed.path, &["rig", "sync"]);

        let envelope = serde_json::to_value(removed.envelope()).expect("serialize envelope");
        assert_eq!(envelope["success"], false);
        assert_eq!(envelope["command"], "rig");
        let error = &envelope["diagnostics"];
        assert_eq!(error["code"], "command.removed");
        assert_eq!(error["details"]["removed_path"], "rig sync");
        assert_eq!(error["details"]["replacement"], Value::Null);
        assert_eq!(error["details"]["hint"], removed.hint);
        assert_eq!(error["details"]["removed_in"], "0.418.9");
        assert!(error["message"]
            .as_str()
            .is_some_and(|message| message.contains("no replacement")));

        let human = removed.human_message();
        assert!(human.contains("`homeboy rig sync` was removed in Homeboy 0.418.9"));
        assert!(human.contains("replacement: no replacement"));
        assert!(human.contains(removed.hint));
    }

    #[test]
    fn removed_command_with_global_flag_and_replacement_names_it() {
        let args = argv(&[
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "cook-continue",
        ]);
        let removed = find_removed_command(&parse_error(&args), &args)
            .expect("agent-task cook-continue must match the registry");
        let envelope = serde_json::to_value(removed.envelope()).expect("serialize envelope");
        assert_eq!(
            envelope["diagnostics"]["details"]["replacement"],
            "homeboy agent-task recover"
        );
        assert!(removed
            .human_message()
            .contains("replacement: homeboy agent-task recover"));
    }

    #[test]
    fn unknown_non_removed_command_keeps_clap_error() {
        let args = argv(&["homeboy", "rig", "definitely-not-a-command"]);
        let error = parse_error(&args);
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
        assert!(find_removed_command(&error, &args).is_none());
    }

    #[test]
    fn removed_registry_paths_are_not_live_commands() {
        for removed in REMOVED_COMMANDS {
            let mut args = vec!["homeboy".to_string()];
            args.extend(removed.path.iter().map(|segment| segment.to_string()));
            let error = parse_error(&args);
            assert_eq!(
                find_removed_command(&error, &args).map(|entry| entry.path),
                Some(removed.path),
                "{:?} must be rejected and matched",
                removed.path
            );
        }
    }
}
