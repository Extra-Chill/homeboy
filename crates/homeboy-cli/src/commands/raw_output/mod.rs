//! Execution of raw (non-JSON-envelope) command output.
//!
//! [`Commands::route`](crate::cli_surface::Commands) decides once whether a
//! command renders raw output and, when it does, hands over the bound
//! [`RawHandler`]. This module only executes that handler; it never re-checks
//! the command shape.

use crate::cli_surface::Commands;
use crate::command_contract::{CommandJsonFamily, CommandRawOutputMode};

use super::output_runtime::{CommandPresentation, CommandRun};
use super::utils::{response as output, tty};
use super::{component, docs, release, review, runner, runs, ssh, trace};

/// A command bound to the raw-output handler the routing decision selected.
pub(crate) enum RawHandler {
    /// `ssh <target>` with no command: requires a TTY, then runs the normal
    /// command path.
    SshInteractive(ssh::SshArgs),
    SelfDocs(docs::DocsArgs),
    ReleaseChangelog(release::changelog::ChangelogArgs),
    Review(review::ReviewArgs),
    Trace(trace::TraceArgs),
    Runs(runs::RunsMarkdown),
    ComponentChanged(component::ComponentChanged),
    Runner(runner::PlainTextRunner),
    Ssh(ssh::SshArgs),
}

impl RawHandler {
    pub(crate) fn mode(&self) -> CommandRawOutputMode {
        match self {
            Self::SshInteractive(_) => CommandRawOutputMode::InteractivePassthrough,
            Self::SelfDocs(_)
            | Self::ReleaseChangelog(_)
            | Self::Review(_)
            | Self::Trace(_)
            | Self::Runs(_) => CommandRawOutputMode::Markdown,
            Self::ComponentChanged(_) | Self::Runner(_) | Self::Ssh(_) => {
                CommandRawOutputMode::PlainText
            }
        }
    }

    pub(crate) fn json_family(&self) -> CommandJsonFamily {
        match self {
            Self::SelfDocs(_) | Self::ReleaseChangelog(_) | Self::Runs(_) => {
                CommandJsonFamily::Workspace
            }
            Self::Review(_) | Self::Trace(_) => CommandJsonFamily::Quality,
            Self::SshInteractive(_)
            | Self::ComponentChanged(_)
            | Self::Runner(_)
            | Self::Ssh(_) => CommandJsonFamily::Ops,
        }
    }
}

pub(crate) enum RawOutcome {
    /// The command finished without producing a run (exit code only).
    Exit(i32),
    /// The command continues on the JSON envelope path.
    Json(Box<Commands>),
    /// The raw output run to emit.
    Run(Box<CommandRun>),
}

pub(crate) fn run(handler: RawHandler, retain_raw_evidence: bool) -> RawOutcome {
    let run = match handler {
        RawHandler::SshInteractive(args) => return validate_interactive_tty(args),
        RawHandler::SelfDocs(args) => raw_stdout_only(docs::run_markdown(args)),
        RawHandler::ReleaseChangelog(args) => {
            raw_stdout_only(release::changelog::run_markdown(args))
        }
        RawHandler::Review(args) => review::raw_output::run_markdown_with_json(args),
        RawHandler::Trace(args) => trace::run_markdown_with_json_artifact(args),
        RawHandler::Runs(markdown) => raw_stdout_only(runs::run_markdown(markdown)),
        RawHandler::ComponentChanged(changed) => raw_stdout_only(component::run_changed(changed)),
        RawHandler::Runner(plain_text) => plain_text.run(),
        RawHandler::Ssh(args) => ssh_raw(args, retain_raw_evidence),
    };
    RawOutcome::Run(Box::new(run))
}

/// Emit only the remote command's stdout to local stdout and its stderr to local
/// stderr, exiting with the remote exit code (#9894).
fn ssh_raw(args: ssh::SshArgs, retain_raw_evidence: bool) -> CommandRun {
    match ssh::execute_raw_command(&args) {
        Ok(execution) => {
            let evidence = retain_raw_evidence.then(|| Ok(execution.output_file_evidence()));
            let completion = execution.completion_phase_stderr();
            CommandRun::from_raw_stdout_streaming(
                "ssh",
                Ok(execution.stdout),
                execution.exit_code,
                evidence,
            )
            .with_presentation(CommandPresentation {
                stdout: None,
                stderr: Some(execution.stderr),
            })
            .with_raw_completion_stderr(completion)
        }
        Err(err) => CommandRun::from_raw_stdout("ssh", Err(err), 1, None),
    }
}

fn raw_stdout_only(result: homeboy::core::Result<(String, i32)>) -> CommandRun {
    match result {
        Ok((content, exit_code)) => {
            CommandRun::from_raw_stdout("unknown", Ok(content), exit_code, None)
        }
        Err(err) => CommandRun::from_raw_stdout("unknown", Err(err), 1, None),
    }
}

fn validate_interactive_tty(args: ssh::SshArgs) -> RawOutcome {
    if tty::require_tty_for_interactive() {
        return RawOutcome::Json(Box::new(Commands::Ssh(args)));
    }

    let err = homeboy::core::Error::validation_invalid_argument(
        "tty",
        "This command requires an interactive TTY. For non-interactive usage, run: homeboy ssh <target> -- <command...>",
        None,
        None,
    );
    output::print_result::<serde_json::Value>(Err(err)).ok();
    RawOutcome::Exit(2)
}
