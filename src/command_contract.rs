//! Command contract aggregation point.
//!
//! `command_contract` is a thin shell that re-exports the public contract
//! surface from focused submodules. Keep this file as the canonical public
//! entry point — downstream code imports everything through
//! `crate::command_contract::*` or `homeboy::command_contract::*` — and put
//! implementation details in the matching submodule:
//!
//! - [`output`] owns response-mode, output-file, JSON-family,
//!   output-descriptor, aggregate-descriptor, response-plan types, and the
//!   `Commands` impl that resolves them.
//! - [`lab`] owns Lab portability contracts and the `Commands` accessors
//!   that surface Lab fields on a descriptor.
//! - [`public_variants`] owns [`PublicOutputVariantContract`] and the
//!   [`PUBLIC_OUTPUT_VARIANT_CONTRACTS`] table that anchors public output
//!   variants to discriminators and golden fixtures.

mod lab;
mod output;
mod public_variants;

pub use lab::{
    lab_runner_supported_labels, lab_runner_unsupported_hint, lab_runner_unsupported_message,
    LabCommandContract, LabCommandPortability, LabCommandRequiredTool, LabRoutingPolicy,
    LabSourcePathMode, LabWorkspaceModePolicy, LAB_TRACE_EXTRA_TOOLS,
};
pub use output::{
    CommandDescriptor, CommandJsonFamily, CommandOutputContractKind, CommandOutputDescriptor,
    CommandOutputFileMode, CommandRawOutputMode, CommandResponseMode, CommandResponsePlan,
    CommandStdoutMode,
};
pub use public_variants::{PublicOutputVariantContract, PUBLIC_OUTPUT_VARIANT_CONTRACTS};
