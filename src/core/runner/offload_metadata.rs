use crate::command_contract::RunnerWorkload;
use crate::core::execution_contract::EXECUTION_CONTRACT;
use crate::core::gate::{collect_plan_gate_results, HomeboyGateResult};
use crate::core::plan::{HomeboyPlan, PlanStepStatus};
use crate::core::proof::{HomeboyProof, HomeboyProofProvenance, HomeboyProofRunner};

pub const LAB_OFFLOAD_METADATA_SCHEMA: &str = EXECUTION_CONTRACT.lab_offload.metadata_schema;

pub fn lab_offload_metadata(
    plan: &HomeboyPlan,
    source: &str,
    runner_id: Option<&str>,
    runner_mode: Option<&str>,
    status: &str,
    remote_workspace: Option<&str>,
    fallback_reason: Option<&str>,
) -> serde_json::Value {
    lab_offload_metadata_with_workspace_mapping(
        plan,
        source,
        runner_id,
        runner_mode,
        status,
        remote_workspace,
        fallback_reason,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn lab_offload_metadata_with_workspace_mapping(
    plan: &HomeboyPlan,
    source: &str,
    runner_id: Option<&str>,
    runner_mode: Option<&str>,
    status: &str,
    remote_workspace: Option<&str>,
    fallback_reason: Option<&str>,
    workspace_mapping: Option<&serde_json::Value>,
) -> serde_json::Value {
    lab_offload_metadata_with_workspace_mapping_and_runner_workload(
        plan,
        source,
        runner_id,
        runner_mode,
        status,
        remote_workspace,
        fallback_reason,
        workspace_mapping,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn lab_offload_metadata_with_workspace_mapping_and_runner_workload(
    plan: &HomeboyPlan,
    source: &str,
    runner_id: Option<&str>,
    runner_mode: Option<&str>,
    status: &str,
    remote_workspace: Option<&str>,
    fallback_reason: Option<&str>,
    workspace_mapping: Option<&serde_json::Value>,
    runner_workload: Option<&RunnerWorkload>,
) -> serde_json::Value {
    let sync_mode = plan_step_input_string(plan, "lab.sync_workspace", "mode");
    let gate_results = collect_plan_gate_results(plan);
    let proof = plan.proof.clone().or_else(|| {
        lab_offload_proof(
            plan,
            &gate_results,
            source,
            runner_id,
            runner_mode,
            status,
            remote_workspace,
            fallback_reason,
        )
    });
    serde_json::json!({
        "schema": LAB_OFFLOAD_METADATA_SCHEMA,
        "plan_id": plan.id,
        "source": source,
        "status": status,
        "runner_id": runner_id,
        "runner_mode": runner_mode,
        "remote_workspace": remote_workspace,
        "sync_mode": sync_mode,
        "fallback_reason": fallback_reason,
        "capability_preflight": plan_step_status(plan, "lab.capability_preflight"),
        "extension_parity": plan_step_status(plan, "lab.extension_parity"),
        "gate_results": gate_results,
        "proof": proof,
        "patch_captured": plan_has_step(plan, "lab.apply_patch"),
        "workspace_mapping": workspace_mapping,
        "runner_workload": runner_workload,
    })
}

#[allow(clippy::too_many_arguments)]
fn lab_offload_proof(
    plan: &HomeboyPlan,
    gate_results: &[HomeboyGateResult],
    source: &str,
    runner_id: Option<&str>,
    runner_mode: Option<&str>,
    status: &str,
    remote_workspace: Option<&str>,
    fallback_reason: Option<&str>,
) -> Option<HomeboyProof> {
    if gate_results.is_empty() {
        return None;
    }

    let mut notes = vec![
        format!("lab_offload_source={source}"),
        format!("status={status}"),
    ];
    if let Some(runner_id) = runner_id {
        notes.push(format!("runner_id={runner_id}"));
    }
    if let Some(runner_mode) = runner_mode {
        notes.push(format!("runner_mode={runner_mode}"));
    }
    if let Some(remote_workspace) = remote_workspace {
        notes.push(format!("remote_workspace={remote_workspace}"));
    }
    if let Some(fallback_reason) = fallback_reason {
        notes.push(format!("fallback_reason={fallback_reason}"));
    }

    Some(
        HomeboyProof::new(
            format!("{}.lab_offload", plan.id),
            HomeboyProofProvenance {
                runner: HomeboyProofRunner::Homeboy,
                run_id: None,
                task_id: None,
                source_refs: vec![plan.id.clone()],
                notes,
            },
        )
        .gates_requiring_ci_equivalent(gate_results.iter().cloned()),
    )
}

fn plan_step_status(plan: &HomeboyPlan, step_id: &str) -> Option<&'static str> {
    plan.steps
        .iter()
        .find(|step| step.id == step_id)
        .map(|step| match step.status {
            PlanStepStatus::Ready => "ready",
            PlanStepStatus::Missing => "missing",
            PlanStepStatus::Disabled => "disabled",
            PlanStepStatus::Skipped => "skipped",
            PlanStepStatus::Running => "running",
            PlanStepStatus::Success => "success",
            PlanStepStatus::PartialSuccess => "partial_success",
            PlanStepStatus::Failed => "failed",
        })
}

fn plan_step_input_string(plan: &HomeboyPlan, step_id: &str, key: &str) -> Option<String> {
    plan.steps
        .iter()
        .find(|step| step.id == step_id)
        .and_then(|step| step.inputs.get(key))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn plan_has_step(plan: &HomeboyPlan, step_id: &str) -> bool {
    plan.steps.iter().any(|step| step.id == step_id)
}

pub fn capture_lab_offload_subprocess_metadata(metadata: serde_json::Value) {
    if let Ok(raw) = serde_json::to_string(&metadata) {
        std::env::set_var(crate::core::observation::LAB_OFFLOAD_METADATA_ENV, raw);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        lab_offload_metadata, lab_offload_metadata_with_workspace_mapping,
        lab_offload_metadata_with_workspace_mapping_and_runner_workload,
        LAB_OFFLOAD_METADATA_SCHEMA,
    };
    use crate::command_contract::{
        RunnerWorkload, RunnerWorkloadAssignment, RunnerWorkloadCommandFamily, RunnerWorkloadKind,
        RunnerWorkloadMutationPolicy, RunnerWorkloadResultRefs, RunnerWorkloadSecrets,
        RunnerWorkloadState, RunnerWorkloadWorkspaceMappings, RUNNER_WORKLOAD_SCHEMA,
    };
    use crate::core::gate::{HomeboyGateKind, HomeboyGateResult, HomeboyGateStatus};
    use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus, PlanValues};
    use crate::core::proof::{HomeboyProof, HomeboyProofProvenance};

    fn lab_plan() -> HomeboyPlan {
        let mut plan = HomeboyPlan::builder_for_description(PlanKind::LabOffload, "test")
            .mode("lab_offload")
            .build();
        plan.steps.push(
            PlanStep::ready("lab.sync_workspace", "lab.sync_workspace")
                .inputs(PlanValues::new().string("mode", "snapshot"))
                .build(),
        );
        plan.steps.push(
            PlanStep::builder(
                "lab.capability_preflight",
                "lab.capability_preflight",
                PlanStepStatus::Success,
            )
            .gate_result(HomeboyGateResult::new(
                "lab.capability_preflight",
                "Lab capability preflight",
                HomeboyGateKind::Capability,
                HomeboyGateStatus::Passed,
            ))
            .build(),
        );
        plan.steps
            .push(PlanStep::ready("lab.extension_parity", "lab.extension_parity").build());
        plan.steps.push(
            PlanStep::builder(
                "lab.apply_patch",
                "lab.apply_patch",
                PlanStepStatus::Success,
            )
            .build(),
        );
        plan
    }

    #[test]
    fn lab_offload_metadata_records_explicit_auto_skipped_and_fallback_states() {
        let plan = lab_plan();
        let explicit = lab_offload_metadata(
            &plan,
            "explicit",
            Some("lab-explicit"),
            Some("direct_ssh"),
            "offloaded",
            Some("/srv/homeboy/project"),
            None,
        );
        assert_eq!(explicit["schema"], LAB_OFFLOAD_METADATA_SCHEMA);
        assert_eq!(explicit["plan_id"], "lab_offload.test");
        assert_eq!(explicit["source"], "explicit");
        assert_eq!(explicit["status"], "offloaded");
        assert_eq!(explicit["runner_id"], "lab-explicit");
        assert_eq!(explicit["runner_mode"], "direct_ssh");
        assert_eq!(explicit["remote_workspace"], "/srv/homeboy/project");
        assert_eq!(explicit["sync_mode"], "snapshot");
        assert_eq!(explicit["capability_preflight"], "success");
        assert_eq!(
            explicit["gate_results"][0]["id"],
            "lab.capability_preflight"
        );
        assert_eq!(explicit["gate_results"][0]["status"], "passed");
        assert_eq!(explicit["proof"]["schema"], "homeboy/proof/v1");
        assert_eq!(explicit["proof"]["id"], "lab_offload.test.lab_offload");
        assert_eq!(explicit["proof"]["scope"], "targeted");
        assert_eq!(
            explicit["proof"]["gates"][0]["id"],
            "lab.capability_preflight"
        );
        assert_eq!(explicit["proof"]["provenance"]["runner"], "homeboy");
        assert_eq!(
            explicit["proof"]["provenance"]["source_refs"][0],
            "lab_offload.test"
        );
        assert_eq!(
            explicit["proof"]["gaps"][0]["kind"],
            "ci_equivalent_not_recorded"
        );
        assert_eq!(explicit["extension_parity"], "ready");
        assert_eq!(explicit["patch_captured"], true);
        assert!(explicit["workspace_mapping"].is_null());
        assert!(explicit["fallback_reason"].is_null());

        let fallback = lab_offload_metadata(
            &plan,
            "automatic",
            Some("lab"),
            Some("reverse"),
            "fallback",
            None,
            Some("runner connect timed out after 3s"),
        );
        assert_eq!(fallback["source"], "automatic");
        assert_eq!(fallback["status"], "fallback");
        assert_eq!(fallback["runner_id"], "lab");
        assert_eq!(
            fallback["fallback_reason"],
            "runner connect timed out after 3s"
        );

        let skipped = lab_offload_metadata(
            &plan,
            "automatic",
            None,
            None,
            "skipped",
            None,
            Some("no_default_runner"),
        );
        assert_eq!(skipped["source"], "automatic");
        assert_eq!(skipped["status"], "skipped");
        assert!(skipped["runner_id"].is_null());
        assert_eq!(skipped["fallback_reason"], "no_default_runner");
    }

    #[test]
    fn lab_offload_metadata_records_workspace_mapping_when_supplied() {
        let plan = lab_plan();
        let mapping = serde_json::json!({
            "schema": "homeboy/workspace-map/v1",
            "workspaces": [
                {
                    "role": "primary",
                    "local_path": "/Users/user/Developer/app",
                    "remote_path": "/srv/homeboy/_lab_workspaces/app-abc",
                    "sync_mode": "snapshot",
                    "snapshot_identity": "snapshot:abc"
                }
            ]
        });

        let metadata = lab_offload_metadata_with_workspace_mapping(
            &plan,
            "explicit",
            Some("lab"),
            Some("direct_ssh"),
            "offloaded",
            Some("/srv/homeboy/_lab_workspaces/app-abc"),
            None,
            Some(&mapping),
        );

        assert_eq!(metadata["workspace_mapping"], mapping);
    }

    #[test]
    fn lab_offload_metadata_records_runner_workload_when_supplied() {
        let plan = lab_plan();
        let workload = RunnerWorkload {
            schema: RUNNER_WORKLOAD_SCHEMA.to_string(),
            workload_id: "workload-1".to_string(),
            kind: RunnerWorkloadKind {
                command_label: "lint".to_string(),
                command_family: RunnerWorkloadCommandFamily::Quality,
            },
            workspace_mappings: RunnerWorkloadWorkspaceMappings {
                source_path_mode: "cwd_or_path_flag".to_string(),
                workspace_mode_policy: "changed_since_git_else_snapshot".to_string(),
                mapping_ref: Some("workspace_mapping".to_string()),
            },
            required_capabilities: Vec::new(),
            required_secrets: RunnerWorkloadSecrets {
                categories: Vec::new(),
            },
            required_extensions: vec!["browser".to_string()],
            mutation_policy: RunnerWorkloadMutationPolicy {
                capture_patch: false,
                mutation_flag: None,
                allow_dirty_lab_workspace: false,
            },
            assignment: RunnerWorkloadAssignment {
                runner_id: Some("lab".to_string()),
                runner_mode: Some("direct_ssh".to_string()),
                source: Some("explicit".to_string()),
            },
            state: RunnerWorkloadState {
                status: "offloaded".to_string(),
                remote_workspace: Some("/srv/homeboy/app".to_string()),
                fallback_reason: None,
            },
            result_refs: RunnerWorkloadResultRefs {
                plan_id: plan.id.clone(),
                proof_id: None,
                workspace_mapping_ref: Some("workspace_mapping".to_string()),
            },
        };

        let metadata = lab_offload_metadata_with_workspace_mapping_and_runner_workload(
            &plan,
            "explicit",
            Some("lab"),
            Some("direct_ssh"),
            "offloaded",
            Some("/srv/homeboy/app"),
            None,
            None,
            Some(&workload),
        );

        assert_eq!(
            metadata["runner_workload"]["schema"],
            RUNNER_WORKLOAD_SCHEMA
        );
        assert_eq!(metadata["runner_workload"]["workload_id"], "workload-1");
        assert_eq!(
            metadata["runner_workload"]["kind"]["command_family"],
            "quality"
        );
        assert_eq!(
            metadata["runner_workload"]["assignment"]["runner_id"],
            "lab"
        );
        assert_eq!(
            metadata["runner_workload"]["required_extensions"][0],
            "browser"
        );
    }

    #[test]
    fn lab_offload_metadata_prefers_plan_proof_when_present() {
        let proof = HomeboyProof::new(
            "explicit-proof",
            HomeboyProofProvenance::homeboy_run("run-1"),
        );
        let mut plan = lab_plan();
        plan.proof = Some(proof);

        let metadata = lab_offload_metadata(
            &plan,
            "automatic",
            Some("lab"),
            Some("direct_ssh"),
            "fallback",
            None,
            Some("runner connect timed out after 3s"),
        );

        assert_eq!(metadata["proof"]["id"], "explicit-proof");
        assert_eq!(metadata["proof"]["provenance"]["run_id"], "run-1");
    }
}
