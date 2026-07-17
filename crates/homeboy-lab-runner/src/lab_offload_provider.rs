//! Runner-side implementation of core's `LabOffloadProvider` hook.
//!
//! Maps core's `LabRoutingRequest` onto the runner's `LabOffloadRequest` and
//! executes the offload, keeping the offload machinery inside the runner layer.

use std::sync::Arc;

use homeboy_core::error::Result;
use homeboy_core::lab_offload::{LabOffloadOutcome, LabOffloadProvider};
use homeboy_core::lab_routing::LabRoutingRequest;

use crate::LabOffloadRequest;

/// The runner layer's `LabOffloadProvider`. Registered with core at startup.
pub struct RunnerLabOffload;

impl LabOffloadProvider for RunnerLabOffload {
    fn execute_lab_offload(&self, request: LabRoutingRequest<'_>) -> Result<LabOffloadOutcome> {
        // Core carries the durable agent-task plan opaquely as JSON so it does
        // not depend on the agent-task subsystem. The runner layer owns the
        // typed plan, so deserialize it here and borrow it for the offload.
        let durable_agent_task_plan = request
            .durable_agent_task_plan
            .map(|plan| {
                serde_json::from_value::<homeboy_core::agent_task_scheduler::AgentTaskPlan>(
                    plan.clone(),
                )
            })
            .transpose()
            .map_err(|error| {
                homeboy_core::error::Error::internal_json(
                    error.to_string(),
                    Some("deserialize durable agent-task plan".to_string()),
                )
            })?;
        crate::execute_lab_offload(LabOffloadRequest {
            command: request.command,
            normalized_args: request.normalized_args,
            explicit_runner: request.explicit_runner,
            placement: request.placement,
            allow_local_fallback: request.allow_local_fallback,
            allow_dirty_lab_workspace: request.allow_dirty_lab_workspace,
            skip_deps_hydration: request.skip_deps_hydration,
            capture_patch: request.capture_patch,
            mutation_flag: request.mutation_flag,
            detach_after_handoff: request.detach_after_handoff,
            output_file_requested: request.output_file_requested,
            read_only_polling: request.read_only_polling,
            local_output_file: request.local_output_file,
            durable_agent_task_plan: durable_agent_task_plan.as_ref(),
            source_path: request.source_path,
            verified_cook_baseline: request.verified_cook_baseline,
            require_controller_git_bundle: request.require_controller_git_bundle,
            reuse_compatible_snapshot: request.reuse_compatible_snapshot,
            job_overrides: request.job_overrides,
        })
    }
}

/// Register the runner Lab-offload provider with core. Called once at startup.
pub fn register() {
    homeboy_core::lab_offload::register_lab_offload_provider(Arc::new(RunnerLabOffload));
}
