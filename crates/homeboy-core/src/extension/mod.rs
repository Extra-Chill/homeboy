pub mod audit_compiler_warning_provider;
pub mod audit_fingerprint_script_provider;
pub mod audit_grammar_source_provider;
pub mod audit_manifest_provider;
pub mod bench;
pub mod build;
mod capability;
mod compiler_warning_contract;
pub mod component_script;
mod env_provider;
mod execution;
mod fingerprint;
mod invocation_context;
// The grammar parsing engine is a language-agnostic primitive; it now lives in
// homeboy-engine-primitives. Re-exported here so existing
// `crate::extension::grammar` / `crate::extension::grammar_items` paths keep
// resolving. (grammar_items is now the `items` submodule of grammar.)
pub use homeboy_engine_primitives::grammar;
pub use homeboy_engine_primitives::grammar::items as grammar_items;
mod lifecycle;
pub mod lint;
mod maintenance;
mod manifest;
mod manifest_action_config;
mod manifest_config;
mod manifest_sidecar;
mod refactor_protocol;
mod registry;
mod repair;
mod runner;
mod runtime_helper;
mod scope;
pub mod self_check;
mod setup_env;
mod summary;
pub mod test;
pub mod trace;
pub mod update_check;
mod update_output;
mod validation;

pub use capability::{
    build_scenario_runner, extract_component_extension_settings, path_list_env_value,
    resolve_execution_context, resolve_extension_for_capability, ExtensionCapability,
    ExtensionExecutionContext, ScenarioRunnerOptions,
};
pub(crate) use capability::{
    extension_guidance_hints, has_linked_extension_for_capability,
    resolve_execution_context_if_available, stderr_tail,
};
pub(crate) use compiler_warning_contract::{
    extensions_for_compiler_warning_contract, run_compiler_warning_contract_script,
    CompilerWarningContract,
};
pub use homeboy_extension_contract::core_compat::{
    core_incompatible_error, evaluate_core_compatibility, installed_homeboy_version,
    validate_core_compatibility, CoreCompatibilityReport, CORE_COMPAT_REMEDIATION_COMMAND,
    CORE_INCOMPATIBLE_DIAGNOSTIC,
};

pub(crate) use execution::{build_settings_json_from_manifest, execute_action};
pub use execution::{
    extension_ready_status, is_extension_compatible, run_action, run_extension, run_setup,
    ExtensionExecutionMode, ExtensionReadyStatus, ExtensionRunResult, ExtensionSetupResult,
    ExtensionStepFilter,
};
pub use fingerprint::{
    run_fingerprint_script, AggregateConstructionSeam, AggregateLiteral, CallSite, DeadCodeMarker,
    FingerprintOutput, HookRef, UnusedParam,
};
pub use homeboy_extension_contract::runner_contract::{
    phase_failure_category_from_exit_code, phase_status_from_exit_code, ExtensionPhaseTiming,
    PhaseFailure, PhaseFailureCategory, PhaseReport, PhaseStatus, RunnerStepFilter,
    VerificationPhase, GENERIC_INFRASTRUCTURE_FAILURE_MARKERS,
};
pub use homeboy_extension_contract::version::{parse_extension_version, VersionConstraint};
pub use homeboy_extension_contract::{DeployArchiveInstallPolicy, DeployRequiredHeader};
pub use invocation_context::ResolvedExtensionInvocationContext;
pub use lifecycle::source_metadata::resolve_source_url;
pub use lifecycle::source_metadata::SourceMetadataRepair;
pub use lifecycle::{
    check_update_available, derive_id_from_url, install, install_for_component,
    install_with_revision, is_git_url, read_source_revision, read_source_url, refresh, slugify_id,
    uninstall, update, InstallForComponentResult, InstallResult, RefreshResult, UpdateAvailable,
    UpdateResult,
};
pub use maintenance::{exec_tool, update_all};
pub use manifest::{
    ActionConfig, ActionType, AgentRuntimeManifestConfig, AuditCapability, AutofixVerifyConfig,
    BehaviorScenarioNames, BenchConfig, BuildConfig, CiCapability, CiJobFidelity, CiJobMapping,
    CiJobSpec, CiLocalContext, CiProfileSpec, CliAutoFlag, CliAutoFlagCondition, CliConfig,
    CliHelpConfig, ComponentEnvConfig, DatabaseCliConfig, DatabaseConfig, DeployCapability,
    DeployOverride, DeployOwnerHint, DeployVerification, DepsConfig, DiscoveryConfig,
    DiscoveryMarkerConfig, DocTarget, ExecutableCapability, ExtensionContractProducer,
    ExtensionContractProducerInvocation, ExtensionContractProducerOutput,
    ExtensionContractProducerOutputKind, ExtensionContractProducerPhase,
    ExtensionDiagnosticsConfig, ExtensionManifest, ExtensionMaterializationHelperManifestRef,
    ExtensionMaterializationSourceContract, ExtensionMaterializationSourceKind,
    ExtensionToolDiagnosticDeclaration, FeatureContextRule, FileContainsCondition, FuzzConfig,
    HttpMethod, IncludeWrapperPolicy, InputConfig, LintChangedFileRoute, LintConfig,
    NotificationTransportConfig, PackageNameSource, PlatformCapability, ProvidesConfig,
    ReleasePreflightConfig, RemotePathInferenceRule, RemotePathRootRule, RequirementsConfig,
    RuntimeConfig, RuntimeRequirementsConfig, ScriptsConfig, SelectOption, SettingConfig,
    SinceTagConfig, SourceSnapshotConfig, StructuredSidecarDeclaration,
    TestChangedFileExclusiveEnv, TestChangedFileRouting, TestChangedFileRoutingStrategy,
    TestConfig, TestDriftConfig, TestMappingConfig, TestPassthroughFilter,
    TestPassthroughFilterStrategy, TestVacuityPolicy, TraceBrowserArtifactMapConfig,
    TraceBrowserEvidenceAdapterConfig, TraceBrowserMetricAliasConfig,
    TraceBrowserSummaryAliasConfig, TraceConfig, VersionPatternConfig,
    EXTENSION_CONTRACT_PRODUCER_SCHEMA, EXTENSION_MATERIALIZATION_SOURCE_SCHEMA,
    NOTIFICATION_TRANSPORT_SCHEMA,
};
pub use refactor_protocol::{
    run_refactor_script, run_refactor_script_result, AdjustedItem, ParsedItem,
    RefactorScriptFailure, RefactorScriptFailureKind, RelatedTests, ResolvedImports,
    RewrittenImport,
};
pub use registry::{
    available_extension_ids, extension_path, find_extension_by_tool, find_extension_for_file_ext,
    is_extension_linked, load_all_extensions, load_extension, merge, save_manifest,
};
pub use repair::{relink, replace, replace_with_revision, ReplaceResult};
pub use runner::{ExtensionRunner, RunnerOutput};
pub use runtime_helper::{
    helper_path, BASH_PREFLIGHT_ENV, COMMAND_CAPTURE_ENV, RUNNER_PRELUDE_ENV, RUNNER_STEPS_ENV,
};
pub use scope::ExtensionScope;
pub use summary::{list_summaries, ActionSummary, ExtensionSummary};
pub use update_output::{
    ExtensionSourceUpdate, SourceMetadataRepairEntry, UpdateAllResult, UpdateEntry,
    UpdateSkippedEntry,
};
pub use validation::{
    extension_provides_build, validate_extension_requirements, validate_required_extensions,
};

pub use homeboy_extension_contract::{core_compat, exec_context, runner_contract, version};

#[cfg(test)]
mod tests;
