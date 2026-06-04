use std::fs;

use crate::commands::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs};
use crate::commands::GlobalArgs;
use crate::test_support::with_isolated_home;

use super::test_fixture::TRACE_FIXTURE_EXTENSION_ID;

use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::TraceCommandOutput;

use super::test_fixture::{init_overlay_component, write_trace_extension};
use super::{run, TraceArgs, TraceSchedule, TraceVariantMatrixMode};

#[test]
fn trace_compare_variant_interleaves_run_order_and_reports_focus_spans() {
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
        super::test_fixture::write_trace_rig(home, "studio-rig", "studio", component_dir.path());
        let output_dir = tempfile::TempDir::new().expect("output dir");

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("compare-variant".to_string()),
                    path: None,
                },
                component_arg: Some("studio".to_string()),
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
        .expect("interleaved compare-variant should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Compare(compare) => {
                assert_eq!(compare.focus_span_ids, vec!["boot_to_ready"]);
                assert_eq!(compare.focus_spans.len(), 1);
                assert_eq!(compare.focus_status.as_deref(), Some("pass"));
            }
            _ => panic!("expected compare output"),
        }

        assert!(output_dir.path().join("baseline.json").is_file());
        assert!(output_dir.path().join("variant.json").is_file());
        assert!(output_dir.path().join("compare.json").is_file());
        assert!(output_dir.path().join("run-order.json").is_file());

        let run_order: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(output_dir.path().join("run-order.json")).expect("run order"),
        )
        .expect("run order json");
        let observed = run_order
            .as_array()
            .expect("run order array")
            .iter()
            .map(|entry| {
                (
                    entry["index"].as_u64().expect("index"),
                    entry["group"].as_str().expect("group").to_string(),
                    entry["iteration"].as_u64().expect("iteration"),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                (1, "baseline".to_string(), 1),
                (2, "variant".to_string(), 1),
                (3, "baseline".to_string(), 2),
                (4, "variant".to_string(), 2),
            ]
        );

        let baseline: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(output_dir.path().join("baseline.json")).expect("baseline"),
        )
        .expect("baseline json");
        let variant: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(output_dir.path().join("variant.json")).expect("variant"),
        )
        .expect("variant json");
        assert_eq!(baseline["schedule"], "interleaved");
        assert_eq!(variant["schedule"], "interleaved");
        assert_eq!(baseline["run_order"][0]["index"], 1);
        assert_eq!(baseline["run_order"][1]["index"], 3);
        assert_eq!(variant["run_order"][0]["index"], 2);
        assert_eq!(variant["run_order"][1]["index"], 4);
        assert!(baseline["runs"]
            .as_array()
            .expect("baseline runs")
            .iter()
            .all(|run| std::path::Path::new(run["artifact_path"].as_str().unwrap()).is_file()));

        let summary = fs::read_to_string(output_dir.path().join("summary.md")).expect("summary");
        assert!(summary.contains("## Run Order"));
        assert!(summary.contains("| 2 | `variant` | 1 |"));
        assert!(summary.contains("## Focus Spans"));
        assert!(summary.contains("`boot_to_ready`"));
    });
}

#[test]
fn trace_compare_variant_uses_component_arg_for_multi_component_named_variants() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let studio_dir = tempfile::TempDir::new().expect("studio dir");
        init_overlay_component(studio_dir.path());
        let wordpress_dir = tempfile::TempDir::new().expect("wordpress dir");
        let patch_path = studio_dir.path().join("fresh-install-mode.patch");
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
        write_multi_component_variant_rig(
            home,
            studio_dir.path(),
            wordpress_dir.path(),
            &patch_path,
        );
        let output_dir = tempfile::TempDir::new().expect("output dir");

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("compare-variant".to_string()),
                    path: None,
                },
                component_arg: Some("studio".to_string()),
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
                variants: vec!["fresh-install-mode".to_string()],
                matrix: TraceVariantMatrixMode::None,
                output_dir: Some(output_dir.path().to_path_buf()),
                keep_overlay: false,
                stale: false,
                force: false,
            },
            &GlobalArgs {},
        )
        .expect("multi-component named variant compare-variant should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Compare(compare) => assert_eq!(compare.span_count, 1),
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
        assert_eq!(baseline["component"], "studio");
        assert!(baseline
            .get("overlays")
            .and_then(|overlays| overlays.as_array())
            .map(|overlays| overlays.is_empty())
            .unwrap_or(true));
        assert_eq!(variant["component"], "studio");
        assert_eq!(variant["overlays"][0]["variant"], "fresh-install-mode");
        assert_eq!(
            variant["overlays"][0]["path"],
            patch_path.to_string_lossy().as_ref()
        );
    });
}

#[test]
fn trace_compare_variant_reports_unknown_named_variant_for_component_arg() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let studio_dir = tempfile::TempDir::new().expect("studio dir");
        init_overlay_component(studio_dir.path());
        let wordpress_dir = tempfile::TempDir::new().expect("wordpress dir");
        let patch_path = studio_dir.path().join("fresh-install-mode.patch");
        fs::write(&patch_path, "").expect("write patch");
        write_multi_component_variant_rig(
            home,
            studio_dir.path(),
            wordpress_dir.path(),
            &patch_path,
        );
        let output_dir = tempfile::TempDir::new().expect("output dir");

        let err = match run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("compare-variant".to_string()),
                    path: None,
                },
                component_arg: Some("studio".to_string()),
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
        assert!(err.message.contains("component 'studio'"));
        assert!(!err.message.contains("multiple components"));
        assert!(err
            .details
            .get("id")
            .and_then(|value| value.as_str())
            .expect("details id")
            .contains("fresh-install-mode"));
    });
}

fn write_multi_component_variant_rig(
    home: &tempfile::TempDir,
    studio_path: &std::path::Path,
    wordpress_path: &std::path::Path,
    overlay_path: &std::path::Path,
) {
    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");
    fs::write(
        rig_dir.join("studio-rig.json"),
        format!(
            r#"{{
                "components": {{
                    "studio": {{ "path": "{}" }},
                    "wordpress": {{ "path": "{}" }}
                }},
                "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                    {{ "path": "${{components.studio.path}}/studio-app-create-site.trace.mjs" }}
                ] }},
                "trace_variants": {{
                    "fresh-install-mode": {{
                        "component": "studio",
                        "overlay": "{}"
                    }}
                }}
            }}"#,
            studio_path.display(),
            wordpress_path.display(),
            overlay_path.display()
        ),
    )
    .expect("write rig");
}
