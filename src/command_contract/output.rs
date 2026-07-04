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
use crate::commands::{
    changelog, file, fleet, logs, observe, report, review, runner, runtime, trace, version,
};

use super::lab::apply_lab_contract_to_descriptor;
use super::spec::registered_command;

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
pub enum CommandDispatchFamily {
    Quality,
    Workspace,
    Ops,
    RawOnly,
}

impl From<CommandJsonFamily> for CommandDispatchFamily {
    fn from(json_family: CommandJsonFamily) -> Self {
        match json_family {
            CommandJsonFamily::Quality => CommandDispatchFamily::Quality,
            CommandJsonFamily::Workspace => CommandDispatchFamily::Workspace,
            CommandJsonFamily::Ops => CommandDispatchFamily::Ops,
            CommandJsonFamily::RawOnly => CommandDispatchFamily::RawOnly,
        }
    }
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
    /// Shared output-routing spec (response mode, output-file mode, JSON family,
    /// output contract). Factored out of the per-struct field group so the same
    /// shape is reused by [`CommandOutputDescriptor`] and the command adapter
    /// contract.
    pub output: CommandOutputDescriptor,
    pub supports_lab_runner: bool,
    pub lab_runner_unsupported_reason: Option<&'static str>,
    pub lab_offload_captures_mutation_patch: bool,
    pub lab_offload_mutation_flag: Option<&'static str>,
}

impl CommandOutputDescriptor {
    pub const fn json_envelope(
        json_family: CommandJsonFamily,
        output_file_mode: CommandOutputFileMode,
    ) -> Self {
        Self {
            response_mode: CommandResponseMode::Json,
            output_file_mode,
            json_family,
            output_contract: CommandOutputContractKind::JsonEnvelope,
        }
    }

    fn with_lab_contract(
        self,
        contract: Option<super::lab::LabCommandContract>,
    ) -> CommandDescriptor {
        let mut descriptor = CommandDescriptor {
            output: self,
            supports_lab_runner: false,
            lab_runner_unsupported_reason: None,
            lab_offload_captures_mutation_patch: false,
            lab_offload_mutation_flag: None,
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
            Commands::Test(args) => args.output_descriptor(output_file_mode),
            Commands::Bench(args) => args.output_descriptor(output_file_mode),
            Commands::Fuzz(args) => args.output_descriptor(output_file_mode),
            Commands::Lint(args) => args.output_descriptor(output_file_mode),
            Commands::Audit(args) => args.output_descriptor(output_file_mode),
            Commands::Observe(_) => observe::adapter(output_file_mode).output_descriptor(),
            Commands::AuditBaseline(_) | Commands::Refactor(_) => {
                registered_json_envelope_descriptor(self, output_file_mode)
            }
            Commands::Refs(_) => workspace_descriptor(
                CommandResponseMode::Json,
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Version(_) => version::adapter(output_file_mode).output_descriptor(),
            Commands::Contract(_) => {
                crate::commands::contract::adapter(output_file_mode).output_descriptor()
            }
            Commands::Runner(args) if runner::is_compact_exec_stdout(args) => {
                raw_ops_descriptor(CommandRawOutputMode::PlainText, output_file_mode)
            }
            Commands::Activity(_)
            | Commands::AgentTask(_)
            | Commands::Project(_)
            | Commands::Component(_)
            | Commands::Config(_)
            | Commands::ArtifactPostprocess(_)
            | Commands::Extension(_)
            | Commands::Manifest(_)
            | Commands::Changelog(_)
            | Commands::Cleanup(_)
            | Commands::Build(_)
            | Commands::Changes(_)
            | Commands::Release(_)
            | Commands::Report(_)
            | Commands::Runner(_)
            | Commands::Runtime(_)
            | Commands::Worktree(_)
            | Commands::Tunnel(_)
            | Commands::Stack(_)
            | Commands::Undo(_) => registered_json_envelope_descriptor(self, output_file_mode),
            Commands::Rig(_) => registered_json_envelope_descriptor(self, output_file_mode),
            Commands::Status(_)
            | Commands::Ci(_)
            | Commands::Server(_)
            | Commands::Db(_)
            | Commands::Deps(_)
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
            | Commands::Ssh(_) => registered_json_envelope_descriptor(self, output_file_mode),
            Commands::Fleet(_) => fleet::adapter(output_file_mode).output_descriptor(),
            Commands::Triage(_) => registered_json_envelope_descriptor(self, output_file_mode),
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

fn registered_json_envelope_descriptor(
    command: &Commands,
    output_file_mode: CommandOutputFileMode,
) -> CommandOutputDescriptor {
    registered_command(command.top_level_name())
        .expect("top-level command should be registered")
        .output_descriptor(output_file_mode)
}
