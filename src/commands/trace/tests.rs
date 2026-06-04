use std::collections::HashMap;
use std::fs;

use crate::test_support::with_isolated_home;

use homeboy::core::component::ScopedExtensionConfig;
use homeboy::core::rig::ComponentSpec;

use super::aggregate_test_support::aggregate_samples;
use super::test_fixture::{
    init_overlay_component, write_trace_extension, write_trace_rig,
    write_trace_rig_with_phase_preset, write_trace_rig_with_span_metadata,
    write_trace_rig_with_variant, TRACE_FIXTURE_EXTENSION_ID,
};
use super::*;

fn trace_args_for_profile(profile: &str) -> TraceArgs {
    TraceArgs {
        comp: PositionalComponentArgs {
            component: None,
            path: None,
        },
        component_arg: None,
        scenario: None,
        scenario_arg: None,
        compare_after: None,
        rig: None,
        profile: Some(profile.to_string()),
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
        output_dir: None,
        keep_overlay: false,
        stale: false,
        force: false,
    }
}

fn write_trace_rig_with_profile(
    home: &tempfile::TempDir,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
) {
    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");
    fs::write(
        rig_dir.join(format!("{}.json", rig_id)),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{ "path": "{}" }}
                    }},
                    "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                        {{ "path": "${{components.{component_id}.path}}/close-window-running-site.trace.mjs" }}
                    ] }},
                    "trace_profiles": {{
                        "studio-window-close": {{
                            "component": "{component_id}",
                            "scenario": "close-window-running-site",
                            "settings": {{
                                "window_title": "Studio",
                                "retry_count": 2
                            }}
                        }}
                    }}
                }}"#,
            path.display()
        ),
    )
    .expect("write rig");
}

#[test]
fn rig_component_path_and_trace_env_are_threaded() {
    let component_dir = tempfile::TempDir::new().expect("component dir");
    let mut components = HashMap::new();
    let mut extensions = HashMap::new();
    extensions.insert(
        "trace-extension".to_string(),
        ScopedExtensionConfig::default(),
    );
    components.insert(
        "studio".to_string(),
        ComponentSpec {
            path: component_dir.path().to_string_lossy().to_string(),
            remote_url: Some("https://github.com/Automattic/studio".to_string()),
            triage_remote_url: None,
            stack: None,
            branch: None,
            extensions: Some(extensions),
        },
    );
    let spec = RigSpec {
        id: "studio-rig".to_string(),
        components,
        ..serde_json::from_str(r#"{"id":"studio-rig"}"#).unwrap()
    };

    let path = rig_component_path(&spec, "studio").expect("path resolves");
    assert_eq!(path, component_dir.path().to_string_lossy());
    let component = rig_component_for_trace(&spec, "studio").expect("component resolves");
    assert_eq!(component.id, "studio");
    assert_eq!(component.local_path, path);
    assert!(component.extensions.is_some());
}

#[test]
fn rig_component_for_trace_synthesizes_trace_workload_extensions() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
                "id": "studio",
                "components": {
                    "studio": { "path": "/tmp/studio" }
                },
                "trace_workloads": {
                    "trace-fixture": [{ "path": "/tmp/create-site.trace.mjs" }]
                }
            }"#,
    )
    .expect("parse rig spec");

    let component = rig_component_for_trace(&rig_spec, "studio").expect("component");

    assert!(component
        .extensions
        .as_ref()
        .expect("extensions")
        .contains_key(TRACE_FIXTURE_EXTENSION_ID));
}

