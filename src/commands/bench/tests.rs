use super::*;
use crate::test_support::with_isolated_home;
use clap::Parser;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

/// Minimal CLI wrapper to exercise clap parsing of `BenchArgs`.
#[derive(Parser)]
struct TestCli {
    #[command(flatten)]
    bench: BenchArgs,
}

fn write_bench_extension(home: &TempDir) {
    let extension_dir = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("extensions")
        .join("nodejs");
    fs::create_dir_all(&extension_dir).expect("mkdir extension");
    fs::write(
        extension_dir.join("nodejs.json"),
        r#"{
                "name": "Node.js",
                "version": "0.0.0",
                "bench": { "extension_script": "bench-runner.sh" }
            }"#,
    )
    .expect("write extension manifest");

    let script_path = extension_dir.join("bench-runner.sh");
    fs::write(
            &script_path,
            r#"#!/bin/sh
if [ -n "$HOMEBOY_BENCH_EXTRA_WORKLOADS" ]; then
  all_scenarios=""
  old_ifs="$IFS"
  IFS=":"
  for workload in $HOMEBOY_BENCH_EXTRA_WORKLOADS; do
    name="$(basename "$workload")"
    name="${name%%.bench.*}"
    name="${name%.*}"
    if [ -n "$all_scenarios" ]; then
      all_scenarios="$all_scenarios $name"
    else
      all_scenarios="$name"
    fi
  done
  IFS="$old_ifs"
else
  all_scenarios="in-tree slow"
fi

# Rig-declared workload selection is owned by Homeboy core because the core
# process builds HOMEBOY_BENCH_EXTRA_WORKLOADS. This intentionally ignores
# HOMEBOY_BENCH_SCENARIOS when extra workloads are present, matching the real
# Node runner class that caused #1843.
if [ -n "$HOMEBOY_BENCH_EXTRA_WORKLOADS" ] || [ "$HOMEBOY_BENCH_LIST_ONLY" = "1" ] || [ -z "$HOMEBOY_BENCH_SCENARIOS" ]; then
  selected="$all_scenarios"
else
  selected=$(printf '%s' "$HOMEBOY_BENCH_SCENARIOS" | tr ',' ' ')
fi

cat > "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
{
  "component_id": "$HOMEBOY_COMPONENT_ID",
  "iterations": ${HOMEBOY_BENCH_ITERATIONS:-0},
  "scenarios": [
JSON

comma=""
for scenario in $selected; do
  cat >> "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
    $comma{ "id": "$scenario", "iterations": ${HOMEBOY_BENCH_ITERATIONS:-0}, "metrics": { "p95_ms": 1.0, "warmup_iterations": ${HOMEBOY_BENCH_WARMUP_ITERATIONS:--1} } }
JSON
  comma=",
"
done

cat >> "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
  ],
  "metric_policies": { "p95_ms": { "direction": "lower_is_better" } }
}
JSON
"#,
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

fn write_registered_component(home: &TempDir, component_id: &str, path: &std::path::Path) {
    let component_dir = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&component_dir).expect("mkdir components");
    fs::write(
        component_dir.join(format!("{}.json", component_id)),
        serde_json::json!({
            "id": component_id,
            "local_path": path,
            "extensions": { "nodejs": {} }
        })
        .to_string(),
    )
    .expect("write component");
}

fn write_rig(home: &TempDir, rig_id: &str, component_id: &str, path: &std::path::Path) {
    write_rig_with_profiles(home, rig_id, component_id, path, "{}");
}

fn write_rig_with_profiles(
    home: &TempDir,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
    bench_profiles: &str,
) {
    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");
    fs::write(
        rig_dir.join(format!("{}.json", rig_id)),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{
                            "path": "{}",
                            "extensions": {{ "nodejs": {{}} }}
                        }}
                    }},
                    "bench": {{ "default_component": "{component_id}" }},
                    "bench_workloads": {{ "nodejs": [
                        "${{components.{component_id}.path}}/rig-extra.bench.js",
                        "${{components.{component_id}.path}}/rig-slow.bench.mjs"
                    ] }},
                    "bench_profiles": {bench_profiles}
                }}"#,
            path.display()
        ),
    )
    .expect("write rig");
}

