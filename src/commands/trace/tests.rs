use std::fs;

use clap::Parser;

use crate::test_support::with_isolated_home;

use super::test_fixture::{
    init_overlay_component, write_missing_trace_artifact_extension,
    write_nested_trace_artifact_extension, write_trace_extension, write_trace_port_env_extension,
    write_trace_rig, write_trace_rig_with_phase_preset, write_trace_rig_with_port_range,
    write_trace_rig_with_span_metadata, write_trace_rig_with_variant,
};
use super::*;

#[derive(Parser)]
struct TestCli {
    #[command(flatten)]
    trace: TraceArgs,
}

#[test]
fn trace_accepts_allow_local_evidence_alias() {
    let cli = TestCli::try_parse_from(["trace", "component", "scenario", "--allow-local-evidence"])
        .expect("trace args parse");

    assert!(cli.trace.allow_local_toolchain);
}

#[test]
fn lab_dispatch_observation_persists_trace_run_before_remote_execution() {
    with_isolated_home(|home| {
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());
        let args = trace_args_for_rig("studio-rig", "studio", "studio-app-create-site");
        let normalized = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--rig".to_string(),
            "studio-rig".to_string(),
            "studio".to_string(),
            "studio-app-create-site".to_string(),
        ];

        let observation = start_lab_dispatch_observation(&args, &normalized, Some("homeboy-lab"))
            .expect("dispatch observation");
        let trace_run = observation
            .store
            .get_trace_run(&observation.run_id)
            .expect("read trace run")
            .expect("trace run exists before remote execution");
        assert_eq!(trace_run.status, "running");
        assert_eq!(trace_run.component_id, "studio");
        assert_eq!(trace_run.scenario_id, "studio-app-create-site");
        assert_eq!(
            trace_run.metadata_json["lab_dispatch"]["phase"],
            "route_before_lab_dispatch"
        );

        let run_id = observation.run_id.clone();
        let store = ObservationStore::open_initialized().expect("store");
        finish_lab_dispatch_observation(
            Some(observation),
            RunStatus::Error,
            serde_json::json!({
                "lab_dispatch": {
                    "phase": "route_lab_dispatch",
                    "status": "timeout"
                }
            }),
        );
        let trace_run = store
            .get_trace_run(&run_id)
            .expect("read trace run")
            .expect("trace run remains present");
        assert_eq!(trace_run.status, "error");
        assert_eq!(trace_run.metadata_json["lab_dispatch"]["status"], "timeout");
    });
}

fn trace_args_for_rig(rig_id: &str, component_id: &str, scenario_id: &str) -> TraceArgs {
    TraceArgs {
        comp: PositionalComponentArgs {
            component: Some(component_id.to_string()),
            path: None,
        },
        component_arg: None,
        scenario: Some(scenario_id.to_string()),
        scenario_arg: None,
        compare_after: None,
        baseline_target: None,
        candidate: None,
        rig: Some(rig_id.to_string()),
        profile: None,
        profiles: false,
        setting_args: SettingArgs::default(),
        json_summary: false,
        report: None,
        experiment: None,
        repeat: 1,
        aggregate: None,
        schedule: TraceSchedule::Grouped,
        focus_spans: Vec::new(),
        spans: Vec::new(),
        phases: Vec::new(),
        attachments: Vec::new(),
        phase_preset: None,
        baseline_args: BaselineArgs::default(),
        regression_threshold: extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
        regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
        overlays: Vec::new(),
        variants: Vec::new(),
        matrix: TraceVariantMatrixMode::None,
        axes: Vec::new(),
        matrix_env: Vec::new(),
        output_dir: None,
        keep_overlay: false,
        stale: false,
        force: false,
        canonical: false,
        allow_local_toolchain: true,
    }
}

#[test]
fn rig_trace_run_uses_rig_owned_workload_extension_without_component_link() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("studio".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("studio-app-create-site".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("studio-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 1,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: Vec::new(),
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: Vec::new(),
                variants: Vec::new(),
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("rig trace run should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Run(result) => {
                assert!(result.passed);
                assert_eq!(result.component, "studio");
                assert_eq!(
                    result.results.expect("results").scenario_id,
                    "studio-app-create-site"
                );
            }
            _ => panic!("expected run output"),
        }
    });
}

