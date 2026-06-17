//! Output contract types and routing logic.
//!
//! This module owns the shapes that describe how a command emits output
//! (`CommandResponseMode`, `CommandStdoutMode`, `CommandOutputFileMode`,
//! `CommandJsonFamily`, `CommandOutputContractKind`) plus the output-only
//! [`CommandOutputDescriptor`], aggregate [`CommandDescriptor`],
//! [`CommandResponsePlan`], and the `Commands` impl that resolves a parsed CLI
//! command into these contracts.
//!
//! Lab-specific fields on [`CommandDescriptor`] are populated by
//! [`crate::command_contract::lab`], which post-processes the descriptor
//! returned from [`Commands::descriptor`].

use crate::cli_surface::Commands;
use crate::commands::{changelog, file, logs, report, review, runtime, trace, version};

use super::lab::apply_lab_contract_to_descriptor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandResponseMode {
    Json,
    Raw(CommandRawOutputMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRawOutputMode {
    InteractivePassthrough,
    Markdown,
    PlainText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStdoutMode {
    JsonEnvelope,
    Raw(CommandRawOutputMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutputFileMode {
    None,
    GenericEnvelope,
    ReviewStableArtifact,
    TraceJsonSummaryArtifact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandJsonFamily {
    Quality,
    Workspace,
    Ops,
    RawOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutputContractKind {
    JsonEnvelope,
    RawOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandOutputDescriptor {
    pub response_mode: CommandResponseMode,
    pub output_file_mode: CommandOutputFileMode,
    pub json_family: CommandJsonFamily,
    pub output_contract: CommandOutputContractKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDescriptor {
    pub response_mode: CommandResponseMode,
    pub output_file_mode: CommandOutputFileMode,
    pub json_family: CommandJsonFamily,
    pub supports_lab_runner: bool,
    pub lab_runner_unsupported_reason: Option<&'static str>,
    pub lab_offload_mutation_flag: Option<&'static str>,
    pub output_contract: CommandOutputContractKind,
}

impl CommandOutputDescriptor {
    pub fn with_lab_contract(
        self,
        contract: Option<super::lab::LabCommandContract>,
    ) -> CommandDescriptor {
        let mut descriptor = CommandDescriptor {
            response_mode: self.response_mode,
            output_file_mode: self.output_file_mode,
            json_family: self.json_family,
            supports_lab_runner: false,
            lab_runner_unsupported_reason: None,
            lab_offload_mutation_flag: None,
            output_contract: self.output_contract,
        };
        apply_lab_contract_to_descriptor(&mut descriptor, contract);
        descriptor
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandResponsePlan {
    pub stdout: CommandStdoutMode,
    pub output_file: CommandOutputFileMode,
}

impl Commands {
    pub fn descriptor(&self, has_output_file: bool) -> CommandDescriptor {
        self.output_descriptor(has_output_file)
            .with_lab_contract(self.lab_contract())
    }

    pub fn output_descriptor(&self, has_output_file: bool) -> CommandOutputDescriptor {
        let output_file_mode = if !has_output_file {
            CommandOutputFileMode::None
        } else {
            match self {
                Commands::Review(_) => CommandOutputFileMode::ReviewStableArtifact,
                Commands::Trace(args) if args.json_summary => {
                    CommandOutputFileMode::TraceJsonSummaryArtifact
                }
                _ => CommandOutputFileMode::GenericEnvelope,
            }
        };

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
            Commands::Docs(args) => workspace_descriptor(
                if crate::commands::docs::is_json_mode(args) {
                    CommandResponseMode::Json
                } else {
                    CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
                },
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Changelog(args) if changelog::is_show_markdown(args) => workspace_descriptor(
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
            Commands::List => CommandOutputDescriptor {
                response_mode: CommandResponseMode::Raw(CommandRawOutputMode::Markdown),
                output_file_mode,
                json_family: CommandJsonFamily::RawOnly,
                output_contract: CommandOutputContractKind::RawOnly,
            },
            Commands::Test(args) => args.output_descriptor(output_file_mode),
            Commands::Bench(args) => args.output_descriptor(output_file_mode),
            Commands::Lint(args) => args.output_descriptor(output_file_mode),
            Commands::Audit(args) => args.output_descriptor(output_file_mode),
            Commands::Observe(_) => {
                quality_json_descriptor(output_file_mode, CommandOutputContractKind::JsonEnvelope)
            }
            Commands::AuditBaseline(_) => {
                quality_json_descriptor(output_file_mode, CommandOutputContractKind::JsonEnvelope)
            }
            Commands::Refactor(_) => CommandOutputDescriptor {
                response_mode: CommandResponseMode::Json,
                output_file_mode,
                json_family: CommandJsonFamily::Workspace,
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Refs(_) => workspace_descriptor(
                CommandResponseMode::Json,
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Version(_) => version::adapter(output_file_mode)
                .contract
                .to_output_descriptor(),
            Commands::AgentTask(_)
            | Commands::Project(_)
            | Commands::Component(_)
            | Commands::Config(_)
            | Commands::Extension(_)
            | Commands::Changelog(_)
            | Commands::Cleanup(_)
            | Commands::Build(_)
            | Commands::Changes(_)
            | Commands::Release(_)
            | Commands::Report(_)
            | Commands::Lab(_)
            | Commands::Runner(_)
            | Commands::Runtime(_)
            | Commands::Worktree(_)
            | Commands::Tunnel(_)
            | Commands::Stack(_)
            | Commands::Undo(_) => CommandOutputDescriptor {
                response_mode: CommandResponseMode::Json,
                output_file_mode,
                json_family: CommandJsonFamily::Workspace,
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Rig(_) => CommandOutputDescriptor {
                response_mode: CommandResponseMode::Json,
                output_file_mode,
                json_family: CommandJsonFamily::Workspace,
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Status(_)
            | Commands::Ci(_)
            | Commands::Server(_)
            | Commands::Db(_)
            | Commands::Deps(_)
            | Commands::Doctor(_)
            | Commands::File(_)
            | Commands::Logs(_)
            | Commands::Deploy(_)
            | Commands::Daemon(_)
            | Commands::Git(_)
            | Commands::Issues(_)
            | Commands::SelfCmd(_)
            | Commands::Auth(_)
            | Commands::Api(_)
            | Commands::Http(_)
            | Commands::Upgrade(_)
            | Commands::Ssh(_) => ops_json_descriptor(output_file_mode, None),
            Commands::Fleet(args) => args.output_descriptor(output_file_mode),
            Commands::Triage(_) => ops_json_descriptor(output_file_mode, None),
        }
    }

    pub fn response_plan(&self, has_output_file: bool) -> CommandResponsePlan {
        let descriptor = self.output_descriptor(has_output_file);

        CommandResponsePlan {
            stdout: match descriptor.response_mode {
                CommandResponseMode::Json => CommandStdoutMode::JsonEnvelope,
                CommandResponseMode::Raw(raw_mode) => CommandStdoutMode::Raw(raw_mode),
            },
            output_file: descriptor.output_file_mode,
        }
    }

    pub fn response_mode(&self, has_output_file: bool) -> CommandResponseMode {
        self.output_descriptor(has_output_file).response_mode
    }

    pub fn output_file_mode(&self, has_output_file: bool) -> CommandOutputFileMode {
        self.output_descriptor(has_output_file).output_file_mode
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

fn quality_json_descriptor(
    output_file_mode: CommandOutputFileMode,
    output_contract: CommandOutputContractKind,
) -> CommandOutputDescriptor {
    CommandOutputDescriptor {
        response_mode: CommandResponseMode::Json,
        output_file_mode,
        json_family: CommandJsonFamily::Quality,
        output_contract,
    }
}

fn ops_json_descriptor(
    output_file_mode: CommandOutputFileMode,
    _lab_runner_unsupported_reason: Option<&'static str>,
) -> CommandOutputDescriptor {
    CommandOutputDescriptor {
        response_mode: CommandResponseMode::Json,
        output_file_mode,
        json_family: CommandJsonFamily::Ops,
        output_contract: CommandOutputContractKind::JsonEnvelope,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;

    fn parsed_command(args: &[&str]) -> Commands {
        Cli::try_parse_from(args)
            .expect("CLI args should parse")
            .command
    }

    #[test]
    fn test_response_mode() {
        assert_eq!(
            parsed_command(&["homeboy", "status"]).response_mode(false),
            CommandResponseMode::Json
        );
        assert_eq!(
            parsed_command(&["homeboy", "review", "--report", "pr-comment"]).response_mode(false),
            CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
        );
        assert_eq!(
            parsed_command(&["homeboy", "trace", "--report", "markdown"]).response_mode(false),
            CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
        );
        assert_eq!(
            Commands::List.response_mode(false),
            CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
        );
    }

    #[test]
    fn test_command_descriptor_drives_behavioral_routing() {
        let bench = parsed_command(&["homeboy", "bench"]);
        let bench_descriptor = bench.descriptor(false);
        assert_eq!(bench_descriptor.json_family, CommandJsonFamily::Quality);
        assert!(bench_descriptor.supports_lab_runner);
        assert_eq!(
            bench_descriptor.output_contract,
            CommandOutputContractKind::JsonEnvelope
        );

        let runs = parsed_command(&["homeboy", "runs", "list"]);
        let runs_descriptor = runs.descriptor(false);
        assert_eq!(runs_descriptor.json_family, CommandJsonFamily::Workspace);
        assert_eq!(runs_descriptor.response_mode, CommandResponseMode::Json);
        assert_eq!(
            runs_descriptor.output_contract,
            CommandOutputContractKind::JsonEnvelope
        );

        let list_descriptor = Commands::List.descriptor(false);
        assert_eq!(list_descriptor.json_family, CommandJsonFamily::RawOnly);
        assert_eq!(
            list_descriptor.response_mode,
            CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
        );
    }

    #[test]
    fn output_descriptor_excludes_lab_policy() {
        let scoped_lint = parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"]);
        let output_descriptor = scoped_lint.output_descriptor(false);

        assert_eq!(output_descriptor.json_family, CommandJsonFamily::Quality);
        assert_eq!(output_descriptor.response_mode, CommandResponseMode::Json);
        assert_eq!(
            output_descriptor.output_contract,
            CommandOutputContractKind::JsonEnvelope
        );

        let aggregate_descriptor = scoped_lint.descriptor(false);
        assert!(!aggregate_descriptor.supports_lab_runner);
        assert!(aggregate_descriptor
            .lab_runner_unsupported_reason
            .is_some_and(|reason| reason.contains("Changed-scope lint")));
    }

    #[test]
    fn test_response_plan() {
        assert_eq!(
            parsed_command(&["homeboy", "status"]).response_plan(false),
            CommandResponsePlan {
                stdout: CommandStdoutMode::JsonEnvelope,
                output_file: CommandOutputFileMode::None,
            }
        );

        assert_eq!(
            parsed_command(&["homeboy", "review", "--report", "pr-comment"]).response_plan(false),
            CommandResponsePlan {
                stdout: CommandStdoutMode::Raw(CommandRawOutputMode::Markdown),
                output_file: CommandOutputFileMode::None,
            }
        );

        assert_eq!(
            parsed_command(&["homeboy", "review", "--report", "pr-comment"]).response_plan(true),
            CommandResponsePlan {
                stdout: CommandStdoutMode::Raw(CommandRawOutputMode::Markdown),
                output_file: CommandOutputFileMode::ReviewStableArtifact,
            }
        );

        assert_eq!(
            parsed_command(&["homeboy", "trace", "--report", "markdown", "--json-summary",])
                .response_plan(true),
            CommandResponsePlan {
                stdout: CommandStdoutMode::Raw(CommandRawOutputMode::Markdown),
                output_file: CommandOutputFileMode::TraceJsonSummaryArtifact,
            }
        );
    }

    #[test]
    fn artifact_get_output_flag_is_command_payload_destination() {
        assert!(parsed_command(&[
            "homeboy",
            "runs",
            "artifact",
            "get",
            "run-1",
            "artifact-1",
            "-o",
            "artifact.bin",
        ])
        .consumes_output_file_as_command_arg());

        assert!(!parsed_command(&["homeboy", "status"]).consumes_output_file_as_command_arg());
    }

    #[test]
    fn test_output_artifact_policy() {
        assert_eq!(
            parsed_command(&["homeboy", "status"]).output_file_mode(true),
            CommandOutputFileMode::GenericEnvelope
        );
        assert_eq!(
            parsed_command(&["homeboy", "review"]).output_file_mode(true),
            CommandOutputFileMode::ReviewStableArtifact
        );
        assert_eq!(
            parsed_command(&["homeboy", "trace", "--json-summary"]).output_file_mode(true),
            CommandOutputFileMode::TraceJsonSummaryArtifact
        );
        assert_eq!(
            parsed_command(&["homeboy", "trace", "--json-summary"]).output_file_mode(false),
            CommandOutputFileMode::None
        );
    }
}
