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
    LabRigWorkloadArguments, LabRoutingPolicy, LabRunnerWorkloadCapability, LabSecretEnvSource,
    LabSourcePathMode, LabWorkspaceModePolicy,
};
use crate::plan::{HomeboyPlan, PlanKind, PlanStep};

/// Per-job overrides carried into a Lab offload.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabJobOverrides {
    pub env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    pub workspace_root: Option<String>,
}

/// Owned, resolved command shared by offload execution and durable staging.
///
/// Field names retain the persisted v1 staging-command shape. Static routing
/// declarations are resolved into this type once, before crossing that boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabOffloadCommand {
    pub hot_label: String,
    pub portable: bool,
    /// Retain local-only diagnostics; portable v1 recipes omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_only_reason: Option<String>,
    pub source_path_mode: LabSourcePathMode,
    pub workspace_mode_policy: LabWorkspaceModePolicy,
    pub capture_mutation_patch: bool,
    pub mutation_flag: Option<String>,
    pub extra_required_capabilities: Vec<String>,
    pub secret_env_sources: Vec<LabSecretEnvSource>,
    pub routing_policy: LabRoutingPolicy,
    pub required_extensions: Vec<String>,
    pub required_capabilities: Vec<LabRunnerWorkloadCapability>,
    pub workload: Option<LabRigWorkloadArguments>,
}

impl LabOffloadCommand {
    pub fn is_portable(&self) -> bool {
        self.portable
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
        if request.placement_decision.selected
            == homeboy_lab_contract::EffectiveExecutionPlacement::Local
        {
            // The controller has already made the authoritative placement
            // decision. A local outcome never needs runner behavior, whether or
            // not the command also carries a Lab contract.
            let mut plan = HomeboyPlan::builder_for_description(PlanKind::LabOffload, "command")
                .mode("lab_offload")
                .build();
            plan.steps.push(
                PlanStep::disabled_with_reason(
                    "lab.select_runner",
                    "lab.select_runner",
                    "command selected local execution",
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
    fn persisted_staging_command_remains_readable_without_conversion() {
        let payload = serde_json::json!({
            "hot_label": "bench",
            "portable": true,
            "source_path_mode": "CwdOrPathFlag",
            "workspace_mode_policy": "GitCheckoutRequired",
            "capture_mutation_patch": true,
            "mutation_flag": "--apply",
            "extra_required_capabilities": ["opaque"],
            "secret_env_sources": ["AgentTask"],
            "routing_policy": {
                "default_lab_offload": true,
                "infer_source_path_tools": true,
                "release_gate": false,
                "requires_extension_parity": true,
                "read_only_polling": false,
                "offload_only_when_hot": false,
                "preserve_result_payload": false
            },
            "required_extensions": ["rust"],
            "required_capabilities": [{"name": "opaque", "required": true}],
            "workload": {
                "kind": "Bench",
                "rig_ids": ["fixture"],
                "component": "homeboy",
                "extension_overrides": ["rust"]
            }
        });
        let command: LabOffloadCommand = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(serde_json::to_value(&command).unwrap(), payload);
        assert!(command.is_portable());
        assert_eq!(command.workload.unwrap().rig_ids, ["fixture"]);
    }

    #[test]
    fn resolved_local_only_command_retains_its_reason_after_persistence() {
        let command = crate::lab_routing::lab_offload_command_from_contract(
            crate::lab_contract::LabCommandContract::local_only("rig up", "controller-owned"),
            Vec::new(),
        );
        let restored: LabOffloadCommand =
            serde_json::from_value(serde_json::to_value(&command).unwrap()).unwrap();
        assert_eq!(restored, command);
        assert!(!restored.is_portable());
        assert_eq!(
            restored.local_only_reason.as_deref(),
            Some("controller-owned")
        );
    }

    #[test]
    fn noop_provider_returns_local_outcome_for_uncontracted_local_decision() {
        let args = ["homeboy".to_string(), "status".to_string()];
        let outcome = NoopProvider
            .execute_lab_offload(crate::lab_routing::LabRoutingRequest {
                placement_decision: crate::lab_routing::compatibility_placement_decision(
                    homeboy_lab_contract::Placement::Auto,
                    None,
                    false,
                ),
                command: None,
                normalized_args: &args,
                explicit_runner: None,
                placement: homeboy_lab_contract::Placement::Auto,
                allow_local_fallback: false,
                allow_dirty_lab_workspace: false,
                skip_deps_hydration: false,
                delete_workspace_on_failure: false,
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
                expected_source_snapshot_identity: None,
                verified_cook_baseline: None,
                require_controller_git_bundle: false,
                job_overrides: LabJobOverrides::default(),
            })
            .expect("local placement does not require a runner provider");

        assert!(matches!(outcome, LabOffloadOutcome::RunLocal { .. }));
    }
}
