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

use super::lab_routing::apply_lab_contract_to_descriptor;
use super::spec::CommandSpec;

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

    pub(crate) fn with_lab_contract(
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
