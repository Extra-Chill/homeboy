//! Lab offload request execution and runner-selection orchestration.
//!
//! This module is the working core of `core::runners::execute_lab_offload`. It
//! turns a `LabOffloadRequest` into either a local-fallback `RunLocal` outcome
//! or an `Offloaded` outcome by:
//!
//! 1. Validating the command contract and the user's runner choice
//!    (`execute_lab_offload`).
//! 2. Preparing/connecting the chosen runner.
//! 3. Walking through workspace sync, capability preflight, argument
//!    remapping, secret hydration, and remote exec inside
//!    `run_lab_offload_inner`.
//! 4. Translating runner failures into either a structured fallback outcome or
//!    a precise validation error, via the helpers at the bottom of the file.
//!
//! Trace-target git-fetch calculation lives in `trace_fetch_refs`; this module
//! only decides when those refs participate in workspace sync.

mod errors;
mod execute;
mod fallback_commands;
mod inner;
mod metadata;
mod overhead;
mod resident;
mod telemetry;
mod types;
mod workspace_stage;

#[cfg(test)]
mod tests;

// Public API surface.
pub use execute::execute_lab_offload;
pub use types::{
    LabLocalExecutionPolicy, LabOffloadCommand, LabOffloadOutcome, LabOffloadRequest,
    LabOffloadSourcePathMode, LabOffloadWorkspaceModePolicy,
};

// Shared external imports, re-exported so submodules can pull them via
// `use super::*`.
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::command_contract::lab_runner_support_summary;
use crate::core::agent_task_lifecycle;
use crate::core::agent_tasks::provider::provider_runner_source_contracts;
use crate::core::engine::shell;
use crate::core::plan::{HomeboyPlan, PlanStep, PlanStepStatus, PlanValues};
use crate::core::redaction::{redact_argv, redact_argv_display};
use crate::core::server::{self, SshClient};
use crate::core::source_snapshot::SourceSnapshot;
use crate::core::{Error, ErrorCode, Result};

use super::super::command_path::preflight_remote_argv_path_translation;
use super::super::daemon_health::runner_daemon_health_failure;
use super::super::execution::{
    append_failure_context_error_summary, lab_offload_handoff_hints,
    runner_exec_failure_context_from_output, runner_exec_failure_context_remediation_hint,
    DaemonJobHandoffState,
};
use super::super::lab_apply::apply_lab_offload_patch;
use super::super::lab_args::{
    inject_agent_task_default_provider_config_in_args, lab_at_file_specs, lab_offload_source_path,
    materialize_agent_task_specs_in_args, preflight_provider_config_paths_materialized_in_args,
    provider_config_runtime_manifest, remap_lab_at_file_args, remap_path_settings_in_args,
    remap_provider_config_in_args, rewrite_lab_offload_args,
    rewrite_runner_resident_lab_offload_args, LabAtFileSpec, LabPathRemap,
};
use super::super::lab_capabilities::lab_runner_capability_contract;
use super::super::lab_command::lab_offload_command_prefix;
use super::super::lab_env::{
    build_lab_offload_env_with_passthroughs, forward_rig_component_path_env,
    forward_wordpress_dependency_paths_env, misplaced_runner_exec_wait_timeout_warning,
    settings_env_diagnostics,
};
use super::super::lab_plan::{base_lab_plan, disabled_select_runner_plan, with_step};
use super::super::lab_selection::{
    prepare_lab_runner_for_offload, release_gate_local_hot_denied_error,
    resolve_lab_runner_selection, status_tunnel_mode, LabRunnerPreparation, LabRunnerSelection,
    LabRunnerSelectionSource,
};
use super::super::lab_workspaces::{
    agent_task_plan_extra_workspaces, agent_task_provider_runtime_component_extra_workspaces,
    lab_extra_workspaces, lab_runtime_overlay_metadata, lab_runtime_overlays,
    lab_workspace_mapping_metadata, path_setting_extra_workspaces,
    preflight_provider_config_source_cli_dependencies, provider_config_extra_workspaces,
    rig_component_path_env_extra_workspaces, runtime_overlay_env_overrides,
    sync_extra_lab_workspaces, sync_lab_runtime_overlays,
    workspace_mapping_entries_for_git_dependency, workspace_mapping_entry,
    workspace_mapping_entry_for_validation_dependency, LabWorkspaceMappingEntry,
};
use super::super::offload_changed_since::LabOffloadChangedSincePreflight;
use super::super::{
    evaluate_lab_runner_capabilities_for_runner, exec, lab_offload_changed_since_ref,
    lab_offload_metadata, lab_offload_metadata_with_workspace_mapping, load,
    plan_managed_runner_source_syncs, preflight_lab_offload_changed_since,
    prepare_git_lab_offload_changed_since, prepare_lab_runner_capability, rig_materialization,
    status, sync_workspace, LabRunnerGateDecision, RunnerCapabilityPreflight, RunnerExecOptions,
    RunnerStatusReport, RunnerWorkspaceApplyOutput, RunnerWorkspaceSyncMode,
    RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
};

use super::super::workload::{build_runner_workload, RunnerWorkloadBuildInput};
use super::agent_task_bridge::{
    agent_task_dispatch_run_isolation_token, ensure_agent_task_dispatch_run_id_with,
    lab_pre_dispatch_failure_message, mirror_agent_task_run_plan_lifecycle,
    parse_offloaded_agent_task_handoff_from_outputs, sync_inline_agent_task_file,
};
use super::evidence::terminal_lab_run_evidence;
use super::fallback::{
    is_build_command, local_execution_denied_error, skipped_automatic_run_local,
    skipped_automatic_run_local_with_overhead, unsupported_build_lab_error,
};
use super::provider_preflight::preflight_agent_task_provider_on_runner;
use super::secrets::{build_lab_secret_env_handoff_plan, preflight_agent_task_runner_secret_env};
use super::trace_fetch_refs::lab_offload_git_fetch_refs;
use super::workspace_plan::{lab_workspace_sync_mode, preflight_required_git_checkout_workspace};
#[cfg(test)]
use super::workspace_plan::{
    lab_workspace_sync_mode_with_source_policy, preflight_patch_provider_git_checkout,
};

// Cross-submodule item visibility for `use super::*` consumers.
pub(crate) use errors::*;
pub(crate) use execute::*;
pub(crate) use fallback_commands::*;
pub(crate) use inner::*;
pub(crate) use metadata::*;
pub(crate) use overhead::*;
pub(crate) use resident::*;
pub(crate) use telemetry::*;
pub(crate) use workspace_stage::*;
