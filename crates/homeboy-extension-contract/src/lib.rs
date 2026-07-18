//! Pure serializable data types for the homeboy extension system.
//!
//! This crate holds behavior-free contract types shared between core, the
//! extension execution subsystem, and downstream consumers. It depends only on
//! leaf crates (`homeboy-error`, `homeboy-audit-contract`), which keeps it a
//! lightweight crate that others can depend on without pulling in the whole
//! core compile unit.
//!
//! Modules and types are re-exported from `homeboy_core::extension` so existing
//! `crate::extension::*` call sites keep working unchanged.

pub mod action_types;
pub mod autofix_config;
pub mod bench_artifact;
pub mod bench_diagnostics;
pub mod bench_distribution;
pub mod bench_gate;
pub mod bench_metric_preset;
pub mod bench_responsiveness;
pub mod bench_result;
pub mod bench_results;
pub mod capability;
pub mod test_analysis;
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
pub use capability::ExtensionCapability;
pub use test_analysis::{
    FailureCategory, FailureCluster, TestAnalysis, TestAnalysisInput, TestFailure,
};
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
pub use ci_context::CiContext;
pub mod core_compat;
pub mod exec_context;
pub mod extension_contract_producer;
pub mod fuzz_config;
pub mod manifest;
pub mod manifest_action_config;
pub mod manifest_capabilities;
pub mod manifest_capability_config;
pub mod manifest_deploy_config;
pub mod manifest_test_config;
pub mod manifest_toolchain_config;
pub mod notification_transport_config;
pub mod runner_contract;
pub mod sidecar_config;
pub use manifest::ExtensionManifest;
pub mod source_metadata_repair;
pub mod test_drift;
pub mod trace_config;
pub mod trace_preview;
pub mod trace_results;
pub mod update_output;
pub mod version;

pub use core_compat::{
    core_incompatible_error, evaluate_core_compatibility, installed_homeboy_version,
    validate_core_compatibility, CoreCompatibilityReport, CORE_COMPAT_REMEDIATION_COMMAND,
    CORE_INCOMPATIBLE_DIAGNOSTIC,
};
pub use manifest_deploy_config::{DeployArchiveInstallPolicy, DeployRequiredHeader};
pub use manifest_test_config::{TestPassthroughFilter, TestPassthroughFilterStrategy};
pub use runner_contract::{
    phase_failure_category_from_exit_code, phase_status_from_exit_code, ExtensionPhaseTiming,
    PhaseFailure, PhaseFailureCategory, PhaseReport, PhaseStatus, RunnerStepFilter,
    VerificationPhase, GENERIC_INFRASTRUCTURE_FAILURE_MARKERS,
};
pub use test_drift::TestDriftConfig;
pub use version::{parse_extension_version, VersionConstraint};
