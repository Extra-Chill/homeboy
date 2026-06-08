use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use homeboy::core::engine::run_dir::{self, RunDir};
use homeboy::core::extension::bench::{
    BenchDiagnostic, BenchDiagnosticSource, BenchResults, BenchRunWorkflowResult,
};
use homeboy::core::git::short_head_revision_at;
use homeboy::core::observation::{
    finding_records_from_budget, merge_metadata, ActiveObservation, NewRunRecord, RunStatus,
};
use homeboy::core::rig::RigStateSnapshot;

use crate::commands::utils::resource_policy;

use super::BenchRunArgs;

pub(super) struct BenchObservation(ActiveObservation);
impl BenchObservation {
    fn run_id(&self) -> &str {
        self.0.run_id()
    }
}

pub(super) struct BenchObservationSummary {
    pub run_id: String,
    pub component_id: String,
    pub rig_id: Option<String>,
    pub store_path: String,
}
pub(super) struct BenchObservationStart<'a> {
    pub component_id: &'a str,
    pub component_label: &'a str,
    pub source_path: &'a Path,
    pub args: &'a BenchRunArgs,
    pub selected_scenarios: &'a [String],
    pub rig_id: Option<&'a str>,
    pub rig_snapshot: Option<&'a RigStateSnapshot>,
    pub run_dir: &'a RunDir,
}
pub(super) fn start(start: BenchObservationStart<'_>) -> Option<BenchObservation> {
    let metadata = bench_observation_initial_metadata(
        start.component_label,
        start.args,
        start.selected_scenarios,
        start.rig_snapshot,
        start.run_dir,
    );
    let observation = ActiveObservation::start_best_effort(
        NewRunRecord::builder("bench")
            .component_id(start.component_id)
            .command(bench_observation_command(
                start.component_id,
                start.args,
                start.rig_id,
            ))
            .cwd_path(start.source_path)
            .current_homeboy_version()
            .git_sha(short_head_revision_at(start.source_path))
            .optional_rig_id(start.rig_id)
            .metadata(metadata.clone())
            .build(),
    )
    .map(BenchObservation);
    if let Some(observation) = &observation {
        write_status_file(
            start.args,
            start.run_dir,
            observation,
            "running",
            None,
            None,
        );
    }
    observation
}

pub(super) fn finish_success(
    observation: Option<BenchObservation>,
    workflow: &mut BenchRunWorkflowResult,
    run_dir: &RunDir,
) -> Option<BenchObservationSummary> {
    let observation = observation?;

    record_bench_observation_artifacts(&observation, workflow, run_dir);
    let metadata =
        bench_observation_finish_metadata(observation.0.initial_metadata().clone(), workflow);
    let status = if workflow.exit_code == 0 {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    };
    let summary = BenchObservationSummary {
        run_id: observation.run_id().to_string(),
        component_id: observation.0.component_id().unwrap_or_default().to_string(),
        rig_id: observation.0.rig_id().map(str::to_string),
        store_path: observation.0.store_path(),
    };
    write_status_file(
        &observation.0.initial_metadata().clone(),
        run_dir,
        &observation,
        workflow.status.as_str(),
        Some(workflow),
        None,
    );
    observation.0.finish(status, Some(metadata));
    Some(summary)
}

pub(super) fn history_hints(summary: &BenchObservationSummary) -> Vec<String> {
    let mut list_command = format!(
        "homeboy runs list --kind bench --component {}",
        summary.component_id
    );
    if let Some(rig_id) = &summary.rig_id {
        list_command.push_str(&format!(" --rig {rig_id}"));
    }

    vec![
        format!("Persisted benchmark run ID: {}", summary.run_id),
        format!("View this run: homeboy runs show {}", summary.run_id),
        format!("List related bench runs: {list_command}"),
        format!("Observation store: {}", summary.store_path),
    ]
}

pub(super) fn finish_error(
    observation: Option<BenchObservation>,
    error: &homeboy::core::Error,
    run_dir: &RunDir,
) {
    let Some(observation) = observation else {
        return;
    };

    record_if_exists(
        &observation,
        "bench_results",
        run_dir.step_file(run_dir::files::BENCH_RESULTS),
    );
    record_if_exists(
        &observation,
        "resource_summary",
        run_dir.step_file(run_dir::files::RESOURCE_SUMMARY),
    );
    record_memory_timeline_artifacts(&observation, run_dir);
    let metadata = merge_metadata(
        observation.0.initial_metadata().clone(),
        serde_json::json!({
            "observation_status": "error",
            "error": error.to_string(),
        }),
    );
    write_status_file(
        &observation.0.initial_metadata().clone(),
        run_dir,
        &observation,
        "error",
        None,
        Some(error),
    );
    observation.0.finish_error(Some(metadata));
}

trait BenchStatusTarget {
    fn status_file(&self) -> Option<PathBuf>;
}

impl BenchStatusTarget for BenchRunArgs {
    fn status_file(&self) -> Option<PathBuf> {
        self.status_file.clone()
    }
}

impl BenchStatusTarget for serde_json::Value {
    fn status_file(&self) -> Option<PathBuf> {
        self.get("status_file")
            .and_then(|value| value.as_str())
            .map(PathBuf::from)
    }
}

