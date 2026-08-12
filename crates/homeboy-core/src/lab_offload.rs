//! Lab-offload contract types and execution hook.
//!
//! `lab_routing` (core) routes a command to Lab offload and consumes the
//! outcome, but the actual offload execution (workspace materialization, remote
//! dispatch, patch capture) is runner behavior. The request/command/outcome
//! types are core-plan-based, so they live here; the execution itself is
//! inverted behind [`LabOffloadProvider`] so `lab_routing` stays in core while
//! the runner crate performs the offload.
//!
//! With no provider registered the no-op provider errors clearly — offload is
//! only reached when a Lab command was actually dispatched.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::lab_contract::{
    LabCommandContract, LabRigWorkloadArguments, LabRunnerWorkloadCapability, LabSourcePathMode,
    LabWorkspaceModePolicy,
};
use crate::plan::{HomeboyPlan, PlanKind, PlanStep};

/// Per-job overrides carried into a Lab offload.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabJobOverrides {
    pub env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    pub workspace_root: Option<String>,
}

/// A resolved Lab command with its required extensions/capabilities/workload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LabOffloadCommand {
    pub command: LabCommandContract,
    pub required_extensions: Vec<String>,
    pub required_capabilities: Vec<LabRunnerWorkloadCapability>,
    pub workload: Option<LabRigWorkloadArguments>,
}

impl std::ops::Deref for LabOffloadCommand {
    type Target = LabCommandContract;

    fn deref(&self) -> &Self::Target {
        &self.command
    }
}

impl std::ops::DerefMut for LabOffloadCommand {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.command
    }
}

pub type LabOffloadSourcePathMode = LabSourcePathMode;
pub type LabOffloadWorkspaceModePolicy = LabWorkspaceModePolicy;

/// The outcome of a Lab offload: a controller plan plus (for executed offloads)
/// captured output.
pub enum LabOffloadOutcome {
    RunLocal {
        plan: HomeboyPlan,
        metadata: Option<serde_json::Value>,
        messages: Vec<String>,
    },
    Offloaded {
        plan: HomeboyPlan,
        stdout: String,
        stderr: String,
        exit_code: i32,
        output_file_content: Option<String>,
    },
    InFlight {
        plan: HomeboyPlan,
        stdout: String,
        stderr: String,
        exit_code: i32,
        output_file_content: Option<String>,
    },
}

/// Executes a Lab offload for a routed request. Implemented by the runner layer.
pub trait LabOffloadProvider: Send + Sync {
    fn execute_lab_offload(
        &self,
        request: crate::lab_routing::LabRoutingRequest<'_>,
    ) -> Result<LabOffloadOutcome>;
}

struct NoopProvider;

impl LabOffloadProvider for NoopProvider {
    fn execute_lab_offload(
        &self,
        request: crate::lab_routing::LabRoutingRequest<'_>,
    ) -> Result<LabOffloadOutcome> {
        if request.command.is_none()
            && request.placement_decision.selected
                == homeboy_lab_runner_contract::EffectiveExecutionPlacement::Local
        {
            // Selecting local execution for an uncontracted command never needs
            // runner behavior, so core can return its typed routing outcome even
            // when this process has no runner layer to register.
            let mut plan = HomeboyPlan::builder_for_description(PlanKind::LabOffload, "command")
                .mode("lab_offload")
                .build();
            plan.steps.push(
                PlanStep::disabled_with_reason(
                    "lab.select_runner",
                    "lab.select_runner",
                    "command has no Lab contract",
                )
                .build(),
            );
            return Ok(LabOffloadOutcome::RunLocal {
                plan,
                metadata: None,
                messages: Vec::new(),
            });
        }
        Err(Error::internal_unexpected(
            "runner subsystem is unavailable: cannot execute a Lab offload",
        ))
    }
}

homeboy_engine_primitives::provider_registry_arc! {
    provider: dyn LabOffloadProvider,
    noop: NoopProvider,
    /// Register the Lab-offload provider. Called once at startup by the runner layer.
    register: pub fn register_lab_offload_provider,
    /// The active provider (or the no-op provider). The `Arc` is cloned out so
    /// the registry lock is not held during the (potentially long) offload.
    active: fn active_provider,
}

/// Execute a Lab offload via the registered provider (or the no-op provider).
pub(crate) fn execute_lab_offload(
    request: crate::lab_routing::LabRoutingRequest<'_>,
) -> Result<LabOffloadOutcome> {
    active_provider().execute_lab_offload(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_provider_returns_local_outcome_for_uncontracted_local_decision() {
        let args = ["homeboy".to_string(), "status".to_string()];
        let outcome = NoopProvider
            .execute_lab_offload(crate::lab_routing::LabRoutingRequest {
                placement_decision: crate::lab_routing::compatibility_placement_decision(
                    homeboy_lab_runner_contract::Placement::Auto,
                    None,
                    false,
                ),
                command: None,
                normalized_args: &args,
                explicit_runner: None,
                placement: homeboy_lab_runner_contract::Placement::Auto,
                allow_local_fallback: false,
                allow_dirty_lab_workspace: false,
                skip_deps_hydration: false,
                preserve_workspace_on_failure: false,
                capture_patch: false,
                mutation_flag: None,
                timeout: None,
                placement_outcome_target: None,
                detach_after_handoff: false,
                output_file_requested: false,
                read_only_polling: false,
                local_output_file: None,
                durable_agent_task_plan: None,
                durable_run_id: None,
                source_path: None,
                verified_cook_baseline: None,
                require_controller_git_bundle: false,
                reuse_compatible_snapshot: false,
                job_overrides: LabJobOverrides::default(),
            })
            .expect("local placement does not require a runner provider");

        assert!(matches!(outcome, LabOffloadOutcome::RunLocal { .. }));
    }
}
