//! `impl Commands` output-descriptor routing split out of `output.rs`.
//!
//! These methods inspect the parsed CLI `Commands` enum to derive per-command
//! output descriptors and response plans. They depend on `cli_surface` and
//! command handler modules, so they are isolated here to keep `output.rs` (the
//! output contract type definitions) free of `commands` dependencies.

use crate::cli_surface::Commands;
use crate::command_contract::CommandSpec;
use crate::commands::raw_output::RawHandler;
use crate::commands::{adapter, component, review, runner, self_cmd, ssh, trace};

use crate::command_contract::{
    CommandDescriptor, CommandJsonFamily, CommandOutputContractKind, CommandOutputDescriptor,
    CommandOutputFileMode, CommandResponseMode,
};

/// The single output-routing decision for a parsed command: its output
/// descriptor plus the command bound to the execution path that descriptor
/// promises.
pub(crate) struct CommandRoute {
    pub(crate) descriptor: CommandOutputDescriptor,
    pub(crate) execution: CommandExecution,
}

pub(crate) enum CommandExecution {
    Json(Box<Commands>),
    Raw(RawHandler),
}

impl CommandRoute {
    fn json(command: Commands, descriptor: CommandOutputDescriptor) -> Self {
        Self {
            descriptor,
            execution: CommandExecution::Json(Box::new(command)),
        }
    }

    fn raw(handler: RawHandler, output_file_mode: CommandOutputFileMode) -> Self {
        Self {
            descriptor: CommandOutputDescriptor {
                response_mode: CommandResponseMode::Raw(handler.mode()),
                output_file_mode,
                json_family: handler.json_family(),
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            execution: CommandExecution::Raw(handler),
        }
    }
}

impl Commands {
    pub fn descriptor(self, spec: &CommandSpec, has_output_file: bool) -> CommandDescriptor {
        let lab_contract = self.lab_contract();
        self.route(spec, has_output_file)
            .descriptor
            .with_lab_contract(lab_contract)
    }