fn set_rig_warmup(home: &TempDir, rig_id: &str, warmup: u64) {
    let rig_path = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("rigs")
        .join(format!("{}.json", rig_id));
    let mut rig_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&rig_path).expect("read rig")).expect("parse rig");
    rig_json["bench"]["warmup_iterations"] = serde_json::json!(warmup);
    fs::write(
        &rig_path,
        serde_json::to_string(&rig_json).expect("serialize rig"),
    )
    .expect("write rig");
}

fn first_warmup_metric(output: BenchOutput) -> f64 {
    match output {
        BenchOutput::Single(result) => result.results.expect("results").scenarios[0]
            .metrics
            .get("warmup_iterations")
            .expect("warmup metric"),
        _ => panic!("expected single output"),
    }
}

fn list_args(component: Option<&str>, rig: Vec<String>) -> BenchListArgs {
    BenchListArgs {
        comp: PositionalComponentArgs {
            component: component.map(str::to_string),
            path: None,
        },
        rig,
        scenario_ids: Vec::new(),
        setting_args: SettingArgs::default(),
        args: Vec::new(),
    }
}

fn run_args(component: Option<&str>, rig: Vec<String>, scenario_ids: Vec<String>) -> BenchArgs {
    BenchArgs {
        command: None,
        run: BenchRunArgs {
            comp: PositionalComponentArgs {
                component: component.map(str::to_string),
                path: None,
            },
            iterations: 1,
            warmup: None,
            runs: 1,
            shared_state: None,
            concurrency: 1,
            baseline_args: BaselineArgs {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            regression_threshold: 5.0,
            setting_args: SettingArgs::default(),
            args: Vec::new(),
            _json: HiddenJsonArgs::default(),
            json_summary: false,
            rig,
            rig_order: BenchRigOrder::Input,
            scenario_ids,
            profile: None,
            ignore_default_baseline: false,
        },
    }
}

fn run_args_with_profile(component: Option<&str>, rig: Vec<String>, profile: &str) -> BenchArgs {
    let mut args = run_args(component, rig, Vec::new());
    args.run.profile = Some(profile.to_string());
    args
}

#[test]
fn filter_strips_boolean_flags() {
    let args = vec!["--ratchet".to_string(), "--filter=Scenario".to_string()];
    let result = filter_homeboy_flags(&args);
    assert_eq!(result, vec!["--filter=Scenario"]);
}

#[test]
fn parses_bench_list_rig_flag() {
    let cli = TestCli::try_parse_from(["bench", "list", "--rig", "studio-bfb"])
        .expect("bench list --rig should parse");

    match cli.bench.command.expect("list command") {
        BenchCommand::List(args) => assert_eq!(args.rig, vec!["studio-bfb".to_string()]),
    }
}

#[test]
fn parses_repeated_scenario_flags() {
    let cli = TestCli::try_parse_from([
        "bench",
        "homeboy",
        "--scenario",
        "studio-agent-runtime",
        "--scenario",
        "wp-admin-load",
    ])
    .expect("bench --scenario should parse");

    assert_eq!(
        cli.bench.run.scenario_ids,
        vec![
            "studio-agent-runtime".to_string(),
            "wp-admin-load".to_string()
        ]
    );
}

#[test]
fn parses_profile_flag() {
    let cli = TestCli::try_parse_from(["bench", "--rig", "studio-bfb", "--profile", "smoke"])
        .expect("bench --profile should parse");

    assert_eq!(cli.bench.run.profile.as_deref(), Some("smoke"));
}

#[test]
fn scenario_and_profile_conflict() {
    let err = match TestCli::try_parse_from([
        "bench",
        "--rig",
        "studio-bfb",
        "--profile",
        "smoke",
        "--scenario",
        "boot",
    ]) {
        Ok(_) => panic!("--scenario and --profile should conflict"),
        Err(err) => err,
    };

    assert!(err.to_string().contains("cannot be used with"));
}

#[test]
fn run_list_uses_rig_default_component_and_workloads() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig(home, "studio-bfb", "studio", component_dir.path());

        let (output, exit_code) = run_list(&list_args(None, vec!["studio-bfb".to_string()]))
            .expect("rig bench list should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::List(result) => {
                assert_eq!(result.component, "studio");
                assert_eq!(result.component_id, "studio");
                assert_eq!(result.count, 2);
                assert_eq!(result.scenarios[0].id, "rig-extra");
            }
            _ => panic!("expected list output"),
        }
    });
}

