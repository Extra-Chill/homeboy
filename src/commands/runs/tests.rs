//! Tests for the `runs` command dispatch, handlers, and output shaping.

use super::*;
use std::collections::BTreeSet;
use std::path::Path;

use homeboy::core::observation::{
    FindingListFilter, NewFindingRecord, NewRunRecord, NewTraceSpanRecord,
    RecordedHomeboyFinding, RunRecord, RunStatus, TraceSpanRecord,
};
use homeboy::test_support::with_isolated_home;
use serde::Deserialize;

struct XdgGuard(Option<String>);

struct EnvGuard {
    key: &'static str,
    prior: Option<String>,
}

impl XdgGuard {
    fn unset() -> Self {
        let prior = std::env::var("XDG_DATA_HOME").ok();
        std::env::remove_var("XDG_DATA_HOME");
        Self(prior)
    }
}

impl Drop for XdgGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn serve_public_artifact_base_once(status: u16) -> String {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind public artifact server");
    let addr = listener.local_addr().expect("server address");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept public artifact probe");
        let mut buffer = [0; 1024];
        let _ = stream.read(&mut buffer);
        let status_text = if status == 200 { "OK" } else { "Not Found" };
        let body = if status == 200 { "{}" } else { "missing" };
        write!(
            stream,
            "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("write public artifact response");
    });
    format!("http://{addr}/homeboy")
}

fn sample_run(kind: &str, component_id: &str, rig_id: &str, metadata: Value) -> NewRunRecord {
    NewRunRecord::builder(kind)
        .component_id(component_id)
        .command(format!("homeboy {kind} {component_id}"))
        .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
        .homeboy_version("test-version")
        .git_sha(Some("abc123".to_string()))
        .rig_id(rig_id)
        .metadata(metadata)
        .build()
}

fn dead_owned_run(id: &str) -> RunRecord {
    RunRecord {
        id: id.to_string(),
        kind: "bench".to_string(),
        component_id: Some("homeboy".to_string()),
        started_at: "2026-05-02T16:46:46Z".to_string(),
        finished_at: None,
        status: "running".to_string(),
        command: Some("homeboy bench".to_string()),
        cwd: Some("/tmp/homeboy-fixture".to_string()),
        homeboy_version: Some("test-version".to_string()),
        git_sha: Some("abc123".to_string()),
        rig_id: Some("studio".to_string()),
        metadata_json: serde_json::json!({ "homeboy_run_owner": { "pid": u32::MAX } }),
    }
}

#[test]
fn run_list_filters_kind_component_rig_and_status() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let bench = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("bench");
        store
            .finish_run(&bench.id, RunStatus::Pass, None)
            .expect("finish bench");
        let trace = store
            .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
            .expect("trace");
        store
            .finish_run(&trace.id, RunStatus::Fail, None)
            .expect("finish trace");

        let (output, _) = list_runs(
            RunsListArgs {
                runner: None,
                kind: Some("bench".to_string()),
                component_id: Some("homeboy".to_string()),
                rig: Some("studio".to_string()),
                scenario_id: None,
                status: Some("pass".to_string()),
                limit: 20,
                include_active_runner_jobs: false,
            },
            "runs.list",
        )
        .expect("list");

        let RunsOutput::List(output) = output else {
            panic!("expected list output");
        };
        assert_eq!(output.runs.len(), 1);
        assert_eq!(output.runs[0].id, bench.id);
    });
}

#[test]
fn run_list_reconciles_owned_dead_running_runs_before_listing() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        store
            .import_run(&dead_owned_run("dead-owned-run"))
            .expect("import stale fixture");

        let (output, _) = list_runs(
            RunsListArgs {
                runner: None,
                kind: Some("bench".to_string()),
                component_id: Some("homeboy".to_string()),
                rig: Some("studio".to_string()),
                scenario_id: None,
                status: None,
                limit: 20,
                include_active_runner_jobs: false,
            },
            "runs.list",
        )
        .expect("list");

        let RunsOutput::List(output) = output else {
            panic!("expected list output");
        };
        assert_eq!(output.runs.len(), 1);
        assert_eq!(output.runs[0].id, "dead-owned-run");
        assert_eq!(output.runs[0].status, "stale");
        assert!(output.runs[0].finished_at.is_some());
        assert_eq!(output.runs[0].status_note, None);

        let stored = store
            .get_run("dead-owned-run")
            .expect("get run")
            .expect("run exists");
        assert_eq!(stored.status, "stale");
        assert_eq!(
            stored.metadata_json["homeboy_reconciled"]["reason"],
            "owner_process_not_running"
        );
    });
}