    pub(crate) fn route(self, spec: &CommandSpec, has_output_file: bool) -> CommandRoute {
        let output_file_mode = if !has_output_file {
            CommandOutputFileMode::None
        } else {
            match &self {
                Commands::Review(args) if args.command.is_none() => {
                    CommandOutputFileMode::ReviewStableArtifact
                }
                Commands::Trace(args) if args.json_summary => {
                    CommandOutputFileMode::TraceJsonSummaryArtifact
                }
                _ => CommandOutputFileMode::GenericEnvelope,
            }
        };

        if let Some(descriptor) = adapter::output_descriptor(&self, output_file_mode) {
            return CommandRoute::json(self, descriptor);
        }

        let raw = |handler| CommandRoute::raw(handler, output_file_mode);
        let quality_json = |command| {
            CommandRoute::json(
                command,
                CommandOutputDescriptor::json_envelope(
                    CommandJsonFamily::Quality,
                    output_file_mode,
                ),
            )
        };

        match self {
            Commands::Component(args) => match component::into_changed(args) {
                Ok(changed) => raw(RawHandler::ComponentChanged(changed)),
                Err(args) => CommandRoute::json(
                    Commands::Component(args),
                    spec.output_descriptor(output_file_mode),
                ),
            },
            Commands::Ssh(args) if ssh::is_raw_command(&args) => raw(RawHandler::Ssh(args)),
            Commands::Ssh(args) if args.subcommand.is_none() && args.command.is_empty() => {
                raw(RawHandler::SshInteractive(args))
            }
            Commands::SelfCmd(args) => match self_cmd::into_docs_markdown(args) {
                Ok(docs) => raw(RawHandler::SelfDocs(docs)),
                Err(args) => CommandRoute::json(
                    Commands::SelfCmd(args),
                    spec.output_descriptor(output_file_mode),
                ),
            },
            Commands::Release(args) => match args.into_markdown_changelog() {
                Ok(changelog) => raw(RawHandler::ReleaseChangelog(changelog)),
                Err(args) => CommandRoute::json(
                    Commands::Release(args),
                    spec.output_descriptor(output_file_mode),
                ),
            },
            Commands::Review(args) if review::is_markdown_mode(&args) => {
                raw(RawHandler::Review(args))
            }
            Commands::Review(args) => quality_json(Commands::Review(args)),
            Commands::Trace(args) if trace::is_markdown_mode(&args) => raw(RawHandler::Trace(args)),
            Commands::Trace(args) => quality_json(Commands::Trace(args)),
            Commands::Runs(args) => {
                let markdown = if has_output_file {
                    Err(args)
                } else {
                    args.into_markdown()
                };
                match markdown {
                    Ok(markdown) => raw(RawHandler::Runs(markdown)),
                    Err(args) => CommandRoute::json(
                        Commands::Runs(args),
                        CommandOutputDescriptor::json_envelope(
                            CommandJsonFamily::Workspace,
                            output_file_mode,
                        ),
                    ),
                }
            }
            Commands::Bench(args) => {
                let descriptor = args.output_descriptor(output_file_mode);
                CommandRoute::json(Commands::Bench(args), descriptor)
            }
            Commands::Fuzz(args) => {
                let descriptor = args.output_descriptor(output_file_mode);
                CommandRoute::json(Commands::Fuzz(args), descriptor)
            }
            Commands::Workload(args) => {
                let descriptor = args.output_descriptor(output_file_mode);
                CommandRoute::json(Commands::Workload(args), descriptor)
            }
            Commands::Runner(args) => match runner::into_plain_text(args) {
                Ok(plain_text) => raw(RawHandler::Runner(plain_text)),
                Err(args) => CommandRoute::json(
                    Commands::Runner(args),
                    spec.output_descriptor(output_file_mode),
                ),
            },
            Commands::Contract(_) => {
                unreachable!("adapter-backed command descriptor returned before legacy routing")
            }
            command => {
                let descriptor = spec.output_descriptor(output_file_mode);
                CommandRoute::json(command, descriptor)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::Cli;
    use crate::command_contract::CommandRawOutputMode;
    use clap::{CommandFactory, FromArgMatches, Parser};

    #[test]
    fn representative_default_output_descriptors_come_from_command_registry() {
        for spec in crate::command_contract::COMMAND_SPECS {
            let Some(argv) = spec.representative_argv else {
                continue;
            };
            let matches = Cli::command()
                .try_get_matches_from(argv)
                .unwrap_or_else(|error| panic!("failed to parse `{}`: {error}", spec.name));
            assert_eq!(matches.subcommand_name(), Some(spec.name));
            let cli = Cli::from_arg_matches(&matches).expect("validated arguments should parse");

            assert_eq!(
                cli.command.route(spec, false).descriptor,
                spec.output_descriptor(CommandOutputFileMode::None),
                "default output descriptor drifted for `{}`",
                spec.name
            );
        }
    }

    #[test]
    fn subcommand_output_overrides_route_raw_responses() {
        let cases: [(&str, &[&str], CommandRawOutputMode); 4] = [
            (
                "component",
                &[
                    "homeboy",
                    "component",
                    "changed",
                    "--base",
                    "HEAD^",
                    "--head",
                    "HEAD",
                    "--format",
                    "json",
                ],
                CommandRawOutputMode::PlainText,
            ),
            (
                "component",
                &[
                    "homeboy",
                    "component",
                    "changed",
                    "--base",
                    "HEAD^",
                    "--head",
                    "HEAD",
                    "--format",
                    "matrix",
                ],
                CommandRawOutputMode::PlainText,
            ),
            (
                "component",
                &[
                    "homeboy",
                    "component",
                    "changed",
                    "--base",
                    "HEAD^",
                    "--head",
                    "HEAD",
                    "--format",
                    "ids",
                ],
                CommandRawOutputMode::PlainText,
            ),
            (
                "runs",
                &[
                    "homeboy",
                    "runs",
                    "report",
                    "failure-digest",
                    "--output-dir",
                    "artifacts",
                    "--results",
                    r#"{"audit":"pass"}"#,
                ],
                CommandRawOutputMode::Markdown,
            ),
        ];
        for (name, argv, raw_mode) in cases {
            let spec = crate::command_contract::COMMAND_SPECS
                .iter()
                .find(|spec| spec.name == name)
                .expect("command spec");
            let cli = Cli::try_parse_from(argv).expect("invocation should parse");
            assert_eq!(
                cli.command.route(spec, false).descriptor.response_mode,
                CommandResponseMode::Raw(raw_mode),
                "{argv:?}"
            );
        }
    }
}
