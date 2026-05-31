use crate::core::plan::{HomeboyPlan, PlanStepStatus};

pub const LAB_OFFLOAD_METADATA_SCHEMA: &str = "homeboy/lab-offload/v1";

pub fn lab_offload_metadata(
    plan: &HomeboyPlan,
    source: &str,
    runner_id: Option<&str>,
    runner_mode: Option<&str>,
    status: &str,
    remote_workspace: Option<&str>,
    fallback_reason: Option<&str>,
) -> serde_json::Value {
    let sync_mode = plan_step_input_string(plan, "lab.sync_workspace", "mode");
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
        "patch_captured": plan_has_step(plan, "lab.apply_patch"),
    })
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

pub fn capture_lab_offload_metadata(metadata: serde_json::Value) {
    capture_lab_offload_subprocess_metadata(metadata);
}

pub fn capture_lab_offload_subprocess_metadata(metadata: serde_json::Value) {
    if let Ok(raw) = serde_json::to_string(&metadata) {
        std::env::set_var(crate::core::observation::LAB_OFFLOAD_METADATA_ENV, raw);
    }
}

#[cfg(test)]
mod tests {
    use super::{lab_offload_metadata, LAB_OFFLOAD_METADATA_SCHEMA};
    use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus, PlanValues};

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
        assert_eq!(explicit["extension_parity"], "ready");
        assert_eq!(explicit["patch_captured"], true);
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
}
