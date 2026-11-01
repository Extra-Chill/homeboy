use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod pi;
pub(crate) mod pi_response;
#[cfg(test)]
mod pi_tests;
#[cfg(test)]
pub(crate) mod test_pi;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) use crate::agent_task::{
    AgentTaskArtifact, AgentTaskArtifactsPathProvenance, AgentTaskDiagnostic, AgentTaskEvidenceRef,
    AgentTaskExecutorRequest, AgentTaskFailureClassification, AgentTaskOutcome,
    AgentTaskOutcomeStatus, AgentTaskRequest, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA, AGENT_TOOL_POLICY_SCHEMA,
    AGENT_TOOL_REQUEST_SCHEMA, AGENT_TOOL_RESULT_SCHEMA,
};
use crate::agent_task_scheduler::{
    AgentTaskExecutionContext, AgentTaskExecutorAdapter, AgentTaskPlan,
    ProviderRouteDiagnosticData, ProviderRouteReadiness,
};
use crate::agent_task_secrets::AgentTaskSecretResolutionError;
use crate::agent_task_timeout::timeout_with_grace;

pub(crate) use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::engine::shell;
pub(crate) use homeboy_core::secret_env_plan::SecretEnvPlan;
use homeboy_core::{component, defaults, Error};

mod admission;
pub(crate) mod artifact_finalization;
mod capacity_readiness;
mod catalog;
pub(crate) mod command_runner;
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
mod runtime_readiness;
mod runtime_types;
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
pub use capacity_readiness::{
    capacity_readiness_from_outcome, capacity_readiness_from_probe_result,
    capacity_readiness_unknown, AgentTaskProviderCapacityReadiness,
    PROVIDER_READINESS_CAPACITY_CLASSIFICATION,
};
pub use catalog::*;
pub use command_runner::{
    provider_command_parts, run_provider_readiness_probe, ProviderReadinessInvocationCapacity,
    ProviderReadinessInvocationResult, PROVIDER_READINESS_RESULT_SCHEMA,
};
pub use dispatchability::{
    admit_plan_provider_dispatchability_for_placement,
    admit_plan_provider_dispatchability_for_runner_ownership,
    admit_plan_provider_dispatchability_with_placement,
    admit_plan_provider_dispatchability_with_providers, evaluate_provider_capacity_with_config,
    evaluate_provider_dispatchability, evaluate_provider_dispatchability_with_cache,
    evaluate_provider_dispatchability_with_config,
    preflight_plan_provider_dispatchability_with_providers,
    preflight_plan_provider_dispatchability_without_runtime_with_providers,
    preflight_provider_dispatchability, preflight_provider_dispatchability_with_config,
    preflight_provider_dispatchability_without_runtime_with_config,
    provider_readiness_is_runner_owned, provider_runtime_readiness_cache_identity_for_plan,
    AgentTaskProviderCredentialStatus, AgentTaskProviderDispatchability,
    AgentTaskProviderDispatchabilityCheck, AgentTaskProviderDispatchabilityChecks,
    AgentTaskProviderDispatchabilityCredentialCheck, AgentTaskProviderLiveInferenceReadiness,
    AgentTaskProviderReadiness, AgentTaskProviderReadinessScope, AgentTaskProviderRuntimeEvidence,
};
pub(crate) use fixture_gate::fixture_provider_outcome;
pub use fixture_gate::is_fixture_backend;
pub(crate) use launch_context::BASE_INHERITED_ENV;
pub use launch_context::{
    AgentTaskProviderLaunchContext, AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_JSON_ENV,
    AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_SCHEMA,
};
pub use resolution::{resolve_provider_for_backend, ProviderResolution};
pub(crate) use runtime_readiness::{effective_provider_config, readiness_request_key};
pub use runtime_readiness::{
    preflight_plan_provider_runtime_readiness_with_providers, ProviderRuntimeReadinessCache,
};
pub use runtime_types::*;
pub use structured_error::{
    normalize_provider_error, normalize_runtime_stream_error,
    normalized_error_failure_classification, normalized_structured_error,
    structured_error_failure_classification, PROVIDER_ACCOUNT_BLOCKED, PROVIDER_ERROR,
    PROVIDER_RATE_LIMITED, PROVIDER_STRUCTURED_ERROR_SCHEMA,
};
pub use types::*;
pub(crate) use usage_cap::provider_capacity_config;
pub use usage_cap::{
    detect_usage_cap, provider_usage_cap_key, provider_usage_cap_key_for_model,
    provider_usage_cap_key_for_request, reset_at_from_outcome, ProviderUsageCapRegistry,
    AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS,
};
pub use workspace_types::*;

#[cfg(test)]
use catalog::component_default_backend;
/// Empty-change classification shared with the scheduler's review-form deferral
/// guard (#15880), so both entry points reach one verdict.
pub(crate) use outcome_normalization::{
    classify_empty_change_outcome, has_known_empty_change_set,
    record_verified_unchanged_empty_attempt, verify_unchanged_empty_checkout,
};

#[cfg(test)]
use command_runner::{
    is_transient_provider_error, provider_command_env, run_provider_command,
    run_provider_command_once, PROVIDER_TRANSIENT_MAX_ATTEMPTS,
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
