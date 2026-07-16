//! Trace run tests for phase chain expansion, named phase presets, and phase
//! templates. Split out of the trace `tests` module to keep each test file
//! focused on a single concern and under the structural line threshold.

use std::fs;

use homeboy::core::ErrorCode;

use crate::test_support::with_isolated_home;

use super::test_fixture::{
    write_trace_extension, write_trace_rig, write_trace_rig_with_phase_preset,
    write_trace_rig_with_phase_template,
};
use super::*;

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
                secret_env: Vec::new(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 1,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                metric_guardrails: Vec::new(),
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
                visual_compare: false,
                visual_artifacts_dir: None,
                visual_compare_provider: None,
                visual_provider_args: Vec::new(),
                visual_threshold: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
                checkout_provenance: None,
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
                secret_env: Vec::new(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 1,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                metric_guardrails: Vec::new(),
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
                visual_compare: false,
                visual_artifacts_dir: None,
                visual_compare_provider: None,
                visual_provider_args: Vec::new(),
                visual_threshold: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
                checkout_provenance: None,
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
                secret_env: Vec::new(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 2,
                aggregate: Some("spans".to_string()),
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                metric_guardrails: Vec::new(),
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
                visual_compare: false,
                visual_artifacts_dir: None,
                visual_compare_provider: None,
                visual_provider_args: Vec::new(),
                visual_threshold: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
                checkout_provenance: None,
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
fn trace_run_expands_phase_template_defaults_and_metadata() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig_with_phase_template(home, "template-rig", "studio", component_dir.path());

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
                rig: Some("template-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                secret_env: Vec::new(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 1,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                metric_guardrails: Vec::new(),
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
                visual_compare: false,
                visual_artifacts_dir: None,
                visual_compare_provider: None,
                visual_provider_args: Vec::new(),
                visual_threshold: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
                checkout_provenance: None,
            },
            &GlobalArgs {},
        )
        .expect("template trace should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Run(result) => {
                let span_ids = result
                    .results
                    .as_ref()
                    .expect("results")
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
                let summary = result
                    .span_summaries
                    .iter()
                    .find(|span| span.id == "phase.boot_to_ready")
                    .expect("span summary");
                let metadata = summary.metadata.as_ref().expect("template metadata");
                assert!(metadata.critical);
                assert_eq!(metadata.category.as_deref(), Some("startup"));
            }
            _ => panic!("expected run output"),
        }
    });
}

#[test]
fn trace_run_rejects_unknown_phase_template_reference() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
        fs::create_dir_all(&rig_dir).expect("mkdir rigs");
        fs::write(
            rig_dir.join("bad-template-rig.json"),
            format!(
                r#"{{
                    "components": {{ "studio": {{ "path": "{}" }} }},
                    "trace_workloads": {{ "{}": [
                        {{
                            "path": "${{components.studio.path}}/studio-app-create-site.trace.mjs",
                            "trace_phase_template": "missing"
                        }}
                    ] }}
                }}"#,
                component_dir.path().display(),
                super::test_fixture::TRACE_FIXTURE_EXTENSION_ID
            ),
        )
        .expect("write rig");

        let error = match run(
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
                rig: Some("bad-template-rig".to_string()),
                profile: None,
                profiles: false,
                setting_args: SettingArgs::default(),
                secret_env: Vec::new(),
                json_summary: false,
                report: None,
                experiment: None,
                repeat: 1,
                aggregate: None,
                schedule: TraceSchedule::Grouped,
                focus_spans: Vec::new(),
                metric_guardrails: Vec::new(),
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
                visual_compare: false,
                visual_artifacts_dir: None,
                visual_compare_provider: None,
                visual_provider_args: Vec::new(),
                visual_threshold: None,
                keep_overlay: false,
                stale: false,
                force: false,
                canonical: false,
                allow_local_toolchain: true,
                checkout_provenance: None,
            },
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("unknown template should fail validation"),
            Err(error) => error,
        };

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert!(error
            .message
            .contains("unknown trace phase template 'missing'"));
    });
}