#[test]
fn run_list_preserves_registered_component_path() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let (output, exit_code) =
            run_list(&list_args(Some("studio"), Vec::new())).expect("plain bench list should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::List(result) => {
                assert_eq!(result.component, "studio");
                assert_eq!(result.component_id, "studio");
                assert_eq!(result.count, 2);
                assert_eq!(result.scenarios[0].id, "in-tree");
            }
            _ => panic!("expected list output"),
        }
    });
}

#[test]
fn run_list_filters_selected_scenario() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let mut args = list_args(Some("studio"), Vec::new());
        args.scenario_ids = vec!["slow".to_string()];
        let (output, exit_code) = run_list(&args).expect("plain bench list should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::List(result) => {
                assert_eq!(result.count, 1);
                assert_eq!(result.scenarios[0].id, "slow");
            }
            _ => panic!("expected list output"),
        }
    });
}

#[test]
fn run_selects_single_scenario() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(Some("studio"), Vec::new(), vec!["slow".to_string()]),
            &GlobalArgs {},
        )
        .expect("selected bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let scenarios = result.results.expect("results").scenarios;
                assert_eq!(scenarios.len(), 1);
                assert_eq!(scenarios[0].id, "slow");
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn parses_rig_run_options_without_component() {
    let cli = TestCli::try_parse_from([
        "bench",
        "--rig",
        "studio-agent-sdk,studio-agent-pi",
        "--scenario",
        "studio-agent-runtime",
        "--runs",
        "3",
        "--iterations",
        "1",
    ])
    .expect("rig bench options without component should parse");

    assert_eq!(
        cli.bench.run.rig,
        vec!["studio-agent-sdk", "studio-agent-pi"]
    );
    assert_eq!(cli.bench.run.scenario_ids, vec!["studio-agent-runtime"]);
    assert_eq!(cli.bench.run.runs, 3);
    assert_eq!(cli.bench.run.iterations, 1);
    assert!(cli.bench.run.comp.id().is_none());
}

#[test]
fn parses_rig_order_flag() {
    let cli = TestCli::try_parse_from([
        "bench",
        "--rig",
        "studio-agent-sdk,studio-agent-pi",
        "--rig-order",
        "reverse",
    ])
    .expect("bench --rig-order should parse");

    assert_eq!(cli.bench.run.rig_order, BenchRigOrder::Reverse);
}

#[test]
fn run_selects_multiple_scenarios() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(
                Some("studio"),
                Vec::new(),
                vec!["in-tree".to_string(), "slow".to_string()],
            ),
            &GlobalArgs {},
        )
        .expect("selected bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let scenario_ids: Vec<String> = result
                    .results
                    .expect("results")
                    .scenarios
                    .into_iter()
                    .map(|scenario| scenario.id)
                    .collect();
                assert_eq!(
                    scenario_ids,
                    vec!["in-tree".to_string(), "slow".to_string()]
                );
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn unknown_scenario_reports_discovered_ids() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let err = match run(
            run_args(Some("studio"), Vec::new(), vec!["missing".to_string()]),
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("unknown scenario should fail"),
            Err(err) => err,
        };
        let message = err.to_string();

        assert!(message.contains("missing"), "got: {}", message);
        assert!(message.contains("in-tree"), "got: {}", message);
        assert!(message.contains("slow"), "got: {}", message);
    });
}

