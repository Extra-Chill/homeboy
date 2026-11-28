use super::*;
use crate::test_support::{with_isolated_home, EnvVarGuard};
use homeboy::core::observation::{ObservationStore, RunListFilter};
use homeboy::rig;
use std::fs;
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
        r#"{"name":"Fixture Bench","version":"0.0.0","structured_sidecars":{"bench.results":true},"bench":{"extension_script":"bench-runner.sh"}}"#,
    )
    .expect("write extension manifest");

    let script_path = extension_dir.join("bench-runner.sh");
    homeboy_test_support::write_executable(&script_path, format!(
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
        ), 0o755)
    .expect("write bench script");
}

fn write_pipeline_rig(
    home: &TempDir,
    rig_id: &str,
    component_path: &std::path::Path,
    log_path: &std::path::Path,
    bench_prepare_command: Option<String>,
    materialization: Option<serde_json::Value>,
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

    let mut spec = serde_json::json!({
        "components": {
            "studio": {
                "path": component_path,
                "extensions": { "fixture-bench": {} }
            }
        },
        "bench": { "default_component": "studio" },
        "pipeline": pipeline
    });
    if let Some(step) = materialization {
        spec["requirements"]["dependency_materialization"] = serde_json::json!([step]);
    }
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
fn bench_prepare_runs_before_check_and_workload_when_declared() {
    for (rig_id, with_prepare, expected) in [
        ("bench-prep-rig", true, vec!["prepare", "check", "workload"]),
        ("plain-rig", false, vec!["check", "workload"]),
    ] {
        with_isolated_home(|home| {
            let component = tempfile::TempDir::new().expect("component");
            let log_dir = tempfile::TempDir::new().expect("log dir");
            let log_path = log_dir.path().join("order.log");
            write_logging_bench_extension(home, &log_path);
            write_pipeline_rig(
                home,
                rig_id,
                component.path(),
                &log_path,
                with_prepare.then(|| format!("printf 'prepare\\n' >> '{}'", log_path.display())),
                None,
            );
            if !with_prepare {
                let spec = rig::load(&home.path().join(".config/homeboy"), rig_id)
                    .expect("load plain rig");
                assert!(rig::run_bench_prepare(&spec, &[])
                    .expect("prepare")
                    .is_none());
            }

            let (_output, exit_code) = run(run_args(None, vec![rig_id.to_string()], Vec::new()))
                .expect("bench should run");

            assert_eq!(exit_code, 0, "{rig_id}");
            assert_eq!(log_lines(&log_path), expected, "{rig_id}");
        });
    }
}

fn bench_materialization(command: String) -> serde_json::Value {
    serde_json::json!({
        "id": "install-fixture",
        "component": "studio",
        "command": command,
        "expected_outputs": [
            {"path": "ready.marker", "kind": "file"},
            {"path": "deps", "kind": "dir"}
        ],
        "safety": "writes_working_tree"
    })
}

#[test]
fn bench_materialization_precedes_prepare_and_reuses_outputs_locally_and_in_lab() {
    for (rig_id, lab) in [("local-bench", false), ("lab-bench", true)] {
        with_isolated_home(|home| {
            let declared = tempfile::tempdir().expect("declared component");
            let runner = tempfile::tempdir().expect("materialized component");
            let effective = if lab { runner.path() } else { declared.path() };
            let log_path = home.path().join("order.log");
            let override_name = rig::expand::rig_component_path_override_env_name(rig_id, "studio");
            let _runner_override = lab.then(|| EnvVarGuard::set(&override_name, runner.path()));
            if lab {
                // Existing controller outputs must not cause a skip in Lab.
                fs::write(declared.path().join("ready.marker"), "controller").unwrap();
                fs::create_dir(declared.path().join("deps")).unwrap();
            }
            write_logging_bench_extension(home, &log_path);
            write_pipeline_rig(
                home,
                rig_id,
                declared.path(),
                &log_path,
                Some(format!(
                    "test -f '${{components.studio.path}}/ready.marker' && printf 'prepare\\n' >> '{}'",
                    log_path.display()
                )),
                Some(bench_materialization(format!(
                    "printf 'install\\n' >> '{}' && mkdir -p deps && touch ready.marker",
                    log_path.display()
                ))),
            );

            // One present output is insufficient to skip the step.
            fs::write(effective.join("ready.marker"), "partial").unwrap();
            for _ in 0..2 {
                let (_, exit_code) = run(run_args(None, vec![rig_id.to_string()], Vec::new()))
                    .expect("bench should run");
                assert_eq!(exit_code, 0, "{rig_id}");
            }
            assert!(effective.join("deps").is_dir());
            assert_eq!(
                log_lines(&log_path),
                ["install", "prepare", "check", "workload", "prepare", "check", "workload"],
                "{rig_id}"
            );
            let runs = ObservationStore::open_initialized()
                .expect("store")
                .list_runs(RunListFilter {
                    rig_id: Some(rig_id.to_string()),
                    ..Default::default()
                })
                .expect("runs");
            let prepare_runs: Vec<_> = runs
                .iter()
                .filter(|run| run.command.as_deref() == Some("rig.bench_prepare"))
                .collect();
            assert_eq!(prepare_runs.len(), 2);
            for run in prepare_runs {
                let steps = &run.metadata_json["pipeline"]["steps"];
                assert_eq!(steps[0]["kind"], "dependency_materialization");
                assert_eq!(steps[0]["status"], "pass");
                assert_eq!(steps[1]["label"], "bench prep");
            }
        });
    }
}

#[test]
fn bench_runs_materialization_without_a_prepare_pipeline_or_output_cache_contract() {
    with_isolated_home(|home| {
        let component = tempfile::tempdir().expect("component");
        let log_path = home.path().join("order.log");
        write_logging_bench_extension(home, &log_path);
        let mut step =
            bench_materialization(format!("printf 'install\\n' >> '{}'", log_path.display()));
        step["expected_outputs"] = serde_json::json!([]);
        let rig_id = "materialization-only-bench";
        write_pipeline_rig(home, rig_id, component.path(), &log_path, None, Some(step));

        let (_, exit_code) =
            run(run_args(None, vec![rig_id.into()], Vec::new())).expect("bench should run");
        assert_eq!(exit_code, 0);
        assert_eq!(log_lines(&log_path), ["install", "check", "workload"]);
    });
}

#[test]
fn missing_bench_materialization_output_blocks_prepare_and_workload() {
    with_isolated_home(|home| {
        let component = tempfile::tempdir().expect("component");
        let log_path = home.path().join("order.log");
        write_logging_bench_extension(home, &log_path);
        write_pipeline_rig(
            home,
            "missing-output-bench",
            component.path(),
            &log_path,
            Some(format!("printf 'prepare\\n' >> '{}'", log_path.display())),
            Some(bench_materialization(format!(
                "printf 'install\\n' >> '{}' && mkdir -p deps",
                log_path.display()
            ))),
        );

        let error = match run(run_args(
            None,
            vec!["missing-output-bench".into()],
            Vec::new(),
        )) {
            Ok(_) => panic!("bench should fail before workload"),
            Err(error) => error,
        };
        assert_eq!(error.details["classification"], "workspace_setup");
        let step = &error.details["pipeline"]["steps"][0];
        assert_eq!(step["kind"], "dependency_materialization");
        assert_eq!(step["status"], "fail");
        let message = step["error"].as_str().expect("step error");
        assert!(message.contains("install-fixture") && message.contains("ready.marker"));
        assert!(!error.hints.is_empty());
        assert_eq!(log_lines(&log_path), ["install"]);
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
            None,
        );

        let error = match run(run_args(
            None,
            vec!["failing-prep-rig".to_string()],
            Vec::new(),
        )) {
            Ok(_) => panic!("bench should fail before workload"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("bench_prepare"));
        assert!(error.to_string().contains("bench prep"));
        assert!(error.to_string().contains("exit 7"));
        assert_eq!(log_lines(&log_path), vec!["prepare"]);
    });
}

#[test]
fn failing_bench_prepare_requirement_reports_context() {
    with_isolated_home(|home| {
        let component = tempfile::TempDir::new().expect("component");
        let log_dir = tempfile::TempDir::new().expect("log dir");
        let log_path = log_dir.path().join("order.log");
        let missing_marker = component.path().join("ready.marker");
        write_logging_bench_extension(home, &log_path);

        let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
        fs::create_dir_all(&rig_dir).expect("mkdir rigs");
        let spec = serde_json::json!({
            "components": {
                "studio": {
                    "path": component.path(),
                    "extensions": { "fixture-bench": {} }
                }
            },
            "bench": { "default_component": "studio" },
            "pipeline": {
                "bench_prepare": [{
                    "kind": "requirement",
                    "label": "prepare fixture marker",
                    "file": missing_marker,
                    "prepare_command": "printf 'prepare-command\\n'; exit 9",
                    "prepare_phases": ["bench_prepare"],
                    "remediation": "create the fixture marker before benchmarking"
                }]
            }
        });
        fs::write(
            rig_dir.join("failing-requirement-rig.json"),
            serde_json::to_string(&spec).expect("serialize rig"),
        )
        .expect("write rig");

        let error = match run(run_args(
            None,
            vec!["failing-requirement-rig".to_string()],
            Vec::new(),
        )) {
            Ok(_) => panic!("bench should fail before workload"),
            Err(error) => error.to_string(),
        };

        assert!(error.contains("prepare fixture marker"), "error: {error}");
        assert!(
            error.contains(missing_marker.to_str().unwrap()),
            "error: {error}"
        );
        assert!(error.contains("printf 'prepare-command"), "error: {error}");
        assert!(
            error.contains("create the fixture marker before benchmarking"),
            "error: {error}"
        );
        assert_eq!(log_lines(&log_path), Vec::<String>::new());
    });
}

#[test]
fn bench_prepare_materializes_check_requirement_prepare_commands_before_check() {
    with_isolated_home(|home| {
        let component = tempfile::TempDir::new().expect("component");
        let log_dir = tempfile::TempDir::new().expect("log dir");
        let log_path = log_dir.path().join("order.log");
        let marker = component.path().join("ready.marker");
        write_logging_bench_extension(home, &log_path);

        let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
        fs::create_dir_all(&rig_dir).expect("mkdir rigs");
        let spec = serde_json::json!({
            "components": {
                "studio": {
                    "path": component.path(),
                    "extensions": { "fixture-bench": {} }
                }
            },
            "bench": { "default_component": "studio" },
            "pipeline": {
                "check": [{
                    "kind": "requirement",
                    "label": "fixture marker",
                    "file": marker,
                    "prepare_command": format!(
                        "printf 'prepare\\n' >> '{}'; printf ready > '{}'",
                        log_path.display(),
                        marker.display()
                    ),
                    "prepare_phases": ["up", "bench_prepare"],
                    "remediation": "prepare the fixture marker before benchmarking"
                }]
            }
        });
        fs::write(
            rig_dir.join("check-requirement-prepare-rig.json"),
            serde_json::to_string(&spec).expect("serialize rig"),
        )
        .expect("write rig");

        let (_output, exit_code) = run(run_args(
            None,
            vec!["check-requirement-prepare-rig".to_string()],
            Vec::new(),
        ))
        .expect("bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(log_lines(&log_path), vec!["prepare", "workload"]);
        assert_eq!(fs::read_to_string(marker).expect("marker"), "ready");
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
            Some(bench_materialization(format!(
                "printf 'install\\n' >> '{}'",
                log_path.display()
            ))),
        );

        let mut args = run_args(None, Vec::new(), Vec::new());
        args.command = Some(BenchCommand::List(list_args(
            None,
            vec!["list-rig".to_string()],
        )));
        let (output, exit_code) = run(args).expect("bench list should run");

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