#[test]
fn run_show_includes_metadata_and_artifacts() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run(
                "bench",
                "homeboy",
                "studio",
                serde_json::json!({ "scenario_metrics": [] }),
            ))
            .expect("run");
        let artifact_path = home.path().join("bench-results.json");
        std::fs::write(&artifact_path, b"{}").expect("artifact");
        store
            .record_artifact(&run.id, "bench_results", &artifact_path)
            .expect("record artifact");

        let (output, _) = show_run(&run.id).expect("show");
        let RunsOutput::Show(output) = output else {
            panic!("expected show output");
        };
        assert_eq!(output.run.summary.id, run.id);
        assert_eq!(
            output.run.metadata["scenario_metrics"],
            serde_json::json!([])
        );
        assert_eq!(output.run.artifacts.len(), 1);
        assert_eq!(output.run.artifacts[0].kind, "bench_results");
    });
}

#[test]
fn run_show_reconciles_owned_dead_running_run_before_displaying() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        store
            .import_run(&dead_owned_run("dead-owned-run"))
            .expect("import stale fixture");
        let (output, _) = show_run("dead-owned-run").expect("show");
        let RunsOutput::Show(output) = output else {
            panic!("expected show output");
        };
        assert_eq!(output.run.summary.status, "stale");
        assert_eq!(
            output.run.metadata["homeboy_reconciled"]["reason"],
            "owner_process_not_running"
        );
    });
}

#[test]
fn artifacts_command_reports_paths() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
            .expect("run");
        let artifact_path = home.path().join("trace-results.json");
        std::fs::write(&artifact_path, b"{}").expect("artifact");
        store
            .record_artifact(&run.id, "trace_results", &artifact_path)
            .expect("record artifact");

        let (output, _) = artifacts(&run.id).expect("artifacts");
        let RunsOutput::Artifacts(output) = output else {
            panic!("expected artifacts output");
        };
        assert_eq!(output.artifacts.len(), 1);
        let reported_path = std::path::PathBuf::from(&output.artifacts[0].path);
        let expected_file_name = format!("{}-trace-results.json", output.artifacts[0].id);
        assert_ne!(reported_path, artifact_path);
        assert!(reported_path.is_file());
        assert_eq!(
            reported_path.file_name().and_then(|name| name.to_str()),
            Some(expected_file_name.as_str())
        );
    });
}

#[test]
fn artifacts_command_reports_url_artifacts() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("run");
        store
            .record_url_artifact(&run.id, "frontend_url", "https://example.test/")
            .expect("record URL artifact");

        let (output, _) = artifacts(&run.id).expect("artifacts");
        let RunsOutput::Artifacts(output) = output else {
            panic!("expected artifacts output");
        };
        assert_eq!(output.artifacts.len(), 1);
        assert_eq!(output.artifacts[0].kind, "frontend_url");
        assert_eq!(output.artifacts[0].artifact_type, "url");
        assert_eq!(
            output.artifacts[0].url.as_deref(),
            Some("https://example.test/")
        );
    });
}

#[test]
fn runner_job_artifact_listing_includes_related_lab_run_artifacts() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let job_id = "job-123";
        let runner_run = RunRecord {
            id: "runner-exec-lab-job-123".to_string(),
            kind: "runner-exec".to_string(),
            component_id: None,
            started_at: "2026-06-12T00:00:00Z".to_string(),
            finished_at: None,
            status: "running".to_string(),
            command: Some("homeboy bench".to_string()),
            cwd: Some("/srv/homeboy/project".to_string()),
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: serde_json::json!({
                "lab": {
                    "runner": { "id": "lab" },
                    "remote_job": { "id": job_id }
                }
            }),
        };
        store.upsert_imported_run(&runner_run).expect("runner run");
        let remote_run = RunRecord {
            id: "bench-run-1".to_string(),
            kind: "bench".to_string(),
            component_id: Some("homeboy".to_string()),
            started_at: "2026-06-12T00:00:01Z".to_string(),
            finished_at: Some("2026-06-12T00:10:00Z".to_string()),
            status: "pass".to_string(),
            command: Some("homeboy bench".to_string()),
            cwd: Some("/srv/homeboy/project".to_string()),
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: serde_json::json!({
                "lab": { "remote_job_id": job_id }
            }),
        };
        store.upsert_imported_run(&remote_run).expect("remote run");
        let summary = home.path().join("homeboy-summary.json");
        std::fs::write(&summary, br#"{"passed":true}"#).expect("summary");
        let artifact = store
            .record_artifact(&remote_run.id, "homeboy_summary", &summary)
            .expect("artifact");

        let artifacts = runs_service::related_lab_artifacts_for_runner_job(&store, &runner_run)
            .expect("related artifacts");

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].id, artifact.id);
        assert_eq!(artifacts[0].run_id, remote_run.id);
    });
}

