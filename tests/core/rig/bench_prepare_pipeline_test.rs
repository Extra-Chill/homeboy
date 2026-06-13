use super::*;
use crate::test_support::with_isolated_home;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

fn write_logging_bench_extension(home: &TempDir, log_path: &std::path::Path) {
    let extension_dir = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("extensions")
        .join("fixture-bench");
    fs::create_dir_all(&extension_dir).expect("mkdir extension");
    fs::write(
        extension_dir.join("fixture-bench.json"),
        r#"{"name":"Fixture Bench","version":"0.0.0","bench":{"extension_script":"bench-runner.sh"}}"#,
    )
    .expect("write extension manifest");

    let script_path = extension_dir.join("bench-runner.sh");
    fs::write(
        &script_path,
        format!(
            r#"#!/bin/sh
if [ "$HOMEBOY_BENCH_LIST_ONLY" != "1" ]; then
  printf 'workload
' >> '{}'
fi
cat > "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
{{"component_id":"$HOMEBOY_COMPONENT_ID","iterations":${{HOMEBOY_BENCH_ITERATIONS:-0}},"scenarios":[{{"id":"ordered","iterations":${{HOMEBOY_BENCH_ITERATIONS:-0}},"metrics":{{"p95_ms":1.0}}}}],"metric_policies":{{"p95_ms":{{"direction":"lower_is_better"}}}}}}
JSON
"#,
            log_path.display()
        ),
    )
    .expect("write bench script");

    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod script");
    }
}

fn write_pipeline_rig(
    home: &TempDir,
    rig_id: &str,
    component_path: &std::path::Path,
    log_path: &std::path::Path,
    bench_prepare_command: Option<String>,
) {
    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");

    let mut pipeline = serde_json::Map::new();
    if let Some(command) = bench_prepare_command {
        pipeline.insert(
            "bench_prepare".to_string(),
            serde_json::json!([{ "kind": "command", "label": "bench prep", "command": command }]),
        );
    }
    pipeline.insert(
        "check".to_string(),
        serde_json::json!([{
            "kind": "command",
            "label": "check rig",
            "command": format!("printf 'check\\n' >> '{}'", log_path.display())
        }]),
    );

    let spec = serde_json::json!({
        "components": {
            "studio": {
                "path": component_path,
                "extensions": { "fixture-bench": {} }
            }
        },
        "bench": { "default_component": "studio" },
        "pipeline": pipeline
    });
    fs::write(
        rig_dir.join(format!("{rig_id}.json")),
        serde_json::to_string(&spec).expect("serialize rig"),
    )
    .expect("write rig");
}

fn log_lines(log_path: &std::path::Path) -> Vec<String> {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn bench_prepare_runs_before_check_and_workload() {
    with_isolated_home(|home| {
        let component = tempfile::TempDir::new().expect("component");
        let log_dir = tempfile::TempDir::new().expect("log dir");
        let log_path = log_dir.path().join("order.log");
        write_logging_bench_extension(home, &log_path);
        write_pipeline_rig(
            home,
            "bench-prep-rig",
            component.path(),
            &log_path,
            Some(format!("printf 'prepare\\n' >> '{}'", log_path.display())),
        );

        let (_output, exit_code) = run(
            run_args(None, vec!["bench-prep-rig".to_string()], Vec::new()),
            &GlobalArgs {},
        )
        .expect("bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(log_lines(&log_path), vec!["prepare", "check", "workload"]);
    });
}

#[test]
fn rig_without_bench_prepare_preserves_existing_check_then_workload_flow() {
    with_isolated_home(|home| {
        let component = tempfile::TempDir::new().expect("component");
        let log_dir = tempfile::TempDir::new().expect("log dir");
        let log_path = log_dir.path().join("order.log");
        write_logging_bench_extension(home, &log_path);
        write_pipeline_rig(home, "plain-rig", component.path(), &log_path, None);

        let (_output, exit_code) = run(
            run_args(None, vec!["plain-rig".to_string()], Vec::new()),
            &GlobalArgs {},
        )
        .expect("bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(log_lines(&log_path), vec!["check", "workload"]);
    });
}

#[test]
fn failing_bench_prepare_aborts_before_check_and_workload() {
    with_isolated_home(|home| {
        let component = tempfile::TempDir::new().expect("component");
        let log_dir = tempfile::TempDir::new().expect("log dir");
        let log_path = log_dir.path().join("order.log");
        write_logging_bench_extension(home, &log_path);
        write_pipeline_rig(
            home,
            "failing-prep-rig",
            component.path(),
            &log_path,
            Some(format!(
                "printf 'prepare\\n' >> '{}'; exit 7",
                log_path.display()
            )),
        );

        let error = match run(
            run_args(None, vec!["failing-prep-rig".to_string()], Vec::new()),
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("bench should fail before workload"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("bench_prepare"));
        assert_eq!(log_lines(&log_path), vec!["prepare"]);
    });
}

#[test]
fn bench_list_rig_discovers_scenarios_without_preflight_or_bench_prepare() {
    with_isolated_home(|home| {
        let component = tempfile::TempDir::new().expect("component");
        let log_dir = tempfile::TempDir::new().expect("log dir");
        let log_path = log_dir.path().join("order.log");
        write_logging_bench_extension(home, &log_path);
        write_pipeline_rig(
            home,
            "list-rig",
            component.path(),
            &log_path,
            Some(format!("printf 'prepare\\n' >> '{}'", log_path.display())),
        );

        let mut args = run_args(None, Vec::new(), Vec::new());
        args.command = Some(BenchCommand::List(list_args(
            None,
            vec!["list-rig".to_string()],
        )));
        let (output, exit_code) = run(args, &GlobalArgs {}).expect("bench list should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::List(result) => {
                assert_eq!(result.count, 1);
                assert_eq!(result.scenarios[0].id, "ordered");
            }
            _ => panic!("expected bench list output"),
        }
        assert_eq!(log_lines(&log_path), Vec::<String>::new());
    });
}