#[test]
fn rig_trace_workload_port_range_sets_invocation_env() {
    with_isolated_home(|home| {
        write_trace_port_env_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig_with_port_range(home, "studio-rig", "studio", component_dir.path());

        let (output, exit_code) = run(
            trace_args_for_rig("studio-rig", "studio", "studio-app-create-site"),
            &GlobalArgs {},
        )
        .expect("rig trace run should receive invocation port env");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Run(result) => {
                assert!(result.passed);
                let assertions = result.results.expect("results").assertions;
                assert!(assertions
                    .iter()
                    .any(|assertion| assertion.id == "invocation-ports"));
            }
            _ => panic!("expected run output"),
        }
    });
}

#[test]
fn trace_run_persists_observation_history() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let (_output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("studio".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("studio-app-create-site".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("studio-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 1,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: Vec::new(),
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: Vec::new(),
                variants: Vec::new(),
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("trace should run");

        assert_eq!(exit_code, 0);
        let store = ObservationStore::open_initialized().expect("store");
        let runs = store
            .list_runs(homeboy::core::observation::RunListFilter {
                kind: Some("trace".to_string()),
                ..Default::default()
            })
            .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "pass");
        assert_eq!(runs[0].component_id.as_deref(), Some("studio"));
        assert_eq!(runs[0].rig_id.as_deref(), Some("studio-rig"));

        let trace_run = store
            .get_trace_run(&runs[0].id)
            .expect("trace run")
            .expect("trace run row");
        assert_eq!(trace_run.component_id, "studio");
        assert_eq!(trace_run.scenario_id, "studio-app-create-site");
        assert_eq!(trace_run.status, "pass");
        assert_eq!(trace_run.metadata_json["span_count"], 1);

        let spans = store.list_trace_spans(&runs[0].id).expect("spans");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].span_id, "boot_to_ready");
        assert_eq!(spans[0].duration_ms, Some(125.0));

        let artifacts = store.list_artifacts(&runs[0].id).expect("artifacts");
        assert!(artifacts.len() >= 3);
        assert!(artifacts
            .iter()
            .any(|artifact| artifact.kind == "trace-results"));
        assert!(artifacts
            .iter()
            .any(|artifact| artifact.kind == "trace-artifact"));
        assert!(artifacts
            .iter()
            .any(|artifact| artifact.kind == "trace-artifacts"));
    });
}

#[test]
fn trace_run_preserves_nested_child_artifact_directories() {
    with_isolated_home(|home| {
        write_nested_trace_artifact_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let (_output, exit_code) = run(
            trace_args_for_rig("studio-rig", "studio", "studio-app-create-site"),
            &GlobalArgs {},
        )
        .expect("trace should run");

        assert_eq!(exit_code, 0);
        let store = ObservationStore::open_initialized().expect("store");
        let runs = store
            .list_runs(homeboy::core::observation::RunListFilter {
                kind: Some("trace".to_string()),
                ..Default::default()
            })
            .expect("runs");
        assert_eq!(runs[0].status, "pass");
        let artifacts = store.list_artifacts(&runs[0].id).expect("artifacts");
        let directory_artifact = artifacts
            .iter()
            .find(|artifact| {
                artifact.kind == "trace-artifacts" && artifact.artifact_type == "directory"
            })
            .expect("trace artifact directory is preserved");
        assert!(std::path::Path::new(&directory_artifact.path)
            .join("wp-codebox-artifacts/runtime-fixture/files/browser/network.jsonl")
            .is_file());
        assert!(artifacts.iter().any(|artifact| {
            artifact.kind == "trace-artifact"
                && artifact.artifact_type == "file"
                && artifact.path.ends_with("network.jsonl")
        }));
    });
}

