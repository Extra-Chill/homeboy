//! Service-level coverage. The CLI adapter in `commands::runs` keeps the
//! full integration coverage (JSON shape, markdown, error messages); here
//! we exercise the standalone service surface so callers outside the CLI
//! can rely on it without re-deriving guarantees from the command tests.

use super::*;
use crate::core::observation::NewRunRecord;
use crate::test_support::with_isolated_home;
use serde_json::Value;

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

fn sample_run(kind: &str) -> NewRunRecord {
    NewRunRecord::builder(kind)
        .component_id("homeboy")
        .command(format!("homeboy {kind}"))
        .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
        .homeboy_version("test-version")
        .git_sha(Some("abc123".to_string()))
        .rig_id("studio")
        .metadata(Value::Null)
        .build()
}

#[test]
fn require_run_returns_validation_error_for_missing_run() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let err = require_run(&store, "missing-run").expect_err("missing");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("run record not found"));
    });
}

#[test]
fn terminal_retention_is_dry_run_by_default_and_deletes_dependent_records_on_apply() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let terminal = store.start_run(sample_run("bench")).expect("terminal run");
        store
            .finish_run(&terminal.id, RunStatus::Pass, None)
            .expect("finish run");
        store
            .record_url_artifact(&terminal.id, "report", "https://example.test/report")
            .expect("artifact");
        let active = store.start_run(sample_run("trace")).expect("active run");
        let path = store.status().expect("status").path;
        drop(store);
        let db = rusqlite::Connection::open(path).expect("raw db");
        db.execute(
            "UPDATE runs SET finished_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
            [&terminal.id],
        )
        .expect("age terminal run");

        let dry = retain_terminal_runs(TerminalRunRetentionOptions {
            apply: false,
            older_than_days: 1,
            limit: 10,
        })
        .expect("dry run");
        assert_eq!(dry.candidate_run_ids, vec![terminal.id.clone()]);

        let applied = retain_terminal_runs(TerminalRunRetentionOptions {
            apply: true,
            older_than_days: 1,
            limit: 10,
        })
        .expect("apply");
        assert_eq!(applied.removed_run_count, 1);
        let store = ObservationStore::open_initialized().expect("reopen");
        assert!(store
            .get_run(&terminal.id)
            .expect("terminal read")
            .is_none());
        assert!(store
            .list_artifacts(&terminal.id)
            .expect("artifact read")
            .is_empty());
        assert!(store.get_run(&active.id).expect("active read").is_some());
    });
}

#[test]
fn missing_run_guidance_prints_runner_routed_retrieval_commands() {
    let hints = missing_run_guidance_for_runner_ids("run-123", vec!["homeboy-lab".to_string()]);
    assert_eq!(
        hints,
        vec![
            "Check runner `homeboy-lab` from the controller: `homeboy runs list --runner homeboy-lab --limit 100`.",
            "Inspect run `run-123` directly on runner `homeboy-lab`: `homeboy runner exec homeboy-lab -- homeboy runs show run-123`.",
            "List artifacts for run `run-123` directly on runner `homeboy-lab`: `homeboy runner exec homeboy-lab -- homeboy runs artifacts run-123`.",
            "Export run `run-123` directly on runner `homeboy-lab`: `homeboy runner exec homeboy-lab -- homeboy runs export --run run-123 --output <dir>`.",
        ]
    );
}

#[test]
fn list_artifacts_for_run_enriches_url_artifact_links() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store.start_run(sample_run("bench")).expect("run");
        store
            .record_url_artifact(&run.id, "frontend_url", "https://example.test/")
            .expect("record URL artifact");

        let artifacts = list_artifacts_for_run(&store, &run.id).expect("artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_type, "url");
        // URL artifacts are enriched: public_url is filled in from the
        // recorded URL so downstream consumers don't need to re-derive it.
        assert_eq!(
            artifacts[0].public_url.as_deref(),
            Some("https://example.test/")
        );
    });
}

#[test]
fn resolve_artifact_for_run_rejects_unknown_artifact_id() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store.start_run(sample_run("bench")).expect("run");
        let err = resolve_artifact_for_run(&store, &run.id, "missing-artifact")
            .expect_err("missing artifact");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("artifact record not found"));
        // With no artifacts recorded, the error is explicit rather than
        // listing phantom names.
        assert!(err.message.contains("no recorded artifacts"));
    });
}

