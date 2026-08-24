//! Command contract aggregation point.
//!
//! `command_contract` is a thin shell that re-exports the public contract
//! surface from focused submodules. Keep this file as the canonical public
//! entry point — downstream code imports everything through
//! `crate::command_contract::*` or `homeboy::command_contract::*` — and put
//! implementation details in the matching submodule:
//!
//! - [`spec`] owns shared top-level command metadata consumed by output,
//!   safety/docs manifests, and command lookup.
//! - [`output`] owns response-mode, output-file, JSON-family,
//!   output-descriptor, aggregate-descriptor,
//!   response-plan types, and the `Commands` impl that resolves them.
//! - [`lab`] owns Lab portability contracts and the `Commands` accessors
//!   that surface Lab fields on a descriptor.
//! - [`public_variants`] owns [`PublicOutputVariantContract`] and the
//!   [`PUBLIC_OUTPUT_VARIANT_CONTRACTS`] table that anchors public output
//!   variants to discriminators and golden fixtures.

mod constants;
mod descriptors;
pub mod export;
mod lab;
mod output;
mod public_variants;
mod registry;
mod spec;

pub use crate::commands::contract_lab_routing::LabCommandRoute;
pub use crate::core::artifact_ref::{
    validate_reviewer_facing_artifact_ref, ArtifactReference, ReviewerFacingArtifactRefError,
};
pub(crate) use constants::{contract_constants, ContractConstantsOutput};
pub use lab::LabCommandRouteSupport;
/// Only `commands::contract_lab_routing_tests` reads this; the shipped binary
/// matches on the contract rather than the mode, so it is compiled out of the
/// lib build instead of carrying an unused re-export.
#[cfg(test)]
pub(crate) use lab::LabSourcePathMode;
pub(crate) use lab::{
    lab_runner_support_summary, scope_composed_lab_cli_arguments, scope_lab_cli_arguments,
    LabRigWorkloadKind, LabWorkspaceModePolicy, LAB_RUNNER_HANDOFF_ENVELOPE_SCHEMA,
    LAB_RUNNER_WORKLOAD_SCHEMA, LAB_TRACE_EXTRA_CAPABILITIES, RUNNER_ARTIFACT_MANIFEST_FILE,
    RUNNER_ARTIFACT_MANIFEST_REF_NAME, RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
    RUNNER_ARTIFACT_MANIFEST_SCHEMA, RUNNER_ARTIFACT_ROOT_DIR_SUFFIX, RUN_LOCATION_INDEX_SCHEMA,
};
pub use lab::{
    CommandPortabilityContract, LabCommandContract, LabCommandPortability, LabCommandRouteContract,
    LabRigWorkloadArguments,
};
pub(crate) use lab::{
    LAB_AGENT_TASK_SECRET_ENV_SOURCES, LAB_NO_EXTRA_CAPABILITIES, LAB_TRACE_SECRET_ENV_SOURCES,
    LAB_TUNNEL_SECRET_ENV_SOURCES, RIG_SOURCE_MANAGEMENT_LAB_UNSUPPORTED_REASON,
    RIG_UP_LAB_UNSUPPORTED_REASON,
};
// Lab-label constants needed by the relocated lab routing in
// commands::contract_lab_routing (the spec module itself stays private).
pub use output::{
    CommandDescriptor, CommandJsonFamily, CommandOutputContractKind, CommandOutputDescriptor,
    CommandOutputFileMode, CommandRawOutputMode, CommandResponseMode, CommandResponsePlan,
};
pub use public_variants::{PublicOutputVariantContract, PUBLIC_OUTPUT_VARIANT_CONTRACTS};
pub(crate) use registry::{registered_contract, registered_contracts, ContractRegistryEntry};
pub(crate) use spec::{
    registered_command, runtime_extension_command_doc_slugs, CommandSafetySpec, CommandSpec,
    COMMAND_SPECS,
};
pub(crate) use spec::{
    AGENT_TASK_AUTH_STATUS_LAB_LABEL, AGENT_TASK_CONTROLLER_FROM_SPEC_LAB_LABEL,
    AGENT_TASK_CONTROLLER_RESUME_LAB_LABEL, AGENT_TASK_FANOUT_COOK_BATCH_LAB_LABEL,
    AGENT_TASK_FANOUT_RUN_PLAN_LAB_LABEL, AGENT_TASK_FANOUT_STATUS_LAB_LABEL,
    AGENT_TASK_FANOUT_SUBMIT_BATCH_LAB_LABEL, AGENT_TASK_PROMOTE_LAB_LABEL,
    AGENT_TASK_PROVIDERS_LAB_LABEL, AGENT_TASK_RUN_LAB_LABEL, AGENT_TASK_STATUS_LAB_LABEL,
    RUNTIME_REFRESH_LAB_LABEL,
};
pub(crate) use spec::{
    AUDIT_LAB_LABEL, BENCH_LAB_LABEL, FUZZ_DOCTOR_LAB_LABEL, FUZZ_LAB_LABEL, LINT_LAB_LABEL,
    REVIEW_LAB_LABEL, RIG_CHECK_LAB_LABEL, RIG_RUN_LAB_LABEL, RIG_SOURCE_MANAGEMENT_LAB_LABEL,
    TEST_LAB_LABEL, TRACE_LAB_LABEL, TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL,
    TUNNEL_SERVICE_EXPOSE_LAB_LABEL, TUNNEL_SERVICE_START_LAB_LABEL, WORKTREE_CLEANUP_LAB_LABEL,
};