#[test]
fn bench_artifact_listing_does_not_include_sibling_lab_run_artifacts() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let job_id = "job-123";
        let requested_run = RunRecord {
            id: "bench-run-1".to_string(),
            kind: "bench".to_string(),
            component_id: Some("homeboy".to_string()),
            started_at: "2026-06-12T00:00:00Z".to_string(),
            finished_at: Some("2026-06-12T00:10:00Z".to_string()),
            status: "fail".to_string(),
            command: Some("homeboy bench".to_string()),
            cwd: Some("/srv/homeboy/project".to_string()),
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: serde_json::json!({
                "lab": {
                    "runner": { "id": "lab" },
                    "remote_job_id": job_id
                }
            }),
        };
        store
            .upsert_imported_run(&requested_run)
            .expect("requested run");
        let sibling_run = RunRecord {
            id: "bench-run-2".to_string(),
            kind: "bench".to_string(),
            component_id: Some("homeboy".to_string()),
            started_at: "2026-06-12T00:00:01Z".to_string(),
            finished_at: Some("2026-06-12T00:10:00Z".to_string()),
            status: "pass".to_string(),
            command: Some("homeboy bench".to_string()),
            cwd: Some("/srv/homeboy/project".to_string()),
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: serde_json::json!({
                "lab": { "remote_job_id": job_id }
            }),
        };
        store
            .upsert_imported_run(&sibling_run)
            .expect("sibling run");
        let requested_summary = home.path().join("requested-summary.json");
        std::fs::write(&requested_summary, br#"{"passed":false}"#).expect("summary");
        let requested_artifact = store
            .record_artifact(&requested_run.id, "bench_results", &requested_summary)
            .expect("requested artifact");
        let sibling_summary = home.path().join("sibling-summary.json");
        std::fs::write(&sibling_summary, br#"{"passed":true}"#).expect("summary");
        store
            .record_artifact(&sibling_run.id, "bench_results", &sibling_summary)
            .expect("sibling artifact");

        let artifacts =
            runs_service::list_artifacts_for_run(&store, &requested_run.id).expect("artifacts");

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].id, requested_artifact.id);
        assert_eq!(artifacts[0].run_id, requested_run.id);
    });
}

#[test]
fn runner_job_show_keeps_local_evidence_when_refresh_runner_is_unavailable() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = RunRecord {
            id: "runner-exec-missing-lab-job-123".to_string(),
            kind: "runner-exec".to_string(),
            component_id: None,
            started_at: "2026-06-12T00:00:00Z".to_string(),
            finished_at: None,
            status: "running".to_string(),
            command: Some("homeboy bench".to_string()),
            cwd: Some("/srv/homeboy/project".to_string()),
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: serde_json::json!({
                "lab": {
                    "runner": { "id": "missing-lab" },
                    "remote_job": { "id": "job-123" }
                }
            }),
        };
        store.upsert_imported_run(&run).expect("runner run");

        let (output, exit_code) = show_run(&run.id).expect("show local evidence");

        assert_eq!(exit_code, 0);
        let RunsOutput::Show(output) = output else {
            panic!("expected show output");
        };
        assert_eq!(output.run.summary.id, run.id);
        assert_eq!(output.run.summary.status, "running");
    });
}

#[test]
fn runner_job_artifacts_keep_local_evidence_when_refresh_runner_is_unavailable() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = RunRecord {
            id: "runner-exec-missing-lab-job-456".to_string(),
            kind: "runner-exec".to_string(),
            component_id: None,
            started_at: "2026-06-12T00:00:00Z".to_string(),
            finished_at: None,
            status: "running".to_string(),
            command: Some("homeboy bench".to_string()),
            cwd: Some("/srv/homeboy/project".to_string()),
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: serde_json::json!({
                "lab": {
                    "runner": { "id": "missing-lab" },
                    "remote_job": { "id": "job-456" }
                }
            }),
        };
        store.upsert_imported_run(&run).expect("runner run");
        let local = home.path().join("timeout-note.txt");
        std::fs::write(&local, b"still readable").expect("artifact");
        let artifact = store
            .record_artifact(&run.id, "timeout_note", &local)
            .expect("artifact");

        let (output, exit_code) = artifacts(&run.id).expect("artifacts local evidence");

        assert_eq!(exit_code, 0);
        let RunsOutput::Artifacts(output) = output else {
            panic!("expected artifacts output");
        };
        assert_eq!(output.artifacts.len(), 1);
        assert_eq!(output.artifacts[0].id, artifact.id);
    });
}

