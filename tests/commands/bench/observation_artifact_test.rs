use std::fs;

use homeboy::core::engine::run_dir::{self, RunDir};
use homeboy_extension::bench::artifact::BenchArtifact;
use homeboy_extension::bench::result_types::BenchRunMetadata;
use homeboy_extension::bench::{BenchRunExecution, BenchRunWorkflowResult};

use super::tests::{bench_args, bench_results, XdgGuard};
use super::{finish_success, start, BenchObservationStart};
use crate::test_support::with_isolated_home;

#[test]
fn bench_observation_reports_missing_and_blocked_artifacts() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let run_dir = RunDir::create().expect("run dir");
        fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");
        fs::write(run_dir.path().join("promoted.json"), b"{}").expect("promoted artifact");

        let mut results = bench_results("homeboy", "cold", 42.0);
        results.scenarios[0].artifacts.insert(
            "promoted".to_string(),
            BenchArtifact {
                path: Some("promoted.json".to_string()),
                url: Some("https://expired-runner.example/promoted.json".to_string()),
                required_durable: true,
                ..BenchArtifact::default()
            },
        );
        results.scenarios[0].artifacts.insert(
            "missing".to_string(),
            BenchArtifact {
                path: Some("bench-artifacts/cold/missing.json".to_string()),
                url: None,
                artifact_type: None,
                kind: Some("json".to_string()),
                label: Some("Missing".to_string()),
                observation_artifact_id: Some("stale-artifact-id".to_string()),
                required_durable: true,
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
                observation_artifact_id: Some("stale-url-id".to_string()),
                required_durable: true,
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
        assert_eq!(workflow.status, "failed");
        assert_eq!(workflow.exit_code, 1);
        assert!(workflow.failure.is_some());
        assert_eq!(
            workflow.results.as_ref().unwrap().scenarios[0].artifacts["promoted"].url,
            None
        );
        assert!(
            workflow.results.as_ref().unwrap().scenarios[0].artifacts["promoted"]
                .observation_artifact_id
                .is_some()
        );
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

#[test]
fn bench_observation_rejects_url_only_artifacts_as_terminal_evidence() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let run_dir = RunDir::create().expect("run dir");
        fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");
        let mut results = bench_results("homeboy", "cold", 42.0);
        results.scenarios[0].artifacts.insert(
            "visual".to_string(),
            BenchArtifact {
                path: None,
                url: Some("https://tunnel.example/visual.png".to_string()),
                artifact_type: Some("image/png".to_string()),
                kind: Some("visual".to_string()),
                label: Some("Visual comparison".to_string()),
                observation_artifact_id: None,
                required_durable: true,
                ..BenchArtifact::default()
            },
        );
        let mut workflow = BenchRunWorkflowResult {
            status: "passed".to_string(),
            component: "homeboy".to_string(),
            exit_code: 0,
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

        finish_success(Some(observation), &mut workflow, &run_dir).expect("observation summary");

        assert_eq!(workflow.status, "failed");
        assert!(workflow
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.class == "bench_artifact_durable_source_missing" }));
        assert!(
            workflow.results.as_ref().unwrap().scenarios[0].artifacts["visual"]
                .observation_artifact_id
                .is_none()
        );
    });
}

#[test]
fn bench_observation_promotes_required_directory_with_tree_identity() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let run_dir = RunDir::create().expect("run dir");
        fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");
        let directory = run_dir.path().join("artifacts/visual");
        fs::create_dir_all(directory.join("nested")).expect("artifact directory");
        fs::write(directory.join("baseline.png"), b"baseline").expect("baseline");
        fs::write(directory.join("nested/diff.png"), b"diff").expect("diff");
        let expected_tree =
            homeboy::core::observation::directory_tree_sha256(&directory).expect("tree hash");
        let mut results = bench_results("homeboy", "cold", 42.0);
        results.scenarios[0].artifacts.insert(
            "visuals".to_string(),
            BenchArtifact {
                path: Some("artifacts/visual".to_string()),
                required_durable: true,
                ..BenchArtifact::default()
            },
        );
        let mut workflow = BenchRunWorkflowResult {
            status: "passed".to_string(),
            component: "homeboy".to_string(),
            exit_code: 0,
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
        .expect("observation");
        let run_id = observation.run_id().to_string();
        finish_success(Some(observation), &mut workflow, &run_dir).expect("summary");

        assert_eq!(workflow.status, "passed");
        let store =
            homeboy::core::observation::ObservationStore::open_initialized().expect("store");
        let artifact_id = workflow.results.as_ref().unwrap().scenarios[0].artifacts["visuals"]
            .observation_artifact_id
            .clone()
            .expect("artifact id");
        let artifact = store
            .get_artifact(&artifact_id)
            .expect("artifact lookup")
            .expect("artifact");
        assert_eq!(artifact.run_id, run_id);
        assert_eq!(artifact.artifact_type, "directory");
        assert_eq!(artifact.sha256.as_deref(), Some(expected_tree.as_str()));
        assert!(std::path::Path::new(&artifact.path).is_dir());
    });
}

#[test]
fn bench_observation_resolves_shared_state_mount_artifacts() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let run_dir = RunDir::create().expect("run dir");
        fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");

        let shared_state = home.path().join("shared-state");
        let host_artifact = shared_state.join("evidence/query-profile.json");
        fs::create_dir_all(host_artifact.parent().expect("artifact parent")).expect("mkdir");
        fs::write(&host_artifact, b"{\"ok\":true}").expect("artifact");

        let mut results = bench_results("homeboy", "cold", 42.0);
        results.run_metadata = Some(BenchRunMetadata {
            homeboy_version: Some("test".to_string()),
            started_at: "2026-06-08T00:00:00Z".to_string(),
            shared_state: Some(shared_state.to_string_lossy().to_string()),
            iterations: 10,
            execution: BenchRunExecution {
                runs: 1,
                concurrency: 1,
            },
            warmup_iterations: None,
            selected_scenarios: Vec::new(),
            env_overrides: Default::default(),
            workloads: Vec::new(),
            provenance: Default::default(),
            runner: None,
            rig_package: None,
            lifecycle: None,
            diagnostics: Vec::new(),
        });
        results.scenarios[0].artifacts.insert(
            "query-profile".to_string(),
            BenchArtifact {
                path: Some("/bench-shared-state/evidence/query-profile.json".to_string()),
                url: None,
                artifact_type: None,
                kind: Some("json".to_string()),
                label: Some("Query profile".to_string()),
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
        assert!(!classes.contains(&"bench_artifact_path_missing"));
        assert!(
            workflow.results.as_ref().unwrap().scenarios[0].artifacts["query-profile"]
                .observation_artifact_id
                .is_some()
        );
    });
}
