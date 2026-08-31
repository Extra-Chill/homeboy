use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(not(test))]
use std::sync::RwLock;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) use crate::agent_task::{
    AgentTaskArtifact, AgentTaskArtifactsPathProvenance, AgentTaskDiagnostic, AgentTaskEvidenceRef,
    AgentTaskExecutionState, AgentTaskExecutorRequest, AgentTaskFailureClassification,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest, AgentTaskTypedArtifact,
    AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
    AGENT_TOOL_POLICY_SCHEMA, AGENT_TOOL_REQUEST_SCHEMA, AGENT_TOOL_RESULT_SCHEMA,
};
use crate::agent_task_gate_executor::{is_repo_local_gate_request, run_repo_local_gate_task};
use crate::agent_task_scheduler::{
    AgentTaskExecutionContext, AgentTaskExecutorAdapter, AgentTaskPlan, ProviderRouteReadiness,
};
use crate::agent_task_secrets::{
    resolve_secret_env_with_fallbacks, secret_env_status_with_fallbacks,
    AgentTaskSecretResolutionError,
};
use crate::agent_task_timeout::timeout_with_grace;
#[cfg(test)]
use homeboy_core::agent_runtime_manifest;
use homeboy_core::agent_runtime_manifest::AgentRuntimeDiscoveryDiagnostic;
pub(crate) use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::engine::shell;
pub(crate) use homeboy_core::secret_env_plan::{SecretEnvPlan, SecretEnvStatus};
use homeboy_core::{component, defaults, Error};

mod admission;
pub(crate) mod artifact_finalization;
mod catalog;
pub(crate) mod command_runner;
mod config_preflight;
mod credential_readiness;
pub mod discovery;
mod dispatchability;
mod executor;
mod fixture_gate;
mod launch_context;
// The `fixture` backend is a test double, not an agent runtime. Its
// implementation is compiled only into test builds; see `fixture_gate`.
#[cfg(any(test, feature = "test-support"))]
mod fixtures;
mod outcome_normalization;
mod resolution;
mod runner_readiness;
mod runtime_preflight_checks;
mod runtime_readiness;
mod runtime_tool_resolution;
mod runtime_types;
mod secret_types;
mod secrets;
pub mod structured_error;
mod types;
mod usage_cap;
mod workspace_types;

#[cfg(test)]
mod tests;

pub use admission::{
    AgentTaskProviderAdmissionAction, AgentTaskProviderAdmissionPlan,
    AgentTaskProviderAdmissionPredicate, AgentTaskProviderAdmissionRequest,
    AGENT_TASK_PROVIDER_ADMISSION_PLAN_SCHEMA,
};
pub use catalog::*;
#[cfg(test)]
pub(crate) use command_runner::run_provider_readiness_invocation_with_test_timeout;
pub use command_runner::{
    probe_provider_executor_resolves, provider_command_parts, run_provider_readiness_invocation,
    validate_provider_immediate_failure_patterns, ProviderExecutorResolution,
    ProviderReadinessInvocationResult, PROVIDER_READINESS_RESULT_SCHEMA,
};
pub(crate) use config_preflight::preflight_plan_provider_config_with_providers;
pub use credential_readiness::{
    preflight_discovered_provider_credentials_for_backend, preflight_provider_credentials,
    preflight_provider_credentials_for_backend, provider_credential_readiness,
    provider_required_secret_env_names, AgentTaskProviderCredentialReadiness,
    AgentTaskProviderCredentialRequirement, AGENT_TASK_PROVIDER_CREDENTIAL_READINESS_SCHEMA,
};
pub use dispatchability::{
    admit_plan_provider_dispatchability_with_providers, evaluate_provider_dispatchability,
    evaluate_provider_dispatchability_with_config,
    preflight_plan_provider_dispatchability_with_providers,
    preflight_plan_provider_dispatchability_without_runtime_with_providers,
    preflight_provider_dispatchability, preflight_provider_dispatchability_with_config,
    preflight_provider_dispatchability_without_runtime_with_config,
    AgentTaskProviderConfigurationDiagnosis, AgentTaskProviderCredentialStatus,
    AgentTaskProviderDispatchability, AgentTaskProviderOwner, AgentTaskProviderRuntimeEvidence,
};
pub(crate) use fixture_gate::fixture_provider_outcome;
pub use fixture_gate::is_fixture_backend;
pub use launch_context::{
    AgentTaskProviderLaunchContext, AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_JSON_ENV,
    AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_SCHEMA,
};
pub use resolution::{resolve_provider_for_backend, ProviderResolution};
pub(crate) use resolution::{
    role_aliases_for_executor, role_aliases_for_provider, selector_runtime_provider_hint,
    timeout_artifact_discovery_for_executor,
};
pub use runtime_preflight_checks::{
    ensure_runtime_preflight_checks, evaluate_runtime_preflight_checks, RuntimePreflightConflict,
    RuntimePreflightReadiness,
};
pub(crate) use runtime_readiness::{
    effective_provider_config, readiness_request_key, readiness_verdict_with_credentials,
};
pub use runtime_readiness::{
    preflight_plan_provider_runtime_readiness_with_providers, ProviderRuntimeReadinessCache,
};
pub(crate) use runtime_tool_resolution::resolve_runtime_tools;
pub use runtime_types::*;
pub use secret_types::*;
pub use secrets::{
    provider_runner_secret_env_for_plan_with_providers,
    provider_secret_sources_for_plan_with_providers,
};
pub use structured_error::{
    normalize_provider_error, normalize_runtime_stream_error,
    normalized_error_failure_classification, normalized_structured_error,
    structured_error_failure_classification, PROVIDER_ACCOUNT_BLOCKED, PROVIDER_ERROR,
    PROVIDER_RATE_LIMITED, PROVIDER_STRUCTURED_ERROR_SCHEMA,
};
pub(crate) use types::wildcard_match;
pub use types::*;
pub(crate) use usage_cap::provider_capacity_config;
pub use usage_cap::{
    detect_usage_cap, provider_usage_cap_key, provider_usage_cap_key_for_model,
    provider_usage_cap_key_for_request, reset_at_from_outcome, ProviderUsageCapRegistry,
    AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS,
};
pub use workspace_types::*;

// AgentTaskProviderRunnerSource lives in the below-core contract (core reads its
// git ref); re-export it here so agent-side consumers keep the same path.
pub use homeboy_lab_runner_contract::AgentTaskProviderRunnerSource;

#[cfg(test)]
use catalog::{
    component_default_backend, enforce_runtime_preflight_checks_for_plan_with_providers,
    validate_provider_runner_readiness_for_backend_with_providers,
};
#[cfg(test)]
use command_runner::{
    immediate_provider_failure, is_transient_provider_error, provider_command_env,
    render_provider_command_display, run_provider_command, run_provider_command_once,
    PROVIDER_TRANSIENT_MAX_ATTEMPTS,
};
#[cfg(test)]
use fixtures::fixture_artifact;
#[cfg(test)]
use outcome_normalization::{
    normalize_homeboy_local_artifact_sizes, normalize_provider_outcome_roles,
    surface_provider_run_result_diagnostics,
};
#[cfg(test)]
use resolution::{provider_requires_cwd_git_checkout_with_providers, select_provider_by_backend};
#[cfg(test)]
use secrets::{apply_provider_runner_secret_env_contracts_with_providers, provider_secret_sources};