#[test]
fn artifact_get_copies_registered_file_without_raw_path_lookup() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("run");
        let artifact_path = home.path().join("bench-results.json");
        std::fs::write(&artifact_path, br#"{"ok":true}"#).expect("artifact");
        let artifact = store
            .record_artifact(&run.id, "bench_results", &artifact_path)
            .expect("record artifact");
        let output_path = home.path().join("downloaded.json");

        let (output, _) = artifact_get(RunsArtifactGetArgs {
            run_id: run.id.clone(),
            artifact_id: artifact.id.clone(),
            output: Some(output_path.clone()),
        })
        .expect("get artifact");

        let RunsOutput::ArtifactGet(output) = output else {
            panic!("expected artifact get output");
        };
        assert_eq!(output.command, "runs.artifact.get");
        assert_eq!(output.artifact_id, artifact.id);
        assert_eq!(
            std::fs::read(&output_path).expect("downloaded"),
            br#"{"ok":true}"#
        );

        let err = match artifact_get(RunsArtifactGetArgs {
            run_id: run.id,
            artifact_id: artifact_path.display().to_string(),
            output: Some(home.path().join("bad.json")),
        }) {
            Ok(_) => panic!("raw paths are not accepted as artifact ids"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("artifact record not found"));
    });
}

#[test]
fn artifact_get_fetches_nested_publication_artifact_store_ref() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("run");
        let locator = "homeboy/workflow-bench/runs/run-1/artifacts/scenario/adapter/attempt-1/nested-result.json";
        let package_root = home.path().join("publication-package");
        let nested_source = package_root.join("scenario/adapter/attempt-1/nested-result.json");
        std::fs::create_dir_all(nested_source.parent().expect("nested parent"))
            .expect("create nested parent");
        std::fs::write(&nested_source, br#"{"nested":true}"#).expect("nested bytes");
        let manifest_path = package_root.join("manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::json!({
                "id": "publication-run-1",
                "artifacts": [{
                    "id": "scenario/adapter/attempt-1/nested-result",
                    "kind": "nested-publication-artifact",
                    "locator": {
                        "type": "artifact-store",
                        "value": locator,
                    },
                    "media_type": "application/json"
                }]
            })
            .to_string(),
        )
        .expect("manifest bytes");
        let manifest_artifact = store
            .record_artifact(&run.id, "publication_manifest", &manifest_path)
            .expect("record manifest");
        let artifact_root = home.path().join(".local/share/homeboy/artifacts");
        let materialized = artifact_root.join(locator);

        assert!(materialized.is_file());
        let nested = store
            .get_artifact_for_run_token(&run.id, "nested-result")
            .expect("lookup nested")
            .expect("nested artifact indexed");
        assert_ne!(nested.id, manifest_artifact.id);
        assert_eq!(nested.path, materialized.to_string_lossy());

        let output_path = home.path().join("downloaded-nested.json");
        let (output, _) = artifact_get(RunsArtifactGetArgs {
            run_id: run.id.clone(),
            artifact_id: "nested-result".to_string(),
            output: Some(output_path.clone()),
        })
        .expect("get nested artifact");

        let RunsOutput::ArtifactGet(output) = output else {
            panic!("expected artifact get output");
        };
        assert_eq!(output.command, "runs.artifact.get");
        assert_eq!(output.artifact_id, nested.id);
        assert_eq!(
            std::fs::read(&output_path).expect("downloaded nested"),
            br#"{"nested":true}"#
        );
    });
}

#[test]
fn artifacts_command_derives_viewer_links_from_public_artifact_url_metadata() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let public_artifact_base = serve_public_artifact_base_once(200);
        let _artifact_url = EnvGuard::set(
            homeboy::core::artifacts::PUBLIC_ARTIFACT_BASE_URL_ENV,
            &public_artifact_base,
        );
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("run");
        let locator = "homeboy/workflow-bench/runs/run-1/artifacts/scenario/adapter/attempt-1/replay.json";
        let package_root = home.path().join("publication-package");
        let nested_source = package_root.join("scenario/adapter/attempt-1/replay.json");
        std::fs::create_dir_all(nested_source.parent().expect("nested parent"))
            .expect("create nested parent");
        std::fs::write(&nested_source, br#"{"steps":[]}"#).expect("nested bytes");
        let manifest_path = package_root.join("manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::json!({
                "id": "publication-run-1",
                "artifacts": [{
                    "id": "scenario/adapter/attempt-1/replay",
                    "kind": "replay-artifact",
                    "locator": {
                        "type": "artifact-store",
                        "value": locator,
                    },
                    "contentType": "application/json",
                    "sha256": {
                        "algorithm": "sha256",
                        "value": "abc123"
                    },
                    "viewer": WORDPRESS_PLAYGROUND_BLUEPRINT_VIEWER.to_metadata(Some(serde_json::json!({
                            "status": "partial",
                            "limitations": ["fixture limitation"]
                    })))
                }]
            })
            .to_string(),
        )
        .expect("manifest bytes");
        store
            .record_artifact(&run.id, "publication_manifest", &manifest_path)
            .expect("record manifest");

        let (output, _) = artifacts(&run.id).expect("artifacts");
        let RunsOutput::Artifacts(output) = output else {
            panic!("expected artifacts output");
        };
        let replay = output
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "replay-artifact")
            .expect("replay artifact");
        let artifact_url = replay.public_url.as_deref().expect("public url");
        assert_eq!(
            artifact_url,
            format!(
                "{public_artifact_base}/homeboy/workflow-bench/runs/run-1/artifacts/scenario/adapter/attempt-1/replay.json"
            )
        );
        assert!(!artifact_url.ends_with("/content"));
        assert_eq!(replay.mime.as_deref(), Some("application/json"));
        assert_eq!(replay.sha256.as_deref(), Some("abc123"));
        assert_eq!(
            replay.viewer_links[0].kind,
            "wordpress-playground-blueprint"
        );
        assert_eq!(
            replay.viewer_url.as_deref(),
            Some(replay.viewer_links[0].url.as_str())
        );
        assert!(replay.viewer_links[0]
            .url
            .starts_with("https://playground.wordpress.net/?blueprint-url="));
        assert!(replay.viewer_links[0]
            .url
            .contains("http%3A%2F%2F127.0.0.1%3A"));
        assert_eq!(
            replay.metadata_json["public_url_validation"]["reachable"],
            true
        );
        assert_eq!(
            replay.viewer_links[0]
                .replay
                .as_ref()
                .and_then(|replay| replay.get("status"))
                .and_then(Value::as_str),
            Some("partial")
        );
        assert_eq!(
            replay.metadata_json["viewer"]["query"]["value"]["source"],
            "public-artifact-url"
        );
    });
}