#[test]
fn trace_run_fails_when_declared_artifact_is_missing() {
    with_isolated_home(|home| {
        write_missing_trace_artifact_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let (output, exit_code) = run(
            trace_args_for_rig("studio-rig", "studio", "studio-app-create-site"),
            &GlobalArgs {},
        )
        .expect("trace should run and report missing artifact");

        assert_eq!(exit_code, 1);
        let TraceCommandOutput::Run(result) = output else {
            panic!("expected run output");
        };
        let results = result.results.expect("trace results");
        assert_eq!(results.status.as_str(), "error");
        assert!(results
            .failure
            .as_deref()
            .expect("failure")
            .contains("wp-codebox-artifacts/runtime-fixture/files/browser/network.jsonl"));
        assert!(results.assertions.iter().any(|assertion| {
            assertion.status == extension_trace::TraceAssertionStatus::Error
                && assertion
                    .message
                    .as_deref()
                    .unwrap_or_default()
                    .contains("wp-codebox-artifacts/runtime-fixture/files/browser/network.jsonl")
        }));
    });
}

#[test]
fn trace_repeat_aggregates_span_timings_and_preserves_artifacts() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("studio".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("studio-app-create-site".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("studio-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 3,
                aggregate: Some("spans".to_string()),
                schedule: TraceSchedule::Interleaved,
                focus_spans: vec!["boot_to_ready".to_string()],
                spans: Vec::new(),
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: Vec::new(),
                variants: Vec::new(),
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("repeat trace should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Aggregate(aggregate) => {
                assert_eq!(aggregate.repeat, 3);
                assert_eq!(aggregate.run_count, 3);
                assert_eq!(aggregate.failure_count, 0);
                assert_eq!(aggregate.schedule.as_deref(), Some("interleaved"));
                assert_eq!(aggregate.run_order.len(), 3);
                assert_eq!(aggregate.run_order[0].index, 1);
                assert_eq!(aggregate.run_order[0].group, "run");
                assert_eq!(aggregate.run_order[0].iteration, 1);
                assert_eq!(aggregate.spans.len(), 1);
                assert_eq!(aggregate.focus_span_ids, vec!["boot_to_ready"]);
                assert_eq!(aggregate.focus_spans.len(), 1);
                let span = &aggregate.spans[0];
                assert_eq!(span.id, "boot_to_ready");
                assert_eq!(span.n, 3);
                assert_eq!(span.min_ms, Some(125));
                assert_eq!(span.median_ms, Some(125));
                assert_eq!(span.avg_ms, Some(125.0));
                assert_eq!(span.stddev_ms, Some(0.0));
                assert_eq!(span.p75_ms, None);
                assert_eq!(span.p90_ms, None);
                assert_eq!(span.p95_ms, None);
                assert_eq!(span.max_ms, Some(125));
                assert!(matches!(span.max_run_index, Some(1..=3)));
                assert!(span
                    .max_artifact_path
                    .as_ref()
                    .is_some_and(|path| std::path::Path::new(path).is_file()));
                assert_eq!(span.failures, 0);
                assert_eq!(span.samples.len(), 3);
                assert!(span.samples.iter().all(|sample| sample.duration_ms == 125));
                assert!(span
                    .samples
                    .iter()
                    .all(|sample| std::path::Path::new(&sample.artifact_path).is_file()));
                assert!(aggregate
                    .runs
                    .iter()
                    .all(|run| std::path::Path::new(&run.artifact_path).is_file()));
            }
            _ => panic!("expected aggregate output"),
        }
    });
}

#[test]
fn trace_repeat_loads_span_metadata_and_reports_unknown_ids() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig_with_span_metadata(home, "studio-rig", "studio", component_dir.path());

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("studio".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("studio-app-create-site".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("studio-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 2,
                aggregate: Some("spans".to_string()),
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: Vec::new(),
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: Vec::new(),
                variants: Vec::new(),
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("repeat trace should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Aggregate(aggregate) => {
                let span = aggregate
                    .spans
                    .iter()
                    .find(|span| span.id == "phase.boot_to_ready")
                    .expect("boot span");
                let metadata = span.metadata.as_ref().expect("span metadata");
                assert!(metadata.critical);
                assert!(metadata.blocking);
                assert!(metadata.cacheable);
                assert!(metadata.prewarmable);
                assert_eq!(metadata.blocks.as_deref(), Some("first_site_render"));
                assert_eq!(metadata.category.as_deref(), Some("wordpress_boot"));
                assert_eq!(aggregate.unmatched_span_metadata_ids, vec!["missing_span"]);
                assert!(aggregate.classification_summaries.iter().any(|summary| {
                    summary.classification == "cacheable_critical"
                        && summary.span_count == 1
                        && summary.total_median_ms == Some(125)
                }));

                let markdown = render_aggregate_markdown(&aggregate);
                assert!(markdown.contains("## Critical Path Classification"));
                assert!(markdown.contains("| `cacheable_critical` | 1 | 125ms | 125.0ms |"));
                assert!(markdown.contains("## Unmatched Span Metadata"));
                assert!(markdown.contains("- `missing_span`"));
            }
            _ => panic!("expected aggregate output"),
        }
    });
}