#[test]
fn rig_trace_list_uses_rig_default_component_and_workloads() {
    let component_dir = tempfile::TempDir::new().expect("component dir");
    let rig_spec: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "studio-rig",
            "components": {{
                "studio": {{ "path": "{}" }}
            }},
            "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                {{ "path": "${{components.studio.path}}/studio-app-create-site.trace.mjs" }},
                {{ "path": "${{components.studio.path}}/studio-list-sites.trace.mjs" }}
            ] }}
        }}"#,
        component_dir.path().display()
    ))
    .expect("parse rig");

    let component_id = resolve_component_id(
        &PositionalComponentArgs {
            component: None,
            path: None,
        },
        Some(&rig_spec),
    )
    .expect("default component");
    let component = rig_component_for_trace(&rig_spec, &component_id).expect("component");
    let workloads = rig::workloads_for_extension(
        &rig_spec,
        rig::RigWorkloadKind::Trace,
        None,
        TRACE_FIXTURE_EXTENSION_ID,
    );
    let scenarios = workloads
        .iter()
        .map(|path| trace_workload_scenario_id(&path.to_string_lossy()))
        .collect::<Vec<_>>();

    assert_eq!(component_id, "studio");
    assert_eq!(component.id, "studio");
    assert_eq!(workloads.len(), 2);
    assert_eq!(scenarios[0], "studio-app-create-site");
    let expected_source = format!(
        "{}/studio-app-create-site.trace.mjs",
        component_dir.path().display()
    );
    assert_eq!(workloads[0].to_string_lossy(), expected_source);
}

#[test]
fn trace_profile_resolves_to_normal_trace_run() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig_with_profile(home, "studio-rig", "studio", component_dir.path());

        let ((output, _artifact_output), exit_code) =
            run_outputs(trace_args_for_profile("studio-window-close"))
                .expect("profile trace should run");

        assert_eq!(exit_code, 0);
        let TraceCommandOutput::Run(run) = output else {
            panic!("expected run output");
        };
        let profile = run.profile.expect("resolved profile metadata");
        assert_eq!(profile.id, "studio-window-close");
        assert_eq!(profile.rig_id.as_deref(), Some("studio-rig"));
        assert_eq!(profile.component, "studio");
        assert_eq!(profile.scenario, "close-window-running-site");
        assert_eq!(
            profile.settings.get("window_title"),
            Some(&serde_json::Value::String("Studio".to_string()))
        );
        assert_eq!(
            profile.settings.get("retry_count"),
            Some(&serde_json::json!(2))
        );
    });
}

#[test]
fn trace_profile_cli_fields_override_profile_fields() {
    with_isolated_home(|home| {
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig_with_profile(home, "studio-rig", "studio", component_dir.path());
        let mut args = trace_args_for_profile("studio-window-close");
        args.scenario = Some("close-window-retry".to_string());
        args.setting_args
            .setting
            .push(("window_title".to_string(), "Studio Dev".to_string()));

        resolve_trace_profile_args(&mut args).expect("profile resolves");

        assert_eq!(args.comp.component.as_deref(), Some("studio"));
        assert_eq!(args.scenario.as_deref(), Some("close-window-retry"));
        assert_eq!(args.rig.as_deref(), Some("studio-rig"));
        assert_eq!(
            args.setting_args.setting,
            vec![
                ("window_title".to_string(), "Studio".to_string()),
                ("window_title".to_string(), "Studio Dev".to_string())
            ]
        );
    });
}

#[test]
fn trace_list_profiles_lists_rig_profiles() {
    with_isolated_home(|home| {
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig_with_profile(home, "studio-rig", "studio", component_dir.path());

        let ((output, _artifact_output), exit_code) = run_outputs(TraceArgs {
            comp: PositionalComponentArgs {
                component: Some("list".to_string()),
                path: None,
            },
            component_arg: None,
            scenario: None,
            scenario_arg: None,
            compare_after: None,
            rig: None,
            profile: None,
            profiles: true,
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
            output_dir: None,
            keep_overlay: false,
            stale: false,
            force: false,
        })
        .expect("profile list should run");

        assert_eq!(exit_code, 0);
        let TraceCommandOutput::List(list) = output else {
            panic!("expected list output");
        };
        assert_eq!(list.command, "trace.list.profiles");
        assert_eq!(list.count, 1);
        assert_eq!(list.profiles[0].id, "studio-window-close");
        assert_eq!(list.profiles[0].rig_id, "studio-rig");
        assert_eq!(list.profiles[0].component.as_deref(), Some("studio"));
        assert_eq!(
            list.profiles[0].scenario.as_deref(),
            Some("close-window-running-site")
        );
    });
}