#[test]
fn artifacts_command_suppresses_viewer_links_when_public_url_is_unreachable() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let public_artifact_base = serve_public_artifact_base_once(404);
        let _artifact_url = EnvGuard::set(
            homeboy::core::artifacts::PUBLIC_ARTIFACT_BASE_URL_ENV,
            &public_artifact_base,
        );
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("run");
        let artifact_path = home.path().join("blueprint.after.json");
        std::fs::write(&artifact_path, br#"{"steps":[]}"#).expect("artifact bytes");
        store
            .record_artifact_with_metadata(
                &run.id,
                "bench_artifact",
                &artifact_path,
                serde_json::json!({
                    "viewer": WORDPRESS_PLAYGROUND_BLUEPRINT_VIEWER.to_metadata(None)
                }),
            )
            .expect("record artifact");

        let (output, _) = artifacts(&run.id).expect("artifacts");
        let RunsOutput::Artifacts(output) = output else {
            panic!("expected artifacts output");
        };
        let artifact = output
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "bench_artifact")
            .expect("bench artifact");

        assert!(artifact.public_url.is_some());
        assert!(artifact.viewer_links.is_empty());
        assert_eq!(artifact.viewer_url, None);
        assert_eq!(
            artifact.metadata_json["public_url_validation"]["reachable"],
            false
        );
        assert_eq!(
            artifact.metadata_json["public_url_validation"]["status_code"],
            404
        );
    });
}

#[test]
fn findings_commands_list_and_show_records() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("lint", "homeboy", "studio", Value::Null))
            .expect("run");
        let recorded = store
            .record_finding(&NewFindingRecord {
                run_id: run.id.clone(),
                tool: "lint".to_string(),
                rule: Some("security".to_string()),
                file: Some("src/foo.php".to_string()),
                line: Some(12),
                severity: Some("error".to_string()),
                fingerprint: Some("src/foo.php::security".to_string()),
                message: "Missing escaping".to_string(),
                fixable: Some(true),
                metadata_json: serde_json::json!({ "category": "security" }),
            })
            .expect("finding");

        let (output, _) = findings::findings(findings::RunsFindingsArgs {
            run_id: run.id,
            tool: Some("lint".to_string()),
            file: Some("src/foo.php".to_string()),
            fingerprint: None,
            limit: 20,
        })
        .expect("list findings");
        let RunsOutput::Findings(output) = output else {
            panic!("expected findings output");
        };
        assert_eq!(output.findings.len(), 1);
        assert_eq!(output.findings[0].id, recorded.id);
        assert_eq!(output.findings[0].finding.message, "Missing escaping");

        let (output, _) = findings::finding(&recorded.id).expect("show finding");
        let RunsOutput::Finding(output) = output else {
            panic!("expected finding output");
        };
        assert_eq!(output.finding.finding.category.as_deref(), Some("security"));
        assert_eq!(output.finding.finding.fix.fixable, Some(true));
    });
}