#[test]
fn cross_rig_run_passes_selector_to_each_rig() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());

        let (output, exit_code) = run(
            run_args(
                None,
                vec!["rig-a".to_string(), "rig-b".to_string()],
                vec!["rig-slow".to_string()],
            ),
            &GlobalArgs {},
        )
        .expect("cross-rig selected bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                for rig in result.rigs {
                    let scenarios = rig.results.expect("rig results").scenarios;
                    assert_eq!(scenarios.len(), 1);
                    assert_eq!(scenarios[0].id, "rig-slow");
                }
            }
            _ => panic!("expected comparison output"),
        }
    });
}

#[test]
fn cross_rig_reverse_order_flips_reference_and_execution_order() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());

        let mut args = run_args(
            None,
            vec!["rig-a".to_string(), "rig-b".to_string()],
            vec!["rig-slow".to_string()],
        );
        args.run.rig_order = BenchRigOrder::Reverse;

        let (output, exit_code) = run(args, &GlobalArgs {})
            .expect("cross-rig selected bench should run in reverse order");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                assert_eq!(result.rigs[0].rig_id, "rig-b");
                assert_eq!(result.rigs[1].rig_id, "rig-a");
            }
            _ => panic!("expected comparison output"),
        }
    });
}

#[test]
fn single_rig_selector_filters_extra_workloads_before_execution() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig(home, "rig-a", "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(
                None,
                vec!["rig-a".to_string()],
                vec!["rig-slow".to_string()],
            ),
            &GlobalArgs {},
        )
        .expect("single-rig selected bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                assert_eq!(result.iterations, 1);
                let scenarios = result.results.expect("results").scenarios;
                assert_eq!(scenarios.len(), 1);
                assert_eq!(scenarios[0].id, "rig-slow");
                assert_eq!(scenarios[0].iterations, 1);
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn run_profile_selects_rig_profile_scenarios() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig_with_profiles(
            home,
            "studio-bfb",
            "studio",
            component_dir.path(),
            r#"{ "substrate": ["rig-extra"] }"#,
        );

        let (output, exit_code) = run(
            run_args_with_profile(None, vec!["studio-bfb".to_string()], "substrate"),
            &GlobalArgs {},
        )
        .expect("profile bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let scenarios = result.results.expect("results").scenarios;
                assert_eq!(scenarios.len(), 1);
                assert_eq!(scenarios[0].id, "rig-extra");
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn single_rig_runs_preserve_run_level_summaries_after_selection() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig(home, "rig-a", "studio", component_dir.path());
        let mut args = run_args(
            None,
            vec!["rig-a".to_string()],
            vec!["rig-slow".to_string()],
        );
        args.run.runs = 3;

        let (output, exit_code) =
            run(args, &GlobalArgs {}).expect("single-rig selected bench should run multiple runs");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Single(result) => {
                let scenarios = result.results.expect("results").scenarios;
                assert_eq!(scenarios.len(), 1);
                let runs = scenarios[0].runs.as_ref().expect("per-run metrics");
                assert_eq!(runs.len(), 3);
                assert!(scenarios[0].runs_summary.as_ref().is_some_and(|summary| {
                    summary.get("p95_ms").is_some_and(|metric| metric.n == 3)
                }));
            }
            _ => panic!("expected single output"),
        }
    });
}

#[test]
fn unknown_profile_lists_available_profiles() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig_with_profiles(
            home,
            "studio-bfb",
            "studio",
            component_dir.path(),
            r#"{ "substrate": ["rig-extra"], "smoke": ["rig-slow"] }"#,
        );

        let err = match run(
            run_args_with_profile(None, vec!["studio-bfb".to_string()], "agentic"),
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("unknown profile should fail"),
            Err(err) => err,
        };
        let message = err.to_string();

        assert!(message.contains("agentic"), "got: {}", message);
        assert!(message.contains("substrate"), "got: {}", message);
        assert!(message.contains("smoke"), "got: {}", message);
    });
}

