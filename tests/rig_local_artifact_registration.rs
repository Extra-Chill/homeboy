use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use homeboy::rig::{PipelineStep, RigSpec};

#[test]
fn failed_nested_cli_registration_is_visible_to_runs_artifacts() {
    let temp = tempfile::tempdir().expect("temp dir");
    let home = temp.path().join("home");
    std::fs::create_dir(&home).expect("home dir");
    let bundle = temp.path().join("wp-codebox-failed-bundle");
    let binary = homeboy_bin();
    let mut rig = rig_spec("nested-cli-artifact-failure");
    rig.pipeline.insert(
        "check".to_string(),
        vec![command_step(format!(
            "mkdir -p {bundle}; printf '{{}}' > {bundle}/failure.json; {binary} rig artifact register --kind wp_codebox_bundle --path {bundle} >/dev/null; exit 23",
            bundle = bundle.display(),
            binary = binary.display(),
        ))],
    );
    let rig_path = temp.path().join("rig.json");
    std::fs::write(
        &rig_path,
        serde_json::to_vec_pretty(&rig).expect("serialize rig"),
    )
    .expect("write rig");

    let check = Command::new(&binary)
        .args(["--placement", "local", "rig", "check"])
        .arg(&rig_path)
        .env("HOME", &home)
        .env_remove("XDG_DATA_HOME")
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("rig check");
    assert_eq!(
        check.status.code(),
        Some(1),
        "rig check stderr: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let check_json: serde_json::Value =
        serde_json::from_slice(&check.stdout).expect("rig check JSON");
    let run_id = check_json["data"]["payload"]["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("run id missing from {check_json}"));
    assert!(
        check_json["data"]["payload"]["artifact_index"]["registered_artifact_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|artifact| artifact["kind"] == "wp_codebox_bundle")
    );

    let output = Command::new(&binary)
        .args(["runs", "artifacts", run_id])
        .env("HOME", &home)
        .env_remove("XDG_DATA_HOME")
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("runs artifacts");
    assert!(output.status.success());
    let stdout: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("runs artifacts JSON");
    let artifacts = stdout["data"]["payload"]["artifacts"]
        .as_array()
        .expect("artifact rows");
    let retained = artifacts
        .iter()
        .find(|artifact| artifact["kind"] == "wp_codebox_bundle")
        .expect("registered bundle row");

    assert_eq!(retained["type"], "directory");
    assert!(std::path::Path::new(retained["path"].as_str().unwrap())
        .join("failure.json")
        .is_file());
    assert_eq!(
        artifacts
            .iter()
            .filter(|artifact| artifact["kind"] == "wp_codebox_bundle")
            .count(),
        1
    );
}

fn rig_spec(id: &str) -> RigSpec {
    RigSpec {
        id: id.to_string(),
        description: "nested CLI artifact fixture".to_string(),
        components: HashMap::new(),
        services: HashMap::new(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        resources: Default::default(),
        lifecycle: Default::default(),
        requirements: Default::default(),
        pipeline: HashMap::new(),
        bench: None,
        fuzz: None,
        trace: Default::default(),
        bench_workloads: HashMap::new(),
        trace_workloads: HashMap::new(),
        fuzz_workloads: Default::default(),
        trace_workload_defaults: HashMap::new(),
        trace_phase_templates: HashMap::new(),
        trace_variants: HashMap::new(),
        trace_profiles: HashMap::new(),
        trace_experiments: HashMap::new(),
        trace_guardrails: Vec::new(),
        bench_profiles: HashMap::new(),
        fuzz_profiles: HashMap::new(),
        app_launcher: None,
    }
}

fn command_step(cmd: String) -> PipelineStep {
    PipelineStep::Command {
        step_id: None,
        depends_on: Vec::new(),
        cmd,
        cwd: None,
        env: HashMap::new(),
        requires_capabilities: Vec::new(),
        requires_providers: Vec::new(),
        provides_capabilities: Vec::new(),
        provides_providers: Vec::new(),
        label: Some("nested WP Codebox command".to_string()),
    }
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