#[test]
fn resolve_artifact_for_run_unknown_id_lists_available_names() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store.start_run(sample_run("bench")).expect("run");
        let source = home.path().join("bench-results.json");
        std::fs::write(&source, br#"{"ok":true}"#).expect("source");
        store
            .record_artifact(&run.id, "bench_results", &source)
            .expect("record");

        let err = resolve_artifact_for_run(&store, &run.id, "does-not-exist")
            .expect_err("unknown artifact");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(
            err.message.contains("available artifact names")
                && err.message.contains("bench_results"),
            "expected available names listing the recorded kind, got: {}",
            err.message
        );
    });
}

#[test]
fn copy_local_file_artifact_writes_bytes_and_reports_metadata() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store.start_run(sample_run("bench")).expect("run");
        let source = home.path().join("bench-results.json");
        std::fs::write(&source, br#"{"ok":true}"#).expect("source");
        let artifact = store
            .record_artifact(&run.id, "bench_results", &source)
            .expect("record");

        let dest = home.path().join("downloaded.json");
        let outcome = copy_local_file_artifact(artifact.clone(), Some(dest.clone())).expect("copy");
        assert_eq!(outcome.run_id, run.id);
        assert_eq!(outcome.artifact_id, artifact.id);
        assert_eq!(outcome.output_path, dest);
        assert_eq!(std::fs::read(&dest).expect("downloaded"), br#"{"ok":true}"#);
    });
}

#[test]
fn classify_artifact_storage_recognizes_local_remote_and_metadata_only() {
    let mut artifact = ArtifactRecord {
        id: "a1".into(),
        run_id: "r1".into(),
        kind: "bench".into(),
        artifact_type: "file".into(),
        path: "/tmp/local".into(),
        url: None,
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: None,
        size_bytes: None,
        mime: None,
        metadata_json: Value::Null,
        created_at: "2026-06-12T00:00:00Z".into(),
    };
    assert_eq!(
        classify_artifact_storage(&artifact),
        ArtifactStorage::LocalFile
    );
    artifact.artifact_type = "metadata-only".into();
    artifact.path = "metadata-only:trace.zip".into();
    assert_eq!(
        classify_artifact_storage(&artifact),
        ArtifactStorage::MetadataOnly
    );
    artifact.artifact_type = "remote_file".into();
    assert_eq!(
        classify_artifact_storage(&artifact),
        ArtifactStorage::Remote
    );
    artifact.artifact_type = "url".into();
    assert_eq!(classify_artifact_storage(&artifact), ArtifactStorage::Other);
}

fn remote_artifact(id: &str, kind: &str) -> ArtifactRecord {
    ArtifactRecord {
        id: id.into(),
        run_id: "run-1".into(),
        kind: kind.into(),
        artifact_type: "remote_file".into(),
        path: format!("runner-artifact://homeboy-lab/run-1/{id}"),
        url: None,
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: None,
        size_bytes: None,
        mime: None,
        metadata_json: serde_json::json!({ "role": "matrix-finding-packets" }),
        created_at: "2026-06-26T00:00:00Z".into(),
    }
}