#[test]
fn trace_run_order_planner_supports_grouped_and_interleaved_variants() {
    let grouped = plan_trace_run_order(2, TraceSchedule::Grouped, &["baseline", "variant"]);
    assert_eq!(
        grouped
            .iter()
            .map(|entry| (entry.index(), entry.group(), entry.iteration()))
            .collect::<Vec<_>>(),
        vec![
            (1, "baseline", 1),
            (2, "baseline", 2),
            (3, "variant", 1),
            (4, "variant", 2),
        ]
    );

    let interleaved = plan_trace_run_order(2, TraceSchedule::Interleaved, &["baseline", "variant"]);
    assert_eq!(
        interleaved
            .iter()
            .map(|entry| (entry.index(), entry.group(), entry.iteration()))
            .collect::<Vec<_>>(),
        vec![
            (1, "baseline", 1),
            (2, "variant", 1),
            (3, "baseline", 2),
            (4, "variant", 2),
        ]
    );

    let stack = vec![
        TraceVariantStackItem {
            label: "a".to_string(),
            overlay: "a.patch".to_string(),
        },
        TraceVariantStackItem {
            label: "b".to_string(),
            overlay: "b.patch".to_string(),
        },
        TraceVariantStackItem {
            label: "c".to_string(),
            overlay: "c.patch".to_string(),
        },
    ];
    let single = expand_variant_matrix(&stack, TraceVariantMatrixMode::Single);
    assert_eq!(
        single
            .iter()
            .map(|combo| combo
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        vec![vec!["a"], vec!["b"], vec!["c"]]
    );

    let cumulative = expand_variant_matrix(&stack, TraceVariantMatrixMode::Cumulative);
    assert_eq!(
        cumulative
            .iter()
            .map(|combo| combo
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        vec![vec!["a"], vec!["a", "b"], vec!["a", "b", "c"]]
    );

    let full_stack = expand_variant_matrix(&stack[..2], TraceVariantMatrixMode::None);
    assert_eq!(full_stack.len(), 1);
    assert_eq!(full_stack[0].label, "a+b");
    assert_eq!(
        full_stack[0]
            .items
            .iter()
            .map(|item| item.overlay.as_str())
            .collect::<Vec<_>>(),
        vec!["a.patch", "b.patch"]
    );
}

#[test]
fn trace_repeat_reports_overlay_touched_files_at_top_level() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        init_overlay_component(component_dir.path());
        let patch_path = component_dir.path().join("overlay.patch");
        fs::write(
            &patch_path,
            r#"diff --git a/scenario.txt b/scenario.txt
--- a/scenario.txt
+++ b/scenario.txt
@@ -1 +1 @@
-base
+overlay
"#,
        )
        .expect("write patch");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("studio".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("studio-app-create-site".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("studio-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 2,
                aggregate: Some("spans".to_string()),
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: Vec::new(),
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: vec![patch_path.to_string_lossy().to_string()],
                variants: Vec::new(),
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("repeat trace should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Aggregate(aggregate) => {
                assert_eq!(aggregate.overlays.len(), 1);
                let component_path = component_dir.path().to_string_lossy();
                assert_eq!(
                    aggregate.overlays[0].component_path,
                    component_path.as_ref()
                );
                assert_eq!(aggregate.overlays[0].touched_files, vec!["scenario.txt"]);
                assert!(!aggregate.overlays[0].kept);
                let value = serde_json::to_value(&aggregate).expect("aggregate serializes");
                assert_eq!(
                    value["overlays"][0]["component_path"],
                    component_path.as_ref()
                );
                assert_eq!(value["overlays"][0]["touched_files"][0], "scenario.txt");
            }
            _ => panic!("expected aggregate output"),
        }
    });
}

#[test]
fn trace_run_resolves_named_variants_and_reports_unknown_names() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        init_overlay_component(component_dir.path());
        let package_dir = tempfile::TempDir::new().expect("package dir");
        write_trace_rig_with_variant(
            home,
            package_dir.path(),
            "studio-rig",
            "studio",
            component_dir.path(),
        );

        let valid_args = TraceArgs {
            comp: PositionalComponentArgs {
                component: Some("studio".to_string()),
                path: None,
            },
            component_arg: None,
            scenario: Some("studio-app-create-site".to_string()),
            scenario_arg: None,
            compare_after: None,
            baseline_target: None,
            candidate: None,
            rig: Some("studio-rig".to_string()),
            profile: None,
            profiles: false,
            setting_args: SettingArgs::default(),
            json_summary: false,
            report: None,
            experiment: None,
            repeat: 1,
            aggregate: None,
            schedule: TraceSchedule::Grouped,
            focus_spans: Vec::new(),
            spans: Vec::new(),
            phases: Vec::new(),
            attachments: Vec::new(),
            phase_preset: None,
            baseline_args: BaselineArgs::default(),
            regression_threshold: extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
            regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
            overlays: Vec::new(),
            variants: vec!["fresh-install-mode".to_string()],
            matrix: TraceVariantMatrixMode::None,
            axes: Vec::new(),
            matrix_env: Vec::new(),
            output_dir: None,
            keep_overlay: false,
            stale: false,
            force: false,
            canonical: false,
            allow_local_toolchain: true,
        };

        let (output, exit_code) =
            run(valid_args.clone(), &GlobalArgs {}).expect("variant trace should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Run(result) => {
                assert_eq!(result.overlays.len(), 1);
                let overlay = &result.overlays[0];
                assert_eq!(overlay.variant.as_deref(), Some("fresh-install-mode"));
                assert_eq!(
                    overlay.path,
                    package_dir
                        .path()
                        .join("overlays/fresh-install-mode.patch")
                        .to_string_lossy()
                );
                assert_eq!(overlay.touched_files, vec!["scenario.txt"]);
                let value = serde_json::to_value(&result).expect("result serializes");
                assert_eq!(value["overlays"][0]["variant"], "fresh-install-mode");
                assert_eq!(
                    value["overlays"][0]["path"],
                    package_dir
                        .path()
                        .join("overlays/fresh-install-mode.patch")
                        .to_string_lossy()
                        .as_ref()
                );
            }
            _ => panic!("expected run output"),
        }
        assert_eq!(
            fs::read_to_string(component_dir.path().join("scenario.txt")).unwrap(),
            "base\n"
        );

        let mut invalid_args = valid_args;
        invalid_args.variants = vec!["missing".to_string()];
        let err = match run(invalid_args, &GlobalArgs {}) {
            Ok(_) => panic!("unknown variant should fail"),
            Err(err) => err,
        };

        assert!(err.message.contains("unknown trace variant 'missing'"));
        assert!(err
            .details
            .get("id")
            .and_then(|value| value.as_str())
            .expect("details id")
            .contains("fresh-install-mode"));
    });
}

