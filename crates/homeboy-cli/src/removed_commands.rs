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
        path: &["agent-task", "resume"],
        replacement: Some("homeboy agent-task recover <id>"),
        hint: "Recover resumes queued and stale-running durable runs, including non-Cook runs.",
        removed_in: "0.474.7",
    },
    RemovedCommand {
        path: &["agent-task", "adopt"],
        replacement: Some("homeboy agent-task recover <cook> --candidate-ref <sha>"),
        hint: "Use recover --candidate-ref; adoption's --model is now --adoption-model, distinct from the retry route override.",
        removed_in: "0.474.7",
    },
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
        path: &["agent-task", "promote"],
        replacement: Some("homeboy agent-task recover"),
        hint: "Cook promotes its own candidates; continue a stopped Cook with `agent-task recover <cook>`.",
        removed_in: "0.474.4",
    },
    RemovedCommand {
        path: &["agent-task", "gate-feedback"],
        replacement: Some("homeboy agent-task recover"),
        hint: "Cook runs deterministic gate feedback automatically; continue a stopped Cook with `agent-task recover <cook>`.",
        removed_in: "0.474.4",
    },
    RemovedCommand {
        path: &["agent-task", "quarantine"],
        replacement: Some("homeboy agent-task recover"),
        hint: "Recover performs the next step for a stopped run; queued runs that fail admission stay queued for the next scan.",
        removed_in: "0.474.2",
    },
    RemovedCommand {
        path: &["agent-task", "rearm"],
        replacement: Some("homeboy agent-task recover"),
        hint: "Recover rearms a failed continuation when the Cook's durable state names that step.",
        removed_in: "0.474.2",
    },
    RemovedCommand {
        path: &["agent-task", "replay-provider-boundary"],
        replacement: Some("homeboy agent-task recover"),
        hint: "Recover continues a Cook from its recorded provider boundary.",
        removed_in: "0.474.2",
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
    fn removed_command_yields_typed_error_naming_its_replacement() {
        let cases: [(&[&str], &[&str], Value, &str); 2] = [
            (
                &[
                    "homeboy",
                    "rig",
                    "sync",
                    "static-site-importer-fixture-matrix",
                ],
                &["rig", "sync"],
                Value::Null,
                "no replacement",
            ),
            // A global flag before the path does not hide the removal.
            (
                &[
                    "homeboy",
                    "--placement",
                    "local",
                    "agent-task",
                    "cook-continue",
                ],
                &["agent-task", "cook-continue"],
                json!("homeboy agent-task recover"),
                "homeboy agent-task recover",
            ),
        ];
        for (args, path, replacement, replacement_label) in cases {
            let args = argv(args);
            let removed = find_removed_command(&parse_error(&args), &args)
                .unwrap_or_else(|| panic!("{path:?} must match the removed-command registry"));
            assert_eq!(removed.path, path);

            let envelope = serde_json::to_value(removed.envelope()).expect("serialize envelope");
            assert_eq!(envelope["success"], false);
            assert_eq!(envelope["command"], path[0]);
            let error = &envelope["diagnostics"];
            assert_eq!(error["code"], "command.removed");
            assert_eq!(error["details"]["removed_path"], path.join(" "));
            assert_eq!(error["details"]["replacement"], replacement);
            assert_eq!(error["details"]["hint"], removed.hint);
            assert_eq!(error["details"]["removed_in"], removed.removed_in);

            let human = removed.human_message();
            assert!(human.contains(&format!(
                "`homeboy {}` was removed in Homeboy {}",
                path.join(" "),
                removed.removed_in
            )));
            assert!(human.contains(&format!("replacement: {replacement_label}")));
        }
    }

    #[test]
    fn unknown_non_removed_command_keeps_clap_error() {
        let args = argv(&["homeboy", "rig", "definitely-not-a-command"]);
        let error = parse_error(&args);
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
        assert!(find_removed_command(&error, &args).is_none());
    }
}