#[test]
fn hydrate_remote_artifacts_rewrites_remote_packets_into_readable_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let packet_path = temp.path().join("finding-packets.json");
    std::fs::write(
        &packet_path,
        br#"{"finding_packets":[{"diagnostic_kind":"missing_title","fixture_id":"home"}]}"#,
    )
    .expect("write packet");

    let local = ArtifactRecord {
        id: "local".into(),
        run_id: "run-1".into(),
        kind: "summary".into(),
        artifact_type: "file".into(),
        path: temp.path().join("summary.json").display().to_string(),
        url: None,
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: None,
        size_bytes: None,
        mime: None,
        metadata_json: Value::Null,
        created_at: "2026-06-26T00:00:00Z".into(),
    };
    let remote = remote_artifact("finding-packets", "bench_artifact");

    let mut downloads = 0;
    let (hydrated, diagnostics) = hydrate_remote_artifacts(
        vec![local.clone(), remote.clone()],
        |_| true,
        |artifact| {
            downloads += 1;
            assert_eq!(artifact.id, "finding-packets");
            Ok(ArtifactFetchOutcome {
                run_id: artifact.run_id.clone(),
                artifact_id: artifact.id.clone(),
                output_path: packet_path.clone(),
                content_type: Some("application/json".into()),
                size_bytes: Some(42),
                sha256: None,
                artifact_ref: None,
            })
        },
    );

    // Only the remote artifact triggers a download; the local file is left
    // untouched.
    assert_eq!(downloads, 1);
    assert!(diagnostics.is_empty());
    assert_eq!(hydrated[0].artifact_type, "file");
    assert_eq!(hydrated[0].path, local.path);
    let hydrated_remote = &hydrated[1];
    assert_eq!(hydrated_remote.artifact_type, "file");
    assert_eq!(hydrated_remote.path, packet_path.display().to_string());
    assert_eq!(hydrated_remote.mime.as_deref(), Some("application/json"));

    // The hydrated record now parses into a non-zero matrix summary, where
    // the un-hydrated `remote_file` record would have summarized 0.
    let summary = crate::core::artifacts::summarize_matrix_artifacts("run-1", &hydrated, &[])
        .expect("summary");
    assert_eq!(summary.finding_count, 1);
    assert_eq!(summary.top_diagnostic_kinds[0].key, "missing_title");
    assert_eq!(summary.top_fixtures[0].key, "home");

    let zero = crate::core::artifacts::summarize_matrix_artifacts("run-1", &[remote], &[])
        .expect("summary");
    assert_eq!(zero.finding_count, 0);
}

#[test]
fn hydrate_remote_artifacts_records_diagnostic_without_aborting() {
    let remote = remote_artifact("finding-packets", "bench_artifact");
    let (hydrated, diagnostics) = hydrate_remote_artifacts(
        vec![remote.clone()],
        |_| true,
        |_| Err(Error::internal_unexpected("runner offline")),
    );
    assert_eq!(hydrated.len(), 1);
    // The original record is preserved on failure.
    assert_eq!(hydrated[0].artifact_type, "remote_file");
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].contains("could not be hydrated"));
    assert!(diagnostics[0].contains("runner offline"));
}

#[test]
fn hydrate_remote_artifacts_skips_non_selected_remote_artifacts() {
    let remote = remote_artifact("trace.zip", "trace");
    let mut downloads = 0;
    let (hydrated, diagnostics) = hydrate_remote_artifacts(
        vec![remote],
        |artifact| artifact.kind == "bench_artifact",
        |_| {
            downloads += 1;
            Err(Error::internal_unexpected("should not download"))
        },
    );
    assert_eq!(downloads, 0);
    assert!(diagnostics.is_empty());
    assert_eq!(hydrated[0].artifact_type, "remote_file");
}

fn labeled_run(id: &str, command: &str, metadata: Value) -> RunRecord {
    RunRecord {
        id: id.into(),
        kind: "bench.matrix".into(),
        component_id: Some("homeboy".into()),
        started_at: "2026-06-26T00:00:00Z".into(),
        finished_at: None,
        status: "fail".into(),
        command: Some(command.into()),
        cwd: None,
        homeboy_version: None,
        git_sha: None,
        rig_id: None,
        metadata_json: metadata,
    }
}

