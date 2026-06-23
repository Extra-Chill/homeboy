use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::bench::{BenchResults, BenchRunWorkflowResult};
use homeboy::core::observation::merge_metadata;
use homeboy::core::rig::RigStateSnapshot;

use crate::commands::bench::BenchRunArgs;
use crate::commands::utils::resource_policy;

pub(super) fn bench_observation_command(
    component_id: &str,
    args: &BenchRunArgs,
    rig_id: Option<&str>,
) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "bench".to_string(),
        component_id.to_string(),
    ];
    if let Some(rig_id) = rig_id {
        parts.push(format!("--rig={rig_id}"));
    }
    if args.iterations != 10 {
        parts.push(format!("--iterations={}", args.iterations));
    }
    if args.runs != 1 {
        parts.push(format!("--runs={}", args.runs));
    }
    if args.concurrency != 1 {
        parts.push(format!("--concurrency={}", args.concurrency));
    }
    parts.join(" ")
}

pub(super) fn bench_observation_initial_metadata(
    component_label: &str,
    args: &BenchRunArgs,
    selected_scenarios: &[String],
    rig_snapshot: Option<&RigStateSnapshot>,
    run_dir: &RunDir,
) -> serde_json::Value {
    let resource_policy = resource_policy::captured_context()
        .as_ref()
        .map(resource_policy::ResourcePolicyContext::to_json)
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "component_label": component_label,
        "iterations": args.iterations,
        "warmup_iterations": args.warmup,
        "runs": args.runs,
        "concurrency": args.concurrency,
        "regression_threshold_percent": args.regression_threshold,
        "baseline": {
            "baseline": args.baseline_args.baseline,
            "ignore_baseline": args.baseline_args.ignore_baseline,
            "ratchet": args.baseline_args.ratchet,
        },
        "profile": args.profile,
        "selected_scenarios": selected_scenarios,
        "shared_state": args.shared_state.as_ref().map(|path| path.to_string_lossy().to_string()),
        "status_file": args.status_file.as_ref().map(|path| path.to_string_lossy().to_string()),
        "run_dir": run_dir.path().to_string_lossy().to_string(),
        "rig_state": rig_snapshot,
        "resource_policy": resource_policy,
    })
}

pub(super) fn bench_observation_finish_metadata(
    initial_metadata: serde_json::Value,
    workflow: &BenchRunWorkflowResult,
) -> serde_json::Value {
    merge_metadata(
        initial_metadata,
        serde_json::json!({
            "observation_status": workflow.status,
            "exit_code": workflow.exit_code,
            "gate_failures": workflow.gate_failures,
            "baseline_status": baseline_status(workflow),
            "failure": workflow.failure,
            "failure_classification": workflow.results.as_ref().and_then(|results| results.failure_classification.clone()),
            "phase_events": workflow.results.as_ref().map(|results| results.phase_events.clone()).unwrap_or_default(),
            "phase_summaries": workflow.results.as_ref().map(|results| results.phase_summaries.clone()).unwrap_or_default(),
            "results": workflow.results,
            "scenario_metrics": workflow.results.as_ref().map(scenario_metric_summaries).unwrap_or_default(),
        }),
    )
}

fn baseline_status(workflow: &BenchRunWorkflowResult) -> Option<&'static str> {
    workflow.baseline_comparison.as_ref().map(|comparison| {
        if comparison.regression {
            "regression"
        } else if comparison.has_improvements {
            "improved"
        } else {
            "unchanged"
        }
    })
}

fn scenario_metric_summaries(results: &BenchResults) -> Vec<serde_json::Value> {
    results
        .scenarios
        .iter()
        .map(|scenario| {
            serde_json::json!({
                "scenario_id": scenario.id,
                "iterations": scenario.iterations,
                "passed": scenario.passed,
                "metrics": scenario.metrics,
                "metric_groups": scenario.metric_groups,
                "timeline_event_count": scenario.timeline.len(),
                "span_definition_count": scenario.span_definitions.len(),
                "span_result_count": scenario.span_results.len(),
                "metadata": scenario.metadata,
                "memory": scenario.memory,
                "artifact_count": scenario.artifacts.len(),
                "run_count": scenario.runs.as_ref().map(Vec::len),
                "runs_summary": scenario.runs_summary,
            })
        })
        .collect()
}
