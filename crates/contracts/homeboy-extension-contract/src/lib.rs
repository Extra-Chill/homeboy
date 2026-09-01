//! Serializable contract types for the Homeboy extension system.
//!
//! This crate depends only on leaf crates, which keeps it cheap for downstream
//! consumers to depend on without pulling in the whole core compile unit.
//!
//! # What is in here
//!
//! Everything in this crate is public surface, but not all of it is the same
//! kind of surface, and the four kinds change for different reasons. Classifying
//! them is what makes it possible to tell a breaking change from an internal one.
//!
//! ## 1. Versioned Extension API — [`api`]
//!
//! The negotiated request/response operations Homeboy serves and consumes:
//! catalog, resolve, readiness, invocation, and the per-capability operations
//! for deployment providers, environments, external-check detail resolvers,
//! recipe runs, and agent-task executors. Versioned explicitly; a change here is
//! an API version change.
//!
//! ## 2. Manifest schema — what an extension author writes
//!
//! The shape of `extension.json` and everything reachable from it:
//! [`manifest`], the `manifest_*` configuration sections, and the declaration
//! contracts for agent-task executors, deployment providers, and external-check
//! detail resolvers. [`exec_context`] belongs here too: it is the environment
//! contract an extension script reads at execution time.
//!
//! Modules reached only indirectly are still part of this surface. `fuzz_config`
//! and `autofix_config` are glob-imported by [`manifest`], and `runtime_helper`
//! is reached through `fuzz_config`.
//!
//! ## 3. Extension-produced domain results
//!
//! Shapes an extension emits for a domain workflow and Homeboy parses: bench,
//! test, trace, and lint results, plus their parsing and analysis helpers. These
//! are contracts because an extension produces them, and they change on their
//! domain's cadence rather than the API's.
//!
//! ## 4. Provider command contracts
//!
//! Versioned JSON stdin/stdout command protocols whose other side is implemented
//! outside Homeboy, such as [`worktree_retention`]. Core invokes these against a
//! configured provider, so the shape is a promise to that provider.
//!
//! # Adding a type
//!
//! Decide which of the four it is first. If a type is reachable from
//! [`manifest`], it is manifest schema no matter how few callers it has. If it
//! is one side of a command protocol, it is a provider contract. Reachability
//! from public surface decides this, not the number of Rust call sites.
//!
//! # Behavior
//!
//! These types are close to behavior-free, but not entirely: [`manifest`]
//! carries accessors and capability projection, `core_compat` and `version`
//! evaluate compatibility constraints, and `bench_stage` verifies stage reuse.
//! Keep behavior here limited to interpreting the contract itself, so that
//! deciding what a declaration means stays with the declaration.

