use std::fs;
use std::path::PathBuf;

use homeboy::core::engine::run_dir::{self, RunDir};
use homeboy::core::extension::bench::artifact::BenchArtifact;
use homeboy::core::extension::bench::{
    parse_bench_results_str, BenchResults, BenchRunWorkflowResult,
};
use homeboy::core::observation::ObservationStore;

use super::*;
use crate::commands::bench::{BenchRigOrder, BenchRunArgs};
use crate::commands::doctor::resources::{
    DoctorOutput, LoadSummary, ProcessSummary, ResourceRecommendation, RigLeaseSummary,
};
use crate::commands::utils::args::{
    BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs,
};
use crate::commands::utils::resource_policy::{self, HotCommand, ResourcePolicyContext};
use crate::test_support::{serve_public_artifact_base_once, with_isolated_home};

pub(super) struct XdgGuard(Option<String>);

struct EnvGuard {
    key: &'static str,
    prior: Option<String>,
}

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

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[test]
fn recorded_bench_artifact_reports_unreachable_public_viewer_url() {
    let public_artifact_base = "https://artifacts.example.test/homeboy";
    let _public_artifact_base = EnvGuard::set(
        homeboy::core::artifacts::PUBLIC_ARTIFACT_BASE_URL_ENV,
        public_artifact_base,
    );
    let mut artifact = BenchArtifact::default();
    let record = ArtifactRecord {
        id: "artifact-1".to_string(),
        run_id: "run-1".to_string(),
        kind: "bench_artifact".to_string(),
        artifact_type: "file".to_string(),
        path: "/tmp/blueprint.after.json".to_string(),
        url: None,
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: None,
        size_bytes: None,
        mime: Some("application/json".to_string()),
        metadata_json: serde_json::json!({
            "viewer": crate::commands::runs::WORDPRESS_PLAYGROUND_BLUEPRINT_VIEWER.to_metadata(None),
            "public_url_validation": {
                "url": "https://artifacts.example.test/homeboy/runs/run-1/artifacts/artifact-1",
                "reachable": false,
                "status_code": 404,
                "error": "public artifact URL returned HTTP 404"
            }
        }),
        created_at: "2026-06-12T00:00:00Z".to_string(),
    };

    let diagnostic = apply_recorded_bench_artifact_links(
        "cold",
        Some(0),
        "blueprint.after",
        &mut artifact,
        &record,
    )
    .expect("unreachable diagnostic");

    assert_eq!(
        artifact.observation_artifact_id.as_deref(),
        Some("artifact-1")
    );
    assert!(artifact.public_url.is_some());
    assert!(artifact.viewer_refs.viewer_links.is_empty());
    assert_eq!(artifact.viewer_refs.viewer_url, None);
    assert_eq!(diagnostic.class, "bench_public_artifact_url_unreachable");
    assert_eq!(diagnostic.metadata["status_code"], 404);
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
        run_id: None,
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
        let public_artifact_base = serve_public_artifact_base_once(200);
        let _public_artifact_base = EnvGuard::set(
            homeboy::core::artifacts::PUBLIC_ARTIFACT_BASE_URL_ENV,
            &public_artifact_base,
        );
        let run_dir = RunDir::create().expect("run dir");
        fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");
        fs::write(run_dir.step_file(run_dir::files::RESOURCE_SUMMARY), b"{}").expect("resources");
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
                viewer: Some(
                    crate::commands::runs::WORDPRESS_PLAYGROUND_BLUEPRINT_VIEWER.to_metadata(None),
                ),
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
        let child_command_failures = vec![
            homeboy::core::extension::bench::parsing::BenchChildCommandFailure {
                argv: vec!["generic-child".to_string(), "run".to_string()],
                command: None,
                exit_status: Some(9),
                signal: None,
                stdout_tail: Some("child stdout tail".to_string()),
                stderr_tail: Some("child stderr tail".to_string()),
                scenario_id: Some("cold".to_string()),
                iteration: Some("5/10".to_string()),
                batch: None,
                artifact_refs: vec![serde_json::json!({
                    "kind": "log",
                    "ref": "runner-artifact://run/child-log"
                })],
            },
        ];
        results.child_command_failures = child_command_failures.clone();
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
        assert_eq!(
            run.metadata_json["child_command_failures"][0]["argv"][0],
            "generic-child"
        );
        assert_eq!(
            run.metadata_json["child_command_failures"][0]["stdout_tail"],
            "child stdout tail"
        );
        assert_eq!(
            run.metadata_json["child_command_failures"][0]["stderr_tail"],
            "child stderr tail"
        );
        assert_eq!(
            run.metadata_json["child_command_failures"][0]["artifact_refs"][0]["ref"],
            "runner-artifact://run/child-log"
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
        let transcript_observation_id = workflow.results.as_ref().unwrap().scenarios[0].artifacts
            ["transcript"]
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
        assert_eq!(
            transcript_record.metadata_json["viewer"]["query"]["value"]["source"],
            "public-artifact-url"
        );
        let transcript_artifact =
            &workflow.results.as_ref().unwrap().scenarios[0].artifacts["transcript"];
        assert!(transcript_artifact
            .public_url
            .as_deref()
            .expect("public url")
            .starts_with(&format!("{public_artifact_base}/")));
        assert!(transcript_artifact
            .viewer_refs
            .viewer_url
            .as_deref()
            .expect("viewer url")
            .starts_with("https://playground.wordpress.net/?blueprint-url="));
        assert_eq!(
            transcript_artifact.viewer_refs.viewer_links[0].kind,
            "wordpress-playground-blueprint"
        );
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
        fs::write(run_dir.step_file(run_dir::files::RESOURCE_SUMMARY), b"{}").expect("resources");
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
        let started: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&status_file).expect("read started status"))
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
        let finished: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&status_file).expect("read finished status"))
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
        fs::create_dir_all(invocation_artifact.parent().expect("artifact parent")).expect("mkdir");
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

        finish_success(Some(observation), &mut workflow, &run_dir).expect("observation summary");
        run_dir.cleanup();

        let persisted_path = workflow.results.as_ref().unwrap().scenarios[0].artifacts["semantic"]
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
fn bench_observation_mirrors_url_artifact_from_run_artifacts_dir() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let run_dir = RunDir::create().expect("run dir");
        fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), b"{}").expect("results");
        let runtime_artifact = run_dir.path().join("artifacts/finding-packets.json");
        fs::create_dir_all(runtime_artifact.parent().expect("artifact parent")).expect("mkdir");
        fs::write(
            &runtime_artifact,
            b"{\"finding_packets\":[{\"diagnostic_kind\":\"missing_block\"}]}",
        )
        .expect("artifact");

        let mut results = bench_results("homeboy", "cold", 42.0);
        results.scenarios[0].artifacts.insert(
            "finding_packets".to_string(),
            BenchArtifact {
                path: None,
                url: Some(
                    "https://homeboy-artifacts.example.test/finding-packets.json".to_string(),
                ),
                artifact_type: Some("url".to_string()),
                kind: Some("finding_packets".to_string()),
                label: Some("Finding packets".to_string()),
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
        let run_id = observation.run_id().to_string();

        finish_success(Some(observation), &mut workflow, &run_dir).expect("observation summary");
        run_dir.cleanup();

        let artifact =
            &workflow.results.as_ref().unwrap().scenarios[0].artifacts["finding_packets"];
        let persisted_path = artifact.path.as_deref().expect("persisted artifact path");
        assert!(PathBuf::from(persisted_path).is_file());
        assert_eq!(
            fs::read_to_string(persisted_path).expect("read persisted"),
            "{\"finding_packets\":[{\"diagnostic_kind\":\"missing_block\"}]}"
        );

        let store = ObservationStore::open_initialized().expect("store");
        let artifacts = store.list_artifacts(&run_id).expect("artifacts");
        let observation_artifact_id = artifact
            .observation_artifact_id
            .as_deref()
            .expect("observation artifact id");
        let record = artifacts
            .iter()
            .find(|artifact| artifact.id == observation_artifact_id)
            .expect("artifact record");
        assert_eq!(record.artifact_type, "file");
        assert_eq!(record.url, None);
        assert_eq!(record.metadata_json["source"], "bench");
        assert_eq!(record.metadata_json["name"], "finding_packets");
        assert_eq!(
            record.metadata_json["url"],
            "https://homeboy-artifacts.example.test/finding-packets.json"
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
        fs::create_dir_all(preserved_artifact.parent().expect("artifact parent")).expect("mkdir");
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

        finish_success(Some(observation), &mut workflow, &run_dir).expect("observation summary");
        run_dir.cleanup();

        let persisted_path = workflow.results.as_ref().unwrap().scenarios[0].artifacts["semantic"]
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

        finish_success(Some(observation), &mut workflow, &run_dir).expect("observation summary");
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
    assert!(hints.iter().any(|hint| hint
        == "Fetch an artifact: homeboy runs artifact get run-123 <artifact-name> -o <path>"));
    assert!(!hints.iter().any(|hint| hint.contains("--to <path>")));
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
        finish_success(Some(observation), &mut workflow, &run_dir).expect("observation summary");

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
        finish_success(Some(observation), &mut workflow, &run_dir).expect("observation summary");

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
        finish_success(Some(observation), &mut workflow, &run_dir).expect("observation summary");

        let store = ObservationStore::open_initialized().expect("store");
        let run = store.get_run(&run_id).expect("read run").expect("run");
        // When no preflight context was captured (e.g. the bench was
        // invoked from a context where main never ran), the metadata
        // explicitly records `null` rather than fabricating a snapshot.
        assert!(run.metadata_json["resource_policy"].is_null());
    });
}
