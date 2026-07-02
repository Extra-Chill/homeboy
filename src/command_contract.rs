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
mod lab;
mod output;
mod public_variants;
mod registry;
pub mod safety_manifest;
mod spec;

pub use crate::core::artifact_ref::{
    validate_reviewer_facing_artifact_ref, ArtifactReference, ReviewerFacingArtifactRefError,
};
pub use constants::{
    artifact_manifest_constants, artifact_postprocess_constants, contract_constants,
    loop_constants, path_materialization_plan_constants, resource_lifecycle_index_constants,
    reviewer_facing_ref_constants, run_location_index_constants, runner_execution_record_constants,
    runtime_artifact_constants, secret_env_plan_constants, AllContractConstants,
    ArtifactManifestConstants, ArtifactPostprocessConstants, ContractConstants,
    ContractConstantsOutput, ExecutorEvidenceConstants, LoopConstants,
    PathMaterializationPlanConstants, ResourceLifecycleIndexConstants, ReviewerFacingRefConstants,
    RunLocationIndexConstants, RunnerExecutionRecordConstants, RuntimeAgentArtifactPaths,
    RuntimeArtifactConstants, RuntimeArtifactFilenames, SecretEnvPlanConstants,
    CONTRACT_CONSTANTS_SCHEMA,
};
pub use lab::{
    lab_runner_support_summary, lab_runner_supported_contract_labels, lab_runner_supported_labels,
    lab_runner_supports_contract_label, lab_runner_unsupported_hint,
    lab_runner_unsupported_message, AgentTaskDispatchIdentity, CommandPortabilityContract,
    LabCommandContract, LabCommandPortability, LabCommandRequiredTool, LabCommandRouteContract,
    LabLocalExecutionPolicy, LabLocalHotPolicy, LabRoutingPolicy, LabRunnerSupportSummary,
    LabSelectedRunnerFallbackPolicy, LabSourcePathMode, LabWorkspaceModePolicy, RunLocationIndex,
    RunnerHandoffArtifactManifestRef, RunnerHandoffEnvelope, RunnerHandoffFollowCommands,
    RunnerWorkload, RunnerWorkloadArtifactRef, RunnerWorkloadAssignment, RunnerWorkloadCapability,
    RunnerWorkloadCommandFamily, RunnerWorkloadExtensionRevision, RunnerWorkloadKind,
    RunnerWorkloadMutationPolicy, RunnerWorkloadResultRefs, RunnerWorkloadSecrets,
    RunnerWorkloadState, RunnerWorkloadWorkspaceMappings, LAB_TRACE_EXTRA_TOOLS,
    RUNNER_ARTIFACT_MANIFEST_FILE, RUNNER_ARTIFACT_MANIFEST_REF_NAME,
    RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA, RUNNER_ARTIFACT_MANIFEST_SCHEMA,
    RUNNER_ARTIFACT_ROOT_DIR_SUFFIX, RUNNER_HANDOFF_ENVELOPE_SCHEMA, RUNNER_WORKLOAD_SCHEMA,
    RUN_LOCATION_INDEX_SCHEMA,
};
pub(crate) use lab::{LAB_NO_EXTRA_TOOLS, RIG_UP_LAB_UNSUPPORTED_REASON};
pub use output::{
    CommandDescriptor, CommandDispatchFamily, CommandJsonFamily, CommandOutputContractKind,
    CommandOutputDescriptor, CommandOutputFileMode, CommandRawOutputMode, CommandResponseMode,
    CommandResponsePlan, CommandStdoutMode,
};
pub use public_variants::{PublicOutputVariantContract, PUBLIC_OUTPUT_VARIANT_CONTRACTS};
pub use registry::{
    registered_contract, registered_contracts, ContractRegistryEntry, ContractRegistrySummary,
};
pub use spec::{
    non_core_command_doc_slugs, registered_command, registered_command_dispatch_family,
    registered_command_json_family, runtime_extension_command_doc_slugs, support_command_doc_slugs,
    CommandDocKind, CommandDocSpec, CommandLabSupportSummary, CommandRegistryEntry,
    CommandSafetySpec, CommandSpec, COMMAND_DOC_REGISTRY, COMMAND_REGISTRY, COMMAND_SPECS,
};
pub(crate) use spec::{
    AUDIT_LAB_LABEL, BENCH_LAB_LABEL, FUZZ_LAB_LABEL, LINT_LAB_LABEL, REVIEW_LAB_LABEL,
    RIG_CHECK_LAB_LABEL, RIG_RUN_LAB_LABEL, TEST_LAB_LABEL, TRACE_LAB_LABEL,
    TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL, TUNNEL_SERVICE_EXPOSE_LAB_LABEL,
    TUNNEL_SERVICE_START_LAB_LABEL, WORKTREE_CLEANUP_LAB_LABEL,
};

pub use crate::core::artifacts::{
    ArtifactPostprocessAction, ArtifactPostprocessPlan, ArtifactPostprocessPlanDescription,
    ArtifactPostprocessResult, ArtifactPostprocessReviewerRef, ArtifactPostprocessRoot,
    ARTIFACT_POSTPROCESS_PLAN_SCHEMA, ARTIFACT_POSTPROCESS_RESULT_SCHEMA,
    ARTIFACT_POSTPROCESS_SCHEMA, RUNTIME_AGENT_FINAL_OUTPUT_ARTIFACT_PATH,
    RUNTIME_AGENT_PATCH_DIFF_ARTIFACT_FILE, RUNTIME_AGENT_PATCH_PATCH_ARTIFACT_FILE,
    RUNTIME_AGENT_RESULT_ARTIFACT_FILE, RUNTIME_AGENT_RESULT_ARTIFACT_FILE_LEGACY_UNDERSCORE,
    RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_FILE, RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_PATH,
};
pub use crate::core::run_lifecycle_status::{RunLifecycleStatus, RUN_LIFECYCLE_STATUS_SCHEMA};
pub use crate::core::run_outcome_envelope::{
    RunOutcomeEnvelope, RunOutcomeHandoffRef, RunOutcomeProjection, RUN_OUTCOME_ENVELOPE_SCHEMA,
};
pub use crate::core::runner_execution_envelope::{
    PathMaterializationEntry, PathMaterializationMode, PathMaterializationPlan,
    PathMaterializationProjection, RunnerExecutionArtifactRef, RunnerExecutionNextAction,
    RunnerExecutionProjection, RunnerExecutionRecord, PATH_MATERIALIZATION_MODE_EXISTING_REMOTE,
    PATH_MATERIALIZATION_MODE_GIT, PATH_MATERIALIZATION_MODE_SNAPSHOT,
    PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_REQUIRE_PATHS,
    PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT, PATH_MATERIALIZATION_PLAN_SCHEMA,
    PATH_MATERIALIZATION_ROLE_PRIMARY_WORKSPACE, PATH_MATERIALIZATION_ROLE_REQUIRED_PATH,
    PATH_MATERIALIZATION_STATUS_MATERIALIZED, PATH_MATERIALIZATION_STATUS_VALIDATED,
    RUNNER_EXECUTION_RECORD_SCHEMA,
};
