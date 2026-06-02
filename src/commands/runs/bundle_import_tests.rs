use std::path::Path;

use homeboy::core::observation::{
    ArtifactRecord, FindingListFilter, NewFindingRecord, NewRunRecord, NewTraceSpanRecord,
    ObservationStore, RecordedHomeboyFinding, RunListFilter, RunRecord, TraceSpanRecord,
};
use homeboy::test_support::with_isolated_home;
use serde::Deserialize;
use serde_json::Value;

use super::bundle::{export_runs, import_runs, RunsExportArgs, RunsImportArgs};

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

fn read_bundle_test_json<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read json")).expect("json")
}