#[test]
fn trace_compare_variant_writes_experiment_bundle() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        init_overlay_component(component_dir.path());
        let patch_path = component_dir.path().join("overlay.patch");
        fs::write(
            &patch_path,
            r#"diff --git a/scenario.txt b/scenario.txt
--- a/scenario.txt
+++ b/scenario.txt
@@ -1 +1 @@
-base
+overlay
"#,
        )
        .expect("write patch");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());
        let output_dir = tempfile::TempDir::new().expect("output dir");

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("compare-variant".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("studio-app-create-site".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("studio-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 2,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: Vec::new(),
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: vec![patch_path.to_string_lossy().to_string()],
                variants: Vec::new(),
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: Some(output_dir.path().to_path_buf()),
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("compare-variant should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Compare(compare) => {
                assert_eq!(compare.span_count, 1);
                assert!(compare.before_path.ends_with("baseline.json"));
                assert!(compare.after_path.ends_with("variant.json"));
            }
            _ => panic!("expected compare output"),
        }
        assert!(output_dir.path().join("baseline.json").is_file());
        assert!(output_dir.path().join("variant.json").is_file());
        assert!(output_dir.path().join("compare.json").is_file());
        let summary = fs::read_to_string(output_dir.path().join("summary.md")).expect("summary");
        assert!(summary.contains("## Baseline Component SHAs"));
        assert!(summary.contains("## Variant Component SHAs"));
        assert!(summary.contains("scenario.txt"));
    });
}