#[test]
fn profile_with_unknown_scenario_lists_discovered_scenarios() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig_with_profiles(
            home,
            "studio-bfb",
            "studio",
            component_dir.path(),
            r#"{ "broken": ["missing-scenario"] }"#,
        );

        let err = match run(
            run_args_with_profile(None, vec!["studio-bfb".to_string()], "broken"),
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("unknown scenario in profile should fail"),
            Err(err) => err,
        };
        let message = err.to_string();

        assert!(message.contains("missing-scenario"), "got: {}", message);
        assert!(message.contains("rig-extra"), "got: {}", message);
        assert!(message.contains("rig-slow"), "got: {}", message);
    });
}

#[test]
fn cross_rig_runs_preserve_run_level_summaries_after_selection() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());
        let mut args = run_args(
            None,
            vec!["rig-a".to_string(), "rig-b".to_string()],
            vec!["rig-slow".to_string()],
        );
        args.run.runs = 3;

        let (output, exit_code) =
            run(args, &GlobalArgs {}).expect("cross-rig selected bench should run multiple runs");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                assert!(result
                    .summary
                    .iter()
                    .all(|summary| summary.rows.iter().all(|row| row.n == Some(3))));
                for rig in result.rigs {
                    let scenarios = rig.results.expect("rig results").scenarios;
                    assert_eq!(scenarios.len(), 1);
                    assert_eq!(scenarios[0].id, "rig-slow");
                    assert_eq!(scenarios[0].runs.as_ref().expect("runs").len(), 3);
                }
            }
            _ => panic!("expected comparison output"),
        }
    });
}

#[test]
fn cross_rig_profile_requires_every_rig_to_define_profile() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig_with_profiles(
            home,
            "rig-a",
            "studio",
            component_a.path(),
            r#"{ "substrate": ["rig-extra"] }"#,
        );
        write_rig_with_profiles(
            home,
            "rig-b",
            "studio",
            component_b.path(),
            r#"{ "smoke": ["rig-slow"] }"#,
        );

        let err = match run(
            run_args_with_profile(
                None,
                vec!["rig-a".to_string(), "rig-b".to_string()],
                "substrate",
            ),
            &GlobalArgs {},
        ) {
            Ok(_) => panic!("missing cross-rig profile should fail"),
            Err(err) => err,
        };
        let message = err.to_string();

        assert!(message.contains("rig-b"), "got: {}", message);
        assert!(message.contains("substrate"), "got: {}", message);
        assert!(message.contains("smoke"), "got: {}", message);
    });
}

#[test]
fn cross_rig_profile_selects_profile_for_each_rig() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig_with_profiles(
            home,
            "rig-a",
            "studio",
            component_a.path(),
            r#"{ "substrate": ["rig-extra"] }"#,
        );
        write_rig_with_profiles(
            home,
            "rig-b",
            "studio",
            component_b.path(),
            r#"{ "substrate": ["rig-slow"] }"#,
        );

        let (output, exit_code) = run(
            run_args_with_profile(
                None,
                vec!["rig-a".to_string(), "rig-b".to_string()],
                "substrate",
            ),
            &GlobalArgs {},
        )
        .expect("cross-rig profile bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                assert_eq!(
                    result.rigs[0]
                        .results
                        .as_ref()
                        .expect("rig a results")
                        .scenarios[0]
                        .id,
                    "rig-extra"
                );
                assert_eq!(
                    result.rigs[1]
                        .results
                        .as_ref()
                        .expect("rig b results")
                        .scenarios[0]
                        .id,
                    "rig-slow"
                );
            }
            _ => panic!("expected comparison output"),
        }
    });
}

