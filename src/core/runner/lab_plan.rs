use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep};

use super::LabOffloadCommand;

pub(super) fn base_lab_plan(command: Option<&LabOffloadCommand>) -> HomeboyPlan {
    let description = command
        .map(|contract| contract.hot_label)
        .unwrap_or("command");
    HomeboyPlan::builder_for_description(PlanKind::LabOffload, description)
        .mode("lab_offload")
        .build()
}

pub(super) fn with_step(mut plan: HomeboyPlan, step: PlanStep) -> HomeboyPlan {
    plan.steps.push(step);
    plan
}

pub(super) fn disabled_select_runner_plan(plan: HomeboyPlan, reason: &'static str) -> HomeboyPlan {
    with_step(
        plan,
        PlanStep::disabled_with_reason("lab.select_runner", "lab.select_runner", reason).build(),
    )
}