#[test]
fn trace_compare_variant_resolves_named_variants() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        init_overlay_component(component_dir.path());
        let package_dir = tempfile::TempDir::new().expect("package dir");
        write_trace_rig_with_variant(
            home,
            package_dir.path(),
            "studio-rig",
            "studio",
            component_dir.path(),
        );
        let output_dir = tempfile::TempDir::new().expect("output dir");

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("compare-variant".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("studio-app-create-site".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("studio-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 2,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: Vec::new(),
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: Vec::new(),
                variants: vec!["fresh-install-mode".to_string()],
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: Some(output_dir.path().to_path_buf()),
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("named variant compare-variant should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Compare(compare) => {
                assert_eq!(compare.span_count, 1);
                assert!(compare.before_path.ends_with("baseline.json"));
                assert!(compare.after_path.ends_with("variant.json"));
            }
            _ => panic!("expected compare output"),
        }
        let baseline: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(output_dir.path().join("baseline.json")).expect("baseline"),
        )
        .expect("baseline json");
        let variant: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(output_dir.path().join("variant.json")).expect("variant"),
        )
        .expect("variant json");
        assert!(baseline
            .get("overlays")
            .and_then(|overlays| overlays.as_array())
            .map(|overlays| overlays.is_empty())
            .unwrap_or(true));
        assert_eq!(variant["overlays"][0]["variant"], "fresh-install-mode");
        assert_eq!(
            variant["overlays"][0]["path"],
            package_dir
                .path()
                .join("overlays/fresh-install-mode.patch")
                .to_string_lossy()
                .as_ref()
        );
    });
}

#[test]
fn trace_compare_variant_reports_unknown_named_variants() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        init_overlay_component(component_dir.path());
        let package_dir = tempfile::TempDir::new().expect("package dir");
        write_trace_rig_with_variant(
            home,
            package_dir.path(),
            "studio-rig",
            "studio",
            component_dir.path(),
        );
        let output_dir = tempfile::TempDir::new().expect("output dir");

        let err = match run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("compare-variant".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("studio-app-create-site".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("studio-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 2,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: Vec::new(),
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: Vec::new(),
                variants: vec!["missing".to_string()],
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: Some(output_dir.path().to_path_buf()),
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("unknown variant should fail"),
            Err(err) => err,
        };

        assert!(err.message.contains("unknown trace variant 'missing'"));
        assert!(err
            .details
            .get("id")
            .and_then(|value| value.as_str())
            .expect("details id")
            .contains("fresh-install-mode"));
    });
}

#[test]
fn trace_run_expands_phase_chain_into_adjacent_and_total_spans() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("studio".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("studio-app-create-site".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("studio-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 1,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: Vec::new(),
                phases: vec![
                    extension_trace::spans::TracePhaseMilestone {
                        label: "boot".to_string(),
                        key: "runner.boot".to_string(),
                    },
                    extension_trace::spans::TracePhaseMilestone {
                        label: "ready".to_string(),
                        key: "runner.ready".to_string(),
                    },
                ],
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: Vec::new(),
                variants: Vec::new(),
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("phase trace should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Run(result) => {
                let results = result.results.expect("results");
                let span_ids = results
                    .span_results
                    .iter()
                    .map(|span| (span.id.as_str(), span.duration_ms))
                    .collect::<Vec<_>>();
                assert_eq!(
                    span_ids,
                    vec![
                        ("phase.boot_to_ready", Some(125)),
                        ("phase.total", Some(125))
                    ]
                );
            }
            _ => panic!("expected run output"),
        }
    });
}

