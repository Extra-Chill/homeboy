use super::*;
use crate::test_support::with_isolated_home;
use clap::error::ErrorKind;
use clap::Parser;
use homeboy::core::extension::bench::aggregate_comparison;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
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
        .join("fixture-bench");
    fs::create_dir_all(&extension_dir).expect("mkdir extension");
    fs::write(
        extension_dir.join("fixture-bench.json"),
        r#"{
                "name": "Fixture Bench",
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

fn write_failing_bench_extension(home: &TempDir) {
    let extension_dir = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("extensions")
        .join("fixture-bench");
    fs::create_dir_all(&extension_dir).expect("mkdir extension");
    fs::write(
        extension_dir.join("fixture-bench.json"),
        r#"{
                "name": "Fixture Bench",
                "version": "0.0.0",
                "bench": { "extension_script": "bench-runner.sh" }
            }"#,
    )
    .expect("write extension manifest");

    let script_path = extension_dir.join("bench-runner.sh");
    fs::write(
        &script_path,
        r#"#!/bin/sh
if [ "$HOMEBOY_BENCH_LIST_ONLY" = "1" ]; then
  cat > "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
{
  "component_id": "$HOMEBOY_COMPONENT_ID",
  "iterations": 0,
  "scenarios": [
    { "id": "studio-agent-site-build", "iterations": 0, "metrics": {} }
  ]
}
JSON
  exit 0
fi

cat > "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
{
  "component_id": "$HOMEBOY_COMPONENT_ID",
  "iterations": ${HOMEBOY_BENCH_ITERATIONS:-0},
  "scenarios": []
}
JSON
printf 'WORKLOAD_ERROR: studio-agent-site-build - warmup iteration 1/1 returned artifact "visual_comparison_dir" with empty path\n' >&2
exit 7
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
            "extensions": { "fixture-bench": {} }
        })
        .to_string(),
    )
    .expect("write component");
}

fn write_rig(home: &TempDir, rig_id: &str, component_id: &str, path: &std::path::Path) {
    write_rig_with_profiles(home, rig_id, component_id, path, "{}");
}

fn install_rig_package(
    home: &TempDir,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
) -> std::path::PathBuf {
    let package_root = home.path().join(format!("{rig_id}-package"));
    let rig_dir = package_root.join("rigs").join(rig_id);
    let workload_dir = package_root.join("workloads");
    fs::create_dir_all(&rig_dir).expect("mkdir package rig");
    fs::create_dir_all(&workload_dir).expect("mkdir package workloads");
    fs::write(workload_dir.join("package-extra.bench.js"), "// fixture\n")
        .expect("write package workload");
    fs::write(
        rig_dir.join("rig.json"),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{
                            "path": "{}",
                            "extensions": {{ "fixture-bench": {{}} }}
                        }}
                    }},
                    "bench": {{ "default_component": "{component_id}" }},
                    "bench_workloads": {{ "fixture-bench": [
                        {{ "path": "${{package.root}}/workloads/package-extra.bench.js" }}
                    ] }}
                }}"#,
            path.display()
        ),
    )
    .expect("write package rig");

    homeboy::core::rig::install(package_root.to_string_lossy().as_ref(), Some(rig_id), false)
        .expect("install rig package");
    package_root
}

fn install_git_rig_package(
    home: &TempDir,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
) -> (std::path::PathBuf, String) {
    let package_root = home.path().join(format!("{rig_id}-git-package"));
    let rig_dir = package_root.join("rigs").join(rig_id);
    fs::create_dir_all(&rig_dir).expect("mkdir package rig");
    fs::write(
        rig_dir.join("rig.json"),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{
                            "path": "{}",
                            "extensions": {{ "fixture-bench": {{}} }}
                        }}
                    }},
                    "bench": {{ "default_component": "{component_id}" }}
                }}"#,
            path.display()
        ),
    )
    .expect("write package rig");

    git(&package_root, &["init", "-b", "main"]);
    git(&package_root, &["config", "user.name", "Test User"]);
    git(&package_root, &["config", "user.email", "test@example.com"]);
    git(&package_root, &["add", "."]);
    git(&package_root, &["commit", "-m", "Initial rig package"]);
    let revision = git_output(&package_root, &["rev-parse", "--short", "HEAD"]);
    fs::write(package_root.join("untracked.txt"), "dirty\n").expect("dirty package");

    homeboy::core::rig::install(package_root.to_string_lossy().as_ref(), Some(rig_id), false)
        .expect("install rig package");
    (package_root, revision)
}

fn git(repo: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .status()
        .expect("run git");
    assert!(status.success(), "git {:?} failed", args);
}

fn git_output(repo: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {:?} failed", args);
    String::from_utf8(output.stdout)
        .expect("git output utf8")
        .trim()
        .to_string()
}

fn write_local_rig_package(
    root: &std::path::Path,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
    workload_stem: &str,
) {
    let rig_dir = root.join("rigs").join(rig_id);
    let workload_dir = root.join("workloads");
    fs::create_dir_all(&rig_dir).expect("mkdir local package rig");
    fs::create_dir_all(&workload_dir).expect("mkdir local package workloads");
    fs::write(
        workload_dir.join(format!("{workload_stem}.bench.js")),
        "// fixture\n",
    )
    .expect("write local package workload");
    fs::write(
        rig_dir.join("rig.json"),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{
                            "path": "{}",
                            "extensions": {{ "fixture-bench": {{}} }}
                        }}
                    }},
                    "bench": {{ "default_component": "{component_id}" }},
                    "bench_workloads": {{ "fixture-bench": [
                        {{ "path": "${{package.root}}/workloads/{workload_stem}.bench.js" }}
                    ] }}
                }}"#,
            path.display()
        ),
    )
    .expect("write local package rig");
}

