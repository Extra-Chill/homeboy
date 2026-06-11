use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::trace as extension_trace;
use homeboy::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanValues};
use homeboy::core::rig;

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

pub(super) fn trace_experiment_settings(
    plan: Option<&TraceExperimentRunPlan>,
) -> homeboy::core::Result<Vec<(String, serde_json::Value)>> {
    let Some(plan) = plan else {
        return Ok(Vec::new());
    };
    plan.execution
        .spec
        .settings
        .iter()
        .map(|(key, value)| {
            Ok((
                key.clone(),
                match value {
                    serde_json::Value::String(value) => serde_json::Value::String(
                        resolve_trace_experiment_string(plan.execution.context, value),
                    ),
                    other => other.clone(),
                },
            ))
        })
        .collect()
}

pub(super) fn trace_experiment_env(
    plan: Option<&TraceExperimentRunPlan>,
) -> homeboy::core::Result<Vec<(String, String)>> {
    let Some(plan) = plan else {
        return Ok(Vec::new());
    };
    plan.execution
        .spec
        .env
        .iter()
        .map(|(key, value)| {
            Ok((
                key.clone(),
                resolve_trace_experiment_string(plan.execution.context, value),
            ))
        })
        .collect()
}

pub(super) fn run_trace_experiment_setup_for_plan(
    plan: Option<&TraceExperimentRunPlan>,
    run_dir: &RunDir,
) -> homeboy::core::Result<()> {
    let Some(plan) = plan else {
        return Ok(());
    };
    validate_trace_experiment_plan_phase(plan, "setup", plan.execution.spec.setup.len())?;
    run_trace_experiment_commands(
        plan,
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
    run_trace_experiment_commands(
        plan,
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

fn run_trace_experiment_commands(
    plan: &TraceExperimentRunPlan,
    phase: &str,
    commands: &[rig::TraceExperimentCommandSpec],
    experiment_env: &BTreeMap<String, String>,
    run_dir: &RunDir,
) -> homeboy::core::Result<()> {
    let context = plan.execution.context;
    let experiment_name = plan.experiment_name();
    for (step, command_spec) in plan.phase_steps(phase).into_iter().zip(commands) {
        let command_text =
            resolve_trace_experiment_string(context, trace_experiment_step_command(step));
        let mut command = Command::new(trace_experiment_shell());
        command.arg("-c").arg(&command_text);
        command.env("HOMEBOY_TRACE_EXPERIMENT", experiment_name);
        command.env("HOMEBOY_TRACE_EXPERIMENT_PHASE", phase);
        command.env("HOMEBOY_RUN_DIR", run_dir.path());
        command.env(
            "HOMEBOY_TRACE_ARTIFACT_DIR",
            run_dir.path().join("artifacts"),
        );
        for (key, value) in experiment_env {
            command.env(key, resolve_trace_experiment_string(context, value));
        }
        for (key, value) in &command_spec.env {
            command.env(key, resolve_trace_experiment_string(context, value));
        }
        if let Some(cwd) = &command_spec.cwd {
            command.current_dir(PathBuf::from(resolve_trace_experiment_string(context, cwd)));
        }
        let status = command.status().map_err(|err| {
            homeboy::core::Error::validation_invalid_argument(
                "--experiment",
                format!(
                    "trace experiment '{}' {} command failed to spawn: {}",
                    experiment_name, phase, err
                ),
                Some(command_text.clone()),
                None,
            )
        })?;
        if !status.success() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "--experiment",
                format!(
                    "trace experiment '{}' {} command exited {}",
                    experiment_name,
                    phase,
                    status.code().unwrap_or(-1)
                ),
                Some(command_text),
                None,
            ));
        }
    }
    Ok(())
}

pub(super) fn collect_trace_experiment_artifacts_for_plan(
    plan: Option<&TraceExperimentRunPlan>,
    run_dir: &RunDir,
    workflow: &mut extension_trace::TraceRunWorkflowResult,
) -> homeboy::core::Result<()> {
    let Some(plan) = plan else {
        return Ok(());
    };
    collect_trace_experiment_artifacts(
        plan.execution.context,
        plan.experiment_name(),
        plan.execution.spec,
        run_dir,
        workflow,
    )
}

fn trace_experiment_step_phase(step: &PlanStep) -> Option<&str> {
    step.inputs.get("phase").and_then(|value| value.as_str())
}

fn trace_experiment_step_command(step: &PlanStep) -> &str {
    step.inputs
        .get("command")
        .and_then(|value| value.as_str())
        .expect("trace experiment plan step missing command input")
}

fn collect_trace_experiment_artifacts(
    context: &TraceRigContext,
    experiment_name: &str,
    experiment: &rig::TraceExperimentSpec,
    run_dir: &RunDir,
    workflow: &mut extension_trace::TraceRunWorkflowResult,
) -> homeboy::core::Result<()> {
    let Some(results) = workflow.results.as_mut() else {
        return Ok(());
    };
    for (index, artifact) in experiment.artifacts.iter().enumerate() {
        let (label, source) = match artifact {
            rig::TraceExperimentArtifactSpec::Path(path) => (
                Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("experiment artifact")
                    .to_string(),
                path.as_str(),
            ),
            rig::TraceExperimentArtifactSpec::Detailed { label, path } => {
                (label.clone(), path.as_str())
            }
        };
        let source_path = PathBuf::from(resolve_trace_experiment_string(context, source));
        if !source_path.is_file() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "--experiment",
                format!(
                    "trace experiment '{}' artifact '{}' does not exist or is not a file",
                    experiment_name,
                    source_path.display()
                ),
                None,
                None,
            ));
        }
        let file_name = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact");
        let relative = PathBuf::from("artifacts")
            .join("experiments")
            .join(experiment_name)
            .join(format!("{:02}-{}", index + 1, file_name));
        let destination = run_dir.path().join(&relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                homeboy::core::Error::internal_io(
                    format!(
                        "Failed to create trace experiment artifact dir {}: {}",
                        parent.display(),
                        err
                    ),
                    Some("trace.experiment.artifact.mkdir".to_string()),
                )
            })?;
        }
        fs::copy(&source_path, &destination).map_err(|err| {
            homeboy::core::Error::internal_io(
                format!(
                    "Failed to collect trace experiment artifact {} to {}: {}",
                    source_path.display(),
                    destination.display(),
                    err
                ),
                Some("trace.experiment.artifact.copy".to_string()),
            )
        })?;
        results.artifacts.push(extension_trace::TraceArtifact {
            label,
            path: relative.to_string_lossy().to_string(),
            kind: None,
        });
    }
    Ok(())
}

#[cfg(unix)]
fn trace_experiment_shell() -> &'static str {
    "/bin/sh"
}

#[cfg(not(unix))]
fn trace_experiment_shell() -> &'static str {
    "sh"
}

fn resolve_trace_experiment_string(context: &TraceRigContext, value: &str) -> String {
    let expanded = rig::expand::expand_vars(&context.rig_spec, value);
    if let Some(root) = context.rig_package_root.as_ref() {
        expanded.replace("${package.root}", &root.to_string_lossy())
    } else {
        expanded
    }
}
