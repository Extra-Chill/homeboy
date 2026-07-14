use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(not(test))]
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::agent_runtime_manifest::AgentRuntimeDiscoveryDiagnostic;
use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskArtifactsPathProvenance, AgentTaskDiagnostic, AgentTaskEvidenceRef,
    AgentTaskExecutionState, AgentTaskExecutorRequest, AgentTaskFailureClassification,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest, AgentTaskTypedArtifact,
    AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
    AGENT_TOOL_POLICY_SCHEMA, AGENT_TOOL_REQUEST_SCHEMA, AGENT_TOOL_RESULT_SCHEMA,
};
use crate::core::agent_task_gate_executor::{is_repo_local_gate_request, run_repo_local_gate_task};
use crate::core::agent_task_scheduler::{
    AgentTaskExecutionContext, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use crate::core::agent_task_secrets::{
    resolve_secret_env_with_fallbacks, secret_env_status_with_fallbacks,
    AgentTaskSecretResolutionError,
};
use crate::core::agent_task_timeout::timeout_with_grace;
use crate::core::command_invocation::CommandInvocation;
use crate::core::engine::shell;
use crate::core::secret_env_plan::{SecretEnvPlan, SecretEnvStatus};
use crate::core::{agent_runtime_manifest, component, defaults, extension, Error};

pub(crate) mod artifact_finalization;
mod catalog;
pub(crate) mod command_runner;
mod config_preflight;
mod executor;
mod fixtures;
mod outcome_normalization;
mod resolution;
mod runner_readiness;
mod runtime_preflight_checks;
mod runtime_types;
mod secret_types;
mod secrets;
mod types;
mod workspace_types;

#[cfg(test)]
mod tests;

pub use catalog::*;
pub(crate) use command_runner::{
    probe_provider_executor_resolves, provider_command_parts, ProviderExecutorResolution,
};
pub(crate) use config_preflight::preflight_plan_provider_config_with_providers;
pub use resolution::{resolve_provider_for_backend, ProviderResolution};
pub(crate) use resolution::{
    role_aliases_for_executor, role_aliases_for_provider, selector_runtime_provider_hint,
    timeout_artifact_discovery_for_executor,
};
pub use runtime_preflight_checks::{
    ensure_runtime_preflight_checks, evaluate_runtime_preflight_checks, RuntimePreflightConflict,
    RuntimePreflightReadiness,
};
pub use runtime_types::*;
pub use secret_types::*;
pub(crate) use secrets::{
    provider_runner_secret_env_for_plan_with_providers,
    provider_secret_sources_for_plan_with_providers,
};
pub(crate) use types::wildcard_match;
pub use types::*;
use types::{default_metadata, is_empty_metadata};
pub use workspace_types::*;

#[cfg(test)]
use catalog::{
    component_default_backend, enforce_runtime_preflight_checks_for_plan_with_providers,
    validate_provider_runner_readiness_for_backend_with_providers,
};
#[cfg(test)]
use command_runner::{
    is_transient_provider_error, provider_command_env, render_provider_command_display,
    run_provider_command, run_provider_command_once, PROVIDER_TRANSIENT_MAX_ATTEMPTS,
};
#[cfg(test)]
use fixtures::fixture_artifact;
#[cfg(test)]
use outcome_normalization::{
    normalize_provider_outcome_roles, surface_provider_run_result_diagnostics,
};
#[cfg(test)]
use resolution::{
    discover_agent_task_executor_providers, provider_requires_cwd_git_checkout_with_providers,
    select_provider_by_backend,
};
#[cfg(test)]
use secrets::{apply_provider_runner_secret_env_contracts_with_providers, provider_secret_sources};
