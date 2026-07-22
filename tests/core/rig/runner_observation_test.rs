//! Observation-store persistence tests for `src/core/rig/runner.rs`.

use std::collections::HashMap;
use std::process::Command;

use crate::runner::{run_check, run_up};
use crate::spec::{ComponentSpec, PipelineStep, RigResourcesSpec, RigSpec};
use crate::{RigSourceMetadata, RigState};
use homeboy_core::observation::{ObservationStore, RunListFilter};
use homeboy_core::paths;
use homeboy_core::resource_lifecycle_index::{
    resource_lifecycle_index_from_artifacts, ResourceLifecycleResourceStatus,
};
use homeboy_core::test_support::with_isolated_home;

fn observation_spec(id: &str) -> RigSpec {
    RigSpec {
        id: id.to_string(),
        description: "observation persistence fixture".to_string(),
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

struct XdgDataHomeGuard(Option<String>);

impl XdgDataHomeGuard {
    fn unset() -> Self {
        let prior = std::env::var("XDG_DATA_HOME").ok();
        std::env::remove_var("XDG_DATA_HOME");
        Self(prior)
    }

    fn set(value: String) -> Self {
        let prior = std::env::var("XDG_DATA_HOME").ok();
        std::env::set_var("XDG_DATA_HOME", value);
        Self(prior)
    }
}

impl Drop for XdgDataHomeGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
}

fn list_rig_runs(rig_id: &str) -> Vec<homeboy_core::observation::RunRecord> {
    ObservationStore::open_initialized()
        .expect("observation store")
        .list_runs(RunListFilter {
            kind: Some("rig".to_string()),
            rig_id: Some(rig_id.to_string()),
            ..RunListFilter::default()
        })
        .expect("list rig runs")
}

#[test]
fn test_run_check_persists_passing_observation() {
    with_isolated_home(|_dir| {
        let _xdg = XdgDataHomeGuard::unset();
        let rig = observation_spec("observed-check-pass");

        let report = run_check(&rig).expect("check succeeds");
        assert!(report.success);

        let runs = list_rig_runs(&rig.id);
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run.status, "pass");
        assert_eq!(run.command.as_deref(), Some("rig.check"));
        assert_eq!(run.rig_id.as_deref(), Some("observed-check-pass"));
        assert_eq!(run.metadata_json["command"], "check");
        assert_eq!(run.metadata_json["pipeline"]["name"], "check");
        assert_eq!(
            run.metadata_json["pipeline"]["steps"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            run.metadata_json["state"]["last_check_result"],
            serde_json::Value::String("pass".to_string())
        );
    });
}

#[test]
fn test_run_check_persists_failing_observation() {
    with_isolated_home(|_dir| {
        let _xdg = XdgDataHomeGuard::unset();
        let mut rig = observation_spec("observed-check-fail");
        rig.pipeline.insert(
            "check".to_string(),
            vec![PipelineStep::Command {
                step_id: None,
                depends_on: Vec::new(),
                cmd: "false".to_string(),
                cwd: None,
                env: HashMap::new(),
                requires_capabilities: Vec::new(),
                requires_providers: Vec::new(),
                provides_capabilities: Vec::new(),
                provides_providers: Vec::new(),
                label: Some("intentional check failure".to_string()),
            }],
        );

        let report = run_check(&rig).expect("check returns failed report");
        assert!(!report.success);
        let run_id = report.run_id.as_deref().expect("run id exposed");
        let artifact_index = report
            .artifact_index
            .as_ref()
            .expect("artifact index exposed");
        assert_eq!(artifact_index.run_id, run_id);
        assert_eq!(artifact_index.rig_id, rig.id);
        assert_eq!(artifact_index.status, "fail");
        assert!(artifact_index
            .artifact_index_path
            .ends_with("rig-artifact-index.json"));
        assert!(std::path::Path::new(&artifact_index.artifact_index_path).is_file());
        assert_eq!(
            artifact_index.evidence_commands.artifacts_command,
            format!("homeboy runs artifacts {run_id}")
        );
        assert!(artifact_index
            .retrieval_commands
            .contains(&format!("homeboy runs evidence {run_id}")));
        assert_eq!(artifact_index.failed_step_refs.len(), 1);
        assert_eq!(artifact_index.failed_step_refs[0].pipeline, "check");
        assert_eq!(artifact_index.failed_step_refs[0].kind, "command");
        assert_eq!(
            artifact_index.failed_step_refs[0].label,
            "intentional check failure"
        );
        assert!(artifact_index
            .key_report_refs
            .iter()
            .any(|artifact| artifact.kind == "rig_artifact_index"));

        let runs = list_rig_runs(&rig.id);
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        let persisted_index = crate::artifact_index_for_run(
            &ObservationStore::open_initialized().expect("store"),
            run,
        )
        .expect("persisted run artifact index");
        assert_eq!(persisted_index.run_id, run_id);
        assert_eq!(persisted_index.failed_step_refs.len(), 1);
        assert_eq!(run.status, "fail");
        assert_eq!(run.metadata_json["pipeline"]["failed"], 1);
        assert_eq!(
            run.metadata_json["pipeline"]["steps"][0]["label"],
            "intentional check failure"
        );
        assert_eq!(run.metadata_json["pipeline"]["steps"][0]["status"], "fail");
        assert!(run.metadata_json["pipeline"]["steps"][0]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("exited 1"));
    });
}

#[test]
fn test_successful_nested_command_retains_registered_file() {
    with_isolated_home(|home| {
        let _xdg = XdgDataHomeGuard::unset();
        let artifact = home.path().join("wp-codebox-result.json");
        let script = home.path().join("register-success.sh");
        write_registration_script(&script, &artifact, "file", 0);
        let mut rig = observation_spec("observed-command-artifact-success");
        rig.pipeline.insert(
            "check".to_string(),
            vec![command_step(format!(
                "sh {} {}",
                script.display(),
                artifact.display()
            ))],
        );

        let report = run_check(&rig).expect("check report");
        assert!(report.success);
        let run_id = report.run_id.as_deref().expect("run id");
        let store = ObservationStore::open_initialized().expect("store");
        let artifacts = store.list_artifacts(run_id).expect("artifacts");
        let result = artifacts
            .iter()
            .find(|artifact| artifact.kind == "wp_codebox_result")
            .expect("registered result");

        assert_eq!(result.artifact_type, "file");
        assert_eq!(result.metadata_json["source"], "rig_command_registration");
        assert_eq!(std::fs::read_to_string(&result.path).unwrap(), "{}");
        assert!(report
            .artifact_index
            .as_ref()
            .unwrap()
            .registered_artifact_refs
            .iter()
            .any(|artifact| artifact.id == result.id));
    });
}

#[test]
fn test_failed_nested_wp_codebox_command_retains_registered_bundle() {
    with_isolated_home(|home| {
        let _xdg = XdgDataHomeGuard::unset();
        let bundle = home.path().join("wp-codebox-failed-bundle");
        let script = home.path().join("register-failure.sh");
        write_registration_script(&script, &bundle, "directory", 23);
        let mut rig = observation_spec("observed-command-artifact-failure");
        rig.pipeline.insert(
            "check".to_string(),
            vec![command_step(format!(
                "sh {} {}",
                script.display(),
                bundle.display()
            ))],
        );

        let report = run_check(&rig).expect("failed check report");
        assert!(!report.success);
        let run_id = report.run_id.as_deref().expect("run id");
        let store = ObservationStore::open_initialized().expect("store");
        let artifacts = store.list_artifacts(run_id).expect("artifacts");
        let retained = artifacts
            .iter()
            .find(|artifact| artifact.kind == "wp_codebox_bundle")
            .expect("failed bundle retained");

        assert_eq!(retained.artifact_type, "directory");
        assert!(std::path::Path::new(&retained.path)
            .join("failure.json")
            .is_file());
        assert_eq!(retained.metadata_json["registration_index"], 0);
        assert!(report
            .artifact_index
            .as_ref()
            .unwrap()
            .registered_artifact_refs
            .iter()
            .any(|artifact| artifact.id == retained.id));
    });
}

#[test]
fn test_run_up_persists_step_order_source_and_component_snapshot() {
    with_isolated_home(|home| {
        let _xdg = XdgDataHomeGuard::unset();
        let repo = home.path().join("component-repo");
        std::fs::create_dir(&repo).expect("repo dir");
        git(&repo, &["init"]);
        git(&repo, &["config", "user.email", "tests@example.com"]);
        git(&repo, &["config", "user.name", "Tests"]);
        std::fs::write(repo.join("README.md"), "fixture").expect("write fixture");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "initial"]);
        let sha = git_output(&repo, &["rev-parse", "HEAD"]);

        let mut rig = observation_spec("observed-up");
        rig.resources = RigResourcesSpec {
            exclusive: vec!["observed-runtime".to_string()],
            paths: vec![repo.to_string_lossy().to_string()],
            ports: vec![9981],
            process_patterns: Vec::new(),
        };
        rig.components.insert(
            "component".to_string(),
            ComponentSpec {
                path: repo.to_string_lossy().to_string(),
                component_id: None,
                path_setting: None,
                checkout_root: None,
                remote_url: None,
                triage_remote_url: None,
                stack: None,
                branch: None,
                r#ref: None,
                default_ref: None,
                extensions: None,
                dependency_cache: None,
            },
        );
        rig.pipeline.insert(
            "up".to_string(),
            vec![
                PipelineStep::Command {
                    step_id: None,
                    depends_on: Vec::new(),
                    cmd: "true".to_string(),
                    cwd: None,
                    env: HashMap::new(),
                    requires_capabilities: Vec::new(),
                    requires_providers: Vec::new(),
                    provides_capabilities: Vec::new(),
                    provides_providers: Vec::new(),
                    label: Some("first".to_string()),
                },
                PipelineStep::Command {
                    step_id: None,
                    depends_on: Vec::new(),
                    cmd: "true".to_string(),
                    cwd: None,
                    env: HashMap::new(),
                    requires_capabilities: Vec::new(),
                    requires_providers: Vec::new(),
                    provides_capabilities: Vec::new(),
                    provides_providers: Vec::new(),
                    label: Some("second".to_string()),
                },
            ],
        );
        write_rig_source_metadata(&rig.id);

        let report = run_up(&rig).expect("up succeeds");
        assert!(report.success);

        let runs = list_rig_runs(&rig.id);
        assert_eq!(runs.len(), 1);
        let metadata = &runs[0].metadata_json;
        assert_eq!(runs[0].status, "pass");
        assert_eq!(metadata["command"], "up");
        assert_eq!(metadata["rig_source"], "https://example.com/rigs.git");
        assert_eq!(metadata["rig_revision"], "abc123");
        assert_eq!(metadata["pipeline"]["steps"][0]["label"], "first");
        assert_eq!(metadata["pipeline"]["steps"][1]["label"], "second");
        assert_eq!(
            metadata["component_snapshot"]["components"]["component"]["sha"],
            sha
        );
        assert_eq!(
            metadata["state"]["materialized"]["components"]["component"]["sha"],
            sha
        );

        let artifacts = ObservationStore::open_initialized()
            .expect("store")
            .list_artifacts(&runs[0].id)
            .expect("list artifacts");
        let resource_index = resource_lifecycle_index_from_artifacts(&artifacts)
            .expect("resource lifecycle index parses")
            .expect("rig lifecycle index artifact exists");
        assert_eq!(resource_index.resources.len(), 3);
        assert!(resource_index.resources.iter().any(|record| {
            record.kind == "rig_exclusive"
                && record.path == "rig://observed-up/exclusive/observed-runtime"
                && record.status == ResourceLifecycleResourceStatus::Active
        }));
        assert!(resource_index
            .resources
            .iter()
            .any(|record| record.kind == "rig_path" && record.path == repo.to_string_lossy()));
    });
}

