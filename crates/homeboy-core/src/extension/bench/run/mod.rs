//! Bench main workflow: invoke extension runner, load JSON, apply baseline.

mod failure;
mod list;
mod memory_timeline;
mod results;
mod runner;
mod scenario;
mod types;
mod workflow;

pub use types::{
    BenchListProfile, BenchListWorkflowArgs, BenchListWorkflowResult, BenchRunFailure,
    BenchRunWorkflowArgs, BenchRunWorkflowResult,
};

pub use list::run_bench_list_workflow;
pub use workflow::run_main_bench_workflow;

#[cfg(test)]
mod tests {
    use super::failure::classify_bench_failure;
    use super::list::bench_component_script_list_env;
    use super::memory_timeline::attach_memory_timeline_artifacts;
    use super::results::{parse_execution_results_file, workload_status_failures};
    use super::run_main_bench_workflow;
    use super::scenario::{
        filter_extra_workloads_by_scenario_ids, normalize_workload_json_scenario_ids,
    };
    use super::types::{BenchListWorkflowArgs, BenchRunWorkflowArgs};
    use super::workflow::bench_component_script_env;
    use crate::extension::bench::parsing::{self, BenchRunExecution};
    use crate::extension::bench::responsiveness::BenchResponsivenessSummary;
    use crate::extension::bench::test_support::{results_with_scenarios, scenario_with_iterations};
    use crate::extension::resolve::path_list_env_value;
    use homeboy_core::component::Component;
    use homeboy_core::engine::invocation::InvocationRequirements;
    use homeboy_core::engine::resource::{
        ChildProcessIdentity, ExtensionChildProcessSample, ExtensionChildResourceSample,
        ExtensionChildResourceSummary,
    };
    use homeboy_core::engine::run_dir::{self, RunDir};
    use homeboy_engine_primitives::baseline::BaselineFlags;
    use homeboy_extension_contract::{RigPackageEvidence, RigPackageFreshness};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn extra_workloads_env_value_joins_paths_for_runner_contract() {
        let paths = vec![
            PathBuf::from("/tmp/bench-one.php"),
            PathBuf::from("/tmp/bench-two.php"),
        ];

        assert_eq!(
            path_list_env_value("bench_workloads", &paths).unwrap(),
            "/tmp/bench-one.php:/tmp/bench-two.php"
        );
    }

    #[test]
    fn filter_extra_workloads_by_selected_scenario_ids_matches_runner_slugs() {
        let workloads = vec![
            PathBuf::from("/tmp/bench/studio-agent-runtime.bench.mjs"),
            PathBuf::from("/tmp/bench/studio-bfb-write-path.bench.js"),
            PathBuf::from("/tmp/bench/WpAdminLoad.php"),
            PathBuf::from("/tmp/bench/generated-rest-request-cases.workload.json"),
        ];

        let filtered = filter_extra_workloads_by_scenario_ids(
            &workloads,
            &[
                "studio-agent-runtime".to_string(),
                "wp-admin-load".to_string(),
                "generated-rest-request-cases".to_string(),
            ],
        );

        assert_eq!(
            filtered,
            vec![
                PathBuf::from("/tmp/bench/studio-agent-runtime.bench.mjs"),
                PathBuf::from("/tmp/bench/WpAdminLoad.php"),
                PathBuf::from("/tmp/bench/generated-rest-request-cases.workload.json"),
            ]
        );
    }

