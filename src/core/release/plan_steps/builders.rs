use crate::core::plan::{PlanStep, PlanValues};

pub(super) type StepConfig = PlanValues;

pub(super) fn ready_step(
    id: &str,
    step_type: &str,
    label: impl Into<String>,
    needs: Vec<String>,
    config: StepConfig,
) -> PlanStep {
    PlanStep::ready_labeled(id, step_type, label, needs, config)
}

pub(super) fn disabled_step(
    id: &str,
    step_type: &str,
    label: impl Into<String>,
    config: StepConfig,
) -> PlanStep {
    PlanStep::disabled(id, step_type)
        .label(label)
        .inputs(config)
        .build()
}

pub(super) fn string_config(key: &str, value: impl Into<String>) -> StepConfig {
    StepConfig::new().string(key, value)
}

pub(super) fn string_array_config(key: &str, values: &[String]) -> StepConfig {
    StepConfig::new().json(key, values)
}
