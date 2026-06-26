//! Bench runner failure diagnostic enrichment.

use crate::core::extension::stderr_tail;

use super::run::BenchRunWorkflowArgs;

pub(crate) fn bench_failure_stderr_tail(stderr: &str, args: &BenchRunWorkflowArgs) -> String {
    enrich_empty_artifact_path_error(stderr_tail(stderr), args)
}

fn enrich_empty_artifact_path_error(tail: String, args: &BenchRunWorkflowArgs) -> String {
    if !tail.contains("returned artifact") || !tail.contains("with empty path") {
        return tail;
    }

    let artifact_key =
        text_between(&tail, "returned artifact \"", "\" with empty path").unwrap_or("<unknown>");
    let workload_id = text_between(&tail, "WORKLOAD_ERROR: ", " - ")
        .or_else(|| text_between(&tail, "WORKLOAD_ERROR: ", " — "));
    let scenario_id = args
        .scenario_ids
        .first()
        .filter(|_| args.scenario_ids.len() == 1)
        .cloned()
        .or_else(|| workload_id.map(str::to_string))
        .unwrap_or_else(|| "<unknown>".to_string());
    let phase = if tail.contains("warmup iteration") {
        "warmup"
    } else {
        "iteration"
    };
    let iteration = text_after(&tail, &format!("{} iteration ", phase))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("<unknown>");
    let context = failure_context(
        args,
        workload_id,
        &scenario_id,
        phase,
        iteration,
        artifact_key,
    );

    format!(
        "{}\n\nBench artifact path validation context: {}.\nBench artifact contract: artifact paths must be non-empty. Omit optional artifacts, or provide a real diagnostics file/directory when evidence is available.",
        tail,
        context.join(", ")
    )
}

fn failure_context(
    args: &BenchRunWorkflowArgs,
    workload_id: Option<&str>,
    scenario_id: &str,
    phase: &str,
    iteration: &str,
    artifact_key: &str,
) -> Vec<String> {
    let mut context = Vec::new();
    if let Some(rig_id) = args.rig_id.as_deref() {
        context.push(format!("rig id `{}`", rig_id));
    }
    context.push(format!("component id `{}`", args.component_id));
    if let Some(workload_id) = workload_id {
        context.push(format!("workload id `{}`", workload_id));
    }
    context.push(format!("scenario id `{}`", scenario_id));
    context.push(format!("phase `{}`", phase));
    context.push(format!("iteration {}", iteration));
    context.push(format!("artifact key `{}`", artifact_key));
    context
}

fn text_between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let after_start = text_after(text, start)?;
    let end_index = after_start.find(end)?;
    Some(&after_start[..end_index])
}

fn text_after<'a>(text: &'a str, start: &str) -> Option<&'a str> {
    let start_index = text.find(start)?;
    Some(&text[start_index + start.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::baseline::BaselineFlags;
    use crate::core::extension::bench::parsing::BenchRunExecution;

    #[test]
    fn test_bench_failure_stderr_tail() {
        let args = bench_args();
        let tail = bench_failure_stderr_tail(
            "WORKLOAD_ERROR: studio-agent-site-build - warmup iteration 1/1 returned artifact \"visual_comparison_dir\" with empty path\n",
            &args,
        );

        assert!(tail.contains("Bench artifact path validation context"));
        assert!(tail.contains("component id `studio`"));
    }

    #[test]
    fn enriches_empty_artifact_path_failure_with_bench_context() {
        let args = bench_args();

        let enriched = enrich_empty_artifact_path_error(
            "WORKLOAD_ERROR: studio-agent-site-build - warmup iteration 1/1 returned artifact \"visual_comparison_dir\" with empty path".to_string(),
            &args,
        );

        assert!(enriched.contains("rig id `studio-bfb`"));
        assert!(enriched.contains("component id `studio`"));
        assert!(enriched.contains("workload id `studio-agent-site-build`"));
        assert!(enriched.contains("scenario id `studio-agent-site-build`"));
        assert!(enriched.contains("phase `warmup`"));
        assert!(enriched.contains("iteration 1/1"));
        assert!(enriched.contains("artifact key `visual_comparison_dir`"));
        assert!(enriched.contains("Omit optional artifacts"));
        assert!(enriched.contains("real diagnostics file/directory"));
    }

    fn bench_args() -> BenchRunWorkflowArgs {
        BenchRunWorkflowArgs {
            component_label: "Studio".to_string(),
            component_id: "studio".to_string(),
            path_override: None,
            settings: Vec::new(),
            settings_json: Vec::new(),
            iterations: 1,
            warmup_iterations: Some(1),
            run_id: None,
            execution: BenchRunExecution {
                runs: 1,
                concurrency: 1,
            },
            baseline_flags: BaselineFlags {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            regression_threshold_percent: 5.0,
            json_summary: false,
            ci_env: Vec::new(),
            passthrough_args: Vec::new(),
            scenario_ids: vec!["studio-agent-site-build".to_string()],
            rig_id: Some("studio-bfb".to_string()),
            shared_state: None,
            extra_workloads: Vec::new(),
            rig_package: None,
            invocation_requirements:
                crate::core::engine::invocation::InvocationRequirements::default(),
        }
    }
}