pub use action_types::HttpMethod;
pub mod api;
pub use ci_config::{
    CiCachePath, CiCachePathRoot, CiCacheSpec, CiCapability, CiJobFidelity, CiJobMapping,
    CiJobSpec, CiLocalContext, CiProfileSpec,
};
pub use manifest_capability_config::AgentRuntimeManifestConfig;
pub use manifest_capability_config::{
    DiscoveryMarkerConfig, RecipeRunProviderDeclaration, RecipeRunProviderDescriptor, ScriptsConfig,
};
pub use manifest_toolchain_config::{
    CliAutoFlag, CliAutoFlagCondition, CliHelpConfig, DatabaseCliConfig, DeployVerification,
    LintChangedFileRoute, RemotePathRootRule, RequirementsConfig, TestSecretEnvProjection,
    TestSettingStringPredicate,
};
pub use manifest_toolchain_config::{CliConfig, RemotePathInferenceRule};
pub mod action_types;
pub mod agent_task_executor_declaration;
pub mod autofix_config;
pub mod bench_artifact;
pub mod bench_diagnostics;
pub mod bench_distribution;
pub mod bench_gate;
pub mod bench_metric_preset;
pub mod bench_responsiveness;
pub mod bench_result;
pub mod bench_results;
pub mod bench_stage;
pub mod capability;
pub mod test_analysis;
pub mod test_duration;
pub mod test_parsing;
pub mod test_result;
pub mod test_results;
pub mod test_workflow;
pub mod trace_parsing;
pub use bench_artifact::{BenchArtifact, BenchArtifactViewer, BenchPreviewLifecycleMetadata};
pub use bench_diagnostics::{
    BenchDiagnostic, BenchDiagnosticSource, BenchPhaseEvent, BenchPhaseFailureClassification,
    BenchPhaseSummary,
};
pub use bench_distribution::BenchRunDistribution;
pub use bench_gate::{BenchGate, BenchGateOp, BenchGateResult};
pub use bench_metric_preset::{BenchMetricPolicyPreset, BenchMetricPolicyPresetKind};
pub use bench_responsiveness::{BenchFailureMemorySample, BenchResponsivenessSummary};
pub use bench_result::{
    BenchChildCommandFailure, BenchMemory, BenchMetricDirection, BenchMetricPhase,
    BenchMetricPolicy, BenchMetrics, BenchProvenance, BenchProvenanceLink, BenchRunExecution,
    BenchRunnerMetadata, BenchWorkloadMetadata, RegressionTest, RigPackageEvidence,
    RigPackageFreshness,
};
pub use bench_results::{BenchResults, BenchRunMetadata, BenchRunSnapshot, BenchScenario};
pub use bench_stage::{
    verify_stage_reuse, BenchStageArtifact, BenchStageEvidence, BenchStageInvalidation,
    BenchStageReuse,
};
pub use capability::ExtensionCapability;
pub use test_analysis::{
    FailureCategory, FailureCluster, TestAnalysis, TestAnalysisInput, TestFailure,
};
pub use test_duration::{SlowTestFinding, TestDurations, TestUnitDuration};
pub use test_parsing::{CoverageOutput, TestFailureSummaryItem, TestSummaryOutput, UncoveredFile};
pub use test_result::{TestCounts, TestScopeOutput};
pub use test_results::{
    AutoFixDriftWorkflowResult, DriftWorkflowResult, MainTestWorkflowResult, TestCommandOutput,
    TestRunWorkflowResult,
};
pub use test_workflow::{
    AutoFixDriftOutput, ChangeType, DriftReport, DriftedTest, ProductionChange, RawTestOutput,
    TestBaselineComparison,
};
pub use trace_parsing::{
    TraceArtifact, TraceAssertion, TraceAssertionStatus, TraceCanonicalCheck,
    TraceComponentsProvenance, TraceDependencyProvenance, TraceEvent, TraceEvidenceMetadata,
    TraceGitProvenance, TraceList, TraceRuntimeAssetProvenance, TraceScenario, TraceSpanDefinition,
    TraceSpanResult, TraceSpanStatus, TraceStatus, TraceTemporalAssertionDefinition,
    TraceToolchainProvenance,
};
pub mod ci_config;
pub mod ci_context;
pub mod lint_result;
pub mod lint_results;
pub use ci_context::CiContext;
pub use lint_result::{
    FormattingFindings, LintSummaryOutput, SelfCheckCaptureMetadata, StreamCaptureMetadata,
};
pub use lint_results::LintCommandOutput;
pub mod core_compat;
pub mod exec_context;
pub mod extension_contract_producer;
pub mod external_check_detail_resolver;
pub mod external_storage_retention;
pub mod fuzz_config;
pub mod hook_event;
pub mod manifest;
pub mod manifest_action_config;
pub mod manifest_artifact_cleanup;
pub mod manifest_capabilities;
pub mod manifest_capability_config;
pub mod manifest_deploy_config;
pub mod manifest_test_config;
pub mod manifest_toolchain_config;
pub mod notification_transport_config;
pub mod worktree_retention;
pub use notification_transport_config::{
    NotificationRouteResolverConfig, NotificationRouteResolverRequest,
    NotificationRouteResolverResponse, NotificationRouteResolverStatus,
    NotificationTransportConfig, NotificationTransportDescriptor,
    NOTIFICATION_ROUTE_RESOLVER_REQUEST_SCHEMA, NOTIFICATION_ROUTE_RESOLVER_SCHEMA,
    NOTIFICATION_TRANSPORT_SCHEMA,
};
pub mod runner_contract;
pub mod runtime_helper;
pub mod sidecar_config;
pub use external_check_detail_resolver::{
    ExternalCheckDetailRequest, ExternalCheckDetailResolverConfig,
    ExternalCheckDetailResolverDeclaration, ExternalCheckDetailResponse,
    EXTERNAL_CHECK_DETAIL_REQUEST_SCHEMA, EXTERNAL_CHECK_DETAIL_RESOLVER_SCHEMA,
    EXTERNAL_CHECK_DETAIL_RESPONSE_SCHEMA,
};
pub use external_storage_retention::{
    ExternalStorageInventory, ExternalStorageItem, ExternalStorageOperation,
    ExternalStorageReclaimResult, ExternalStorageReclaimTarget, ExternalStorageRequest,
    ExternalStorageResourceClass, ExternalStorageRetentionConfig,
    ExternalStorageRetentionProviderConfig, ExternalStorageRoot,
    DEFAULT_EXTERNAL_STORAGE_PROVIDER_TIMEOUT_SECONDS, EXTERNAL_STORAGE_RETENTION_SCHEMA,
    MAX_EXTERNAL_STORAGE_RECLAIM_TARGETS, MAX_EXTERNAL_STORAGE_REQUEST_BYTES,
};
pub use hook_event::HookEvent;
pub use manifest::ExtensionManifest;
pub use manifest_artifact_cleanup::{
    ArtifactCleanupCategory, ArtifactCleanupConfig, ArtifactCleanupDeclaration,
    ArtifactCleanupScope, DEFAULT_NESTED_SCOPE_MAX_DEPTH,
};
pub use worktree_retention::{
    WorktreeRetentionBlockers, WorktreeRetentionBounds, WorktreeRetentionContinuation,
    WorktreeRetentionEffects, WorktreeRetentionInventoryCompleteness, WorktreeRetentionOperation,
    WorktreeRetentionRef, WorktreeRetentionRequest, WorktreeRetentionResponse,
    WorktreeRetentionState, DEFAULT_WORKTREE_RETENTION_TIMEOUT_MS,
    MAX_WORKTREE_RETENTION_OUTPUT_BYTES, MAX_WORKTREE_RETENTION_REQUEST_BYTES,
    WORKTREE_RETENTION_SCHEMA,
};
pub mod source_metadata_repair;
pub mod test_drift;
pub mod test_inventory_config;
pub mod trace_config;
pub mod trace_preview;
pub mod trace_results;
pub mod trace_spec;
pub mod update_output;
pub mod version;

