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
        let mut durable_agent_task_plan = request
            .durable_agent_task_plan
            .map(|plan| {
                serde_json::from_value::<homeboy_agents::agent_task_scheduler::AgentTaskPlan>(
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
        if let Some(plan) = durable_agent_task_plan.as_mut() {
            validate_runner_plan(plan)?;
            if !plan.metadata.is_object() {
                plan.metadata = serde_json::json!({ "legacy_metadata": plan.metadata });
            }
            plan.metadata["execution_placement_decision"] =
                serde_json::to_value(&request.placement_decision).map_err(|error| {
                    homeboy_core::error::Error::internal_json(
                        error.to_string(),
                        Some("serialize execution placement decision".to_string()),
                    )
                })?;
        }
        crate::execute_lab_offload(LabOffloadRequest {
            placement_decision: request.placement_decision,
            command: request.command,
            normalized_args: request.normalized_args,
            explicit_runner: request.explicit_runner,
            placement: request.placement,
            allow_local_fallback: request.allow_local_fallback,
            allow_dirty_lab_workspace: request.allow_dirty_lab_workspace,
            skip_deps_hydration: request.skip_deps_hydration,
            preserve_workspace_on_failure: request.preserve_workspace_on_failure,
            capture_patch: request.capture_patch,
            mutation_flag: request.mutation_flag,
            placement_outcome_target: request.placement_outcome_target,
            detach_after_handoff: request.detach_after_handoff,
            output_file_requested: request.output_file_requested,
            read_only_polling: request.read_only_polling,
            local_output_file: request.local_output_file,
            durable_agent_task_plan: durable_agent_task_plan.as_ref(),
            durable_run_id: request.durable_run_id,
            source_path: request.source_path,
            expected_source_snapshot_identity: request.expected_source_snapshot_identity,
            verified_cook_baseline: request.verified_cook_baseline,
            require_controller_git_bundle: request.require_controller_git_bundle,
            reuse_compatible_snapshot: request.reuse_compatible_snapshot,
            job_overrides: request.job_overrides,
        })
    }
}

fn validate_runner_plan(plan: &homeboy_agents::agent_task_scheduler::AgentTaskPlan) -> Result<()> {
    plan.validate_managed_services().map_err(|message| {
        homeboy_core::error::Error::validation_invalid_argument(
            "services.cleanup_deadline_ms",
            message,
            None,
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use homeboy_agents::agent_task_scheduler::{
        AgentTaskManagedService, AgentTaskManagedServiceLifecycle, AgentTaskPlan,
    };

    #[test]
    fn runner_rejects_an_invalid_opaque_plan_before_execution() {
        let mut plan = AgentTaskPlan::new("invalid-runner-plan", Vec::new());
        plan.services.push(AgentTaskManagedService {
            version: AgentTaskManagedService::VERSION,
            id: "invalid".to_string(),
            command: vec!["fixture".to_string()],
            cwd: None,
            env: HashMap::new(),
            env_allowlist: Vec::new(),
            secret_env: Vec::new(),
            secret_env_plan: None,
            host: "127.0.0.1".to_string(),
            port: None,
            port_env: None,
            socket_handoff: false,
            readiness: None,
            cleanup_deadline_ms: 0,
            public_url: None,
            browser_origin_probe: None,
            lifecycle: AgentTaskManagedServiceLifecycle::Plan,
            target: None,
        });

        let error = validate_runner_plan(&plan).expect_err("invalid opaque plan");
        assert_eq!(
            error.code,
            homeboy_core::error::ErrorCode::ValidationInvalidArgument
        );
    }
}

/// Register the runner Lab-offload provider with core. Called once at startup.
pub fn register() {
    homeboy_core::lab_offload::register_lab_offload_provider(Arc::new(RunnerLabOffload));
}