#[test]
fn latest_run_command_returns_newest_matching_run() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let old = store
            .start_run(sample_run("lint", "homeboy", "studio", Value::Null))
            .expect("old");
        store
            .finish_run(&old.id, RunStatus::Pass, None)
            .expect("finish old");
        let latest = store
            .start_run(sample_run("lint", "homeboy", "studio", Value::Null))
            .expect("latest");
        store
            .finish_run(&latest.id, RunStatus::Fail, None)
            .expect("finish latest");

        let (output, _) = latest::latest_run(latest::RunsLatestRunArgs {
            kind: Some("lint".to_string()),
            component_id: Some("homeboy".to_string()),
            rig: Some("studio".to_string()),
            status: None,
        })
        .expect("latest run");

        let RunsOutput::LatestRun(output) = output else {
            panic!("expected latest run output");
        };
        assert_eq!(output.command, "runs.latest-run");
        assert_eq!(output.run.id, latest.id);
    });
}

#[test]
fn latest_finding_command_uses_latest_matching_run() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let old_run = store
            .start_run(sample_run("lint", "homeboy", "studio", Value::Null))
            .expect("old run");
        store
            .record_finding(&NewFindingRecord {
                run_id: old_run.id.clone(),
                tool: "lint".to_string(),
                rule: Some("security".to_string()),
                file: Some("src/foo.php".to_string()),
                line: Some(12),
                severity: Some("error".to_string()),
                fingerprint: Some("old".to_string()),
                message: "Old finding".to_string(),
                fixable: Some(true),
                metadata_json: serde_json::json!({}),
            })
            .expect("old finding");
        let latest_run = store
            .start_run(sample_run("lint", "homeboy", "studio", Value::Null))
            .expect("latest run");
        let latest_finding = store
            .record_finding(&NewFindingRecord {
                run_id: latest_run.id.clone(),
                tool: "lint".to_string(),
                rule: Some("security".to_string()),
                file: Some("src/foo.php".to_string()),
                line: Some(12),
                severity: Some("error".to_string()),
                fingerprint: Some("latest".to_string()),
                message: "Latest finding".to_string(),
                fixable: Some(true),
                metadata_json: serde_json::json!({}),
            })
            .expect("latest finding");

        let (output, _) = findings::latest_finding(findings::RunsLatestFindingArgs {
            kind: Some("lint".to_string()),
            component_id: Some("homeboy".to_string()),
            rig: Some("studio".to_string()),
            status: None,
            tool: Some("lint".to_string()),
            file: Some("src/foo.php".to_string()),
        })
        .expect("latest finding command");

        let RunsOutput::LatestFinding(output) = output else {
            panic!("expected latest finding output");
        };
        assert_eq!(output.command, "runs.latest-finding");
        assert_eq!(output.run.id, latest_run.id);
        assert_eq!(output.finding.id, latest_finding.id);
        assert_eq!(output.finding.finding.message, "Latest finding");
    });
}

#[test]
fn bench_history_orders_and_filters_by_scenario() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let old = store
            .start_run(sample_run(
                "bench",
                "homeboy",
                "studio",
                serde_json::json!({
                    "scenario_metrics": [{
                        "scenario_id": "cold",
                        "metrics": { "p95_ms": 10.0 }
                    }]
                }),
            ))
            .expect("old");
        store
            .finish_run(&old.id, RunStatus::Pass, None)
            .expect("finish old");
        let new = store
            .start_run(sample_run(
                "bench",
                "homeboy",
                "studio",
                serde_json::json!({
                    "scenario_metrics": [{
                        "scenario_id": "cold",
                        "metrics": { "p95_ms": 12.0 }
                    }]
                }),
            ))
            .expect("new");
        store
            .finish_run(&new.id, RunStatus::Pass, None)
            .expect("finish new");

        let (output, _) = list_runs(
            RunsListArgs {
                runner: None,
                kind: Some("bench".to_string()),
                component_id: Some("homeboy".to_string()),
                rig: Some("studio".to_string()),
                scenario_id: Some("cold".to_string()),
                status: None,
                limit: 20,
                include_active_runner_jobs: false,
            },
            "runs.list",
        )
        .expect("history");
        let RunsOutput::List(output) = output else {
            panic!("expected history output");
        };
        assert_eq!(output.runs.len(), 2);
        assert_eq!(output.runs[0].id, new.id);
        assert_eq!(output.runs[1].id, old.id);
    });
}

#[test]
fn missing_and_mismatched_run_ids_return_clear_errors() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let trace = store
            .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
            .expect("trace");

        let missing = show_run("missing-run").err().expect("missing should fail");
        assert_eq!(missing.code.as_str(), "validation.invalid_argument");
        assert!(missing.message.contains("run record not found"));

        let mismatch = bench_compare(&trace.id, &trace.id, &[])
            .err()
            .expect("kind mismatch should fail");
        assert_eq!(mismatch.code.as_str(), "validation.invalid_argument");
        assert!(mismatch.message.contains("expected 'bench'"));
    });
}