fn write_status_file<T: BenchStatusTarget>(
    target: &T,
    run_dir: &RunDir,
    observation: &BenchObservation,
    status: &str,
    workflow: Option<&BenchRunWorkflowResult>,
    error: Option<&homeboy::core::Error>,
) {
    let Some(path) = target.status_file() else {
        return;
    };
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let _ = fs::create_dir_all(parent);
    }
    let artifacts = observation
        .0
        .store()
        .list_artifacts(observation.run_id())
        .unwrap_or_default();
    let payload = serde_json::json!({
        "schema": "homeboy/bench-status/v1",
        "command": "bench.status",
        "run_id": observation.run_id(),
        "kind": observation.0.run().kind,
        "status": status,
        "updated_at": chrono::Utc::now().to_rfc3339(),
        "started_at": observation.0.run().started_at,
        "finished": workflow.is_some() || error.is_some(),
        "component_id": observation.0.component_id(),
        "rig_id": observation.0.rig_id(),
        "homeboy_version": observation.0.run().homeboy_version,
        "git_sha": observation.0.run().git_sha,
        "cwd": observation.0.run().cwd,
        "run_dir": run_dir.path().to_string_lossy().to_string(),
        "observation_store": observation.0.store_path(),
        "artifact_count": artifacts.len(),
        "artifacts": artifacts,
        "failure": workflow.and_then(|workflow| workflow.failure.as_ref()),
        "failure_classification": workflow.and_then(|workflow| workflow.results.as_ref()?.failure_classification.as_ref()),
        "phase_summaries": workflow.and_then(|workflow| workflow.results.as_ref().map(|results| results.phase_summaries.clone())).unwrap_or_default(),
        "error": error.map(|error| error.to_string()),
        "exit_code": workflow.map(|workflow| workflow.exit_code),
        "gate_failures": workflow.map(|workflow| workflow.gate_failures.clone()).unwrap_or_default(),
        "hints": workflow.and_then(|workflow| workflow.hints.clone()).unwrap_or_default(),
    });
    if let Ok(json) = serde_json::to_vec_pretty(&payload) {
        let _ = fs::write(path, json);
    }
}