pub use trace_spec::{
    TraceDependencySpec, TraceNativePublicPreviewSpec, TracePreviewAssetFanoutSpec,
    TracePublicPreviewMode, TracePublicPreviewSpec,
};

pub use core_compat::{
    core_incompatible_error, evaluate_core_compatibility, evaluate_core_compatibility_for_version,
    installed_homeboy_version, validate_core_compatibility, CoreCompatibilityReport,
    CORE_COMPAT_REMEDIATION_COMMAND, CORE_INCOMPATIBLE_DIAGNOSTIC,
};
pub use manifest_deploy_config::{
    DeployArchiveInstallPolicy, DeployRequiredHeader, DeploymentProviderLayeredInputManifest,
    DeploymentProviderManifest, DEPLOYMENT_PROVIDER_PAYLOAD_SCHEMA,
};
pub use manifest_test_config::{TestPassthroughFilter, TestPassthroughFilterStrategy};
pub use runner_contract::{
    phase_failure_category_from_exit_code, phase_status_from_exit_code, ExtensionPhaseTiming,
    PhaseFailure, PhaseFailureCategory, PhaseReport, PhaseStatus, RunnerStepFilter,
    VerificationPhase, GENERIC_INFRASTRUCTURE_FAILURE_MARKERS,
};
pub use runtime_helper::RuntimeHelperRequirement;
pub use test_drift::TestDriftConfig;
pub use test_inventory_config::{TestInventoryConfig, TestInventoryRunner};
pub use version::{parse_extension_version, VersionConstraint};