    #[test]
    fn normalize_workload_json_scenario_ids_updates_legacy_filename_ids_only() {
        let mut legacy = scenario_with_iterations("generated-rest-request-cases.workload", &[], 0);
        legacy.file = Some("tests/bench/generated-rest-request-cases.workload.json".to_string());
        let mut explicit = scenario_with_iterations("custom-declared-id", &[], 0);
        explicit.file = Some("tests/bench/custom-source.workload.json".to_string());
        let mut plain = scenario_with_iterations("plain-workload", &[], 0);
        plain.file = Some("tests/bench/plain-workload.php".to_string());
        let mut results = results_with_scenarios("woocommerce", 0, vec![legacy, explicit, plain]);

        normalize_workload_json_scenario_ids(&mut results);

        let ids: Vec<&str> = results
            .scenarios
            .iter()
            .map(|scenario| scenario.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "generated-rest-request-cases",
                "custom-declared-id",
                "plain-workload",
            ]
        );
    }

    #[test]
    fn component_script_bench_env_includes_typed_settings_json() {
        let run_dir = RunDir::create().expect("run dir");
        let mut args = bench_run_workflow_args_fixture();
        args.settings = vec![("workflow_bench_env.FOO".to_string(), "bar".to_string())];
        args.settings_json = vec![(
            "workflow_bench_env".to_string(),
            serde_json::json!({ "WORKFLOW_BENCH_RUN_ID": "component-script-run" }),
        )];
        let env = bench_component_script_env(&args, &run_dir).expect("component-script env");

        assert_eq!(
            env.iter()
                .find_map(|(key, value)| (key == "HOMEBOY_BENCH_RESULTS_FILE").then_some(value)),
            Some(
                &run_dir
                    .step_file(run_dir::files::BENCH_RESULTS)
                    .to_string_lossy()
                    .to_string()
            )
        );
        let settings = env
            .iter()
            .find_map(|(key, value)| (key == "HOMEBOY_SETTINGS_JSON").then_some(value))
            .expect("settings json env");
        let parsed: serde_json::Value = serde_json::from_str(settings).expect("settings json");
        assert_eq!(
            parsed["workflow_bench_env"]["WORKFLOW_BENCH_RUN_ID"],
            "component-script-run"
        );
        assert!(parsed["workflow_bench_env"]["FOO"].is_null());
        run_dir.cleanup();
    }

    #[test]
    fn component_script_bench_env_forwards_rig_package_root() {
        let run_dir = RunDir::create().expect("run dir");
        let mut args = bench_run_workflow_args_fixture();
        args.rig_package = Some(rig_package_evidence("/tmp/rig-package"));

        let env = bench_component_script_env(&args, &run_dir).expect("component-script env");

        assert_eq!(
            env.iter().find_map(|(key, value)|
                (key == "HOMEBOY_BENCH_RIG_PACKAGE_ROOT").then_some(value)
            ),
            Some(&"/tmp/rig-package".to_string())
        );
        run_dir.cleanup();
    }

    #[test]
    fn component_script_bench_env_serializes_passthrough_args() {
        let run_dir = RunDir::create().expect("run dir");
        for (passthrough_args, expected) in [
            (Vec::new(), "[]"),
            (
                vec![
                    "--workload-option".to_string(),
                    "value with spaces".to_string(),
                ],
                r#"["--workload-option","value with spaces"]"#,
            ),
        ] {
            let mut args = bench_run_workflow_args_fixture();
            args.passthrough_args = passthrough_args;

            let env = bench_component_script_env(&args, &run_dir).expect("component-script env");

            assert_eq!(
                env.iter()
                    .find_map(|(key, value)| (key == "HOMEBOY_BENCH_ARGS_JSON").then_some(value)),
                Some(&expected.to_string())
            );
        }
        run_dir.cleanup();
    }

    #[test]
    fn component_script_bench_env_forwards_only_nonblank_run_id() {
        let run_dir = RunDir::create().expect("run dir");
        for (run_id, expected) in [
            (Some("proof-2026-06"), Some("proof-2026-06")),
            (None, None),
            (Some("   "), None),
        ] {
            let mut args = bench_run_workflow_args_fixture();
            args.run_id = run_id.map(str::to_string);

            let env = bench_component_script_env(&args, &run_dir).expect("component-script env");

            assert_eq!(
                env.iter()
                    .find_map(|(key, value)| (key == "HOMEBOY_BENCH_RUN_ID").then_some(value))
                    .map(String::as_str),
                expected,
                "run_id: {run_id:?}"
            );
        }
        run_dir.cleanup();
    }

    fn bench_run_workflow_args_fixture() -> BenchRunWorkflowArgs {
        BenchRunWorkflowArgs {
            component_label: "studio-web".to_string(),
            component_id: "studio-web".to_string(),
            path_override: None,
            settings: Vec::new(),
            settings_json: Vec::new(),
            iterations: 10,
            warmup_iterations: None,
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
            scenario_ids: Vec::new(),
            rig_id: None,
            shared_state: None,
            extra_workloads: Vec::new(),
            env_provider_extensions: Vec::new(),
            rig_package: None,
            invocation_requirements: InvocationRequirements::default(),
        }
    }

    fn rig_package_evidence(package_root: &str) -> RigPackageEvidence {
        RigPackageEvidence {
            rig_id: "fixture".to_string(),
            package_root: package_root.to_string(),
            source: "fixture".to_string(),
            source_root: None,
            rig_path: None,
            discovery_path: None,
            installed_source_revision: None,
            current_source_revision: None,
            source_content_hash: None,
            source_ref: None,
            source_dirty: false,
            linked: false,
            materialized: true,
            freshness: RigPackageFreshness::Verified,
            freshness_verified: true,
            freshness_message: None,
            refresh_command: None,
        }
    }

    #[test]
    fn component_script_bench_list_env_projects_list_contract() {
        let env = bench_component_script_list_env(&BenchListWorkflowArgs {
            component_label: "studio-web".to_string(),
            component_id: "studio-web".to_string(),
            path_override: None,
            settings: Vec::new(),
            settings_json: vec![(
                "workflow_bench_env".to_string(),
                serde_json::json!({ "WORKFLOW_BENCH_SCENARIO": "plain-site-sample-plugin" }),
            )],
            passthrough_args: vec!["--scenario-filter".to_string(), "plain".to_string()],
            scenario_ids: Vec::new(),
            extra_workloads: Vec::new(),
            env_provider_extensions: Vec::new(),
            rig_package: None,
            profiles: Vec::new(),
        })
        .expect("component-script list env");
        let value_of = |name: &str| {
            env.iter()
                .find_map(|(key, value)| (key == name).then_some(value.as_str()))
        };

        assert_eq!(value_of("HOMEBOY_BENCH_LIST_ONLY"), Some("1"));
        assert_eq!(
            value_of("HOMEBOY_BENCH_ARGS_JSON"),
            Some(r#"["--scenario-filter","plain"]"#)
        );
        assert_eq!(value_of("HOMEBOY_BENCH_RIG_PACKAGE_ROOT"), None);
        let parsed: serde_json::Value =
            serde_json::from_str(value_of("HOMEBOY_SETTINGS_JSON").expect("settings json env"))
                .expect("settings json");
        assert_eq!(
            parsed["workflow_bench_env"]["WORKFLOW_BENCH_SCENARIO"],
            "plain-site-sample-plugin"
        );
    }

    #[test]
    fn failed_execution_parse_ignores_unselected_duplicate_scenario_ids() {
        let run_dir = RunDir::create().expect("run dir");
        let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
        fs::write(
            &results_file,
            r#"{
                "component_id": "woocommerce",
                "iterations": 1,
                "scenarios": [
                    {
                        "id": "rest-product-batch-import",
                        "file": "tests/bench/rest-product-batch-import.php",
                        "iterations": 1,
                        "metrics": { "p95_ms": 5.0 }
                    },
                    {
                        "id": "checkout-concurrent-create-order",
                        "file": "tests/bench/checkout-concurrent-create-order.php",
                        "iterations": 1,
                        "metrics": { "p95_ms": 10.0 }
                    },
                    {
                        "id": "checkout-concurrent-create-order",
                        "iterations": 1,
                        "metrics": { "p95_ms": 20.0 }
                    }
                ]
            }"#,
        )
        .expect("write results file");

        let parsed = parse_execution_results_file(
            &results_file,
            &["rest-product-batch-import".to_string()],
            false,
            None,
            None,
        )
        .expect("failed runner parse should not validate unselected duplicates")
        .expect("parsed results");

        assert_eq!(parsed.scenarios.len(), 1);
        assert_eq!(parsed.scenarios[0].id, "rest-product-batch-import");
    }

    #[test]
    fn failed_execution_parse_discards_inventory_only_results() {
        let run_dir = RunDir::create().expect("run dir");
        let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
        fs::write(
            &results_file,
            r#"{
                "component_id": "woocommerce",
                "iterations": 0,
                "scenarios": [
                    {
                        "id": "cart-session-overwrite-race",
                        "file": "tests/bench/cart-session-overwrite-race.php",
                        "source": "rig",
                        "default_iterations": 1,
                        "iterations": 0,
                        "metrics": {}
                    },
                    {
                        "id": "generated-rest-request-cases.workload",
                        "file": "tests/bench/generated-rest-request-cases.workload.json",
                        "source": "rig",
                        "default_iterations": 1,
                        "iterations": 0,
                        "metrics": {}
                    }
                ]
            }"#,
        )
        .expect("write results file");

        let parsed = parse_execution_results_file(&results_file, &[], false, None, None)
            .expect("failed inventory parse should succeed");

        assert!(
            parsed.is_none(),
            "inventory-only failed payload must not be surfaced as measured bench results"
        );
        run_dir.cleanup();
    }

    #[test]
    fn failed_execution_parse_keeps_measured_failure_results() {
        let run_dir = RunDir::create().expect("run dir");
        let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
        fs::write(
            &results_file,
            r#"{
                "component_id": "woocommerce",
                "iterations": 1,
                "failure_classification": {
                    "kind": "assertion_failure",
                    "phase": "bench",
                    "status": "failed"
                },
                "scenarios": [
                    {
                        "id": "checkout-concurrent-create-order",
                        "file": "tests/bench/checkout-concurrent-create-order.php",
                        "iterations": 1,
                        "metrics": { "failed_count": 1 }
                    }
                ]
            }"#,
        )
        .expect("write results file");

        let parsed = parse_execution_results_file(&results_file, &[], false, None, None)
            .expect("failed measured parse should succeed")
            .expect("measured results");

        assert_eq!(parsed.iterations, 1);
        assert_eq!(parsed.scenarios[0].metrics.get("failed_count"), Some(1.0));
        assert_eq!(
            parsed
                .failure_classification
                .as_ref()
                .map(|value| value.kind.as_str()),
            Some("assertion_failure")
        );
        run_dir.cleanup();
    }

    #[test]
    fn workload_status_failures_flag_unhealthy_child_counters() {
        let cases: [(&str, &str, &[&str]); 4] = [
            (
                r#""adapter_count": 2, "passed_count": 0, "failed_count": 1, "unsupported_count": 1"#,
                "{}",
                &["failed_count=1", "unsupported_count=1"],
            ),
            (
                r#""adapter_count": 1, "passed_count": 0, "failed_count": 0, "unsupported_count": 0, "warning_count": 1"#,
                "{}",
                &["warning_count=1", "passed_count=0"],
            ),
            (
                r#""adapter_count": 1, "passed_count": 0, "failed_count": 0, "unsupported_count": 0"#,
                r#"{ "result_counts": { "warning": 1 } }"#,
                &["warning_count=1", "passed_count=0"],
            ),
            (
                r#""adapter_count": 2, "passed_count": 2, "failed_count": 0, "unsupported_count": 0"#,
                "{}",
                &[],
            ),
        ];
        for (metrics, metadata, expected) in cases {
            let results = parsing::parse_bench_results_str(&format!(
                r#"{{
                    "component_id": "studio-web",
                    "iterations": 1,
                    "scenarios": [{{
                        "id": "workflow-bench",
                        "iterations": 1,
                        "metrics": {{ {metrics} }},
                        "metadata": {metadata}
                    }}]
                }}"#
            ))
            .expect("parse bench results");

            let failures = workload_status_failures(&results);

            if expected.is_empty() {
                assert!(failures.is_empty(), "metrics: {metrics}: {failures:?}");
                continue;
            }
            assert_eq!(failures.len(), 1, "metrics: {metrics}");
            assert!(failures[0].contains("workflow-bench"));
            for fragment in expected {
                assert!(failures[0].contains(fragment), "{}", failures[0]);
            }
        }
    }

    #[test]
    fn component_script_bench_failed_metrics_mark_workflow_failed() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_dir = RunDir::create().expect("run dir");
            let component_dir = run_dir.path().join("component");
            let script_dir = component_dir.join("scripts");
            fs::create_dir_all(&script_dir).expect("script dir");
            fs::write(
                script_dir.join("bench.sh"),
                r#"#!/bin/sh
cat > "$HOMEBOY_BENCH_RESULTS_FILE" <<'JSON'
{
  "component_id": "workflow-bench-fixture",
  "iterations": 1,
  "scenarios": [
    {
      "id": "workflow-bench",
      "iterations": 1,
      "metrics": {
        "adapter_count": 1,
        "passed_count": 0,
        "failed_count": 1
      },
      "artifacts": {
        "report": {
          "path": "bench-report.json",
          "kind": "json",
          "label": "Bench report"
        }
      }
    }
  ]
}
JSON
printf '{}' > "$(dirname "$HOMEBOY_BENCH_RESULTS_FILE")/bench-report.json"
"#,
            )
            .expect("bench script");
            let mut component = Component::new(
                "workflow-bench-fixture".to_string(),
                component_dir.to_string_lossy().to_string(),
                String::new(),
                None,
            );
            component.scripts = Some(homeboy_core::component::ComponentScriptsConfig {
                bench: vec![format!("sh {}", script_dir.join("bench.sh").display())],
                ..Default::default()
            });

            let result = run_main_bench_workflow(
                &component,
                &component_dir,
                BenchRunWorkflowArgs {
                    component_label: "Workflow Bench Fixture".to_string(),
                    component_id: "workflow-bench-fixture".to_string(),
                    iterations: 1,
                    ..bench_run_workflow_args_fixture()
                },
                &run_dir,
            )
            .expect("bench workflow");

            assert_eq!(result.status, "failed");
            assert!(result.results.is_some());
            assert!(result
                .gate_failures
                .iter()
                .any(|failure| failure.contains("failed_count=1")));
            assert!(result.results.as_ref().unwrap().scenarios[0]
                .artifacts
                .contains_key("report"));
            run_dir.cleanup();
        });
    }

    #[test]
    fn attach_memory_timeline_adds_metrics_and_artifacts() {
        let run_dir = RunDir::create().expect("run dir");
        let mut results = results_with_scenarios(
            "homeboy",
            1,
            vec![scenario_with_iterations("cold-start", &[], 1)],
        );
        let child = ExtensionChildResourceSummary {
            child: ChildProcessIdentity {
                root_pid: 42,
                command_label: "bench fixture".to_string(),
            },
            phase: None,
            started_at: "2026-06-08T00:00:00Z".to_string(),
            finished_at: "2026-06-08T00:00:01Z".to_string(),
            duration_ms: 1000,
            peak: homeboy_core::engine::resource::ChildResourcePeakSample {
                sampled_peak_rss_bytes: Some(2 * 1024 * 1024),
                sampled_peak_cpu_percent: Some(3.5),
            },
            sampled_peak_at_ms: Some(100),
            sampled_peak_child_count: Some(1),
            samples: vec![ExtensionChildResourceSample {
                elapsed_ms: 100,
                timestamp: "2026-06-08T00:00:00.100Z".to_string(),
                root_pid: 42,
                phase: None,
                rss_bytes: 2 * 1024 * 1024,
                cpu_percent: 3.5,
                child_count: 1,
                processes: vec![ExtensionChildProcessSample {
                    pid: 42,
                    parent_pid: 1,
                    rss_bytes: 2 * 1024 * 1024,
                    cpu_percent: 3.5,
                    command: "bench".to_string(),
                }],
            }],
            warnings: Vec::new(),
        };
        let mut phase_child = child.clone();
        phase_child.child.root_pid = 43;
        phase_child.child.command_label = "npm install".to_string();
        phase_child.phase = Some("install".to_string());
        phase_child.peak.sampled_peak_rss_bytes = Some(3 * 1024 * 1024);
        phase_child.samples[0].root_pid = 43;
        phase_child.samples[0].phase = Some("install".to_string());
        phase_child.samples[0].rss_bytes = 3 * 1024 * 1024;
        homeboy_core::engine::resource::record_extension_child_resource(
            run_dir.path(),
            &phase_child,
        )
        .expect("record phase resource");

        attach_memory_timeline_artifacts(&mut results, Some(&child), &run_dir, None)
            .expect("attach memory timeline");

        assert_eq!(results.metric_groups["memory"]["peak_rss_mb"], 2.0);
        assert_eq!(results.metric_groups["memory"]["peak_install_rss_mb"], 3.0);
        assert_eq!(results.scenarios[0].metrics.values["peak_rss_mb"], 2.0);
        assert_eq!(
            results.scenarios[0].metrics.values["peak_install_rss_mb"],
            3.0
        );
        assert_eq!(
            results.metadata["phase_memory"]["phases"]["install"]["peak_rss_mb"].as_f64(),
            Some(3.0)
        );
        assert!(results.scenarios[0]
            .artifacts
            .contains_key("memory_timeline_json"));
        assert!(results.scenarios[0]
            .artifacts
            .contains_key("phase_memory_timeline_json"));
        assert!(run_dir.step_file("bench-memory-timeline.json").is_file());
        assert!(run_dir.step_file("bench-memory-timeline.csv").is_file());
        assert!(run_dir
            .step_file("bench-memory-timeline-phases.json")
            .is_file());
        assert!(run_dir
            .step_file("bench-memory-timeline-phases.csv")
            .is_file());

        run_dir.cleanup();
    }

    #[test]
    fn classifies_failed_run_with_missed_pings_as_responsiveness_loss() {
        let responsiveness = BenchResponsivenessSummary {
            missed_ping_count: 2,
            max_ping_gap_ms: 15_000,
            last_ping_at: Some("2026-06-08T00:00:00Z".to_string()),
            ping_count: 3,
            missed_ping_window_ms: 5_000,
        };

        let classification =
            classify_bench_failure(false, 1, "", Some(&responsiveness)).expect("classification");

        assert_eq!(classification.kind, "responsiveness_loss");
        assert_eq!(classification.phase, "responsiveness");
        assert_eq!(classification.status, "missed_ping");
        assert!(classification
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("last ping"));
    }

    #[test]
    fn classifies_timeout_assertion_and_worker_failures() {
        let timeout = classify_bench_failure(false, 124, "", None).expect("timeout");
        assert_eq!(timeout.kind, "timeout");

        let assertion = classify_bench_failure(false, 1, "expected button to be visible", None)
            .expect("assertion");
        assert_eq!(assertion.kind, "assertion_failure");

        let worker =
            classify_bench_failure(false, 1, "recipe-run failed: unresolved_local_url", None)
                .expect("worker exception");
        assert_eq!(worker.kind, "worker_exception");
        assert_eq!(worker.phase, "workload");
        assert_eq!(worker.status, "error");
    }

    #[test]
    fn main_bench_workflow_rejects_zero_concurrency() {
        let run_dir = RunDir::create().expect("run dir");
        let err = run_main_bench_workflow(
            &Component::default(),
            &PathBuf::from("/tmp/homeboy"),
            BenchRunWorkflowArgs {
                iterations: 1,
                execution: BenchRunExecution {
                    runs: 1,
                    concurrency: 0,
                },
                ..bench_run_workflow_args_fixture()
            },
            &run_dir,
        )
        .expect_err("zero concurrency must fail before runner resolution");

        assert!(format!("{}", err).contains("concurrency"));
    }
}