#[test]
fn run_list_requires_rig_default_component_when_component_omitted() {
    with_isolated_home(|home| {
        let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
        fs::create_dir_all(&rig_dir).expect("mkdir rigs");
        fs::write(rig_dir.join("empty.json"), r#"{ "bench": {} }"#).expect("write rig");

        let err = match run_list(&list_args(None, vec!["empty".to_string()])) {
            Ok(_) => panic!("missing default component should error"),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(
            message.contains("bench.default_component"),
            "expected default-component error, got: {}",
            message
        );
    });
}

#[test]
fn bench_workloads_for_extension_filters_and_expands_paths() {
    std::env::set_var("HOMEBOY_TEST_BENCH_ROOT", "/tmp/private-benches");
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
                "id": "studio",
                "components": {
                    "playground": { "path": "/tmp/playground" }
                },
                "bench_workloads": {
                    "wordpress": [
                        "${env.HOMEBOY_TEST_BENCH_ROOT}/cold-boot.php",
                        "${components.playground.path}/fixtures/wc-loaded.php"
                    ],
                    "nodejs": ["/tmp/node-only.bench.ts"]
                }
            }"#,
    )
    .expect("parse rig spec");

    let workloads = bench_workloads_for_extension(&rig_spec, None, "wordpress");

    assert_eq!(
        workloads,
        vec![
            PathBuf::from("/tmp/private-benches/cold-boot.php"),
            PathBuf::from("/tmp/playground/fixtures/wc-loaded.php"),
        ]
    );
    assert!(bench_workloads_for_extension(&rig_spec, None, "rust").is_empty());
}

#[test]
fn bench_workloads_for_extension_expands_package_root_when_available() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
                "id": "studio-agent-sdk",
                "bench_workloads": {
                    "nodejs": [
                        "${package.root}/bench/studio-agent-runtime.bench.mjs",
                        "/tmp/absolute.bench.mjs"
                    ]
                }
            }"#,
    )
    .expect("parse rig spec");
    let package = PathBuf::from("/tmp/homeboy-rigs/Automattic/studio");

    let workloads = bench_workloads_for_extension(&rig_spec, Some(&package), "nodejs");

    assert_eq!(
        workloads,
        vec![
            PathBuf::from(
                "/tmp/homeboy-rigs/Automattic/studio/bench/studio-agent-runtime.bench.mjs"
            ),
            PathBuf::from("/tmp/absolute.bench.mjs"),
        ]
    );
}

#[test]
fn bench_workloads_for_extension_leaves_package_root_unexpanded_without_metadata() {
    let rig_spec: RigSpec = serde_json::from_str(
        r#"{
                "id": "manual",
                "bench_workloads": {
                    "nodejs": ["${package.root}/bench/manual.bench.mjs"]
                }
            }"#,
    )
    .expect("parse rig spec");

    let workloads = bench_workloads_for_extension(&rig_spec, None, "nodejs");

    assert_eq!(
        workloads,
        vec![PathBuf::from("${package.root}/bench/manual.bench.mjs")]
    );
}

#[test]
fn filter_strips_all_boolean_flags() {
    let args = vec![
        "--baseline".to_string(),
        "--ignore-baseline".to_string(),
        "--ratchet".to_string(),
        "--json-summary".to_string(),
        "--json".to_string(),
    ];
    assert!(filter_homeboy_flags(&args).is_empty());
}

#[test]
fn filter_strips_iterations_space_form() {
    let args = vec![
        "--iterations".to_string(),
        "50".to_string(),
        "--filter=Scenario".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);
}

#[test]
fn filter_strips_iterations_equals_form() {
    let args = vec!["--iterations=50".to_string(), "--keep".to_string()];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn parses_warmup_flag() {
    let cli = TestCli::try_parse_from(["bench", "homeboy", "--warmup", "3"])
        .expect("bench --warmup should parse");

    assert_eq!(cli.bench.run.warmup, Some(3));
}

#[test]
fn rejects_negative_warmup_flag() {
    let err = match TestCli::try_parse_from(["bench", "homeboy", "--warmup", "-1"]) {
        Ok(_) => panic!("negative warmup must fail at parse time"),
        Err(err) => err,
    };

    assert!(
        err.to_string().contains("invalid value"),
        "expected invalid value error, got: {}",
        err
    );
}

#[test]
fn filter_strips_warmup_forms() {
    let args = vec![
        "--warmup".to_string(),
        "3".to_string(),
        "--filter=Scenario".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);

    let args = vec!["--warmup=3".to_string(), "--keep".to_string()];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn unrigged_bench_forwards_cli_warmup() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let mut args = run_args(Some("studio"), Vec::new(), Vec::new());
        args.run.warmup = Some(4);

        let (output, exit_code) = run(args, &GlobalArgs {}).expect("bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(first_warmup_metric(output), 4.0);
    });
}