#[test]
fn rig_trace_list_uses_scoped_workload_preflight() {
    let spec: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "studio-rig",
            "components": {{
                "studio": {{ "path": "/tmp/studio" }}
            }},
            "pipeline": {{
                "check": [
                    {{
                        "kind": "check",
                        "label": "desktop app packaged",
                        "groups": ["desktop-app"],
                        "command": "true"
                    }},
                    {{
                        "kind": "check",
                        "label": "unrelated cli symlink",
                        "groups": ["cli-dev-copy"],
                        "command": "false"
                    }}
                ]
            }},
            "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                {{
                    "path": "${{components.studio.path}}/studio-app-create-site.trace.mjs",
                    "check_groups": ["desktop-app"]
                }}
            ] }}
        }}"#
    ))
    .expect("parse rig");

    run_rig_workload_preflight(
        &spec,
        Some(TRACE_FIXTURE_EXTENSION_ID),
        rig::RigWorkloadKind::Trace,
    )
    .expect("scoped workload preflight should bypass unrelated failed check");

    assert!(run_rig_workload_preflight(&spec, None, rig::RigWorkloadKind::Trace).is_err());
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
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
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
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
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
        assert_eq!(artifacts.len(), 2);
        assert!(artifacts
            .iter()
            .any(|artifact| artifact.kind == "trace-results"));
        assert!(artifacts
            .iter()
            .any(|artifact| artifact.kind == "trace-artifact"));
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
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
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
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
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
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
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
            output_dir: None,
            keep_overlay: false,
            stale: false,
            force: false,
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
                output_dir: Some(output_dir.path().to_path_buf()),
                keep_overlay: false,
                stale: false,
                force: false,
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
                output_dir: Some(output_dir.path().to_path_buf()),
                keep_overlay: false,
                stale: false,
                force: false,
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
                output_dir: Some(output_dir.path().to_path_buf()),
                keep_overlay: false,
                stale: false,
                force: false,
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
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
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
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
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
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
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
fn aggregate_span_reports_percentiles_when_sample_size_is_sufficient() {
    let span = aggregate_span(
        "boot_to_ready".to_string(),
        aggregate_samples(&[
            200, 10, 190, 20, 180, 30, 170, 40, 160, 50, 150, 60, 140, 70, 130, 80, 120, 90, 110,
            100,
        ]),
        2,
    );

    assert_eq!(span.n, 20);
    assert_eq!(span.min_ms, Some(10));
    assert_eq!(span.median_ms, Some(105));
    assert_eq!(span.avg_ms, Some(105.0));
    assert!(span
        .stddev_ms
        .is_some_and(|value| (value - 57.66281297335398).abs() < 0.000001));
    assert_eq!(span.p75_ms, Some(150));
    assert_eq!(span.p90_ms, Some(180));
    assert_eq!(span.p95_ms, Some(190));
    assert_eq!(span.max_ms, Some(200));
    assert_eq!(span.max_run_index, Some(1));
    assert_eq!(
        span.max_artifact_path.as_deref(),
        Some("/tmp/trace-run-1.json")
    );
    assert_eq!(span.failures, 2);
    assert_eq!(span.samples.len(), 20);
    assert_eq!(span.samples[0].run_index, 1);
    assert_eq!(span.samples[0].duration_ms, 200);
    assert_eq!(span.samples[0].artifact_path, "/tmp/trace-run-1.json");
}

#[test]
fn aggregate_span_reports_run_and_artifact_for_max_sample() {
    let span = aggregate_span(
        "submit_to_running".to_string(),
        aggregate_samples(&[340, 11_757, 410]),
        0,
    );

    assert_eq!(span.max_ms, Some(11_757));
    assert_eq!(span.max_run_index, Some(2));
    assert_eq!(
        span.max_artifact_path.as_deref(),
        Some("/tmp/trace-run-2.json")
    );
}

