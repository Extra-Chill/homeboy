use std::fs;

use homeboy::core::engine::run_dir::{self, RunDir};
use homeboy::core::extension::bench::artifact::BenchArtifact;
use homeboy::core::extension::bench::BenchRunWorkflowResult;

use super::tests::{bench_args, bench_results, XdgGuard};
use super::{finish_success, start, BenchObservationStart};
use crate::test_support::with_isolated_home;

#[test]
fn bench_observation_reports_missing_and_blocked_artifacts() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let run_dir = RunDir::create().expect("run dir");
        fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");

        let mut results = bench_results("homeboy", "cold", 42.0);
        results.scenarios[0].artifacts.insert(
            "missing".to_string(),
            BenchArtifact {
                path: Some("bench-artifacts/cold/missing.json".to_string()),
                url: None,
                artifact_type: None,
                kind: Some("json".to_string()),
                label: Some("Missing".to_string()),
                observation_artifact_id: None,
                ..BenchArtifact::default()
            },
        );
        results.scenarios[0].artifacts.insert(
            "escape".to_string(),
            BenchArtifact {
                path: Some("../escape.json".to_string()),
                url: None,
                artifact_type: None,
                kind: Some("json".to_string()),
                label: Some("Escape".to_string()),
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

        finish_success(Some(observation), &mut workflow, &run_dir).expect("observation summary");

        let classes: Vec<_> = workflow
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.class.as_str())
            .collect();
        assert!(classes.contains(&"bench_artifact_path_missing"));
        assert!(classes.contains(&"bench_artifact_path_blocked"));
        assert!(
            workflow.results.as_ref().unwrap().scenarios[0].artifacts["missing"]
                .observation_artifact_id
                .is_none()
        );
        assert!(
            workflow.results.as_ref().unwrap().scenarios[0].artifacts["escape"]
                .observation_artifact_id
                .is_none()
        );
    });
}