fn bench_observation_command(
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

fn bench_observation_initial_metadata(
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

fn bench_observation_finish_metadata(
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

fn record_bench_observation_artifacts(
    observation: &BenchObservation,
    workflow: &mut BenchRunWorkflowResult,
    run_dir: &RunDir,
) {
    if let Some(results) = workflow.results.as_mut() {
        workflow
            .diagnostics
            .extend(persist_bench_result_artifact_paths(
                observation,
                results,
                run_dir,
            ));
        rewrite_bench_results_file(results, run_dir);
    }

    record_if_exists(
        observation,
        "bench_results",
        run_dir.step_file(run_dir::files::BENCH_RESULTS),
    );
    record_if_exists(
        observation,
        "resource_summary",
        run_dir.step_file(run_dir::files::RESOURCE_SUMMARY),
    );
    record_memory_timeline_artifacts(observation, run_dir);

    let Some(results) = workflow.results.as_ref() else {
        return;
    };
    observation.0.record_findings(&finding_records_from_budget(
        observation.run_id(),
        &results.budget_findings,
    ));
}

fn record_if_exists(observation: &BenchObservation, kind: &str, path: PathBuf) {
    observation.0.record_artifact_if_file(kind, &path);
}

fn record_memory_timeline_artifacts(observation: &BenchObservation, run_dir: &RunDir) {
    let Ok(entries) = fs::read_dir(run_dir.path()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("bench-memory-timeline")
            && (name.ends_with(".json") || name.ends_with(".csv"))
        {
            observation
                .0
                .record_artifact_if_file("bench_memory_timeline", &path);
        }
    }
}

fn persist_bench_result_artifact_paths(
    observation: &BenchObservation,
    results: &mut BenchResults,
    run_dir: &RunDir,
) -> Vec<BenchDiagnostic> {
    let mut diagnostics = Vec::new();
    for scenario in &mut results.scenarios {
        for (name, artifact) in &mut scenario.artifacts {
            if let Some(diagnostic) =
                persist_bench_artifact(observation, &scenario.id, None, name, artifact, run_dir)
            {
                diagnostics.push(diagnostic);
            }
        }
        if let Some(runs) = &mut scenario.runs {
            for (run_index, run) in runs.iter_mut().enumerate() {
                for (name, artifact) in &mut run.artifacts {
                    if let Some(diagnostic) = persist_bench_artifact(
                        observation,
                        &scenario.id,
                        Some(run_index),
                        name,
                        artifact,
                        run_dir,
                    ) {
                        diagnostics.push(diagnostic);
                    }
                }
            }
        }
    }
    diagnostics
}

fn persist_bench_artifact(
    observation: &BenchObservation,
    scenario_id: &str,
    run_index: Option<usize>,
    name: &str,
    artifact: &mut homeboy::core::extension::bench::BenchArtifact,
    run_dir: &RunDir,
) -> Option<BenchDiagnostic> {
    let kind = artifact.kind.clone().unwrap_or_else(|| name.to_string());
    let metadata = bench_artifact_metadata(scenario_id, run_index, name, artifact);

    if let Some(url) = artifact.url.clone() {
        return match observation.0.store().record_url_artifact_with_metadata(
            observation.run_id(),
            &kind,
            &url,
            metadata,
        ) {
            Ok(record) => {
                artifact.observation_artifact_id = Some(record.id);
                None
            }
            Err(error) => Some(bench_artifact_diagnostic(
                scenario_id,
                run_index,
                name,
                "bench_artifact_url_record_failed",
                format!("failed to record bench URL artifact `{name}`: {error}"),
                serde_json::json!({ "url": url }),
            )),
        };
    }

    let Some(original_path) = artifact.path.clone() else {
        return None;
    };
    if bench_artifact_path_is_blocked(&original_path) {
        return Some(bench_artifact_diagnostic(
            scenario_id,
            run_index,
            name,
            "bench_artifact_path_blocked",
            format!("blocked bench artifact path `{original_path}` for `{name}`"),
            serde_json::json!({ "path": original_path }),
        ));
    }

    let path = resolve_bench_artifact_path(&original_path, run_dir);
    let record = if path.is_file() {
        observation.0.store().record_artifact_with_metadata(
            observation.run_id(),
            "bench_artifact",
            &path,
            metadata,
        )
    } else if path.is_dir() {
        observation
            .0
            .store()
            .record_directory_artifact_with_metadata(
                observation.run_id(),
                "bench_artifact",
                &path,
                metadata,
            )
    } else {
        return Some(bench_artifact_diagnostic(
            scenario_id,
            run_index,
            name,
            "bench_artifact_path_missing",
            format!("bench artifact path for `{name}` was not found: {original_path}"),
            serde_json::json!({
                "path": original_path,
                "resolved_path": path.to_string_lossy().to_string(),
            }),
        ));
    };

    match record {
        Ok(record) => {
            artifact.path = Some(record.path);
            artifact.observation_artifact_id = Some(record.id);
            None
        }
        Err(error) => Some(bench_artifact_diagnostic(
            scenario_id,
            run_index,
            name,
            "bench_artifact_record_failed",
            format!("failed to persist bench artifact `{name}`: {error}"),
            serde_json::json!({
                "path": original_path,
                "resolved_path": path.to_string_lossy().to_string(),
            }),
        )),
    }
}

fn bench_artifact_metadata(
    scenario_id: &str,
    run_index: Option<usize>,
    name: &str,
    artifact: &homeboy::core::extension::bench::BenchArtifact,
) -> serde_json::Value {
    serde_json::json!({
        "source": "bench",
        "scenario_id": scenario_id,
        "run_index": run_index,
        "name": name,
        "kind": artifact.kind,
        "label": artifact.label,
        "original_path": artifact.path,
        "url": artifact.url,
    })
}

fn bench_artifact_path_is_blocked(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn bench_artifact_diagnostic(
    scenario_id: &str,
    run_index: Option<usize>,
    name: &str,
    class: &str,
    message: String,
    metadata: serde_json::Value,
) -> BenchDiagnostic {
    let mut diagnostic_metadata = BTreeMap::new();
    diagnostic_metadata.insert("artifact_name".to_string(), serde_json::json!(name));
    if let Some(object) = metadata.as_object() {
        for (key, value) in object {
            diagnostic_metadata.insert(key.clone(), value.clone());
        }
    }
    BenchDiagnostic {
        class: class.to_string(),
        message: Some(message),
        source: Some(match run_index {
            Some(run_index) => BenchDiagnosticSource::ScenarioRun {
                scenario_id: scenario_id.to_string(),
                run_index,
            },
            None => BenchDiagnosticSource::Scenario {
                scenario_id: scenario_id.to_string(),
            },
        }),
        metadata: diagnostic_metadata,
    }
}

fn rewrite_bench_results_file(results: &BenchResults, run_dir: &RunDir) {
    let Ok(json) = serde_json::to_vec_pretty(results) else {
        return;
    };
    let _ = fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), json);
}

fn resolve_bench_artifact_path(path: &str, run_dir: &RunDir) -> PathBuf {
    let artifact_path = PathBuf::from(path);
    if artifact_path.exists() {
        return artifact_path;
    }
    if artifact_path.is_absolute() {
        if let Some(preserved_path) = resolve_preserved_invocation_artifact(&artifact_path, run_dir)
        {
            return preserved_path;
        }
        return artifact_path;
    }
    let run_dir_path = run_dir.path().join(path);
    if run_dir_path.exists() {
        return run_dir_path;
    }
    artifact_path
}

fn resolve_preserved_invocation_artifact(path: &Path, run_dir: &RunDir) -> Option<PathBuf> {
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let name = component.as_os_str().to_string_lossy();
        let Some(short_id) = name.strip_suffix(".a") else {
            continue;
        };

        let mut preserved = run_dir
            .path()
            .join("invocations")
            .join(format!("inv-{short_id}"))
            .join("artifacts");
        for rest in components {
            preserved.push(rest.as_os_str());
        }
        if preserved.exists() {
            return Some(preserved);
        }
        return None;
    }
    None
}

#[cfg(test)]
#[path = "../../../tests/commands/bench/observation_artifact_test.rs"]
mod observation_artifact_test;

#[cfg(test)]
mod tests {
    use std::fs;

    use homeboy::core::engine::run_dir::{self, RunDir};
    use homeboy::core::extension::bench::artifact::BenchArtifact;
    use homeboy::core::extension::bench::{
        parse_bench_results_str, BenchResults, BenchRunWorkflowResult,
    };
    use homeboy::core::observation::ObservationStore;

    use super::*;
    use crate::commands::bench::BenchRigOrder;
    use crate::commands::doctor::resources::{
        DoctorOutput, LoadSummary, ProcessSummary, ResourceRecommendation, RigLeaseSummary,
    };
    use crate::commands::utils::args::{
        BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs,
    };
    use crate::commands::utils::resource_policy::{self, HotCommand, ResourcePolicyContext};
    use crate::test_support::with_isolated_home;

    pub(super) struct XdgGuard(Option<String>);

    impl XdgGuard {
        pub(super) fn unset() -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::remove_var("XDG_DATA_HOME");
            Self(prior)
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    pub(super) fn bench_results(component_id: &str, scenario_id: &str, p95: f64) -> BenchResults {
        serde_json::from_value(serde_json::json!({
            "component_id": component_id,
            "iterations": 10,
            "scenarios": [
                {
                    "id": scenario_id,
                    "iterations": 10,
                    "metrics": { "p95_ms": p95 }
                }
            ],
            "metric_policies": {
                "p95_ms": { "direction": "lower_is_better" }
            }
        }))
        .expect("bench results")
    }

    pub(super) fn bench_args() -> BenchRunArgs {
        BenchRunArgs {
            comp: PositionalComponentArgs {
                component: Some("homeboy".to_string()),
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            iterations: 10,
            warmup: None,
            runs: 1,
            shared_state: None,
            concurrency: 1,
            matrix: Vec::new(),
            runner_pool: None,
            matrix_max_tasks: None,
            matrix_max_queue_depth: None,
            expected_artifact: Vec::new(),
            baseline_args: BaselineArgs::default(),
            regression_threshold: 5.0,
            setting_args: SettingArgs::default(),
            args: Vec::new(),
            json_summary: false,
            status_file: None,
            report: Vec::new(),
            rig: Vec::new(),
            rig_order: BenchRigOrder::Input,
            rig_concurrency: 1,
            scenario_ids: Vec::new(),
            profile: None,
            ci_profile: None,
            ignore_default_baseline: false,
        }
    }

    #[test]
    fn bench_observation_persists_success_with_metrics_and_artifacts() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let run_dir = RunDir::create().expect("run dir");
            fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");
            fs::write(run_dir.step_file(run_dir::files::RESOURCE_SUMMARY), b"{}")
                .expect("resources");
            let artifact_path = run_dir.path().join("bench-artifacts/cold/transcript.json");
            fs::create_dir_all(artifact_path.parent().expect("artifact parent")).expect("mkdir");
            fs::write(&artifact_path, b"{\"ok\":true}").expect("artifact");

            let mut results = bench_results("homeboy", "cold", 42.0);
            results.scenarios[0].artifacts.insert(
                "transcript".to_string(),
                BenchArtifact {
                    path: Some("bench-artifacts/cold/transcript.json".to_string()),
                    url: None,
                    artifact_type: None,
                    kind: Some("json".to_string()),
                    label: Some("Transcript".to_string()),
                    observation_artifact_id: None,
                    ..BenchArtifact::default()
                },
            );
            results.scenarios[0].artifacts.insert(
                "admin".to_string(),
                BenchArtifact {
                    path: None,
                    url: Some("https://example.test/wp-admin/".to_string()),
                    artifact_type: Some("url".to_string()),
                    kind: Some("admin_url".to_string()),
                    label: Some("Admin".to_string()),
                    observation_artifact_id: None,
                    ..BenchArtifact::default()
                },
            );
            let mut workflow = BenchRunWorkflowResult {
                status: "passed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 0,
                iterations: 10,
                results: Some(results),
                gate_results: Vec::new(),
                gate_failures: Vec::new(),
                baseline_comparison: None,
                hints: None,
                failure: None,
                diagnostics: Vec::new(),
            };

            let args = bench_args();
            let selected_scenarios = vec!["cold".to_string()];
            let observation = start(BenchObservationStart {
                component_id: "homeboy",
                component_label: "homeboy",
                source_path: home.path(),
                args: &args,
                selected_scenarios: &selected_scenarios,
                rig_id: None,
                rig_snapshot: None,
                run_dir: &run_dir,
            })
            .expect("start observation");
            let run_id = observation.run_id().to_string();
            let summary = finish_success(Some(observation), &mut workflow, &run_dir)
                .expect("observation summary");
            assert_eq!(summary.run_id, run_id);
            assert_eq!(summary.component_id, "homeboy");
            assert_eq!(summary.rig_id, None);

            let hints = history_hints(&summary);
            assert!(hints
                .iter()
                .any(|hint| hint == &format!("View this run: homeboy runs show {run_id}")));
            assert!(hints.iter().any(|hint| hint
                == "List related bench runs: homeboy runs list --kind bench --component homeboy"));
            assert!(hints
                .iter()
                .any(|hint| hint.starts_with("Observation store: ")));

            let store = ObservationStore::open_initialized().expect("store");
            let run = store.get_run(&run_id).expect("read run").expect("run");
            assert_eq!(run.kind, "bench");
            assert_eq!(run.status, "pass");
            assert_eq!(run.component_id.as_deref(), Some("homeboy"));
            assert_eq!(run.metadata_json["selected_scenarios"][0], "cold");
            assert_eq!(
                run.metadata_json["scenario_metrics"][0]["scenario_id"],
                "cold"
            );
            assert_eq!(
                run.metadata_json["scenario_metrics"][0]["metrics"]["p95_ms"],
                42.0
            );

            let artifacts = store.list_artifacts(&run_id).expect("artifacts");
            let kinds: Vec<_> = artifacts
                .iter()
                .map(|artifact| artifact.kind.as_str())
                .collect();
            assert!(kinds.contains(&"bench_results"));
            assert!(kinds.contains(&"resource_summary"));
            assert!(kinds.contains(&"bench_artifact"));
            assert!(kinds.contains(&"admin_url"));
            assert!(artifacts
                .iter()
                .any(|artifact| artifact.artifact_type == "url"
                    && artifact.url.as_deref() == Some("https://example.test/wp-admin/")));
            let persisted_transcript = workflow.results.as_ref().unwrap().scenarios[0].artifacts
                ["transcript"]
                .path
                .as_deref()
                .expect("persisted transcript path");
            assert_ne!(persisted_transcript, "bench-artifacts/cold/transcript.json");
            assert!(PathBuf::from(persisted_transcript).is_file());
            let transcript_observation_id = workflow.results.as_ref().unwrap().scenarios[0]
                .artifacts["transcript"]
                .observation_artifact_id
                .as_deref()
                .expect("transcript observation artifact id");
            let transcript_record = artifacts
                .iter()
                .find(|artifact| artifact.id == transcript_observation_id)
                .expect("transcript artifact record");
            assert_eq!(transcript_record.metadata_json["source"], "bench");
            assert_eq!(transcript_record.metadata_json["scenario_id"], "cold");
            assert_eq!(transcript_record.metadata_json["name"], "transcript");
            assert_eq!(transcript_record.metadata_json["kind"], "json");
            assert!(
                workflow.results.as_ref().unwrap().scenarios[0].artifacts["admin"]
                    .observation_artifact_id
                    .is_some()
            );
        });
    }

    #[test]
    fn bench_observation_writes_status_file_at_start_and_finish() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let run_dir = RunDir::create().expect("run dir");
            fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");
            fs::write(run_dir.step_file(run_dir::files::RESOURCE_SUMMARY), b"{}")
                .expect("resources");
            let status_file = home.path().join("bench-status.json");
            let mut args = bench_args();
            args.status_file = Some(status_file.clone());
            let selected_scenarios = vec!["cold".to_string()];

            let observation = start(BenchObservationStart {
                component_id: "homeboy",
                component_label: "homeboy",
                source_path: home.path(),
                args: &args,
                selected_scenarios: &selected_scenarios,
                rig_id: None,
                rig_snapshot: None,
                run_dir: &run_dir,
            })
            .expect("start observation");
            let started: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&status_file).expect("read started status"),
            )
            .expect("started status json");
            assert_eq!(started["schema"], "homeboy/bench-status/v1");
            assert_eq!(started["status"], "running");
            assert_eq!(started["finished"], false);

            let mut workflow = BenchRunWorkflowResult {
                status: "passed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 0,
                iterations: 10,
                results: Some(bench_results("homeboy", "cold", 42.0)),
                gate_results: Vec::new(),
                gate_failures: Vec::new(),
                baseline_comparison: None,
                hints: Some(vec!["persisted".to_string()]),
                failure: None,
                diagnostics: Vec::new(),
            };
            let run_id = observation.run_id().to_string();
            finish_success(Some(observation), &mut workflow, &run_dir).expect("finish observation");
            let finished: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&status_file).expect("read finished status"),
            )
            .expect("finished status json");
            assert_eq!(finished["run_id"], run_id);
            assert_eq!(finished["status"], "passed");
            assert_eq!(finished["finished"], true);
            assert_eq!(finished["exit_code"], 0);
            assert_eq!(finished["artifact_count"], 2);
            assert_eq!(finished["hints"][0], "persisted");
        });
    }

    #[test]
    fn bench_observation_persists_phase_evidence_and_classification() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let run_dir = RunDir::create().expect("run dir");
            fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");

            let results = parse_bench_results_str(
                r#"{
                    "component_id": "homeboy",
                    "iterations": 1,
                    "phase_events": [
                        { "phase": "runtime_startup", "status": "started", "t_ms": 0 },
                        { "phase": "runtime_startup", "status": "heartbeat", "t_ms": 500, "message": "booting" },
                        { "phase": "runtime_startup", "status": "timeout", "t_ms": 1000, "message": "runtime did not become ready" }
                    ],
                    "scenarios": [
                        { "id": "cold", "iterations": 1, "metrics": { "p95_ms": 42.0 } }
                    ]
                }"#,
            )
            .expect("bench results");
            let mut workflow = BenchRunWorkflowResult {
                status: "failed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 124,
                iterations: 1,
                results: Some(results),
                gate_results: Vec::new(),
                gate_failures: Vec::new(),
                baseline_comparison: None,
                hints: None,
                failure: None,
                diagnostics: Vec::new(),
            };

            let args = bench_args();
            let observation = start(BenchObservationStart {
                component_id: "homeboy",
                component_label: "homeboy",
                source_path: home.path(),
                args: &args,
                selected_scenarios: &["cold".to_string()],
                rig_id: None,
                rig_snapshot: None,
                run_dir: &run_dir,
            })
            .expect("start observation");
            let run_id = observation.run_id().to_string();
            finish_success(Some(observation), &mut workflow, &run_dir).expect("finish observation");

            let store = ObservationStore::open_initialized().expect("store");
            let run = store.get_run(&run_id).expect("read run").expect("run");
            assert_eq!(
                run.metadata_json["phase_events"].as_array().unwrap().len(),
                3
            );
            assert_eq!(
                run.metadata_json["phase_summaries"][0]["phase"],
                "runtime_startup"
            );
            assert_eq!(run.metadata_json["phase_summaries"][0]["status"], "timeout");
            assert_eq!(
                run.metadata_json["phase_summaries"][0]["heartbeat_count"],
                1
            );
            assert_eq!(
                run.metadata_json["failure_classification"],
                serde_json::json!({
                    "kind": "timeout",
                    "phase": "runtime_startup",
                    "status": "timeout",
                    "message": "runtime did not become ready"
                })
            );
        });
    }

    #[test]
    fn bench_observation_rewrites_invocation_artifacts_to_persisted_paths() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let run_dir = RunDir::create().expect("run dir");
            fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");
            let invocation_artifact = run_dir
                .path()
                .join("invocations/inv-1/artifacts/semantic-fidelity.json");
            fs::create_dir_all(invocation_artifact.parent().expect("artifact parent"))
                .expect("mkdir");
            fs::write(&invocation_artifact, b"{\"score\":1}").expect("artifact");

            let mut results = bench_results("homeboy", "cold", 42.0);
            let original_path = invocation_artifact.to_string_lossy().to_string();
            results.scenarios[0].artifacts.insert(
                "semantic".to_string(),
                BenchArtifact {
                    path: Some(original_path.clone()),
                    url: None,
                    artifact_type: None,
                    kind: Some("json".to_string()),
                    label: Some("Semantic fidelity".to_string()),
                    observation_artifact_id: None,
                    ..BenchArtifact::default()
                },
            );
            let mut workflow = BenchRunWorkflowResult {
                status: "passed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 0,
                iterations: 10,
                results: Some(results),
                gate_results: Vec::new(),
                gate_failures: Vec::new(),
                baseline_comparison: None,
                hints: None,
                failure: None,
                diagnostics: Vec::new(),
            };

            let args = bench_args();
            let selected_scenarios = vec!["cold".to_string()];
            let observation = start(BenchObservationStart {
                component_id: "homeboy",
                component_label: "homeboy",
                source_path: home.path(),
                args: &args,
                selected_scenarios: &selected_scenarios,
                rig_id: None,
                rig_snapshot: None,
                run_dir: &run_dir,
            })
            .expect("start observation");
            let run_id = observation.run_id().to_string();

            finish_success(Some(observation), &mut workflow, &run_dir)
                .expect("observation summary");
            run_dir.cleanup();

            let persisted_path = workflow.results.as_ref().unwrap().scenarios[0].artifacts
                ["semantic"]
                .path
                .as_deref()
                .expect("persisted artifact path");
            assert_ne!(persisted_path, original_path);
            assert!(PathBuf::from(persisted_path).is_file());
            assert_eq!(
                fs::read_to_string(persisted_path).expect("read persisted"),
                "{\"score\":1}"
            );

            let store = ObservationStore::open_initialized().expect("store");
            let artifacts = store.list_artifacts(&run_id).expect("artifacts");
            let bench_results_artifact = artifacts
                .iter()
                .find(|artifact| artifact.kind == "bench_results")
                .expect("bench results artifact");
            let persisted_results_json: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&bench_results_artifact.path).expect("read bench results"),
            )
            .expect("parse persisted bench results");
            assert_eq!(
                persisted_results_json["scenarios"][0]["artifacts"]["semantic"]["path"],
                persisted_path
            );
        });
    }

    #[test]
    fn bench_observation_rewrites_cleaned_short_invocation_artifact_paths() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let run_dir = RunDir::create().expect("run dir");
            fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");
            let preserved_artifact = run_dir
                .path()
                .join("invocations/inv-cleaned-artifacts/artifacts/semantic-fidelity.json");
            fs::create_dir_all(preserved_artifact.parent().expect("artifact parent"))
                .expect("mkdir");
            fs::write(&preserved_artifact, b"{\"score\":1}").expect("artifact");

            let mut results = bench_results("homeboy", "cold", 42.0);
            let original_path = "/tmp/hb/cleaned-artifacts.a/semantic-fidelity.json".to_string();
            results.scenarios[0].artifacts.insert(
                "semantic".to_string(),
                BenchArtifact {
                    path: Some(original_path.clone()),
                    url: None,
                    artifact_type: None,
                    kind: Some("json".to_string()),
                    label: Some("Semantic fidelity".to_string()),
                    observation_artifact_id: None,
                    ..BenchArtifact::default()
                },
            );
            let mut workflow = BenchRunWorkflowResult {
                status: "passed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 0,
                iterations: 10,
                results: Some(results),
                gate_results: Vec::new(),
                gate_failures: Vec::new(),
                baseline_comparison: None,
                hints: None,
                failure: None,
                diagnostics: Vec::new(),
            };

            let args = bench_args();
            let selected_scenarios = vec!["cold".to_string()];
            let observation = start(BenchObservationStart {
                component_id: "homeboy",
                component_label: "homeboy",
                source_path: home.path(),
                args: &args,
                selected_scenarios: &selected_scenarios,
                rig_id: None,
                rig_snapshot: None,
                run_dir: &run_dir,
            })
            .expect("start observation");

            finish_success(Some(observation), &mut workflow, &run_dir)
                .expect("observation summary");
            run_dir.cleanup();

            let persisted_path = workflow.results.as_ref().unwrap().scenarios[0].artifacts
                ["semantic"]
                .path
                .as_deref()
                .expect("persisted artifact path");
            assert_ne!(persisted_path, original_path);
            assert!(PathBuf::from(persisted_path).is_file());
            assert_eq!(
                fs::read_to_string(persisted_path).expect("read persisted"),
                "{\"score\":1}"
            );
        });
    }

    #[test]
    fn bench_observation_persists_workload_artifact_directories() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let run_dir = RunDir::create().expect("run dir");
            fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");
            let artifact_dir = run_dir
                .path()
                .join("invocations/inv-1/artifacts/visual-comparisons");
            fs::create_dir_all(&artifact_dir).expect("mkdir");
            fs::write(
                artifact_dir.join("visual-comparison-skipped.json"),
                b"{\"skip\":true}",
            )
            .expect("artifact");

            let mut results = bench_results("homeboy", "cold", 42.0);
            let original_path = artifact_dir.to_string_lossy().to_string();
            results.scenarios[0].artifacts.insert(
                "visual_comparison_dir".to_string(),
                BenchArtifact {
                    path: Some(original_path.clone()),
                    url: None,
                    artifact_type: Some("directory".to_string()),
                    kind: Some("visual_comparison_dir".to_string()),
                    label: Some("Visual comparisons".to_string()),
                    observation_artifact_id: None,
                    ..BenchArtifact::default()
                },
            );
            let mut workflow = BenchRunWorkflowResult {
                status: "passed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 0,
                iterations: 10,
                results: Some(results),
                gate_results: Vec::new(),
                gate_failures: Vec::new(),
                baseline_comparison: None,
                hints: None,
                failure: None,
                diagnostics: Vec::new(),
            };

            let args = bench_args();
            let selected_scenarios = vec!["cold".to_string()];
            let observation = start(BenchObservationStart {
                component_id: "homeboy",
                component_label: "homeboy",
                source_path: home.path(),
                args: &args,
                selected_scenarios: &selected_scenarios,
                rig_id: None,
                rig_snapshot: None,
                run_dir: &run_dir,
            })
            .expect("start observation");
            let run_id = observation.run_id().to_string();

            finish_success(Some(observation), &mut workflow, &run_dir)
                .expect("observation summary");
            run_dir.cleanup();

            let persisted_path = workflow.results.as_ref().unwrap().scenarios[0].artifacts
                ["visual_comparison_dir"]
                .path
                .as_deref()
                .expect("persisted artifact path");
            assert_ne!(persisted_path, original_path);
            let persisted_dir = PathBuf::from(persisted_path);
            assert!(persisted_dir.is_dir());
            assert_eq!(
                fs::read_to_string(persisted_dir.join("visual-comparison-skipped.json"))
                    .expect("read persisted"),
                "{\"skip\":true}"
            );

            let store = ObservationStore::open_initialized().expect("store");
            let artifacts = store.list_artifacts(&run_id).expect("artifacts");
            assert!(artifacts
                .iter()
                .any(|artifact| artifact.kind == "bench_artifact"
                    && artifact.artifact_type == "directory"
                    && artifact.path == persisted_path));
        });
    }

    #[test]
    fn bench_observation_persists_workflow_error() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let run_dir = RunDir::create().expect("run dir");
            fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");
            let mut args = bench_args();
            args.rig = vec!["studio".to_string()];

            let observation = start(BenchObservationStart {
                component_id: "homeboy",
                component_label: "homeboy",
                source_path: home.path(),
                args: &args,
                selected_scenarios: &[],
                rig_id: Some("studio"),
                rig_snapshot: None,
                run_dir: &run_dir,
            })
            .expect("start observation");
            let run_id = observation.run_id().to_string();
            let error = homeboy::core::Error::validation_invalid_argument(
                "bench",
                "synthetic bench error",
                None,
                None,
            );
            finish_error(Some(observation), &error, &run_dir);

            let store = ObservationStore::open_initialized().expect("store");
            let run = store.get_run(&run_id).expect("read run").expect("run");
            assert_eq!(run.status, "error");
            assert_eq!(run.rig_id.as_deref(), Some("studio"));
            assert!(run.metadata_json["error"]
                .as_str()
                .expect("error string")
                .contains("synthetic bench error"));
            assert_eq!(store.list_artifacts(&run_id).expect("artifacts").len(), 1);
        });
    }

    #[test]
    fn history_hints_include_rig_filter_when_present() {
        let hints = history_hints(&BenchObservationSummary {
            run_id: "run-123".to_string(),
            component_id: "studio".to_string(),
            rig_id: Some("studio-trunk".to_string()),
            store_path: "/tmp/homeboy.sqlite".to_string(),
        });

        assert!(hints.iter().any(|hint| hint
            == "List related bench runs: homeboy runs list --kind bench --component studio --rig studio-trunk"));
    }

    fn synthetic_resources(recommendation: ResourceRecommendation) -> DoctorOutput {
        DoctorOutput {
            command: "doctor.resources",
            recommendation,
            load: LoadSummary {
                one: Some(18.2),
                five: Some(15.0),
                fifteen: Some(12.0),
                cpu_count: 18,
                recommendation,
            },
            memory: None,
            processes: ProcessSummary {
                relevant_count: 3,
                top_cpu: Vec::new(),
                top_rss: Vec::new(),
                recommendation: ResourceRecommendation::Ok,
            },
            rig_leases: RigLeaseSummary {
                active_count: 0,
                leases: Vec::new(),
                recommendation: ResourceRecommendation::Ok,
            },
            notes: Vec::new(),
        }
    }

    #[test]
    fn bench_observation_persists_resource_policy_warning_for_hot_machine() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            resource_policy::reset_captured_context_for_test();

            let synthetic = synthetic_resources(ResourceRecommendation::Hot);
            let warning = resource_policy::evaluate(HotCommand::lab_supported("bench"), &synthetic)
                .expect("synthetic warning");
            resource_policy::capture_context(ResourcePolicyContext::from_evaluation(
                HotCommand::lab_supported("bench"),
                &synthetic,
                Some(&warning),
                false,
            ));

            let run_dir = RunDir::create().expect("run dir");
            fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");

            let args = bench_args();
            let observation = start(BenchObservationStart {
                component_id: "homeboy",
                component_label: "homeboy",
                source_path: home.path(),
                args: &args,
                selected_scenarios: &[],
                rig_id: None,
                rig_snapshot: None,
                run_dir: &run_dir,
            })
            .expect("start observation");
            let run_id = observation.run_id().to_string();

            let mut workflow = BenchRunWorkflowResult {
                status: "passed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 0,
                iterations: 10,
                results: None,
                gate_results: Vec::new(),
                gate_failures: Vec::new(),
                baseline_comparison: None,
                hints: None,
                failure: None,
                diagnostics: Vec::new(),
            };
            finish_success(Some(observation), &mut workflow, &run_dir)
                .expect("observation summary");

            let store = ObservationStore::open_initialized().expect("store");
            let run = store.get_run(&run_id).expect("read run").expect("run");
            let policy = &run.metadata_json["resource_policy"];
            assert_eq!(policy["command"], "bench");
            assert_eq!(policy["severity"], "hot");
            assert_eq!(policy["force_hot"], false);
            assert_eq!(policy["warned"], true);
            assert!(policy["message"]
                .as_str()
                .expect("message string")
                .contains("Resource policy warning"));
            assert_eq!(policy["host"]["load_severity"], "hot");
            assert_eq!(policy["host"]["load_one"], 18.2);
            assert_eq!(policy["host"]["cpu_count"], 18);

            resource_policy::reset_captured_context_for_test();
        });
    }

    #[test]
    fn bench_observation_records_force_hot_bypass() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            resource_policy::reset_captured_context_for_test();

            let synthetic = synthetic_resources(ResourceRecommendation::Hot);
            let warning = resource_policy::evaluate(HotCommand::lab_supported("bench"), &synthetic)
                .expect("synthetic warning");
            resource_policy::capture_context(ResourcePolicyContext::from_evaluation(
                HotCommand::lab_supported("bench"),
                &synthetic,
                Some(&warning),
                true,
            ));

            let run_dir = RunDir::create().expect("run dir");
            fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");

            let args = bench_args();
            let observation = start(BenchObservationStart {
                component_id: "homeboy",
                component_label: "homeboy",
                source_path: home.path(),
                args: &args,
                selected_scenarios: &[],
                rig_id: None,
                rig_snapshot: None,
                run_dir: &run_dir,
            })
            .expect("start observation");
            let run_id = observation.run_id().to_string();

            let mut workflow = BenchRunWorkflowResult {
                status: "passed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 0,
                iterations: 10,
                results: None,
                gate_results: Vec::new(),
                gate_failures: Vec::new(),
                baseline_comparison: None,
                hints: None,
                failure: None,
                diagnostics: Vec::new(),
            };
            finish_success(Some(observation), &mut workflow, &run_dir)
                .expect("observation summary");

            let store = ObservationStore::open_initialized().expect("store");
            let run = store.get_run(&run_id).expect("read run").expect("run");
            let policy = &run.metadata_json["resource_policy"];
            assert_eq!(policy["severity"], "hot");
            assert_eq!(policy["force_hot"], true);
            assert_eq!(policy["warned"], true);

            resource_policy::reset_captured_context_for_test();
        });
    }

    #[test]
    fn bench_observation_omits_resource_policy_when_not_captured() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            resource_policy::reset_captured_context_for_test();

            let run_dir = RunDir::create().expect("run dir");
            fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");

            let args = bench_args();
            let observation = start(BenchObservationStart {
                component_id: "homeboy",
                component_label: "homeboy",
                source_path: home.path(),
                args: &args,
                selected_scenarios: &[],
                rig_id: None,
                rig_snapshot: None,
                run_dir: &run_dir,
            })
            .expect("start observation");
            let run_id = observation.run_id().to_string();

            let mut workflow = BenchRunWorkflowResult {
                status: "passed".to_string(),
                component: "homeboy".to_string(),
                exit_code: 0,
                iterations: 10,
                results: None,
                gate_results: Vec::new(),
                gate_failures: Vec::new(),
                baseline_comparison: None,
                hints: None,
                failure: None,
                diagnostics: Vec::new(),
            };
            finish_success(Some(observation), &mut workflow, &run_dir)
                .expect("observation summary");

            let store = ObservationStore::open_initialized().expect("store");
            let run = store.get_run(&run_id).expect("read run").expect("run");
            // When no preflight context was captured (e.g. the bench was
            // invoked from a context where main never ran), the metadata
            // explicitly records `null` rather than fabricating a snapshot.
            assert!(run.metadata_json["resource_policy"].is_null());
        });
    }
}