#[test]
fn export_one_run_writes_directory_bundle() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("run");
        store
            .finish_run(&run.id, RunStatus::Pass, None)
            .expect("finish");
        let output = home.path().join("bundle");

        let (result, _) = export_runs(RunsExportArgs {
            run: Some(run.id.clone()),
            since: None,
            output: output.clone(),
        })
        .expect("export");

        let RunsOutput::Export(result) = result else {
            panic!("expected export output");
        };
        assert_eq!(result.run_count, 1);
        assert!(output.join("manifest.json").exists());
        assert!(output.join("runs.json").exists());
        assert!(output.join("artifacts.json").exists());
        assert!(output.join("trace_spans.json").exists());
        assert!(output.join("findings.json").exists());
        assert!(output.join("test_failures.json").exists());
        let runs: Vec<RunRecord> = read_bundle_test_json(&output.join("runs.json"));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);
    });
}

#[test]
fn export_includes_findings_and_test_failures() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("test", "homeboy", "studio", Value::Null))
            .expect("run");
        let lint = store
            .record_finding(&NewFindingRecord {
                run_id: run.id.clone(),
                tool: "lint".to_string(),
                rule: Some("style".to_string()),
                file: Some("src/lib.rs".to_string()),
                line: Some(3),
                severity: Some("warning".to_string()),
                fingerprint: Some("lint::src/lib.rs".to_string()),
                message: "style drift".to_string(),
                fixable: Some(true),
                metadata_json: serde_json::json!({ "record_kind": "lint" }),
            })
            .expect("lint finding");
        let failure = store
            .record_finding(&NewFindingRecord {
                run_id: run.id.clone(),
                tool: "test".to_string(),
                rule: Some("assertion".to_string()),
                file: Some("tests/fail.rs".to_string()),
                line: Some(42),
                severity: Some("error".to_string()),
                fingerprint: Some("test::fails".to_string()),
                message: "assertion failed".to_string(),
                fixable: None,
                metadata_json: serde_json::json!({
                    "record_kind": "failure",
                    "source_sidecar": "test-failures",
                }),
            })
            .expect("test failure");
        let output = home.path().join("findings-bundle");

        let (result, _) = export_runs(RunsExportArgs {
            run: Some(run.id),
            since: None,
            output: output.clone(),
        })
        .expect("export");

        let RunsOutput::Export(result) = result else {
            panic!("expected export output");
        };
        assert_eq!(result.finding_count, 2);
        assert_eq!(result.test_failure_count, 1);
        let findings: Vec<RecordedHomeboyFinding> =
            read_bundle_test_json(&output.join("findings.json"));
        let test_failures: Vec<RecordedHomeboyFinding> =
            read_bundle_test_json(&output.join("test_failures.json"));
        assert_eq!(findings, vec![lint.into(), failure.clone().into()]);
        assert_eq!(test_failures, vec![failure.into()]);
    });
}

#[test]
fn export_since_writes_multiple_runs() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let first = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("first");
        let second = store
            .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
            .expect("second");
        let output = home.path().join("recent-bundle");

        export_runs(RunsExportArgs {
            run: None,
            since: Some("1d".to_string()),
            output: output.clone(),
        })
        .expect("export recent");

        let runs: Vec<RunRecord> = read_bundle_test_json(&output.join("runs.json"));
        let ids = runs
            .iter()
            .map(|run| run.id.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids, BTreeSet::from([first.id, second.id]));
    });
}

#[test]
fn export_artifacts_is_metadata_only() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("run");
        let artifact_path = home.path().join("bench-results.json");
        std::fs::write(&artifact_path, br#"{"ok":true}"#).expect("artifact");
        let artifact = store
            .record_artifact(&run.id, "bench_results", &artifact_path)
            .expect("record artifact");
        let output = home.path().join("artifact-bundle");

        export_runs(RunsExportArgs {
            run: Some(run.id),
            since: None,
            output: output.clone(),
        })
        .expect("export");

        let artifacts: Vec<ArtifactRecord> =
            read_bundle_test_json(&output.join("artifacts.json"));
        assert_eq!(artifacts, vec![artifact]);
        assert!(!output.join("files").exists());
    });
}

#[test]
fn export_rewrites_unproven_remote_artifact_paths_as_metadata_only() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("run");
        store
            .import_artifact(&ArtifactRecord {
                id: "remote-trace".to_string(),
                run_id: run.id.clone(),
                kind: "trace".to_string(),
                artifact_type: "file".to_string(),
                path: "/srv/remote-only/trace.zip".to_string(),
                url: None,
                public_url: None,
                viewer_url: None,
                viewer_links: Vec::new(),
                sha256: None,
                size_bytes: None,
                mime: None,
                metadata_json: serde_json::json!({}),
                created_at: chrono::Utc::now().to_rfc3339(),
            })
            .expect("artifact");
        let output = home.path().join("remote-artifact-bundle");

        export_runs(RunsExportArgs {
            run: Some(run.id),
            since: None,
            output: output.clone(),
        })
        .expect("export");

        let artifacts: Vec<ArtifactRecord> =
            read_bundle_test_json(&output.join("artifacts.json"));
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_type, "metadata-only");
        assert_eq!(artifacts[0].path, "metadata-only:trace.zip");
    });
}

