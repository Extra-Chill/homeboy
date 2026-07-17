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
            durable_agent_task_plan: request.durable_agent_task_plan,
            source_path: request.source_path,
            verified_cook_baseline: request.verified_cook_baseline,
            require_controller_git_bundle: request.require_controller_git_bundle,
            job_overrides: request.job_overrides,
        })
    }
}

/// Register the runner Lab-offload provider with core. Called once at startup.
pub fn register() {
    homeboy_core::lab_offload::register_lab_offload_provider(Arc::new(RunnerLabOffload));
}