fn current_dir_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn write_rig_with_component_script(
    home: &TempDir,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
) {
    let script_path = path.join("native-bench.sh");
    fs::write(
        &script_path,
        r#"#!/bin/sh
cat > "$HOMEBOY_BENCH_RESULTS_FILE" <<JSON
{
  "component_id": "$HOMEBOY_COMPONENT_ID",
  "iterations": ${HOMEBOY_BENCH_ITERATIONS:-0},
  "scenarios": [
    { "id": "native-script", "iterations": ${HOMEBOY_BENCH_ITERATIONS:-0}, "metrics": {} }
  ]
}
JSON
"#,
    )
    .expect("write native bench script");

    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod script");
    }

    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");
    fs::write(
        rig_dir.join(format!("{}.json", rig_id)),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{
                            "path": "{}",
                            "scripts": {{ "bench": ["./native-bench.sh"] }}
                        }}
                    }},
                    "bench": {{ "default_component": "{component_id}" }},
                    "bench_workloads": {{ "fixture-bench": [
                        {{ "path": "${{components.{component_id}.path}}/rig-extra.bench.js" }}
                    ] }}
                }}"#,
            path.display()
        ),
    )
    .expect("write rig");
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
                            "extensions": {{ "fixture-bench": {{}} }}
                        }}
                    }},
                    "bench": {{ "default_component": "{component_id}" }},
                    "bench_workloads": {{ "fixture-bench": [
                        {{ "path": "${{components.{component_id}.path}}/rig-extra.bench.js" }},
                        {{ "path": "${{components.{component_id}.path}}/rig-slow.bench.mjs" }}
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
        extension_override: ExtensionOverrideArgs::default(),
        rig,
        scenario_ids: Vec::new(),
        setting_args: SettingArgs::default(),
        args: Vec::new(),
    }
}

pub(super) fn run_args(
    component: Option<&str>,
    rig: Vec<String>,
    scenario_ids: Vec<String>,
) -> BenchArgs {
    BenchArgs {
        command: None,
        json: false,
        run: BenchRunArgs {
            comp: PositionalComponentArgs {
                component: component.map(str::to_string),
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            iterations: 1,
            warmup: None,
            runs: 1,
            run_id: None,
            shared_state: None,
            concurrency: 1,
            matrix: Vec::new(),
            runner_pool: None,
            matrix_max_tasks: None,
            matrix_max_queue_depth: None,
            expected_artifact: Vec::new(),
            baseline_args: BaselineArgs {
                baseline: false,
                ignore_baseline: true,
                ratchet: false,
            },
            regression_threshold: 5.0,
            setting_args: SettingArgs::default(),
            args: Vec::new(),
            json_summary: false,
            status_file: None,
            report: Vec::new(),
            rig,
            rig_order: BenchRigOrder::Input,
            rig_concurrency: 1,
            scenario_ids,
            profile: None,
            ci_profile: None,
            ignore_default_baseline: false,
        },
    }
}

fn run_args_with_profile(component: Option<&str>, rig: Vec<String>, profile: &str) -> BenchArgs {
    let mut args = run_args(component, rig, Vec::new());
    args.run.profile = Some(profile.to_string());
    args
}

fn ctx_with_accepted_setting_keys(
    accepted: &[&str],
) -> homeboy::core::engine::execution_context::ExecutionContext {
    use homeboy::core::engine::execution_context::ExecutionContext;
    ExecutionContext {
        component: Default::default(),
        component_id: "studio".to_string(),
        source_path: std::path::PathBuf::from("/tmp/studio"),
        git_root: None,
        extension_id: Some("rust".to_string()),
        extension_path: None,
        settings: Vec::new(),
        accepted_setting_keys: accepted.iter().map(|s| s.to_string()).collect(),
    }
}

mod cross_rig_tests;
mod output_tests;
mod parsing_tests;
mod run_list_tests;
mod scenario_run_tests;
mod settings_tests;
mod warmup_tests;

#[cfg(test)]
#[path = "../../../../tests/core/rig/bench_default_baseline_dispatch_test.rs"]
mod bench_default_baseline_dispatch_test;
#[cfg(test)]
#[path = "../../../../tests/core/rig/bench_default_baseline_output_test.rs"]
mod bench_default_baseline_output_test;
#[cfg(test)]
#[path = "../../../../tests/core/rig/bench_prepare_pipeline_test.rs"]
mod bench_prepare_pipeline_test;
#[cfg(test)]
#[path = "../../../../tests/core/rig/bench_resource_lease_test.rs"]
mod bench_resource_lease_test;
#[cfg(test)]
#[path = "../../../../tests/core/rig/bench_rig_concurrency_dispatch_test.rs"]
mod bench_rig_concurrency_dispatch_test;
#[cfg(test)]
#[path = "../../../../tests/core/rig/bench_rig_path_override_test.rs"]
mod bench_rig_path_override_test;
