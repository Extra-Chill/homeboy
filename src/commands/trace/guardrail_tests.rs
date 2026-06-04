use std::fs;

use crate::test_support::with_isolated_home;

use super::test_fixture::{write_trace_extension, write_trace_rig};
use super::*;

fn trace_args_for_rig(rig_id: &str) -> TraceArgs {
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
        rig: Some(rig_id.to_string()),
        profile: None,
        profiles: false,
        setting_args: SettingArgs::default(),
        json_summary: false,
        report: None,
        experiment: None,
        repeat: 1,
        aggregate: Some("spans".to_string()),
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
    }
}

#[test]
fn trace_aggregate_runs_passing_guardrails() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let rig_path = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("rigs")
            .join("studio-rig.json");
        let mut rig_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&rig_path).expect("read rig"))
                .expect("parse rig");
        rig_json["trace_guardrails"] = serde_json::json!([
            { "label": "behavior smoke", "command": "true" }
        ]);
        fs::write(
            &rig_path,
            serde_json::to_string_pretty(&rig_json).expect("serialize rig"),
        )
        .expect("write rig");

        let (output, exit_code) =
            run_repeat(trace_args_for_rig("studio-rig")).expect("trace aggregate should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Aggregate(aggregate) => {
                assert!(aggregate.passed);
                assert_eq!(aggregate.guardrail_failure_count, 0);
                assert_eq!(aggregate.guardrails.len(), 1);
                assert_eq!(aggregate.guardrails[0].label, "behavior smoke");
                assert!(aggregate.guardrails[0].passed);
            }
            _ => panic!("expected aggregate output"),
        }
    });
}

#[test]
fn trace_aggregate_fails_guardrails_without_losing_timing_artifacts() {
    with_isolated_home(|home| {
        write_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());

        let rig_path = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("rigs")
            .join("studio-rig.json");
        let mut rig_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&rig_path).expect("read rig"))
                .expect("parse rig");
        rig_json["trace_guardrails"] = serde_json::json!([
            { "label": "behavior smoke", "command": "sh -c 'printf regression >&2; exit 7'" }
        ]);
        fs::write(
            &rig_path,
            serde_json::to_string_pretty(&rig_json).expect("serialize rig"),
        )
        .expect("write rig");

        let (output, exit_code) = run_repeat(trace_args_for_rig("studio-rig"))
            .expect("trace aggregate should preserve artifacts");

        assert_eq!(exit_code, 1);
        match output {
            TraceCommandOutput::Aggregate(aggregate) => {
                assert!(!aggregate.passed);
                assert_eq!(aggregate.status, "fail");
                assert_eq!(aggregate.failure_count, 0);
                assert_eq!(aggregate.guardrail_failure_count, 1);
                assert_eq!(aggregate.runs.len(), 1);
                assert_eq!(aggregate.runs[0].status, "pass");
                assert!(!aggregate.runs[0].artifact_path.is_empty());
                assert_eq!(aggregate.spans[0].id, "boot_to_ready");
                assert_eq!(aggregate.spans[0].median_ms, Some(125));
                assert_eq!(aggregate.guardrails[0].status, "fail");
                assert!(aggregate.guardrails[0]
                    .failure
                    .as_deref()
                    .unwrap_or_default()
                    .contains("regression"));

                let artifact = serde_json::to_value(&aggregate).expect("serialize aggregate");
                assert_eq!(artifact["guardrail_failure_count"], 1);
                assert_eq!(artifact["guardrails"][0]["label"], "behavior smoke");
                assert_eq!(artifact["runs"][0]["status"], "pass");
            }
            _ => panic!("expected aggregate output"),
        }
    });
}
