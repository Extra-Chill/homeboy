use std::fs;

use crate::commands::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs};
use crate::test_support::with_isolated_home;

use homeboy::core::extension::trace as extension_trace;

use super::test_fixture::TRACE_FIXTURE_EXTENSION_ID;
use super::{execute_trace_run, TraceArgs, TraceSchedule, TraceVariantMatrixMode};

fn trace_args_for_rig(rig_id: &str, scenario: &str) -> TraceArgs {
    TraceArgs {
        comp: PositionalComponentArgs {
            component: None,
            path: None,
        },
        component_arg: None,
        scenario: Some(scenario.to_string()),
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
        metric_guardrails: Vec::new(),
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
        checkout_provenance: None,
    }
}

fn write_trace_experiment_extension(home: &tempfile::TempDir, fail: bool) {
    let extension_dir = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("extensions")
        .join(TRACE_FIXTURE_EXTENSION_ID);
    fs::create_dir_all(&extension_dir).expect("mkdir extension");
    fs::write(
        extension_dir.join(format!("{TRACE_FIXTURE_EXTENSION_ID}.json")),
        r#"{
                "name": "Fixture Trace",
                "version": "0.0.0",
                "trace": { "extension_script": "trace-runner.sh" }
            }"#,
    )
    .expect("write extension manifest");

    let exit_line = if fail { "exit 9" } else { "exit 0" };
    let trace_status = if fail { "error" } else { "pass" };
    let script_path = extension_dir.join("trace-runner.sh");
    fs::write(
        &script_path,
        format!(
            r#"#!/bin/sh
set -eu
if [ "$HOMEBOY_TRACE_LIST_ONLY" = "1" ]; then
  cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{{"component_id":"$HOMEBOY_COMPONENT_ID","scenarios":[{{"id":"product-workflow","source":"fixture"}}]}}
JSON
  exit 0
fi

test "$TRACE_PRODUCT_MODE" = "template"
test -f "$HOMEBOY_TRACE_COMPONENT_PATH/experiment-state.txt"
case "$HOMEBOY_SETTINGS_JSON" in *TEMPLATE_PATH*) ;; *) exit 8 ;; esac
printf '%s\n' "$HOMEBOY_SETTINGS_JSON" > "$HOMEBOY_TRACE_ARTIFACT_DIR/settings.json"
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{{"component_id":"$HOMEBOY_COMPONENT_ID","scenario_id":"$HOMEBOY_TRACE_SCENARIO","status":"{trace_status}","timeline":[],"span_results":[],"assertions":[],"artifacts":[{{"label":"runner settings","path":"artifacts/settings.json"}}]}}
JSON
{exit_line}
"#
        ),
    )
    .expect("write trace script");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod script");
    }
}

fn write_trace_experiment_rig(home: &tempfile::TempDir, component_path: &std::path::Path) {
    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");
    fs::write(
        rig_dir.join("studio-rig.json"),
        format!(
            r#"{{
                "components": {{
                    "studio": {{ "path": "{}" }}
                }},
                "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                    {{ "path": "${{components.studio.path}}/product-workflow.trace.mjs" }}
                ] }},
                "trace_experiments": {{
                    "template": {{
                        "setup": [
                            {{ "command": "printf setup > experiment-state.txt", "cwd": "${{components.studio.path}}" }}
                        ],
                        "settings": {{
                            "TEMPLATE_PATH": "${{components.studio.path}}/experiment-state.txt"
                        }},
                        "env": {{
                            "TRACE_PRODUCT_MODE": "template"
                        }},
                        "artifacts": [
                            {{ "label": "setup state", "path": "${{components.studio.path}}/experiment-state.txt" }}
                        ],
                        "teardown": [
                            {{ "command": "printf teardown > teardown.txt && rm -f experiment-state.txt", "cwd": "${{components.studio.path}}" }}
                        ]
                    }}
                }}
            }}"#,
            component_path.display()
        ),
    )
    .expect("write rig");
}

#[test]
fn trace_experiment_runs_setup_settings_artifacts_and_teardown() {
    with_isolated_home(|home| {
        write_trace_experiment_extension(home, false);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_experiment_rig(home, component_dir.path());

        let mut args = trace_args_for_rig("studio-rig", "product-workflow");
        args.experiment = Some("template".to_string());
        let execution = execute_trace_run(args).expect("trace experiment should run");

        assert_eq!(execution.workflow.exit_code, 0);
        assert!(!component_dir.path().join("experiment-state.txt").exists());
        assert_eq!(
            fs::read_to_string(component_dir.path().join("teardown.txt")).expect("teardown marker"),
            "teardown"
        );
        let results = execution.workflow.results.expect("results");
        assert!(results.artifacts.iter().any(|artifact| {
            artifact.label == "setup state"
                && artifact.path == "artifacts/experiments/template/01-experiment-state.txt"
        }));
        assert_eq!(
            fs::read_to_string(
                execution
                    .run_dir
                    .path()
                    .join("artifacts/experiments/template/01-experiment-state.txt")
            )
            .expect("collected artifact"),
            "setup"
        );
        let settings_json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(execution.run_dir.path().join("artifacts/settings.json"))
                .expect("settings artifact"),
        )
        .expect("settings json");
        assert_eq!(
            settings_json["TEMPLATE_PATH"],
            serde_json::Value::String(
                component_dir
                    .path()
                    .join("experiment-state.txt")
                    .to_string_lossy()
                    .to_string()
            )
        );
    });
}

#[test]
fn trace_experiment_runs_teardown_when_trace_fails() {
    with_isolated_home(|home| {
        write_trace_experiment_extension(home, true);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_experiment_rig(home, component_dir.path());

        let mut args = trace_args_for_rig("studio-rig", "product-workflow");
        args.experiment = Some("template".to_string());
        let execution = execute_trace_run(args).expect("trace failure still returns workflow");

        assert_eq!(execution.workflow.exit_code, 9);
        assert!(execution.workflow.failure.is_some());
        assert!(!component_dir.path().join("experiment-state.txt").exists());
        assert_eq!(
            fs::read_to_string(component_dir.path().join("teardown.txt")).expect("teardown marker"),
            "teardown"
        );
    });
}
