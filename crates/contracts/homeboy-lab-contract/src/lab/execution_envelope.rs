//! Typed Lab adapters for the transport-neutral runner execution envelope.

use homeboy_runner_contract::{
    RunnerExecutionEnvelope, RunnerExecutionMutationPolicy, RunnerExecutionResultRefs,
    RunnerExecutionSource,
};

use super::workload::LabRunnerWorkload;

pub fn runner_execution_envelope_from_workload(
    workload: LabRunnerWorkload,
) -> RunnerExecutionEnvelope {
    let mutation_policy = RunnerExecutionMutationPolicy {
        capture_patch: workload.mutation_policy.capture_patch,
        mutation_flag: workload.mutation_policy.mutation_flag.clone(),
        allow_dirty_workspace: workload.mutation_policy.allow_dirty_lab_workspace,
    };
    let result_refs = RunnerExecutionResultRefs {
        plan_id: Some(workload.result_refs.plan_id.clone()),
        job_id: workload.result_refs.job_id.clone(),
        run_id: workload.result_refs.proof_id.clone(),
        mirror_run_id: workload.result_refs.mirror_run_id.clone(),
        artifacts: workload.result_refs.artifacts.clone(),
        ..RunnerExecutionResultRefs::default()
    };

    RunnerExecutionEnvelope {
        schema: homeboy_runner_contract::RUNNER_EXECUTION_ENVELOPE_SCHEMA.to_string(),
        envelope_id: workload.workload_id.clone(),
        source: RunnerExecutionSource {
            kind: "runner_workload".to_string(),
            ref_id: Some(workload.workload_id.clone()),
        },
        runner_workload: Some(serde_json::to_value(workload).expect("Lab workload serializes")),
        agent_task: None,
        secret_env: None,
        env_materialization: None,
        dispatch: None,
        lifecycle: None,
        lifecycle_policy: Default::default(),
        artifact_declarations: Vec::new(),
        loop_policy: Default::default(),
        mutation_policy,
        publication_intent: Default::default(),
        result_refs,
        metadata: serde_json::Value::Null,
    }
}

pub fn lab_runner_workload_from_execution_envelope(
    envelope: &RunnerExecutionEnvelope,
) -> Result<Option<LabRunnerWorkload>, serde_json::Error> {
    envelope
        .runner_workload
        .as_ref()
        .map(|workload| serde_json::from_value(workload.clone()))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lab::workload::{
        LabRunnerWorkloadAssignment, LabRunnerWorkloadCommandFamily, LabRunnerWorkloadKind,
        LabRunnerWorkloadMutationPolicy, LabRunnerWorkloadResultRefs, LabRunnerWorkloadSecrets,
        LabRunnerWorkloadState, LabRunnerWorkloadWorkspaceMappings, LAB_RUNNER_WORKLOAD_SCHEMA,
    };
    use crate::secret_env_plan::SecretEnvPlan;

    #[test]
    fn lab_workload_round_trips_through_the_opaque_runner_boundary() {
        let workload = LabRunnerWorkload {
            schema: LAB_RUNNER_WORKLOAD_SCHEMA.to_string(),
            workload_id: "plan-1.runner_workload".to_string(),
            kind: LabRunnerWorkloadKind {
                command_label: "test".to_string(),
                command_family: LabRunnerWorkloadCommandFamily::Quality,
            },
            agent_task: None,
            notification_route: None,
            workspace_mappings: LabRunnerWorkloadWorkspaceMappings {
                source_path_mode: "cwd_or_path_flag".to_string(),
                workspace_mode_policy: "git".to_string(),
                mapping_ref: Some("mapping-1".to_string()),
            },
            required_capabilities: Vec::new(),
            required_secrets: LabRunnerWorkloadSecrets {
                categories: Vec::new(),
                secret_env_plan: SecretEnvPlan::default(),
            },
            required_extensions: Vec::new(),
            required_extension_revisions: Vec::new(),
            mutation_policy: LabRunnerWorkloadMutationPolicy {
                capture_patch: true,
                mutation_flag: Some("--apply".to_string()),
                allow_dirty_lab_workspace: false,
            },
            assignment: LabRunnerWorkloadAssignment {
                runner_id: Some("runner-a".to_string()),
                runner_mode: Some("ssh".to_string()),
                source: Some("default".to_string()),
            },
            state: LabRunnerWorkloadState {
                status: "assigned".to_string(),
                remote_workspace: Some("/workspace/project".to_string()),
                fallback_reason: None,
            },
            result_refs: LabRunnerWorkloadResultRefs {
                plan_id: "plan-1".to_string(),
                proof_id: Some("proof-1".to_string()),
                workspace_mapping_ref: Some("mapping-1".to_string()),
                job_id: Some("job-1".to_string()),
                mirror_run_id: None,
                artifacts: Vec::new(),
            },
        };
        let envelope = runner_execution_envelope_from_workload(workload.clone());
        let value = serde_json::to_value(&envelope).expect("envelope serializes");
        assert_eq!(
            value["runner_workload"],
            serde_json::to_value(&workload).unwrap()
        );
        assert_eq!(
            lab_runner_workload_from_execution_envelope(&envelope).unwrap(),
            Some(workload)
        );
    }
}