#[test]
fn trace_run_expands_named_workload_phase_preset() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig_with_phase_preset(home, "preset-rig", "studio", component_dir.path());

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("studio".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("studio-app-create-site".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("preset-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 1,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: Vec::new(),
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: Some("startup".to_string()),
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: Vec::new(),
                variants: Vec::new(),
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("preset trace should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Run(result) => {
                let results = result.results.expect("results");
                let span_ids = results
                    .span_results
                    .iter()
                    .map(|span| (span.id.as_str(), span.duration_ms))
                    .collect::<Vec<_>>();
                assert_eq!(
                    span_ids,
                    vec![
                        ("phase.boot_to_ready", Some(125)),
                        ("phase.total", Some(125))
                    ]
                );
            }
            _ => panic!("expected run output"),
        }
    });
}

#[test]
fn trace_aggregate_spans_uses_workload_default_phase_preset() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig_with_phase_preset(home, "preset-rig", "studio", component_dir.path());

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("studio".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("studio-app-create-site".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("preset-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 2,
                aggregate: Some("spans".to_string()),
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: Vec::new(),
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: Vec::new(),
                variants: Vec::new(),
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("aggregate trace should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Aggregate(aggregate) => {
                let span_ids = aggregate
                    .spans
                    .iter()
                    .map(|span| span.id.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(span_ids, vec!["phase.boot_to_ready", "phase.total"]);
            }
            _ => panic!("expected aggregate output"),
        }
    });
}

#[test]
fn trace_repeat_counts_failed_runs_as_span_failures() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("studio".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("missing-scenario".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("studio-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 2,
                aggregate: Some("spans".to_string()),
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: vec![extension_trace::spans::parse_span_definition(
                    "boot_to_ready:runner.boot:runner.ready",
                )
                .expect("span")],
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: Vec::new(),
                variants: Vec::new(),
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("repeat aggregate should return failed output");

        assert_eq!(exit_code, 1);
        let TraceCommandOutput::Aggregate(aggregate) = output else {
            panic!("expected aggregate output");
        };
        assert_eq!(aggregate.run_count, 2);
        assert_eq!(aggregate.failure_count, 2);
        assert_eq!(aggregate.runs.len(), 2);
        assert!(aggregate.runs.iter().all(|run| !run.passed));
        let span = aggregate
            .spans
            .iter()
            .find(|span| span.id == "boot_to_ready")
            .expect("span aggregate");
        assert_eq!(span.n, 0);
        assert_eq!(span.failures, 2);
        assert!(span.samples.is_empty());
        assert_eq!(span.min_ms, None);
        assert_eq!(span.median_ms, None);
        assert_eq!(span.max_ms, None);
    });
}

#[test]
fn failed_trace_run_persists_observation_history() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let (_output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("studio".to_string()),
                    path: None,
                },
                component_arg: None,
                scenario: Some("missing-scenario".to_string()),
                scenario_arg: None,
                compare_after: None,
                baseline_target: None,
                candidate: None,
                rig: Some("studio-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 1,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                spans: Vec::new(),
                phases: Vec::new(),
                attachments: Vec::new(),
                phase_preset: None,
                baseline_args: BaselineArgs::default(),
                regression_threshold:
                    extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
                regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
                overlays: Vec::new(),
                variants: Vec::new(),
                matrix: TraceVariantMatrixMode::None,
                axes: Vec::new(),
                matrix_env: Vec::new(),
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
            },
            &GlobalArgs {},
        )
        .expect("trace command should return structured failure output");

        assert_eq!(exit_code, 3);
        let store = ObservationStore::open_initialized().expect("store");
        let runs = store
            .list_runs(homeboy::core::observation::RunListFilter {
                kind: Some("trace".to_string()),
                ..Default::default()
            })
            .expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "error");

        let trace_run = store
            .get_trace_run(&runs[0].id)
            .expect("trace run")
            .expect("trace run row");
        assert_eq!(trace_run.status, "error");
        assert!(trace_run.metadata_json["failure"]["stderr_excerpt"]
            .as_str()
            .expect("stderr excerpt")
            .contains("unknown scenario missing-scenario"));
    });
}