#[test]
fn unrigged_bench_omits_warmup_env_by_default() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_registered_component(home, "studio", component_dir.path());

        let (output, exit_code) = run(
            run_args(Some("studio"), Vec::new(), Vec::new()),
            &GlobalArgs {},
        )
        .expect("bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(first_warmup_metric(output), -1.0);
    });
}

#[test]
fn single_rig_bench_forwards_rig_warmup() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig(home, "rig-a", "studio", component_dir.path());
        set_rig_warmup(home, "rig-a", 6);

        let (output, exit_code) = run(
            run_args(None, vec!["rig-a".to_string()], Vec::new()),
            &GlobalArgs {},
        )
        .expect("rig bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(first_warmup_metric(output), 6.0);
    });
}

#[test]
fn cli_warmup_overrides_rig_warmup() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_dir = tempfile::TempDir::new().expect("component dir");
        write_rig(home, "rig-a", "studio", component_dir.path());
        set_rig_warmup(home, "rig-a", 6);

        let mut args = run_args(None, vec!["rig-a".to_string()], Vec::new());
        args.run.warmup = Some(2);

        let (output, exit_code) = run(args, &GlobalArgs {}).expect("rig bench should run");

        assert_eq!(exit_code, 0);
        assert_eq!(first_warmup_metric(output), 2.0);
    });
}

#[test]
fn cross_rig_bench_uses_each_rig_warmup() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());
        set_rig_warmup(home, "rig-a", 2);
        set_rig_warmup(home, "rig-b", 5);

        let (output, exit_code) = run(
            run_args(
                None,
                vec!["rig-a".to_string(), "rig-b".to_string()],
                Vec::new(),
            ),
            &GlobalArgs {},
        )
        .expect("cross-rig bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                assert_eq!(
                    result.rigs[0]
                        .results
                        .as_ref()
                        .expect("rig-a results")
                        .scenarios[0]
                        .metrics
                        .get("warmup_iterations"),
                    Some(2.0)
                );
                assert_eq!(
                    result.rigs[1]
                        .results
                        .as_ref()
                        .expect("rig-b results")
                        .scenarios[0]
                        .metrics
                        .get("warmup_iterations"),
                    Some(5.0)
                );
            }
            _ => panic!("expected comparison output"),
        }
    });
}

#[test]
fn cross_rig_cli_warmup_overrides_all_rigs() {
    with_isolated_home(|home| {
        write_bench_extension(home);
        let component_a = tempfile::TempDir::new().expect("component a");
        let component_b = tempfile::TempDir::new().expect("component b");
        write_rig(home, "rig-a", "studio", component_a.path());
        write_rig(home, "rig-b", "studio", component_b.path());
        set_rig_warmup(home, "rig-a", 2);
        set_rig_warmup(home, "rig-b", 5);

        let mut args = run_args(
            None,
            vec!["rig-a".to_string(), "rig-b".to_string()],
            Vec::new(),
        );
        args.run.warmup = Some(9);

        let (output, exit_code) = run(args, &GlobalArgs {}).expect("cross-rig bench should run");

        assert_eq!(exit_code, 0);
        match output {
            BenchOutput::Comparison(result) => {
                assert_eq!(result.rigs.len(), 2);
                for rig in result.rigs {
                    assert_eq!(
                        rig.results.expect("rig results").scenarios[0]
                            .metrics
                            .get("warmup_iterations"),
                        Some(9.0)
                    );
                }
            }
            _ => panic!("expected comparison output"),
        }
    });
}

