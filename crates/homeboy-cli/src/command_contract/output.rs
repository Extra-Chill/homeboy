//! Output contract types and routing logic.
//!
//! This module owns the shapes that describe how a command emits output
//! (`CommandResponseMode`, `CommandOutputFileMode`, `CommandJsonFamily`,
//! `CommandOutputContractKind`) plus the output-only
//! [`CommandOutputDescriptor`], aggregate [`CommandDescriptor`],
//! [`CommandResponsePlan`], and the `Commands` impl that resolves a parsed CLI
//! command into these contracts.
//!
//! Lab-specific fields on [`CommandDescriptor`] are populated by
//! [`crate::command_contract::lab`], which post-processes the descriptor
//! returned from [`Commands::descriptor`].

use super::lab::{LabCommandContract, LabCommandPortability};
use super::spec::CommandSpec;

/// Populate the Lab-offload fields on a [`CommandDescriptor`] from a resolved
/// [`LabCommandContract`]. Pure data transformation over contract/descriptor
/// types (no CLI dependency), so it lives with the descriptor definitions
/// rather than the CLI-coupled routing in `lab/placement.rs`.
fn apply_lab_contract_to_descriptor(
    descriptor: &mut CommandDescriptor,
    contract: Option<LabCommandContract>,
) {
    descriptor.supports_lab_runner = contract
        .is_some_and(|contract| matches!(contract.portability, LabCommandPortability::Portable));
    descriptor.lab_runner_unsupported_reason =
        contract.and_then(|contract| match contract.portability {
            LabCommandPortability::Portable => None,
            LabCommandPortability::LocalOnly(reason) => Some(reason),
        });
    descriptor.lab_offload_captures_mutation_patch =
        contract.is_some_and(|contract| contract.capture_mutation_patch);
    descriptor.lab_offload_mutation_flag = contract.and_then(|contract| contract.mutation_flag);
}

/// How a command emits its response, both as the descriptor's declared mode and
/// as the resolved stdout plan.
///
/// One type, not two: the former `CommandStdoutMode` had the same shape and was
/// produced only by an identity mapping from this enum (`Json` ->
/// `JsonEnvelope`, `Raw(m)` -> `Raw(m)`). The two exported contract strings that
/// distinguished them (`response_mode: "json"` vs `stdout_mode: "json_envelope"`)
/// are unchanged and are now produced by two renderers over this one type.
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

/// The output family a command belongs to.
///
/// Also the dispatch family: the former `CommandDispatchFamily` carried a
/// byte-identical variant list and was only ever produced by an identity `From`
/// impl over this enum, so every command's `json_family` and `dispatch_family`
/// were the same value by construction.
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutputContractKind {
    JsonEnvelope,
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
    pub stdout: CommandResponseMode,
    pub output_file: CommandOutputFileMode,
}