#[test]
fn export_trace_spans_when_present() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
            .expect("run");
        let span = store
            .record_trace_span(
                NewTraceSpanRecord::builder(&run.id, "boot", "ok")
                    .duration_ms(Some(12.5))
                    .from_event(Some("start"))
                    .to_event(Some("ready"))
                    .metadata(serde_json::json!({ "phase": "cold" }))
                    .build(),
            )
            .expect("span");
        let output = home.path().join("trace-bundle");

        export_runs(RunsExportArgs {
            run: Some(run.id),
            since: None,
            output: output.clone(),
        })
        .expect("export");

        let spans: Vec<TraceSpanRecord> =
            read_bundle_test_json(&output.join("trace_spans.json"));
        assert_eq!(spans, vec![span]);
    });
}

#[test]
fn import_into_empty_db_and_reimport_is_idempotent() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let bundle = home.path().join("portable-bundle");
        let run_id = {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("trace", "homeboy", "studio", Value::Null))
                .expect("run");
            let artifact_path = home.path().join("trace.json");
            std::fs::write(&artifact_path, b"{}").expect("artifact");
            store
                .record_artifact(&run.id, "trace_results", &artifact_path)
                .expect("artifact record");
            store
                .record_trace_span(
                    NewTraceSpanRecord::builder(&run.id, "first", "ok")
                        .duration_ms(Some(1.0))
                        .to_event(Some("done"))
                        .build(),
                )
                .expect("span");
            store
                .record_finding(&NewFindingRecord {
                    run_id: run.id.clone(),
                    tool: "test".to_string(),
                    rule: Some("assertion".to_string()),
                    file: Some("tests/fail.rs".to_string()),
                    line: Some(42),
                    severity: Some("error".to_string()),
                    fingerprint: Some("test::fails".to_string()),
                    message: "assertion failed".to_string(),
                    fixable: None,
                    metadata_json: serde_json::json!({ "record_kind": "failure" }),
                })
                .expect("finding");
            export_runs(RunsExportArgs {
                run: Some(run.id.clone()),
                since: None,
                output: bundle.clone(),
            })
            .expect("export");
            run.id
        };
        std::fs::remove_file(home.path().join(".local/share/homeboy/homeboy.sqlite"))
            .expect("remove db");

        import_runs(RunsImportArgs {
            input: Some(bundle.clone()),
            ..RunsImportArgs::default()
        })
        .expect("import");
        import_runs(RunsImportArgs {
            input: Some(bundle.clone()),
            ..RunsImportArgs::default()
        })
        .expect("second import is idempotent");

        let store = ObservationStore::open_initialized().expect("store");
        assert!(store.get_run(&run_id).expect("get").is_some());
        assert_eq!(store.list_artifacts(&run_id).expect("artifacts").len(), 1);
        assert_eq!(store.list_trace_spans(&run_id).expect("spans").len(), 1);
        assert_eq!(
            store
                .list_findings(FindingListFilter {
                    run_id: Some(run_id),
                    ..FindingListFilter::default()
                })
                .expect("findings")
                .len(),
            1
        );
    });
}

#[test]
fn malformed_bundle_validation_fails_clearly() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let bundle = home.path().join("bad-bundle");
        std::fs::create_dir_all(&bundle).expect("bundle dir");
        std::fs::write(bundle.join("manifest.json"), "not json").expect("manifest");

        let err = match import_runs(RunsImportArgs {
            input: Some(bundle),
            ..RunsImportArgs::default()
        }) {
            Ok(_) => panic!("malformed bundle should fail"),
            Err(err) => err,
        };

        assert_eq!(err.code.as_str(), "validation.invalid_json");
    });
}

#[test]
fn conflicting_existing_rows_fail_clearly() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("run");
        let bundle = home.path().join("conflict-bundle");
        export_runs(RunsExportArgs {
            run: Some(run.id.clone()),
            since: None,
            output: bundle.clone(),
        })
        .expect("export");
        let mut runs: Vec<RunRecord> = read_bundle_test_json(&bundle.join("runs.json"));
        runs[0].status = "pass".to_string();
        std::fs::write(
            bundle.join("runs.json"),
            serde_json::to_string_pretty(&runs).expect("json"),
        )
        .expect("rewrite runs");

        let err = match import_runs(RunsImportArgs {
            input: Some(bundle),
            ..RunsImportArgs::default()
        }) {
            Ok(_) => panic!("conflicting import should fail"),
            Err(err) => err,
        };

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err
            .message
            .contains("conflicts with imported bundle record"));
    });
}

fn read_bundle_test_json<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read json")).expect("json")
}