#[test]
fn parses_shared_state_and_concurrency_flags() {
    let cli = TestCli::try_parse_from([
        "bench",
        "homeboy",
        "--shared-state",
        "/tmp/foo",
        "--concurrency",
        "4",
    ])
    .expect("shared-state and concurrency flags should parse");

    assert_eq!(cli.bench.run.shared_state, Some(PathBuf::from("/tmp/foo")));
    assert_eq!(cli.bench.run.concurrency, 4);
}

#[test]
fn filter_strips_shared_state_and_concurrency_forms() {
    let args = vec![
        "--shared-state".to_string(),
        "/tmp/foo".to_string(),
        "--concurrency".to_string(),
        "4".to_string(),
        "--filter=Scenario".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);

    let args = vec![
        "--shared-state=/tmp/foo".to_string(),
        "--concurrency=4".to_string(),
        "--keep".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn filter_strips_rig_order_forms() {
    let args = vec![
        "--rig-order".to_string(),
        "reverse".to_string(),
        "--filter=Scenario".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--filter=Scenario"]);

    let args = vec!["--rig-order=reverse".to_string(), "--keep".to_string()];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn filter_strips_regression_threshold_forms() {
    let args = vec![
        "--regression-threshold".to_string(),
        "10".to_string(),
        "--keep".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);

    let args = vec![
        "--regression-threshold=10".to_string(),
        "--keep".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), vec!["--keep"]);
}

#[test]
fn filter_preserves_unknown_flags() {
    let args = vec![
        "--filter=Scenario".to_string(),
        "--verbose".to_string(),
        "extra".to_string(),
    ];
    assert_eq!(filter_homeboy_flags(&args), args);
}

#[test]
fn filter_handles_empty() {
    assert!(filter_homeboy_flags(&[]).is_empty());
}

#[test]
fn filter_handles_mixed() {
    let args = vec![
        "--ratchet".to_string(),
        "--iterations".to_string(),
        "25".to_string(),
        "--filter=hot_path".to_string(),
        "--regression-threshold=7.5".to_string(),
        "--verbose".to_string(),
    ];
    assert_eq!(
        filter_homeboy_flags(&args),
        vec!["--filter=hot_path", "--verbose"]
    );
}

#[test]
fn bench_output_single_serializes_without_wrapper_key() {
    // Backcompat: single-rig and bare-bench output must serialize
    // identically to the pre-cross-rig shape (no top-level
    // discriminator field). The `untagged` enum representation
    // gives us that for free, but pin it with a test so a future
    // refactor can't quietly break consumers.
    let single = BenchCommandOutput {
        passed: true,
        status: "passed".to_string(),
        component: "studio".to_string(),
        exit_code: 0,
        iterations: 10,
        artifacts: Vec::new(),
        results: None,
        gate_failures: Vec::new(),
        baseline_comparison: None,
        hints: None,
        rig_state: None,
        failure: None,
    };
    let value = serde_json::to_value(BenchOutput::Single(single)).unwrap();
    assert!(value.get("comparison").is_none());
    assert_eq!(value.get("passed"), Some(&serde_json::Value::Bool(true)));
    assert_eq!(
        value.get("component"),
        Some(&serde_json::Value::String("studio".to_string()))
    );
}

#[test]
fn bench_output_comparison_serializes_with_discriminator() {
    let (cmp, _) = aggregate_comparison("studio".to_string(), 10, Vec::new());
    let value = serde_json::to_value(BenchOutput::Comparison(cmp)).unwrap();
    assert_eq!(
        value.get("comparison"),
        Some(&serde_json::Value::String("cross_rig".to_string()))
    );
}
#[cfg(test)]
#[path = "../../../tests/core/rig/bench_default_baseline_dispatch_test.rs"]
mod bench_default_baseline_dispatch_test;
