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
    BenchListWorkflowArgs, BenchListWorkflowResult, BenchRunFailure, BenchRunWorkflowArgs,
    BenchRunWorkflowResult,
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
    use super::runner::instance_results_filename;
    use super::scenario::{
        filter_extra_workloads_by_scenario_ids, normalize_workload_json_scenario_ids,
    };
    use super::types::{BenchListWorkflowArgs, BenchListWorkflowResult, BenchRunWorkflowArgs};
    use super::workflow::bench_component_script_env;
    use crate::core::component::Component;
    use crate::core::engine::baseline::BaselineFlags;
    use crate::core::engine::invocation::InvocationRequirements;
    use crate::core::engine::resource::{
        self, ChildProcessIdentity, ExtensionChildProcessSample, ExtensionChildResourceSample,
        ExtensionChildResourceSummary,
    };
    use crate::core::engine::run_dir::{self, RunDir};
    use crate::core::extension::bench::parsing::{
        self, BenchResults, BenchRunExecution, BenchScenario,
    };
    use crate::core::extension::bench::responsiveness::BenchResponsivenessSummary;
    use crate::core::extension::bench::test_support::{
        results_with_scenarios, scenario_with_iterations,
    };
    use crate::core::extension::path_list_env_value;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn instance_results_filename_is_distinct_per_instance() {
        assert_eq!(instance_results_filename(0), "bench-results-i0.json");
        assert_eq!(instance_results_filename(7), "bench-results-i7.json");
        assert_ne!(instance_results_filename(0), instance_results_filename(1));
    }

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
        let env = bench_component_script_env(
            &BenchRunWorkflowArgs {
                component_label: "studio-web".to_string(),
                component_id: "studio-web".to_string(),
                path_override: None,
                settings: vec![("workflow_bench_env.FOO".to_string(), "bar".to_string())],
                settings_json: vec![(
                    "workflow_bench_env".to_string(),
                    serde_json::json!({ "WORKFLOW_BENCH_RUN_ID": "component-script-run" }),
                )],
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
            },
            &run_dir,
        )
        .expect("component-script env");

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
    fn component_script_bench_env_forwards_run_id_proof_label() {
        let run_dir = RunDir::create().expect("run dir");
        let mut args = bench_run_workflow_args_fixture();
        args.run_id = Some("proof-2026-06".to_string());

        let env = bench_component_script_env(&args, &run_dir).expect("component-script env");

        assert_eq!(
            env.iter()
                .find_map(|(key, value)| (key == "HOMEBOY_BENCH_RUN_ID").then_some(value)),
            Some(&"proof-2026-06".to_string()),
            "run-id proof label should forward to HOMEBOY_BENCH_RUN_ID"
        );
        run_dir.cleanup();
    }

    #[test]
    fn component_script_bench_env_omits_run_id_when_absent_or_blank() {
        let run_dir = RunDir::create().expect("run dir");

        let mut absent = bench_run_workflow_args_fixture();
        absent.run_id = None;
        let env = bench_component_script_env(&absent, &run_dir).expect("component-script env");
        assert!(
            !env.iter().any(|(key, _)| key == "HOMEBOY_BENCH_RUN_ID"),
            "no --run-id should leave HOMEBOY_BENCH_RUN_ID unset"
        );

        let mut blank = bench_run_workflow_args_fixture();
        blank.run_id = Some("   ".to_string());
        let env = bench_component_script_env(&blank, &run_dir).expect("component-script env");
        assert!(
            !env.iter().any(|(key, _)| key == "HOMEBOY_BENCH_RUN_ID"),
            "whitespace-only --run-id should be ignored, not forwarded blank"
        );
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

    #[test]
    fn component_script_bench_list_env_includes_typed_settings_json() {
        let env = bench_component_script_list_env(&BenchListWorkflowArgs {
            component_label: "studio-web".to_string(),
            component_id: "studio-web".to_string(),
            path_override: None,
            settings: Vec::new(),
            settings_json: vec![(
                "workflow_bench_env".to_string(),
                serde_json::json!({ "WORKFLOW_BENCH_SCENARIO": "plain-site-sample-plugin" }),
            )],
            passthrough_args: Vec::new(),
            scenario_ids: Vec::new(),
            extra_workloads: Vec::new(),
            env_provider_extensions: Vec::new(),
            rig_package: None,
        })
        .expect("component-script list env");

        assert!(env
            .iter()
            .any(|(key, value)| key == "HOMEBOY_BENCH_LIST_ONLY" && value == "1"));
        let settings = env
            .iter()
            .find_map(|(key, value)| (key == "HOMEBOY_SETTINGS_JSON").then_some(value))
            .expect("settings json env");
        let parsed: serde_json::Value = serde_json::from_str(settings).expect("settings json");
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

        let parsed = parse_execution_results_file(&results_file, &[], false, None)
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

        let parsed = parse_execution_results_file(&results_file, &[], false, None)
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
    fn workload_status_failures_catches_failed_and_unsupported_children() {
        let results = parsing::parse_bench_results_str(
            r#"{
                "component_id": "studio-web",
                "iterations": 1,
                "scenarios": [{
                    "id": "workflow-bench",
                    "iterations": 1,
                    "metrics": {
                        "adapter_count": 2,
                        "passed_count": 0,
                        "failed_count": 1,
                        "unsupported_count": 1
                    }
                }]
            }"#,
        )
        .expect("parse bench results");

        let failures = workload_status_failures(&results);

        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("workflow-bench"));
        assert!(failures[0].contains("failed_count=1"));
        assert!(failures[0].contains("unsupported_count=1"));
    }

    #[test]
    fn workload_status_failures_catches_warning_children() {
        let results = parsing::parse_bench_results_str(
            r#"{
                "component_id": "studio-web",
                "iterations": 1,
                "scenarios": [{
                    "id": "workflow-bench",
                    "iterations": 1,
                    "metrics": {
                        "adapter_count": 1,
                        "passed_count": 0,
                        "failed_count": 0,
                        "unsupported_count": 0,
                        "warning_count": 1
                    }
                }]
            }"#,
        )
        .expect("parse bench results");

        let failures = workload_status_failures(&results);

        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("workflow-bench"));
        assert!(failures[0].contains("warning_count=1"));
        assert!(failures[0].contains("passed_count=0"));
    }

    #[test]
    fn workload_status_failures_catches_metadata_warning_result_counts() {
        let results = parsing::parse_bench_results_str(
            r#"{
                "component_id": "studio-web",
                "iterations": 1,
                "scenarios": [{
                    "id": "workflow-bench",
                    "iterations": 1,
                    "metrics": {
                        "adapter_count": 1,
                        "passed_count": 0,
                        "failed_count": 0,
                        "unsupported_count": 0
                    },
                    "metadata": {
                        "result_counts": {
                            "warning": 1
                        }
                    }
                }]
            }"#,
        )
        .expect("parse bench results");

        let failures = workload_status_failures(&results);

        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("workflow-bench"));
        assert!(failures[0].contains("warning_count=1"));
        assert!(failures[0].contains("passed_count=0"));
    }

    #[test]
    fn workload_status_failures_allows_clean_child_counters() {
        let results = parsing::parse_bench_results_str(
            r#"{
                "component_id": "studio-web",
                "iterations": 1,
                "scenarios": [{
                    "id": "workflow-bench",
                    "iterations": 1,
                    "metrics": {
                        "adapter_count": 2,
                        "passed_count": 2,
                        "failed_count": 0,
                        "unsupported_count": 0
                    }
                }]
            }"#,
        )
        .expect("parse bench results");

        assert!(workload_status_failures(&results).is_empty());
    }

    #[test]
    fn component_script_bench_failed_metrics_mark_workflow_failed() {
        crate::test_support::with_isolated_home(|_| {
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
            component.scripts = Some(crate::core::component::ComponentScriptsConfig {
                bench: vec![format!("sh {}", script_dir.join("bench.sh").display())],
                ..Default::default()
            });

            let result = run_main_bench_workflow(
                &component,
                &component_dir,
                BenchRunWorkflowArgs {
                    component_label: "Workflow Bench Fixture".to_string(),
                    component_id: "workflow-bench-fixture".to_string(),
                    path_override: None,
                    settings: Vec::new(),
                    settings_json: Vec::new(),
                    iterations: 1,
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
        let mut results = BenchResults {
            component_id: "homeboy".to_string(),
            iterations: 1,
            provenance: Default::default(),
            run_metadata: None,
            metadata: BTreeMap::new(),
            metric_groups: BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: BTreeMap::new(),
            diagnostics: Vec::new(),
            child_command_failures: Vec::new(),
            phase_events: Vec::new(),
            phase_summaries: Vec::new(),
            failure_classification: None,
            responsiveness: None,
            budget_findings: Vec::new(),
            scenarios: vec![BenchScenario {
                id: "cold-start".to_string(),
                file: None,
                source: None,
                default_iterations: None,
                tags: Vec::new(),
                iterations: 1,
                metrics: parsing::BenchMetrics::default(),
                metric_groups: BTreeMap::new(),
                timeline: Vec::new(),
                span_definitions: Vec::new(),
                span_results: Vec::new(),
                gates: Vec::new(),
                gate_results: Vec::new(),
                metadata: BTreeMap::new(),
                provenance: Default::default(),
                passed: true,
                memory: None,
                artifacts: BTreeMap::new(),
                diagnostics: Vec::new(),
                runs: None,
                runs_summary: None,
            }],
            metric_policies: BTreeMap::new(),
            metric_policy_presets: BTreeMap::new(),
        };
        let child = ExtensionChildResourceSummary {
            child: ChildProcessIdentity {
                root_pid: 42,
                command_label: "bench fixture".to_string(),
            },
            phase: None,
            started_at: "2026-06-08T00:00:00Z".to_string(),
            finished_at: "2026-06-08T00:00:01Z".to_string(),
            duration_ms: 1000,
            peak: crate::core::engine::resource::ChildResourcePeakSample {
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
        resource::record_extension_child_resource(run_dir.path(), &phase_child)
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
    fn classifies_timeout_and_assertion_failures() {
        let timeout = classify_bench_failure(false, 124, "", None).expect("timeout");
        assert_eq!(timeout.kind, "timeout");

        let assertion = classify_bench_failure(false, 1, "expected button to be visible", None)
            .expect("assertion");
        assert_eq!(assertion.kind, "assertion_failure");
    }

    #[test]
    fn test_run_bench_list_workflow() {
        let result = BenchListWorkflowResult {
            component: "homeboy".to_string(),
            component_id: "homeboy".to_string(),
            count: 1,
            rig_package: None,
            scenarios: vec![BenchScenario {
                id: "audit-self".to_string(),
                file: Some("src/bin/bench-audit-self.rs".to_string()),
                source: Some("in_tree".to_string()),
                default_iterations: Some(10),
                tags: Vec::new(),
                iterations: 0,
                metrics: parsing::BenchMetrics {
                    values: BTreeMap::new(),
                    distributions: BTreeMap::new(),
                },
                metric_groups: BTreeMap::new(),
                timeline: Vec::new(),
                span_definitions: Vec::new(),
                span_results: Vec::new(),
                gates: Vec::new(),
                gate_results: Vec::new(),
                metadata: BTreeMap::new(),
                provenance: Default::default(),
                passed: true,
                memory: None,
                artifacts: BTreeMap::new(),
                diagnostics: Vec::new(),
                runs: None,
                runs_summary: None,
            }],
        };

        assert_eq!(result.count, result.scenarios.len());
        assert_eq!(result.scenarios[0].iterations, 0);
        assert!(result.scenarios[0].metrics.values.is_empty());
        assert_eq!(result.scenarios[0].default_iterations, Some(10));
    }

    #[test]
    fn test_run_main_bench_workflow() {
        let run_dir = RunDir::create().expect("run dir");
        let err = run_main_bench_workflow(
            &Component::default(),
            &PathBuf::from("/tmp/homeboy"),
            BenchRunWorkflowArgs {
                component_label: "homeboy".to_string(),
                component_id: "homeboy".to_string(),
                path_override: None,
                settings: Vec::new(),
                settings_json: Vec::new(),
                iterations: 1,
                warmup_iterations: None,
                run_id: None,
                execution: BenchRunExecution {
                    runs: 1,
                    concurrency: 0,
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
            },
            &run_dir,
        )
        .expect_err("zero concurrency must fail before runner resolution");

        assert!(format!("{}", err).contains("concurrency"));
    }
}
