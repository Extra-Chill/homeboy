use serde::Serialize;

use homeboy::core::agent_task::{
    expand_agent_task_matrix, AgentTaskExecutor, AgentTaskMatrixAggregate, AgentTaskMatrixAxis,
    AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
};
use homeboy::core::agent_task_scheduler::{
    AgentTaskExecutionContext, AgentTaskPlan, AgentTaskScheduleOptions, AgentTaskScheduler,
};

use super::{matrix, BenchReportFormat, BenchRunArgs};

#[derive(Serialize)]
pub struct BenchMatrixFanoutOutput {
    pub command: &'static str,
    pub component: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rigs: Vec<String>,
    pub executor_backend: String,
    pub scheduler: homeboy::core::agent_task_scheduler::AgentTaskAggregate,
    pub matrix: AgentTaskMatrixAggregate,
    pub report: BenchMatrixFanoutReport,
}

#[derive(Serialize)]
pub struct BenchMatrixFanoutReport {
    pub format: Option<BenchReportFormat>,
    pub passed: bool,
    pub cells: usize,
    pub succeeded: usize,
    pub blocked: usize,
    pub failed: usize,
    pub cancelled_cells: usize,
    pub timed_out_cells: usize,
}

pub(super) fn run_matrix_fanout(
    run_args: &BenchRunArgs,
) -> homeboy::core::Result<BenchMatrixFanoutOutput> {
    if run_args.baseline_args.baseline || run_args.baseline_args.ratchet {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "matrix",
            "matrix fan-out does not write bench baselines; run single-rig bench baseline commands separately",
            None,
            None,
        ));
    }

    let component = effective_matrix_component(run_args)?;
    let axes = parse_matrix_axes(&run_args.matrix)?;
    let executor_backend = run_args
        .runner_pool
        .clone()
        .unwrap_or_else(|| "local".to_string());
    let plan_id = format!("bench/{component}");
    let template = matrix_template_request(&plan_id, &component, &executor_backend, run_args);
    let matrix_plan =
        expand_agent_task_matrix(plan_id.clone(), axes, template).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument("matrix", error.message, None, None)
        })?;
    let mut schedule = AgentTaskPlan::new(
        plan_id,
        matrix_plan
            .cells
            .iter()
            .map(|cell| cell.task.clone())
            .collect(),
    );
    schedule.group_key = Some("bench.matrix".to_string());
    schedule.options = AgentTaskScheduleOptions {
        max_concurrency: usize::try_from(run_args.concurrency)
            .unwrap_or(usize::MAX)
            .max(1),
        max_tasks: run_args.matrix_max_tasks,
        max_queue_depth: run_args.matrix_max_queue_depth,
        ..AgentTaskScheduleOptions::default()
    };
    schedule.metadata = serde_json::json!({
        "command": "bench.matrix",
        "component": component,
        "rigs": run_args.rig,
        "report": run_args.report,
    });

    let scheduler = AgentTaskScheduler::new(LocalBenchMatrixExecutor);
    let scheduler_aggregate = scheduler.run(schedule);
    let matrix_aggregate =
        AgentTaskMatrixAggregate::from_outcomes(&matrix_plan, &scheduler_aggregate.outcomes);
    let report = BenchMatrixFanoutReport {
        format: run_args.report.first().copied(),
        passed: matrix_aggregate.passed,
        cells: matrix_aggregate.cells.len(),
        succeeded: scheduler_aggregate.totals.succeeded,
        blocked: scheduler_aggregate.totals.blocked,
        failed: scheduler_aggregate.totals.failed,
        cancelled_cells: scheduler_aggregate.totals.cancelled,
        timed_out_cells: scheduler_aggregate.totals.timed_out,
    };

    Ok(BenchMatrixFanoutOutput {
        command: "bench.matrix",
        component,
        rigs: run_args.rig.clone(),
        executor_backend,
        scheduler: scheduler_aggregate,
        matrix: matrix_aggregate,
        report,
    })
}

fn effective_matrix_component(run_args: &BenchRunArgs) -> homeboy::core::Result<String> {
    if let Some(component) = run_args.comp.id() {
        return Ok(component.to_string());
    }

    if run_args.rig.len() == 1 {
        let spec = homeboy::core::rig::load(&run_args.rig[0])?;
        if let Some(component) = spec
            .bench
            .as_ref()
            .and_then(|bench| matrix::bench_component_ids(bench).into_iter().next())
        {
            return Ok(component);
        }
    }

    Err(homeboy::core::Error::validation_invalid_argument(
        "component",
        "matrix fan-out requires a component argument or a single rig with bench.default_component",
        None,
        None,
    ))
}

fn parse_matrix_axes(raw_axes: &[String]) -> homeboy::core::Result<Vec<AgentTaskMatrixAxis>> {
    raw_axes
        .iter()
        .map(|raw| {
            let (name, raw_values) = raw.split_once('=').ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "matrix",
                    format!("matrix axis must be NAME=value,value; got '{raw}'"),
                    Some(raw.clone()),
                    None,
                )
            })?;
            let values = raw_values
                .split(',')
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            Ok(AgentTaskMatrixAxis {
                name: name.to_string(),
                values,
            })
        })
        .collect()
}

