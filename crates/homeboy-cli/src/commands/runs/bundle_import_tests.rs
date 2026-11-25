use std::path::Path;

use homeboy::core::observation::{
    ArtifactRecord, FindingListFilter, NewFindingRecord, NewTraceSpanRecord, ObservationStore,
    RecordedHomeboyFinding, RunListFilter, RunRecord, TraceSpanRecord,
};
use homeboy::test_support::with_isolated_home;
use serde::Deserialize;
use serde_json::Value;

use super::bundle::{export_runs, import_runs, RunsExportArgs, RunsImportArgs};
use super::dead_owned_run;
use super::RunsOutput;

/// The observation store the enclosing isolated home installs.
///
/// A test is the entry point for its own unit of work, so opening once here
/// is a boundary open, not an ambient one inside production code (#7505).
fn test_store() -> homeboy::core::observation::ObservationStore {
    homeboy::core::observation::ObservationStore::open_initialized().expect("observation store")
}

#[test]
fn lab_bundle_run_id_conflicts_are_remapped_idempotently() {
    with_isolated_home(|home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
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
        export_runs(
            &test_store(),
            RunsExportArgs {
                run: Some(run.id.clone()),
                since: None,
                output: bundle.clone(),
            },
        )
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

        import_runs(
            &test_store(),
            RunsImportArgs {
                input: Some(bundle.clone()),
                ..RunsImportArgs::default()
            },
        )
        .expect("import remaps conflict");
        import_runs(
            &test_store(),
            RunsImportArgs {
                input: Some(bundle),
                ..RunsImportArgs::default()
            },
        )
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
fn runs_export_import_preserves_file_and_directory_artifacts_with_checksum_refs() {
    const ENVELOPE: &[u8] = br#"{"schema":"homeboy/fuzz-result-envelope/v1"}"#;
    let bundle_root = tempfile::tempdir().expect("bundle root");
    let bundle = bundle_root.path().join("fuzz-bundle");
    let mut exported_run_id = String::new();
    let mut exported_refs = Vec::new();

    with_isolated_home(|home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run("fuzz", "homeboy", "rig-a", Value::Null))
            .expect("run");
        exported_run_id = run.id.clone();
        let artifact_path = home.path().join("fuzz-result-envelope.json");
        std::fs::write(&artifact_path, ENVELOPE).expect("artifact file");
        store
            .record_artifact(&run.id, "fuzz_result_envelope", &artifact_path)
            .expect("artifact");
        let artifact_dir = home.path().join("fuzz-artifacts");
        std::fs::create_dir_all(artifact_dir.join("cases")).expect("artifact dir");
        std::fs::write(
            artifact_dir.join("cases/case-1.json"),
            br#"{"id":"case-1"}"#,
        )
        .expect("case file");
        std::fs::write(artifact_dir.join("summary.txt"), "portable directory").expect("summary");
        store
            .record_directory_artifact(&run.id, "fuzz_artifacts", &artifact_dir)
            .expect("directory artifact");

        let (output, exit) = export_runs(
            &test_store(),
            RunsExportArgs {
                run: Some(run.id.clone()),
                since: None,
                output: bundle.clone(),
            },
        )
        .expect("export");
        assert_eq!(exit, 0);
        let RunsOutput::Export(exported) = output else {
            panic!("expected export output");
        };
        assert_eq!(exported.artifact_count, 2);
        assert_eq!(exported.artifact_byte_count, 2);

        let artifacts: Vec<ArtifactRecord> = read_bundle_test_json(&bundle.join("artifacts.json"));
        let directory = artifacts
            .iter()
            .find(|artifact| artifact.artifact_type == "directory")
            .expect("directory artifact");
        assert!(directory.path.starts_with("bundle://artifact-bytes/"));
        assert_eq!(
            directory.metadata_json["portable_bundle"]["archive_format"].as_str(),
            Some("zip")
        );

        let artifact_bytes: Vec<Value> = read_bundle_test_json(&bundle.join("artifact_bytes.json"));
        assert_eq!(artifact_bytes.len(), 2);
        for byte_ref in &artifact_bytes {
            let path = bundle.join(byte_ref["path"].as_str().expect("byte path"));
            assert!(path.is_file());
            let sha256 = byte_ref["sha256"].as_str().map(str::to_string);
            let size_bytes = byte_ref["size_bytes"].as_i64();
            assert!(sha256.is_some());
            if byte_ref["archive_format"].as_str() != Some("zip") {
                assert_eq!(std::fs::read(&path).expect("bundled bytes"), ENVELOPE);
                assert_eq!(size_bytes, Some(ENVELOPE.len() as i64));
            }
            exported_refs.push((sha256, size_bytes));
        }
    });

    with_isolated_home(|_| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        import_runs(
            &test_store(),
            RunsImportArgs {
                input: Some(bundle.clone()),
                ..RunsImportArgs::default()
            },
        )
        .expect("import");
        let store = ObservationStore::open_initialized().expect("store");
        let imported = store.list_artifacts(&exported_run_id).expect("artifacts");
        assert_eq!(imported.len(), 2);
        for artifact in &imported {
            assert!(exported_refs.contains(&(artifact.sha256.clone(), artifact.size_bytes)));
        }

        let file = imported
            .iter()
            .find(|artifact| artifact.artifact_type == "file")
            .expect("imported file");
        assert!(file.path.starts_with(&bundle.to_string_lossy().to_string()));
        assert_eq!(std::fs::read(&file.path).expect("file bytes"), ENVELOPE);

        let directory = imported
            .iter()
            .find(|artifact| artifact.artifact_type == "directory")
            .expect("imported directory");
        let imported_dir = Path::new(&directory.path);
        assert_eq!(
            std::fs::read_to_string(imported_dir.join("summary.txt")).expect("summary"),
            "portable directory"
        );
        assert_eq!(
            std::fs::read(imported_dir.join("cases/case-1.json")).expect("case"),
            br#"{"id":"case-1"}"#
        );
    });
}

use crate::commands::runs::test_support::sample_run;
fn read_bundle_test_json<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read json")).expect("json")
}
