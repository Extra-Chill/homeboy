use std::path::Path;

use homeboy::core::observation::{
    ArtifactRecord, FindingListFilter, NewFindingRecord, NewRunRecord, NewTraceSpanRecord,
    ObservationStore, RecordedHomeboyFinding, RunListFilter, RunRecord, TraceSpanRecord,
};
use homeboy::test_support::with_isolated_home;
use serde::Deserialize;
use serde_json::Value;

use super::bundle::{export_runs, import_runs, RunsExportArgs, RunsImportArgs};
use super::dead_owned_run;
use super::RunsOutput;

struct XdgGuard(Option<String>);

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

#[test]
fn lab_bundle_run_id_conflicts_are_remapped_idempotently() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
            .expect("run");
        let artifact_path = home.path().join("artifact.txt");
        std::fs::write(&artifact_path, "artifact").expect("artifact file");
        store
            .record_artifact(&run.id, "trace-results", &artifact_path)
            .expect("artifact");
        store
            .record_trace_span(
                NewTraceSpanRecord::builder(&run.id, "span-1", "pass")
                    .duration_ms(Some(10.0))
                    .metadata(serde_json::json!({ "step": "fixture" }))
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

        let bundle = home.path().join("lab-conflict-bundle");
        export_runs(RunsExportArgs {
            run: Some(run.id.clone()),
            since: None,
            output: bundle.clone(),
        })
        .expect("export");

        let conflicting_id = "runner-exec-conflict-fixture";
        store
            .import_run(&dead_owned_run(conflicting_id))
            .expect("local conflicting run");

        let mut runs: Vec<RunRecord> = read_bundle_test_json(&bundle.join("runs.json"));
        let original_id = runs[0].id.clone();
        runs[0].id = conflicting_id.to_string();
        runs[0].kind = "runner-exec".to_string();
        runs[0].metadata_json = serde_json::json!({
            "lab": {
                "runner": { "id": "lab" },
                "remote_job_id": "job-123"
            }
        });
        std::fs::write(
            bundle.join("runs.json"),
            serde_json::to_string_pretty(&runs).expect("json"),
        )
        .expect("rewrite runs");

        let mut artifacts: Vec<ArtifactRecord> =
            read_bundle_test_json(&bundle.join("artifacts.json"));
        for artifact in &mut artifacts {
            if artifact.run_id == original_id {
                artifact.run_id = conflicting_id.to_string();
            }
        }
        std::fs::write(
            bundle.join("artifacts.json"),
            serde_json::to_string_pretty(&artifacts).expect("json"),
        )
        .expect("rewrite artifacts");

        let mut spans: Vec<TraceSpanRecord> =
            read_bundle_test_json(&bundle.join("trace_spans.json"));
        for span in &mut spans {
            if span.run_id == original_id {
                span.run_id = conflicting_id.to_string();
            }
        }
        std::fs::write(
            bundle.join("trace_spans.json"),
            serde_json::to_string_pretty(&spans).expect("json"),
        )
        .expect("rewrite spans");

        for file_name in ["findings.json", "test_failures.json"] {
            let mut findings: Vec<RecordedHomeboyFinding> =
                read_bundle_test_json(&bundle.join(file_name));
            for finding in &mut findings {
                if finding.run_id == original_id {
                    finding.run_id = conflicting_id.to_string();
                }
            }
            std::fs::write(
                bundle.join(file_name),
                serde_json::to_string_pretty(&findings).expect("json"),
            )
            .expect("rewrite findings");
        }

        import_runs(RunsImportArgs {
            input: Some(bundle.clone()),
            ..RunsImportArgs::default()
        })
        .expect("import remaps conflict");
        import_runs(RunsImportArgs {
            input: Some(bundle),
            ..RunsImportArgs::default()
        })
        .expect("second import remains idempotent");

        let store = ObservationStore::open_initialized().expect("store");
        let imported = store
            .list_runs(RunListFilter {
                kind: Some("runner-exec".to_string()),
                ..RunListFilter::default()
            })
            .expect("list imported runs")
            .into_iter()
            .find(|run| run.id.starts_with(&format!("{conflicting_id}-imported-")))
            .expect("remapped imported run");
        assert_ne!(imported.id, conflicting_id);
        assert_eq!(
            store.list_artifacts(&imported.id).expect("artifacts").len(),
            1
        );
        assert_eq!(
            store.list_trace_spans(&imported.id).expect("spans").len(),
            1
        );
        assert_eq!(
            store
                .list_findings(FindingListFilter {
                    run_id: Some(imported.id),
                    ..FindingListFilter::default()
                })
                .expect("findings")
                .len(),
            1
        );
    });
}

#[test]
fn runs_export_import_preserves_file_artifact_bytes_with_checksum_refs() {
    let bundle_root = tempfile::tempdir().expect("bundle root");
    let bundle = bundle_root.path().join("fuzz-bundle");
    let mut exported_run_id = String::new();
    let mut bundled_bytes = std::path::PathBuf::new();
    let mut exported_sha256 = None;
    let mut exported_size_bytes = None;

    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("fuzz", "homeboy", "rig-a", Value::Null))
            .expect("run");
        exported_run_id = run.id.clone();
        let artifact_path = home.path().join("fuzz-result-envelope.json");
        std::fs::write(
            &artifact_path,
            br#"{"schema":"homeboy/fuzz-result-envelope/v1"}"#,
        )
        .expect("artifact file");
        store
            .record_artifact(&run.id, "fuzz_result_envelope", &artifact_path)
            .expect("artifact");

        let (output, exit) = export_runs(RunsExportArgs {
            run: Some(run.id.clone()),
            since: None,
            output: bundle.clone(),
        })
        .expect("export");
        assert_eq!(exit, 0);
        let RunsOutput::Export(exported) = output else {
            panic!("expected export output");
        };
        assert_eq!(exported.artifact_count, 1);
        assert_eq!(exported.artifact_byte_count, 1);

        let artifact_bytes: Vec<Value> = read_bundle_test_json(&bundle.join("artifact_bytes.json"));
        assert_eq!(artifact_bytes.len(), 1);
        let byte_ref = artifact_bytes[0]["path"].as_str().expect("byte path");
        bundled_bytes = bundle.join(byte_ref);
        assert_eq!(
            std::fs::read(&bundled_bytes).expect("bundled bytes"),
            br#"{"schema":"homeboy/fuzz-result-envelope/v1"}"#
        );
        assert!(artifact_bytes[0]["sha256"].as_str().is_some());
        exported_sha256 = artifact_bytes[0]["sha256"].as_str().map(str::to_string);
        exported_size_bytes = artifact_bytes[0]["size_bytes"].as_i64();
        assert_eq!(
            exported_size_bytes,
            Some(br#"{"schema":"homeboy/fuzz-result-envelope/v1"}"#.len() as i64)
        );
    });

    with_isolated_home(|_| {
        let _xdg = XdgGuard::unset();
        import_runs(RunsImportArgs {
            input: Some(bundle.clone()),
            ..RunsImportArgs::default()
        })
        .expect("import");
        let store = ObservationStore::open_initialized().expect("store");
        let imported_artifact = store
            .list_artifacts(&exported_run_id)
            .expect("artifacts")
            .into_iter()
            .next()
            .expect("artifact");
        assert_eq!(imported_artifact.artifact_type, "file");
        assert_eq!(imported_artifact.path, bundled_bytes.to_string_lossy());
        assert_eq!(imported_artifact.sha256, exported_sha256);
        assert_eq!(imported_artifact.size_bytes, exported_size_bytes);
    });
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

fn read_bundle_test_json<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read json")).expect("json")
}
