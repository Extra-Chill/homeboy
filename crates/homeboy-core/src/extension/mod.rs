pub mod audit_compiler_warning_provider;
pub mod audit_fingerprint_script_provider;
pub mod audit_grammar_source_provider;
pub mod audit_manifest_provider;
pub mod bench;
pub mod build;
mod capability;
pub mod catalog;
mod compiler_warning_contract;
pub mod component_script;
mod env_provider;
mod execution;
mod fingerprint;
pub mod lifecycle;
pub mod lint;
mod maintenance;
mod manifest;
mod manifest_sidecar;
pub mod recipe_run;
mod refactor_protocol;
pub mod resolve;
mod runner;
mod runtime_helper;
pub mod self_check;
mod setup_env;
mod summary;
pub mod test;
pub mod trace;
pub mod update_check;
mod validation;

pub use capability::{build_scenario_runner, ScenarioRunnerOptions};
pub use compiler_warning_contract::{
    extensions_for_compiler_warning_contract, run_compiler_warning_contract_script,
    CompilerWarningContract,
};
pub(crate) use homeboy_core::extension::resolve::{extension_guidance_hints, stderr_tail};

pub use env_provider::{
    declared_secret_names as env_provider_secret_names,
    resolve_installed as resolve_installed_env_provider,
    resolve_installed_all as resolve_installed_env_providers, EnvProviderCommandPayload,
    EnvProviderContribution, ENV_PROVIDER_COMMAND_PAYLOAD_ENV,
};
pub(crate) use execution::build_settings_json_from_manifest;
pub use execution::execute_action;
pub use execution::{
    run_action, run_deployment_provider, run_extension, run_setup, ExtensionExecutionMode,
    ExtensionRunResult, ExtensionSetupResult, ExtensionStepFilter,
};
pub use fingerprint::run_fingerprint_script;
pub use maintenance::{exec_tool, update_all};
pub use manifest::{
    deployment_provider_layered_input, deployment_providers, structured_sidecar_schema_version,
    structured_sidecars,
};
pub use recipe_run::{
    recipe_run_provider_inventory, render_recipe_run_command, resolve_recipe_run_provider,
    RecipeRunProviderInventoryEntry, RecipeRunProviderValidation, RecipeRunRequest,
};
pub use refactor_protocol::{
    run_refactor_script, run_refactor_script_result, AdjustedItem, ParsedItem,
    RefactorScriptFailure, RefactorScriptFailureKind, RelatedTests, ResolvedImports,
    RewrittenImport,
};
pub use runner::{ExtensionRunner, RunnerOutput, STRICT_VALIDATION_DEPENDENCIES_ENV};
pub use runtime_helper::{
    declared_helper_env_names, helper_path, provision_declared_helpers, RuntimeHelperProvision,
    BASH_PREFLIGHT_ENV, COMMAND_CAPTURE_ENV, RUNNER_PRELUDE_ENV, RUNNER_STEPS_ENV,
    RUNTIME_SETTINGS_HELPER_ENV, RUNTIME_SETTINGS_HELPER_ID,
};
pub use summary::{list_summaries, list_summaries_with, ActionSummary, ExtensionSummary};
pub use validation::{
    extension_provides_build, validate_extension_requirements, validate_required_extensions,
};

#[cfg(test)]
mod tests;