#[test]
fn test_observation_store_failure_does_not_fail_rig_check() {
    with_isolated_home(|home| {
        let data_home_file = home.path().join("not-a-directory");
        std::fs::write(&data_home_file, "file").expect("write blocking data home file");
        let _xdg = XdgDataHomeGuard::set(data_home_file.to_string_lossy().to_string());
        let rig = observation_spec("observed-check-db-unavailable");

        let report = run_check(&rig).expect("observation failure must not fail check");

        assert!(report.success);
        assert_eq!(
            RigState::load(&rig.id)
                .expect("state still writes")
                .last_check_result
                .as_deref(),
            Some("pass")
        );
    });
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
        label: Some("nested WP Codebox run".to_string()),
    }
}

fn write_registration_script(
    script: &std::path::Path,
    artifact: &std::path::Path,
    artifact_type: &str,
    exit_code: i32,
) {
    let create = if artifact_type == "directory" {
        "mkdir -p \"$1\"\nprintf '{}' > \"$1/failure.json\"\n"
    } else {
        "printf '{}' > \"$1\"\n"
    };
    let kind = if artifact_type == "directory" {
        "wp_codebox_bundle"
    } else {
        "wp_codebox_result"
    };
    std::fs::write(
        script,
        format!(
            "#!/bin/sh\n{create}printf '{{\"schema\":\"homeboy/rig-command-artifacts/v1\",\"run_id\":\"%s\",\"artifacts\":[{{\"kind\":\"{kind}\",\"artifact_type\":\"{artifact_type}\",\"path\":\"{}\"}}]}}\\n' \"$HOMEBOY_ACTIVE_RUN_ID\" > \"$HOMEBOY_RIG_ARTIFACT_MANIFEST\"\nexit {exit_code}\n",
            artifact.display(),
        ),
    )
    .expect("registration script");
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
        .expect("utf8")
        .trim()
        .to_string()
}

fn write_rig_source_metadata(rig_id: &str) {
    let path = paths::rig_source_metadata(rig_id).expect("metadata path");
    std::fs::create_dir_all(path.parent().expect("metadata parent")).expect("metadata dir");
    std::fs::write(
        path,
        serde_json::to_string(&RigSourceMetadata {
            source: "https://example.com/rigs.git".to_string(),
            source_root: Some("/tmp/rigs".to_string()),
            package_path: "/tmp/rigs".to_string(),
            rig_path: "/tmp/rigs/rig.json".to_string(),
            discovery_path: Some("/tmp/rigs".to_string()),
            linked: false,
            materialized: false,
            source_revision: Some("abc123".to_string()),
            source_ref: Some("main".to_string()),
            source_dirty: false,
        })
        .expect("serialize source metadata"),
    )
    .expect("write source metadata");
}
