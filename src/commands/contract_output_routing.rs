//! `impl Commands` output-descriptor routing split out of `output.rs`.
//!
//! These methods inspect the parsed CLI `Commands` enum to derive per-command
//! output descriptors and response plans. They depend on `cli_surface` and
//! command handler modules, so they are isolated here to keep `output.rs` (the
//! output contract type definitions) free of `commands` dependencies.

use crate::cli_surface::Commands;
use crate::command_contract::CommandSpec;
use crate::commands::{adapter, file, logs, report, review, runner, runtime, trace};

use crate::command_contract::{
    CommandDescriptor, CommandJsonFamily, CommandOutputContractKind, CommandOutputDescriptor,
    CommandOutputFileMode, CommandRawOutputMode, CommandResponseMode, CommandResponsePlan,
    CommandStdoutMode,
};

impl Commands {
    pub fn descriptor(&self, spec: &CommandSpec, has_output_file: bool) -> CommandDescriptor {
        self.output_descriptor(spec, has_output_file)
            .with_lab_contract(self.lab_contract())
    }

    pub fn output_descriptor(
        &self,
        spec: &CommandSpec,
        has_output_file: bool,
    ) -> CommandOutputDescriptor {
        let output_file_mode = if !has_output_file {
            CommandOutputFileMode::None
        } else {
            match self {
                Commands::Review(args) if args.command.is_none() => {
                    CommandOutputFileMode::ReviewStableArtifact
                }
                Commands::Trace(args) if args.json_summary => {
                    CommandOutputFileMode::TraceJsonSummaryArtifact
                }
                _ => CommandOutputFileMode::GenericEnvelope,
            }
        };

        if let Some(descriptor) = adapter::output_descriptor(self, output_file_mode) {
            return descriptor;
        }

        match self {
            Commands::Ssh(args) if args.subcommand.is_none() && args.command.is_empty() => {
                raw_ops_descriptor(
                    CommandRawOutputMode::InteractivePassthrough,
                    output_file_mode,
                )
            }
            Commands::Logs(args) if logs::is_interactive(args) => raw_ops_descriptor(
                CommandRawOutputMode::InteractivePassthrough,
                output_file_mode,
            ),
            Commands::File(args) if file::is_raw_read(args) => {
                raw_ops_descriptor(CommandRawOutputMode::PlainText, output_file_mode)
            }
            Commands::SelfCmd(args) if crate::commands::self_cmd::is_docs_markdown(args) => {
                workspace_descriptor(
                    CommandResponseMode::Raw(CommandRawOutputMode::Markdown),
                    output_file_mode,
                    CommandOutputContractKind::JsonEnvelope,
                )
            }
            Commands::Release(args) if args.is_changelog_markdown() => workspace_descriptor(
                CommandResponseMode::Raw(CommandRawOutputMode::Markdown),
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Review(args) => CommandOutputDescriptor {
                response_mode: markdown_or_json_response(review::is_markdown_mode(args)),
                output_file_mode,
                json_family: CommandJsonFamily::Quality,
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Trace(args) => CommandOutputDescriptor {
                response_mode: markdown_or_json_response(trace::is_markdown_mode(args)),
                output_file_mode,
                json_family: CommandJsonFamily::Quality,
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Runs(args) => workspace_descriptor(
                if !has_output_file && args.is_markdown_mode() {
                    CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
                } else {
                    CommandResponseMode::Json
                },
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Runtime(args) if runtime::is_plain_mode(args) => {
                raw_ops_descriptor(CommandRawOutputMode::PlainText, output_file_mode)
            }
            Commands::Report(args) if report::is_markdown_mode(args) => workspace_descriptor(
                CommandResponseMode::Raw(CommandRawOutputMode::Markdown),
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Bench(args) => args.output_descriptor(output_file_mode),
            Commands::Fuzz(args) => args.output_descriptor(output_file_mode),
            Commands::Runner(args) if runner::is_compact_exec_stdout(args) => {
                raw_ops_descriptor(CommandRawOutputMode::PlainText, output_file_mode)
            }
            Commands::Fleet(_) | Commands::Observe(_) | Commands::Contract(_) => {
                unreachable!("adapter-backed command descriptor returned before legacy routing")
            }
            _ => spec.output_descriptor(output_file_mode),
        }
    }

    pub fn response_plan(&self, spec: &CommandSpec, has_output_file: bool) -> CommandResponsePlan {
        let descriptor = self.output_descriptor(spec, has_output_file);

        CommandResponsePlan {
            stdout: match descriptor.response_mode {
                CommandResponseMode::Json => CommandStdoutMode::JsonEnvelope,
                CommandResponseMode::Raw(raw_mode) => CommandStdoutMode::Raw(raw_mode),
            },
            output_file: descriptor.output_file_mode,
        }
    }

    pub fn response_mode(&self, spec: &CommandSpec, has_output_file: bool) -> CommandResponseMode {
        self.output_descriptor(spec, has_output_file).response_mode
    }

    pub fn output_file_mode(
        &self,
        spec: &CommandSpec,
        has_output_file: bool,
    ) -> CommandOutputFileMode {
        self.output_descriptor(spec, has_output_file)
            .output_file_mode
    }

    pub fn consumes_output_file_as_command_arg(&self) -> bool {
        matches!(self, Commands::Runs(args) if args.is_artifact_get())
    }
}

fn raw_ops_descriptor(
    raw_mode: CommandRawOutputMode,
    output_file_mode: CommandOutputFileMode,
) -> CommandOutputDescriptor {
    CommandOutputDescriptor {
        response_mode: CommandResponseMode::Raw(raw_mode),
        output_file_mode,
        json_family: CommandJsonFamily::Ops,
        output_contract: CommandOutputContractKind::JsonEnvelope,
    }
}

fn workspace_descriptor(
    response_mode: CommandResponseMode,
    output_file_mode: CommandOutputFileMode,
    output_contract: CommandOutputContractKind,
) -> CommandOutputDescriptor {
    CommandOutputDescriptor {
        response_mode,
        output_file_mode,
        json_family: CommandJsonFamily::Workspace,
        output_contract,
    }
}

fn markdown_or_json_response(markdown: bool) -> CommandResponseMode {
    if markdown {
        CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
    } else {
        CommandResponseMode::Json
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::Cli;
    use clap::{CommandFactory, FromArgMatches};

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
                cli.command.output_descriptor(spec, false),
                spec.output_descriptor(CommandOutputFileMode::None),
                "default output descriptor drifted for `{}`",
                spec.name
            );
        }
    }
}
