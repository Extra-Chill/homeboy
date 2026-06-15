pub mod bench;
pub mod build;
mod capability;
mod compiler_warning_contract;
pub mod component_script;
mod env_provider;
mod execution;
mod fingerprint;
pub mod grammar;
pub mod grammar_items;
mod grammar_strings;
mod lifecycle;
pub mod lint;
mod maintenance;
mod manifest;
mod manifest_action_config;
mod manifest_config;
mod manifest_deploy_config;
mod manifest_sidecar;
mod manifest_test_config;
mod refactor_protocol;
mod registry;
mod repair;
mod runner;
mod runner_contract;
mod runtime_helper;
mod scope;
pub mod self_check;
mod summary;
pub mod test;
pub mod trace;
pub mod update_check;
mod update_output;
mod validation;
pub mod version;

pub mod exec_context;

pub use capability::{
    build_scenario_runner, extract_component_extension_settings, path_list_env_value,
    resolve_execution_context, resolve_extension_for_capability, ExtensionCapability,
    ExtensionExecutionContext, ScenarioRunnerOptions,
};
pub(crate) use capability::{extension_guidance_hints, stderr_tail};
pub(crate) use compiler_warning_contract::{
    extensions_for_compiler_warning_contract, run_compiler_warning_contract_script,
    CompilerWarningContract,
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
pub(crate) use lifecycle::source_metadata::resolve_source_url;
pub use lifecycle::source_metadata::SourceMetadataRepair;
pub use lifecycle::{
    check_update_available, derive_id_from_url, install, install_for_component,
    install_with_revision, is_git_url, read_source_revision, slugify_id, uninstall, update,
    InstallForComponentResult, InstallResult, UpdateAvailable, UpdateResult,
};
pub use maintenance::{exec_tool, update_all};
pub use manifest::{
    ActionConfig, ActionType, AuditCapability, AutofixVerifyConfig, BenchConfig, BuildConfig,
    CiCapability, CiJobFidelity, CiJobMapping, CiJobSpec, CiLocalContext, CiProfileSpec,
    CliAutoFlag, CliAutoFlagCondition, CliConfig, CliHelpConfig, ComponentEnvConfig,
    DatabaseCliConfig, DatabaseConfig, DeployCapability, DeployOverride, DeployOwnerHint,
    DeployVerification, DepsConfig, DiscoveryConfig, DiscoveryMarkerConfig, DocTarget,
    ExecutableCapability, ExtensionManifest, FeatureContextRule, FileContainsCondition, HttpMethod,
    InputConfig, LintChangedFileRoute, LintConfig, OutputConfig, OutputSchema, PlatformCapability,
    ProvidesConfig, ReleasePreflightConfig, RemotePathInferenceRule, RemotePathRootRule,
    RequirementsConfig, RuntimeConfig, RuntimeRequirementsConfig, ScriptsConfig, SelectOption,
    SettingConfig, SinceTagConfig, StructuredSidecarDeclaration, TestChangedFileExclusiveEnv,
    TestChangedFileRouting, TestChangedFileRoutingStrategy, TestConfig, TestDriftConfig,
    TestMappingConfig, TestPassthroughFilter, TestPassthroughFilterStrategy,
    TraceBrowserArtifactMapConfig, TraceBrowserEvidenceAdapterConfig,
    TraceBrowserMetricAliasConfig, TraceBrowserSummaryAliasConfig, TraceConfig,
    VersionPatternConfig,
};
pub use manifest_deploy_config::{DeployArchiveInstallPolicy, DeployRequiredHeader};
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
pub use runner_contract::{
    phase_failure_category_from_exit_code, phase_status_from_exit_code, ExtensionPhaseTiming,
    PhaseFailure, PhaseFailureCategory, PhaseReport, PhaseStatus, RunnerStepFilter,
    VerificationPhase,
};
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
pub use version::{parse_extension_version, VersionConstraint};

#[cfg(test)]
mod tests;
