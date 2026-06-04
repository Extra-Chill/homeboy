use std::fs;

use crate::test_support::with_isolated_home;

use super::*;

#[test]
fn generic_trace_runner_runs_shell_only_component_workload() {
    with_isolated_home(|_| {
        let component_dir = tempfile::TempDir::new().expect("component dir");
        let trace_dir = component_dir.path().join("traces");
        fs::create_dir_all(&trace_dir).expect("mkdir traces");
        fs::write(
            trace_dir.join("smoke.trace.sh"),
            r#"#!/bin/sh
set -eu
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","scenario_id":"$HOMEBOY_TRACE_SCENARIO","status":"pass","timeline":[{"t_ms":0,"source":"generic","event":"ran"}],"assertions":[{"id":"generic-shell","status":"pass","message":"generic trace ran"}],"artifacts":[]}
JSON
"#,
        )
        .expect("write trace workload");

        let (output, exit_code) = run(
            TraceArgs {
                comp: PositionalComponentArgs {
                    component: Some("shell-only".to_string()),
                    path: Some(component_dir.path().to_string_lossy().to_string()),
                },
                component_arg: None,
                scenario: Some("smoke".to_string()),
                scenario_arg: None,
                compare_after: None,
                rig: None,
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
            },
            &GlobalArgs {},
        )
        .expect("generic trace should run");

        assert_eq!(exit_code, 0);
        match output {
            TraceCommandOutput::Run(result) => {
                let results = result.results.expect("trace results");
                assert_eq!(results.component_id, "shell-only");
                assert_eq!(results.scenario_id, "smoke");
                assert_eq!(results.status.as_str(), "pass");
            }
            _ => panic!("expected trace run output"),
        }
    });
}