fn matrix_template_request(
    plan_id: &str,
    component: &str,
    executor_backend: &str,
    run_args: &BenchRunArgs,
) -> AgentTaskRequest {
    AgentTaskRequest {
        schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: format!("{plan_id}/template"),
        group_key: Some(plan_id.to_string()),
        parent_plan_id: None,
        executor: AgentTaskExecutor {
            backend: executor_backend.to_string(),
            selector: None,
            required_capabilities: vec!["bench".to_string(), "matrix".to_string()],
            secret_env: Vec::new(),
            model: None,
            config: serde_json::json!({}),
        },
        instructions:
            "Run the selected benchmark matrix cell and return normalized artifacts/evidence."
                .to_string(),
        inputs: serde_json::json!({
            "command": "bench",
            "component": component,
            "rigs": run_args.rig,
            "scenarios": run_args.scenario_ids,
            "profile": run_args.profile,
            "ci_profile": run_args.ci_profile,
            "iterations": run_args.iterations,
            "report": run_args.report,
        }),
        source_refs: Vec::new(),
        workspace: AgentTaskWorkspace::default(),
        policy: Default::default(),
        limits: Default::default(),
        expected_artifacts: run_args.expected_artifact.clone(),
        metadata: serde_json::json!({ "product_command": "bench.matrix" }),
    }
}

#[derive(Clone)]
struct LocalBenchMatrixExecutor;

impl homeboy::core::agent_task_scheduler::AgentTaskExecutorAdapter for LocalBenchMatrixExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        context: AgentTaskExecutionContext,
    ) -> homeboy::core::agent_task::AgentTaskOutcome {
        homeboy::core::agent_task::AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id.clone(),
            status: homeboy::core::agent_task::AgentTaskOutcomeStatus::NoOp,
            summary: Some(format!(
                "matrix cell scheduled for executor '{}'",
                request.executor.backend
            )),
            failure_classification: None,
            artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: serde_json::Value::Null,
            workflow: None,
            follow_up: None,
            metadata: serde_json::json!({
                "executor_backend": request.executor.backend,
                "matrix": request.metadata.get("matrix").cloned().unwrap_or(serde_json::Value::Null),
                "scheduler_plan_id": context.plan_id,
                "attempt": context.attempt,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::bench::BenchArgs;
    use crate::commands::utils::args::{
        BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs,
    };
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        bench: BenchArgs,
    }

    fn run_args(component: Option<&str>) -> BenchRunArgs {
        BenchRunArgs {
            comp: PositionalComponentArgs {
                component: component.map(str::to_string),
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            iterations: 1,
            warmup: None,
            runs: 1,
            shared_state: None,
            concurrency: 1,
            matrix: Vec::new(),
            runner_pool: None,
            matrix_max_tasks: None,
            matrix_max_queue_depth: None,
            expected_artifact: Vec::new(),
            baseline_args: BaselineArgs {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            regression_threshold: 5.0,
            setting_args: SettingArgs::default(),
            args: Vec::new(),
            json_summary: false,
            status_file: None,
            report: Vec::new(),
            rig: Vec::new(),
            rig_order: crate::commands::bench::BenchRigOrder::Input,
            rig_concurrency: 1,
            scenario_ids: Vec::new(),
            profile: None,
            ci_profile: None,
            ignore_default_baseline: false,
            allow_stale_dependencies: false,
        }
    }

    #[test]
    fn parses_matrix_fanout_flags() {
        let cli = TestCli::try_parse_from([
            "bench",
            "studio-web",
            "--matrix",
            "model=gpt-5.5,kimi",
            "--matrix",
            "prompt=site-a,site-b",
            "--runner-pool",
            "codebox",
            "--concurrency",
            "8",
            "--max-queue-depth",
            "3",
            "--expect-artifact",
            "bench-results",
            "--report",
            "side-by-side",
        ])
        .expect("bench matrix fan-out should parse");

        assert_eq!(cli.bench.run.matrix.len(), 2);
        assert_eq!(cli.bench.run.runner_pool.as_deref(), Some("codebox"));
        assert_eq!(cli.bench.run.concurrency, 8);
        assert_eq!(cli.bench.run.matrix_max_queue_depth, Some(3));
        assert_eq!(cli.bench.run.expected_artifact, vec!["bench-results"]);
        assert_eq!(cli.bench.run.report, vec![BenchReportFormat::SideBySide]);
    }

    #[test]
    fn matrix_fanout_dispatches_through_scheduler_and_aggregates_cells() {
        let mut args = run_args(Some("studio-web"));
        args.matrix = vec![
            "model=gpt-5.5,kimi".to_string(),
            "prompt=site-a,site-b".to_string(),
        ];
        args.runner_pool = Some("local".to_string());
        args.concurrency = 2;
        args.matrix_max_queue_depth = Some(3);
        args.expected_artifact = vec!["bench-results".to_string()];

        let output = run_matrix_fanout(&args).expect("matrix fan-out runs");

        assert_eq!(output.command, "bench.matrix");
        assert_eq!(output.executor_backend, "local");
        assert_eq!(output.scheduler.queue.max_concurrency, 2);
        assert_eq!(output.scheduler.queue.max_queue_depth, Some(3));
        assert_eq!(output.scheduler.totals.succeeded, 3);
        assert_eq!(output.scheduler.totals.blocked, 1);
        assert_eq!(output.scheduler.totals.failed, 0);
        assert_eq!(output.matrix.cells.len(), 4);
        assert!(!output.matrix.passed);
        assert_eq!(output.report.cells, 4);
        assert_eq!(output.report.succeeded, 3);
        assert_eq!(output.report.blocked, 1);
        assert_eq!(output.report.failed, 0);
        assert!(output
            .matrix
            .cells
            .iter()
            .any(|cell| cell.axes["model"] == "kimi" && cell.axes["prompt"] == "site-b"));
    }
}