pub use crate::core::artifacts::{
    ArtifactPostprocessAction, ArtifactPostprocessPlan, ArtifactPostprocessPlanDescription,
    ArtifactPostprocessResult, ArtifactPostprocessReviewerRef, ArtifactPostprocessRoot,
    ARTIFACT_POSTPROCESS_PLAN_SCHEMA, ARTIFACT_POSTPROCESS_RESULT_SCHEMA,
    ARTIFACT_POSTPROCESS_SCHEMA, RUNTIME_AGENT_ARTIFACT_PATHS_SCHEMA,
    RUNTIME_AGENT_FINAL_OUTPUT_ARTIFACT_PATH, RUNTIME_AGENT_PATCH_DIFF_ARTIFACT_FILE,
    RUNTIME_AGENT_PATCH_PATCH_ARTIFACT_FILE, RUNTIME_AGENT_RESULT_ARTIFACT_FILE,
    RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_FILE, RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_PATH,
    RUN_ARTIFACT_EVENTS_FILE, RUN_ARTIFACT_FANOUT_RUN_FILE, RUN_ARTIFACT_LOOP_POLICY_FILE,
    RUN_ARTIFACT_LOOP_RESULT_FILE, RUN_ARTIFACT_OUTCOME_FILE, RUN_ARTIFACT_RESULTS_FILE,
    RUN_ARTIFACT_STATUS_FILE,
};
pub use crate::core::run_lifecycle_status::{RunLifecycleStatus, RUN_LIFECYCLE_STATUS_SCHEMA};
pub use crate::core::run_outcome_envelope::{
    RunOutcomeEnvelope, RunOutcomeHandoffRef, RunOutcomeProjection, RUN_OUTCOME_ENVELOPE_FILE,
    RUN_OUTCOME_ENVELOPE_SCHEMA,
};
pub use crate::core::runner_execution_envelope::{
    PathMaterializationEntry, PathMaterializationMode, PathMaterializationPlan,
    PathMaterializationProjection, RunnerExecutionNextAction, RunnerExecutionProjection,
    RunnerExecutionRecord, PATH_MATERIALIZATION_MODE_EXISTING_REMOTE,
    PATH_MATERIALIZATION_MODE_GIT, PATH_MATERIALIZATION_MODE_SNAPSHOT,
    PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_REQUIRE_PATHS,
    PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT, PATH_MATERIALIZATION_PLAN_SCHEMA,
    PATH_MATERIALIZATION_ROLE_PRIMARY_WORKSPACE, PATH_MATERIALIZATION_ROLE_REQUIRED_PATH,
    PATH_MATERIALIZATION_STATUS_MATERIALIZED, PATH_MATERIALIZATION_STATUS_VALIDATED,
    RUNNER_EXECUTION_RECORD_SCHEMA,
};
