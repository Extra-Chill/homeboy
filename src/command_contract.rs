//! Command contract aggregation point.
//!
//! `command_contract` is a thin shell that re-exports the public contract
//! surface from focused submodules. Keep this file as the canonical public
//! entry point — downstream code imports everything through
//! `crate::command_contract::*` or `homeboy::command_contract::*` — and put
//! implementation details in the matching submodule:
//!
//! - [`output`] owns response-mode, output-file, JSON-family,
//!   command-registry, output-descriptor, aggregate-descriptor,
//!   response-plan types, and the `Commands` impl that resolves them.
//! - [`lab`] owns Lab portability contracts and the `Commands` accessors
//!   that surface Lab fields on a descriptor.
//! - [`public_variants`] owns [`PublicOutputVariantContract`] and the
//!   [`PUBLIC_OUTPUT_VARIANT_CONTRACTS`] table that anchors public output
//!   variants to discriminators and golden fixtures.

mod lab;
mod output;
mod public_variants;

pub use lab::{
    lab_runner_support_summary, lab_runner_supported_contract_labels, lab_runner_supported_labels,
    lab_runner_supports_contract_label, lab_runner_unsupported_hint,
    lab_runner_unsupported_message, LabCommandContract, LabCommandPortability,
    LabCommandRequiredTool, LabCommandRouteContract, LabLocalExecutionPolicy, LabLocalHotPolicy,
    LabRoutingPolicy, LabRunnerSupportSummary, LabSelectedRunnerFallbackPolicy, LabSourcePathMode,
    LabWorkspaceModePolicy, RunnerWorkload, RunnerWorkloadArtifactRef, RunnerWorkloadAssignment,
    RunnerWorkloadCapability, RunnerWorkloadCommandFamily, RunnerWorkloadKind,
    RunnerWorkloadMutationPolicy, RunnerWorkloadResultRefs, RunnerWorkloadSecrets,
    RunnerWorkloadState, RunnerWorkloadWorkspaceMappings, LAB_TRACE_EXTRA_TOOLS,
    RUNNER_WORKLOAD_SCHEMA,
};
pub(crate) use lab::{
    AUDIT_LAB_LABEL, BENCH_LAB_LABEL, FUZZ_LAB_LABEL, LINT_LAB_LABEL, REVIEW_LAB_LABEL,
    TEST_LAB_LABEL,
};
pub use output::{
    registered_command, registered_command_dispatch_family, registered_command_json_family,
    CommandDescriptor, CommandDispatchFamily, CommandJsonFamily, CommandOutputContractKind,
    CommandOutputDescriptor, CommandOutputFileMode, CommandRawOutputMode, CommandRegistryEntry,
    CommandResponseMode, CommandResponsePlan, CommandStdoutMode, COMMAND_REGISTRY,
};
pub use public_variants::{PublicOutputVariantContract, PUBLIC_OUTPUT_VARIANT_CONTRACTS};
