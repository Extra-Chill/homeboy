//! Tests for `runs export` / `runs import` bundle round-tripping.

use std::collections::BTreeSet;
use std::path::Path;

use homeboy::core::observation::{
    ArtifactRecord, FindingListFilter, NewFindingRecord, NewTraceSpanRecord, ObservationStore,
    RecordedHomeboyFinding, RunRecord, RunStatus, TraceSpanRecord,
};
use homeboy::test_support::with_isolated_home;
use serde::Deserialize;
use serde_json::Value;

use super::super::{export_runs, import_runs, RunsExportArgs, RunsImportArgs, RunsOutput};
use super::{sample_run, XdgGuard};

fn read_bundle_test_json<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read json")).expect("json")
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
fn export_embeds_local_file_artifact_bytes() {
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

        let artifacts: Vec<ArtifactRecord> = read_bundle_test_json(&output.join("artifacts.json"));
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].id, artifact.id);
        assert_eq!(artifacts[0].artifact_type, "file");
        assert!(artifacts[0].path.starts_with("bundle://artifact-bytes/"));
        assert_eq!(artifacts[0].size_bytes, Some(11));
        assert!(artifacts[0].sha256.is_some());
        assert!(artifacts[0].metadata_json.get("portable_bundle").is_some());
        assert!(output.join("artifact_bytes.json").exists());
        assert!(output.join("artifact-bytes").exists());
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

        let artifacts: Vec<ArtifactRecord> = read_bundle_test_json(&output.join("artifacts.json"));
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

        let spans: Vec<TraceSpanRecord> = read_bundle_test_json(&output.join("trace_spans.json"));
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
