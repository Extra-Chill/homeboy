use std::fs;

use crate::test_support::with_isolated_home;

use super::matrix::{expand_trace_matrix, matrix_cell_env, parse_trace_matrix_axis};
use super::test_fixture::{write_trace_extension, write_trace_rig, TRACE_FIXTURE_EXTENSION_ID};
use super::*;

fn write_matrix_sensitive_trace_extension(home: &tempfile::TempDir) {
    write_trace_extension(home);
    let script_path = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("extensions")
        .join(TRACE_FIXTURE_EXTENSION_ID)
        .join("trace-runner.sh");
    fs::write(
        &script_path,
        r#"#!/bin/sh
set -eu
if [ "$HOMEBOY_TRACE_LIST_ONLY" = "1" ]; then
  cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","scenarios":[{"id":"matrix-check","source":"fixture"}]}
JSON
  exit 0
fi
if [ "${HOMEBOY_TRACE_MATRIX_VIEWPORT:-}" = "mobile" ]; then
  status="fail"
else
  status="pass"
fi
mkdir -p "$HOMEBOY_TRACE_ARTIFACT_DIR"
printf 'viewport=%s\nlocation=%s\n' "${HOMEBOY_TRACE_MATRIX_VIEWPORT:-}" "${HOMEBOY_TRACE_MATRIX_ECE_LOCATIONS:-}" > "$HOMEBOY_TRACE_ARTIFACT_DIR/matrix-env.txt"
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","scenario_id":"$HOMEBOY_TRACE_SCENARIO","status":"$status","timeline":[{"t_ms":0,"source":"runner","event":"boot"},{"t_ms":25,"source":"runner","event":"ready"}],"span_results":[{"id":"boot_to_ready","from":"runner.boot","to":"runner.ready","status":"ok","duration_ms":25,"from_t_ms":0,"to_t_ms":25}],"assertions":[],"artifacts":[{"label":"matrix env","path":"artifacts/matrix-env.txt"}]}
JSON
"#,
    )
    .expect("write matrix trace script");

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

#[test]
fn trace_matrix_expands_axes_and_exports_env() {
    let axes = vec![
        parse_trace_matrix_axis("viewport=desktop,mobile").expect("viewport axis"),
        parse_trace_matrix_axis("ece_locations=product,none").expect("location axis"),
    ];

    let cells = expand_trace_matrix(&axes);

    assert_eq!(cells.len(), 4);
    assert_eq!(cells[0].label, "ece_locations-product__viewport-desktop");
    assert_eq!(cells[3].values.get("viewport"), Some(&"mobile".to_string()));
    assert_eq!(
        matrix_cell_env(&cells[0])
            .into_iter()
            .find(|(key, _)| key == "HOMEBOY_TRACE_MATRIX_ECE_LOCATIONS")
            .map(|(_, value)| value),
        Some("product".to_string())
    );
}

#[test]
fn trace_matrix_reports_failed_cells_and_cell_artifacts() {
    with_isolated_home(|home| {
        write_matrix_sensitive_trace_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_trace_rig(home, "studio-rig", "studio", component_dir.path());
        let output_dir = tempfile::TempDir::new().expect("matrix output dir");

        let ((output, _artifact_output), exit_code) = run_outputs(TraceArgs {
            comp: PositionalComponentArgs {
                component: Some("matrix".to_string()),
                path: None,
            },
            component_arg: None,
            scenario: Some("studio".to_string()),
            scenario_arg: None,
            compare_after: Some("matrix-check".into()),
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
            phases: Vec::new(),
            attachments: Vec::new(),
            phase_preset: None,
            baseline_args: BaselineArgs::default(),
            regression_threshold: extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
            regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
            overlays: Vec::new(),
            variants: Vec::new(),
            matrix: TraceVariantMatrixMode::None,
            axes: vec![
                parse_trace_matrix_axis("viewport=desktop,mobile").expect("viewport axis"),
                parse_trace_matrix_axis("ece_locations=product").expect("location axis"),
            ],
            matrix_env: Vec::new(),
            output_dir: Some(output_dir.path().to_path_buf()),
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
        })
        .expect("matrix run");

        assert_eq!(exit_code, 1);
        let TraceCommandOutput::ScenarioMatrix(matrix) = output else {
            panic!("expected scenario matrix output");
        };
        assert_eq!(matrix.command, "trace.matrix");
        assert_eq!(matrix.cell_count, 2);
        assert_eq!(matrix.failure_count, 1);
        assert!(std::path::Path::new(&matrix.matrix_path).exists());
        assert!(std::path::Path::new(&matrix.summary_path).exists());
        let failed = matrix
            .cells
            .iter()
            .find(|cell| !cell.passed)
            .expect("failed mobile cell");
        assert_eq!(failed.axes.get("viewport"), Some(&"mobile".to_string()));
        assert_eq!(failed.status, "fail");
        assert_eq!(failed.exit_code, 1);
        assert!(std::path::Path::new(&failed.output_path).exists());
        assert!(std::path::Path::new(&failed.artifact_path).exists());
        assert!(std::path::Path::new(&failed.artifact_dir)
            .join("matrix-env.txt")
            .exists());

        let summary = fs::read_to_string(matrix.summary_path).expect("summary");
        assert!(summary.contains("| `ece_locations-product__viewport-mobile` |"));
        assert!(summary.contains("`fail`"));
    });
}
