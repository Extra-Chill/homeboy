use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::trace as extension_trace;
use homeboy::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanValues};
use homeboy::core::rig;
use homeboy::core::trace_experiment::{self, TraceExperimentContext};

use super::{TraceArgs, TraceRigContext};

pub(super) struct TraceExperimentRunPlan<'a> {
    plan: HomeboyPlan,
    execution: TraceExperimentExecutionContext<'a>,
}

struct TraceExperimentExecutionContext<'a> {
    spec: &'a rig::TraceExperimentSpec,
    context: &'a TraceRigContext,
}

impl TraceExperimentRunPlan<'_> {
    fn experiment_name(&self) -> &str {
        self.plan
            .inputs
            .get("experiment")
            .and_then(|value| value.as_str())
            .expect("trace experiment plan missing experiment input")
    }

    fn phase_steps(&self, phase: &str) -> Vec<&PlanStep> {
        let kind = format!("trace.experiment.{phase}");
        self.plan
            .steps
            .iter()
            .filter(|step| step.kind == kind && trace_experiment_step_phase(step) == Some(phase))
            .collect()
    }
}

pub(super) fn trace_experiment_plan_for_args<'a>(
    args: &TraceArgs,
    rig_context: Option<&'a TraceRigContext>,
) -> homeboy::core::Result<Option<TraceExperimentRunPlan<'a>>> {
    let Some(name) = args.experiment.as_deref() else {
        return Ok(None);
    };
    let context = rig_context.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "--experiment",
            "trace experiment plans require --rig so Homeboy can read rig metadata",
            None,
            None,
        )
    })?;
    let experiment = context
        .rig_spec
        .trace_experiments
        .get(name)
        .ok_or_else(|| {
            let available = context
                .rig_spec
                .trace_experiments
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            homeboy::core::Error::validation_invalid_argument(
                "--experiment",
                format!(
                    "unknown trace experiment '{}' for rig '{}'",
                    name, context.rig_spec.id
                ),
                Some(format!(
                    "available experiments: {}",
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                )),
                None,
            )
        })?;
    Ok(Some(TraceExperimentRunPlan {
        plan: trace_experiment_plan(&context.rig_spec.id, name, experiment),
        execution: TraceExperimentExecutionContext {
            spec: experiment,
            context,
        },
    }))
}

fn trace_experiment_plan(
    rig_id: &str,
    name: &str,
    experiment: &rig::TraceExperimentSpec,
) -> HomeboyPlan {
    HomeboyPlan::builder_for_description(PlanKind::Trace, format!("{rig_id} {name}"))
        .mode("experiment")
        .inputs(
            PlanValues::new()
                .string("rig_id", rig_id)
                .string("experiment", name),
        )
        .steps(trace_experiment_steps(name, experiment))
        .summarize()
        .build()
}

fn trace_experiment_steps(name: &str, experiment: &rig::TraceExperimentSpec) -> Vec<PlanStep> {
    let setup =
        experiment.setup.iter().enumerate().map(|(index, command)| {
            trace_experiment_step("setup", name, index + 1, &command.command)
        });
    let teardown = experiment
        .teardown
        .iter()
        .enumerate()
        .map(|(index, command)| {
            trace_experiment_step("teardown", name, index + 1, &command.command)
        });

    setup.chain(teardown).collect()
}

fn trace_experiment_step(phase: &str, name: &str, index: usize, command: &str) -> PlanStep {
    PlanStep::ready(
        format!("trace.experiment.{phase}.{index}"),
        format!("trace.experiment.{phase}"),
    )
    .label(format!("{phase} trace experiment {name}"))
    .scope(vec![name.to_string()])
    .inputs(
        PlanValues::new()
            .string("experiment", name)
            .number("index", index as u64)
            .string("phase", phase)
            .string("command", command),
    )
    .build()
}

fn trace_experiment_context<'a>(context: &'a TraceRigContext) -> TraceExperimentContext<'a> {
    TraceExperimentContext {
        rig_spec: &context.rig_spec,
        package_root: context.rig_package_root.as_deref(),
    }
}

pub(super) fn trace_experiment_settings(
    plan: Option<&TraceExperimentRunPlan>,
) -> homeboy::core::Result<Vec<(String, serde_json::Value)>> {
    let Some(plan) = plan else {
        return Ok(Vec::new());
    };
    Ok(trace_experiment::resolve_settings(
        &trace_experiment_context(plan.execution.context),
        plan.execution.spec,
    ))
}

pub(super) fn trace_experiment_env(
    plan: Option<&TraceExperimentRunPlan>,
) -> homeboy::core::Result<Vec<(String, String)>> {
    let Some(plan) = plan else {
        return Ok(Vec::new());
    };
    Ok(trace_experiment::resolve_env(
        &trace_experiment_context(plan.execution.context),
        plan.execution.spec,
    ))
}

pub(super) fn run_trace_experiment_setup_for_plan(
    plan: Option<&TraceExperimentRunPlan>,
    run_dir: &RunDir,
) -> homeboy::core::Result<()> {
    let Some(plan) = plan else {
        return Ok(());
    };
    validate_trace_experiment_plan_phase(plan, "setup", plan.execution.spec.setup.len())?;
    trace_experiment::run_phase(
        &trace_experiment_context(plan.execution.context),
        plan.experiment_name(),
        "setup",
        &plan.execution.spec.setup,
        &plan.execution.spec.env,
        run_dir,
    )
}

pub(super) fn run_trace_experiment_teardown_for_plan(
    plan: Option<&TraceExperimentRunPlan>,
    run_dir: &RunDir,
) -> homeboy::core::Result<()> {
    let Some(plan) = plan else {
        return Ok(());
    };
    validate_trace_experiment_plan_phase(plan, "teardown", plan.execution.spec.teardown.len())?;
    trace_experiment::run_phase(
        &trace_experiment_context(plan.execution.context),
        plan.experiment_name(),
        "teardown",
        &plan.execution.spec.teardown,
        &plan.execution.spec.env,
        run_dir,
    )
}

fn validate_trace_experiment_plan_phase(
    plan: &TraceExperimentRunPlan,
    phase: &str,
    command_count: usize,
) -> homeboy::core::Result<()> {
    let planned_count = plan.phase_steps(phase).len();
    if planned_count == command_count {
        return Ok(());
    }

    Err(homeboy::core::Error::internal_unexpected(format!(
        "trace experiment '{}' {} plan has {} steps for {} commands",
        plan.experiment_name(),
        phase,
        planned_count,
        command_count
    )))
}

pub(super) fn collect_trace_experiment_artifacts_for_plan(
    plan: Option<&TraceExperimentRunPlan>,
    run_dir: &RunDir,
    workflow: &mut extension_trace::TraceRunWorkflowResult,
) -> homeboy::core::Result<()> {
    let Some(plan) = plan else {
        return Ok(());
    };
    trace_experiment::collect_artifacts(
        &trace_experiment_context(plan.execution.context),
        plan.experiment_name(),
        plan.execution.spec,
        run_dir,
        workflow,
    )
}

fn trace_experiment_step_phase(step: &PlanStep) -> Option<&str> {
    step.inputs.get("phase").and_then(|value| value.as_str())
}