#[test]
fn match_run_label_resolves_label_from_command_id_and_metadata() {
    let cases = [
        ("persisted-id", "", serde_json::json!(null)),
        (
            "command-label",
            "--run-id command-label",
            serde_json::json!(null),
        ),
        (
            "command-equals-label",
            "--run-id=command-equals-label",
            serde_json::json!(null),
        ),
        (
            "requested",
            "",
            serde_json::json!({ "requested_run_id": "requested" }),
        ),
        (
            "lab-label",
            "",
            serde_json::json!({ "lab": { "run_label": "lab-label" } }),
        ),
        (
            "lab-explicit",
            "",
            serde_json::json!({ "lab": { "explicit_run_id": "lab-explicit" } }),
        ),
        (
            "lab-requested",
            "",
            serde_json::json!({ "lab": { "requested_run_id": "lab-requested" } }),
        ),
        (
            "lab-mirror",
            "",
            serde_json::json!({ "lab": { "mirror_run_id": "lab-mirror" } }),
        ),
        (
            "proof",
            "",
            serde_json::json!({ "proof": { "provenance": { "run_id": "proof" } } }),
        ),
        (
            "caller",
            "",
            serde_json::json!({ "caller_run_id": "caller" }),
        ),
        (
            "mirror",
            "",
            serde_json::json!({ "mirror_run_id": "mirror" }),
        ),
        (
            "persisted",
            "",
            serde_json::json!({ "persisted_run_id": "persisted" }),
        ),
        ("run", "", serde_json::json!({ "run_id": "run" })),
    ];
    let runs = cases
        .iter()
        .enumerate()
        .map(|(index, (label, command, metadata))| {
            let id = if index == 0 {
                (*label).to_string()
            } else {
                format!("uuid-{index}")
            };
            labeled_run(&id, command, metadata.clone())
        })
        .collect::<Vec<_>>();

    for (index, (label, _, _)) in cases.iter().enumerate() {
        assert_eq!(
            match_run_label(&runs, label).map(|run| run.id),
            Some(runs[index].id.clone()),
            "label source `{label}` should resolve"
        );
    }

    // An unknown label matches nothing.
    assert!(match_run_label(&runs, "nope").is_none());
}

#[test]
fn resolve_run_id_or_label_returns_local_run_id_unchanged() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store.start_run(sample_run("bench.matrix")).expect("run");
        let resolved = resolve_run_id_or_label(&store, &run.id).expect("resolve existing run id");
        assert_eq!(resolved, run.id);
    });
}

#[test]
fn require_run_resolves_requested_run_id_metadata_alias() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(
                NewRunRecord::builder("bench")
                    .component_id("homeboy")
                    .command("homeboy bench homeboy --run-id proof-label")
                    .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
                    .rig_id("studio")
                    .metadata(serde_json::json!({ "requested_run_id": "proof-label" }))
                    .build(),
            )
            .expect("run");

        let resolved = require_run(&store, "proof-label").expect("alias resolves");
        assert_eq!(resolved.id, run.id);
        assert_eq!(
            resolve_run_id_or_label(&store, "proof-label").expect("alias id"),
            run.id
        );
    });
}

#[test]
fn list_artifacts_for_run_resolves_requested_run_id_alias() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(
                NewRunRecord::builder("bench")
                    .component_id("homeboy")
                    .command("homeboy bench homeboy --run-id proof-label")
                    .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
                    .rig_id("studio")
                    .metadata(serde_json::json!({ "requested_run_id": "proof-label" }))
                    .build(),
            )
            .expect("run");
        let source = home.path().join("bench-results.json");
        std::fs::write(&source, br#"{"ok":true}"#).expect("source");
        store
            .record_artifact(&run.id, "bench_results", &source)
            .expect("record");

        let artifacts = list_artifacts_for_run(&store, "proof-label").expect("artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].run_id, run.id);
        assert_eq!(artifacts[0].kind, "bench_results");
    });
}

#[test]
fn require_run_rejects_ambiguous_requested_run_id_alias() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let first = store
            .start_run(
                NewRunRecord::builder("bench")
                    .component_id("homeboy")
                    .cwd_path(std::path::Path::new("/tmp/homeboy-fixture-a"))
                    .rig_id("studio-a")
                    .metadata(serde_json::json!({ "requested_run_id": "shared-label" }))
                    .build(),
            )
            .expect("first run");
        let second = store
            .start_run(
                NewRunRecord::builder("bench")
                    .component_id("homeboy")
                    .cwd_path(std::path::Path::new("/tmp/homeboy-fixture-b"))
                    .rig_id("studio-b")
                    .metadata(serde_json::json!({ "requested_run_id": "shared-label" }))
                    .build(),
            )
            .expect("second run");

        let err = require_run(&store, "shared-label").expect_err("ambiguous label");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err
            .message
            .contains("run label `shared-label` is ambiguous"));
        let joined = err
            .details
            .get("tried")
            .and_then(Value::as_array)
            .expect("disambiguation entries")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains(&first.id));
        assert!(joined.contains(&second.id));
        assert!(joined.contains("started_at="));
        assert!(joined.contains("component=homeboy"));
        assert!(joined.contains("rig=studio-a"));
        assert!(joined.contains("rig=studio-b"));
    });
}