#[test]
fn aggregate_span_omits_percentiles_for_small_sample_sizes() {
    let single = aggregate_span("single".to_string(), aggregate_samples(&[42]), 0);
    assert_eq!(single.min_ms, Some(42));
    assert_eq!(single.median_ms, Some(42));
    assert_eq!(single.avg_ms, Some(42.0));
    assert_eq!(single.stddev_ms, Some(0.0));
    assert_eq!(single.p75_ms, None);
    assert_eq!(single.p90_ms, None);
    assert_eq!(single.p95_ms, None);
    assert_eq!(single.max_ms, Some(42));

    let four_samples = aggregate_span("four".to_string(), aggregate_samples(&[10, 20, 30, 40]), 0);
    assert_eq!(four_samples.p75_ms, Some(30));
    assert_eq!(four_samples.p90_ms, None);
    assert_eq!(four_samples.p95_ms, None);
}

#[test]
fn aggregate_markdown_includes_percentile_columns() {
    let aggregate = extension_trace::TraceAggregateOutput {
        command: "trace.aggregate.spans",
        passed: true,
        status: "pass".to_string(),
        component: "studio".to_string(),
        scenario_id: "create-site".to_string(),
        phase_preset: None,
        repeat: 20,
        run_count: 20,
        failure_count: 0,
        exit_code: 0,
        rig_state: None,
        schedule: None,
        run_order: Vec::new(),
        overlays: Vec::new(),
        runs: Vec::new(),
        spans: vec![aggregate_span(
            "boot_to_ready".to_string(),
            aggregate_samples(&((1..=20).map(|value| value * 10).collect::<Vec<_>>())),
            0,
        )],
        guardrails: Vec::new(),
        guardrail_failure_count: 0,
        focus_span_ids: Vec::new(),
        focus_spans: Vec::new(),
        classification_summaries: Vec::new(),
        unmatched_span_metadata_ids: Vec::new(),
        profile: None,
    };

    let markdown = render_aggregate_markdown(&aggregate);

    assert!(markdown
        .contains("| Span | n | min | median | avg | stddev | p75 | p90 | p95 | max | failures |"));
    assert!(markdown.contains(
        "| `boot_to_ready` | 20 | 10ms | 105ms | 105.0ms | 57.7ms | 150ms | 180ms | 190ms | 200ms | 0 |"
    ));
    assert!(markdown.contains("| Span | max | max run | max artifact |"));
    assert!(markdown.contains("| `boot_to_ready` | 200ms | 20 | `/tmp/trace-run-20.json` |"));
}

#[test]
fn aggregate_json_serializes_available_percentiles() {
    let span = aggregate_span(
        "boot_to_ready".to_string(),
        aggregate_samples(&((1..=20).map(|value| value * 10).collect::<Vec<_>>())),
        0,
    );

    let value = serde_json::to_value(&span).expect("span serializes");

    assert_eq!(value["p75_ms"], 150);
    assert_eq!(value["p90_ms"], 180);
    assert_eq!(value["p95_ms"], 190);
    assert!(value["stddev_ms"]
        .as_f64()
        .is_some_and(|stddev| stddev > 57.6));
    assert_eq!(value["samples"].as_array().expect("samples").len(), 20);
    assert_eq!(value["samples"][0]["run_index"], 1);
    assert_eq!(value["samples"][0]["duration_ms"], 10);
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
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
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
fn aggregate_json_omits_unavailable_percentiles() {
    let span = aggregate_span("boot_to_ready".to_string(), aggregate_samples(&[10, 20]), 0);

    let value = serde_json::to_value(&span).expect("span serializes");

    assert!(value.get("p75_ms").is_none());
    assert!(value.get("p90_ms").is_none());
    assert!(value.get("p95_ms").is_none());
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
                output_dir: None,
                keep_overlay: false,
                stale: false,
                force: false,
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
