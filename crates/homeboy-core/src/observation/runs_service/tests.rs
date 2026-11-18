//! Service-level coverage. The CLI adapter in `commands::runs` keeps the
//! full integration coverage (JSON shape, markdown, error messages); here
//! we exercise the standalone service surface so callers outside the CLI
//! can rely on it without re-deriving guarantees from the command tests.

use super::*;
use crate::observation::NewRunRecord;
use crate::test_support::with_isolated_home;
use serde_json::Value;
use std::sync::{mpsc, Mutex};

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
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let store = ObservationStore::open_initialized().expect("store");
        let err = require_run(&store, "missing-run").expect_err("missing");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("run record not found"));
    });
}

#[test]
fn terminal_retention_is_dry_run_by_default_and_deletes_owned_lifecycle_on_apply() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let store = ObservationStore::open_initialized().expect("store");
        let terminal = store
            .start_run(sample_run("agent-task"))
            .expect("terminal run");
        store
            .finish_run(&terminal.id, RunStatus::Pass, None)
            .expect("finish run");
        store
            .record_url_artifact(&terminal.id, "report", "https://example.test/report")
            .expect("artifact");
        let artifact_path = crate::artifacts::root()
            .expect("artifact root")
            .join(&terminal.id)
            .join("report.json");
        std::fs::create_dir_all(artifact_path.parent().expect("artifact parent"))
            .expect("artifact parent");
        std::fs::write(&artifact_path, "{}").expect("artifact bytes");
        let local_artifact = store
            .record_artifact(&terminal.id, "local_report", &artifact_path)
            .expect("local artifact");
        let recorded_artifact_path = {
            let path = std::path::PathBuf::from(&local_artifact.path);
            if path.is_absolute() {
                path
            } else {
                crate::artifacts::root().expect("artifact root").join(path)
            }
        };
        let lifecycle_dir = crate::paths::homeboy_data()
            .expect("homeboy data")
            .join("agent-task-runs")
            .join(&terminal.id);
        std::fs::create_dir_all(&lifecycle_dir).expect("lifecycle directory");
        std::fs::write(lifecycle_dir.join("plan.json"), "{}").expect("lifecycle evidence");
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
            age_policy: TerminalRunAgePolicy::resolve(30, Some(1)),
            limit: 10,
        })
        .expect("dry run");
        assert_eq!(dry.candidate_run_ids, vec![terminal.id.clone()]);
        assert_eq!(dry.artifact_cleanup.len(), 1);
        assert_eq!(dry.lifecycle_directories.len(), 1);
        assert!(dry.lifecycle_directories[0].exists);
        assert!(lifecycle_dir.exists());
        assert!(recorded_artifact_path.exists());

        let applied = retain_terminal_runs(TerminalRunRetentionOptions {
            apply: true,
            age_policy: TerminalRunAgePolicy::resolve(30, Some(1)),
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
        assert!(!lifecycle_dir.exists());
        assert!(!recorded_artifact_path.exists());
        assert!(store.get_run(&active.id).expect("active read").is_some());
    });
}

/// Unix-only: blocking a candidate run requires an artifact path that exists
/// but fails the safety revalidation, and a symlink is the portable-enough way
/// to produce that state.
#[cfg(unix)]
#[test]
fn terminal_retention_counts_only_the_runs_it_actually_deleted() {
    with_isolated_home(|home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let store = ObservationStore::open_initialized().expect("store");

        // Two aged terminal runs. One holds an artifact that cleanup must
        // refuse (a symlink), so retention plans it as blocked and deletes
        // only the other one.
        let removable = store.start_run(sample_run("test")).expect("removable run");
        let blocked = store.start_run(sample_run("test")).expect("blocked run");
        for run in [&removable, &blocked] {
            store
                .finish_run(&run.id, RunStatus::Pass, None)
                .expect("finish run");
        }

        let source = home.path().join("source.json");
        std::fs::write(&source, "{}").expect("source bytes");
        let removable_artifact = store
            .record_artifact(&removable.id, "report", &source)
            .expect("removable artifact");
        let blocked_artifact = store
            .record_artifact(&blocked.id, "report", &source)
            .expect("blocked artifact");

        // Replace the recorded bytes with a symlink so classification returns
        // `skip` with `exists = true`, which is what blocks a candidate run.
        let blocked_path = std::path::PathBuf::from(&blocked_artifact.path);
        std::fs::remove_file(&blocked_path).expect("drop blocked bytes");
        std::os::unix::fs::symlink(&source, &blocked_path).expect("symlink blocked bytes");

        let path = store.status().expect("status").path;
        drop(store);
        let db = rusqlite::Connection::open(path).expect("raw db");
        for id in [&removable.id, &blocked.id] {
            db.execute(
                "UPDATE runs SET finished_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
                [id],
            )
            .expect("age terminal run");
        }
        drop(db);

        let applied = retain_terminal_runs(TerminalRunRetentionOptions {
            apply: true,
            age_policy: TerminalRunAgePolicy::resolve(30, Some(1)),
            limit: 10,
        })
        .expect("apply");

        assert_eq!(applied.candidate_run_ids.len(), 2);
        assert_eq!(applied.skipped_run_ids, vec![blocked.id.clone()]);
        // Before the accounting fix this reported `1 - 1 == 0` because the
        // planning-loop skip was subtracted from a set it was never in.
        assert_eq!(applied.removed_run_count, 1);

        let store = ObservationStore::open_initialized().expect("reopen");
        assert!(store
            .get_run(&removable.id)
            .expect("removable read")
            .is_none());
        assert!(store.get_run(&blocked.id).expect("blocked read").is_some());
        assert!(!std::path::Path::new(&removable_artifact.path).exists());
        assert!(std::fs::symlink_metadata(&blocked_path).is_ok());
    });
}

#[test]
fn terminal_retention_keeps_runs_that_own_an_active_task_worktree() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let store = ObservationStore::open_initialized().expect("store");
        let owner = store.start_run(sample_run("agent-task")).expect("owner");
        let released = store.start_run(sample_run("agent-task")).expect("released");
        for run in [&owner, &released] {
            store
                .finish_run(&run.id, RunStatus::Pass, None)
                .expect("finish run");
        }
        // The owner's worktree is still active; the other run's was removed.
        crate::worktree::record_for_run_for_test(
            "fixture@owned",
            &owner.id,
            crate::worktree::TaskWorktreeState::Active,
        );
        crate::worktree::record_for_run_for_test(
            "fixture@released",
            &released.id,
            crate::worktree::TaskWorktreeState::Removed,
        );
        let path = store.status().expect("status").path;
        drop(store);
        let db = rusqlite::Connection::open(path).expect("raw db");
        for id in [&owner.id, &released.id] {
            db.execute(
                "UPDATE runs SET finished_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
                [id],
            )
            .expect("age terminal run");
        }
        drop(db);

        let applied = retain_terminal_runs(TerminalRunRetentionOptions {
            apply: true,
            age_policy: TerminalRunAgePolicy::resolve(30, Some(1)),
            limit: 10,
        })
        .expect("apply");

        assert_eq!(applied.skipped_run_ids, vec![owner.id.clone()]);
        assert_eq!(applied.removed_run_count, 1);
        let store = ObservationStore::open_initialized().expect("reopen");
        assert!(store.get_run(&owner.id).expect("owner read").is_some());
        assert!(store
            .get_run(&released.id)
            .expect("released read")
            .is_none());
    });
}

/// Seed one row in every run-owned child table for `run_id`.
fn seed_run_children(tx: &rusqlite::Transaction<'_>, run_id: &str) {
    let digest = format!("{:0>64}", run_id.len());
    let rows = [
        "INSERT INTO artifacts(id, run_id, kind, path, created_at, artifact_type, url) VALUES (?1 || ':artifact', ?1, 'report', 'https://example.test/r', '2000-01-01T00:00:00Z', 'url', 'https://example.test/r')",
        "INSERT INTO control_plane_action_claims(run_id, idempotency_digest, request_digest, state, owner_pid, accepted_at) VALUES (?1, ?2, ?2, 'completed', 1, '2000-01-01T00:00:00Z')",
        "INSERT INTO control_plane_event_appends(run_id, idempotency_digest, request_digest, event_id, sequence, created_at) VALUES (?1, ?2, ?2, ?1 || ':event', 1, '2000-01-01T00:00:00Z')",
        "INSERT INTO control_plane_work_intents(id, run_id, kind, version, payload_json, created_at) VALUES (?1 || ':intent', ?1, 'test', 1, '{}', '2000-01-01T00:00:00Z')",
        "INSERT INTO findings(id, run_id, tool, message, created_at) VALUES (?1 || ':finding', ?1, 'lint', 'm', '2000-01-01T00:00:00Z')",
        "INSERT INTO trace_spans(id, run_id, span_id, status) VALUES (?1 || ':span', ?1, 'span', 'pass')",
        "INSERT INTO trace_runs(run_id, component_id, scenario_id, status) VALUES (?1, 'homeboy', 'scenario', 'pass')",
    ];
    assert_eq!(
        rows.len(),
        crate::observation::store::RUN_OWNED_CHILD_TABLES.len()
    );
    for sql in rows {
        let uses_digest = sql.contains("?2");
        if uses_digest {
            tx.execute(sql, rusqlite::params![run_id, digest])
        } else {
            tx.execute(sql, rusqlite::params![run_id])
        }
        .expect("seed child row");
    }
}

fn run_child_count(db: &rusqlite::Connection, run_ids: &[String]) -> i64 {
    let mut total = 0;
    for table in crate::observation::store::RUN_OWNED_CHILD_TABLES {
        let mut statement = db
            .prepare(&format!("SELECT COUNT(*) FROM {table} WHERE run_id = ?1"))
            .expect("prepare child count");
        for id in run_ids {
            total += statement
                .query_row([id], |row| row.get::<_, i64>(0))
                .expect("child count");
        }
    }
    total
}

#[test]
fn terminal_retention_batches_deletes_and_applies_per_kind_age_floors() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let store = ObservationStore::open_initialized().expect("store");
        let path = store.status().expect("status").path;
        drop(store);

        let now = Utc::now();
        let ago = |days: i64| (now - Duration::days(days)).to_rfc3339();
        // (kind, finished days ago, status, expected removed)
        let cohorts: [(&str, i64, &str, bool); 7] = [
            ("cli-invocation", 5, "pass", true),
            ("cleanup", 5, "fail", true),
            ("cleanup.artifacts", 3, "error", true),
            ("review", 60, "pass", true),
            ("review", 10, "pass", false),
            ("agent-task", 10, "stale", false),
            ("cli-invocation", 1, "pass", false),
        ];
        let mut db = rusqlite::Connection::open(&path).expect("raw db");
        let tx = db.transaction().expect("seed tx");
        let mut removed = Vec::new();
        let mut retained = Vec::new();
        for (cohort, (kind, days, status, expect_removed)) in cohorts.iter().enumerate() {
            let count = if *expect_removed { 700 } else { 50 };
            for index in 0..count {
                let id = format!("{kind}-{cohort}-{index}");
                tx.execute(
                    "INSERT INTO runs(id, kind, started_at, finished_at, status) VALUES (?1, ?2, ?3, ?3, ?4)",
                    rusqlite::params![id, kind, ago(*days), status],
                )
                .expect("seed run");
                seed_run_children(&tx, &id);
                if *expect_removed {
                    removed.push(id);
                } else {
                    retained.push(id);
                }
            }
        }
        // Running runs, even very old internal ones, are never touched.
        for index in 0..5 {
            let id = format!("running-{index}");
            tx.execute(
                "INSERT INTO runs(id, kind, started_at, status) VALUES (?1, 'cli-invocation', ?2, 'running')",
                rusqlite::params![id, ago(90)],
            )
            .expect("seed running run");
            seed_run_children(&tx, &id);
            retained.push(id);
        }
        tx.commit().expect("commit seed");
        assert_eq!(removed.len(), 2_800);

        let mut total_removed = 0;
        loop {
            let pass = retain_terminal_runs(TerminalRunRetentionOptions {
                apply: true,
                age_policy: TerminalRunAgePolicy::configured(30),
                limit: 1000,
            })
            .expect("apply retention");
            assert!(pass.skipped_run_ids.is_empty());
            if pass.removed_run_count == 0 {
                break;
            }
            total_removed += pass.removed_run_count;
        }
        assert_eq!(total_removed, removed.len());

        let remaining = |ids: &[String]| -> i64 {
            ids.iter()
                .map(|id| {
                    db.query_row("SELECT COUNT(*) FROM runs WHERE id = ?1", [id], |row| {
                        row.get::<_, i64>(0)
                    })
                    .expect("run count")
                })
                .sum()
        };
        assert_eq!(remaining(&removed), 0);
        assert_eq!(run_child_count(&db, &removed), 0);
        assert_eq!(remaining(&retained), retained.len() as i64);
        assert_eq!(
            run_child_count(&db, &retained),
            (retained.len() * crate::observation::store::RUN_OWNED_CHILD_TABLES.len()) as i64
        );
    });
}

#[test]
fn missing_run_guidance_prints_runner_routed_retrieval_commands() {
    let hints = missing_run_guidance_for_runner_ids("run-123", vec!["homeboy-lab".to_string()]);
    assert_eq!(
        hints,
        vec![
            "Resolve runner-owned artifacts non-destructively from the controller: `homeboy runs artifacts run-123 --runner homeboy-lab` (routes to the generation that retains the run without rotating the shared tunnel).",
            "Check runner `homeboy-lab` from the controller: `homeboy runs list --runner homeboy-lab --limit 100`.",
            "Inspect run `run-123` directly on runner `homeboy-lab`: `homeboy runner exec homeboy-lab -- homeboy runs show run-123`.",
            "List artifacts for run `run-123` directly on runner `homeboy-lab`: `homeboy runner exec homeboy-lab -- homeboy runs artifacts run-123`.",
            "If the admission daemon is stale, read retained evidence without a refresh: `homeboy runner exec homeboy-lab --read-only-artifact -- homeboy runs artifacts run-123`.",
            "Export run `run-123` directly on runner `homeboy-lab`: `homeboy runner exec homeboy-lab -- homeboy runs export --run run-123 --output <dir>`.",
        ]
    );
}

#[test]
fn list_artifacts_for_run_enriches_url_artifact_links() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
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
fn load_run_with_artifacts_uses_the_resolved_canonical_run_id() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(
                NewRunRecord::builder("bench")
                    .component_id("homeboy")
                    .metadata(serde_json::json!({"run_id": "durable-label"}))
                    .build(),
            )
            .expect("run");
        store
            .record_url_artifact(&run.id, "frontend_url", "https://example.test/")
            .expect("record URL artifact");

        let (resolved, artifacts) =
            load_run_with_artifacts(&store, "durable-label").expect("resolved run and artifacts");

        assert_eq!(resolved.id, run.id);
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].run_id, run.id);
        assert_eq!(
            artifacts[0].public_url.as_deref(),
            Some("https://example.test/")
        );
    });
}

#[test]
fn resolve_artifact_for_run_rejects_unknown_artifact_id() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
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
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
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
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
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

fn labeled_run(id: &str, command: &str, metadata: Value) -> RunRecord {
    RunRecord {
        id: id.into(),
        kind: "bench.matrix".into(),
        component_id: Some("homeboy".into()),
        started_at: "2026-06-26T00:00:00Z".into(),
        status: "fail".into(),
        command: Some(command.into()),
        metadata_json: metadata,
        ..Default::default()
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
fn require_run_resolves_requested_run_id_metadata_alias() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
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
    });
}

#[test]
fn require_run_reads_terminal_lab_review_alias_while_runner_probe_is_stalled() {
    struct StalledProvider {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl RunnerEvidenceProvider for StalledProvider {
        fn mirror_connected_runner_run(&self, _run_id: &str) -> Result<Option<RunRecord>> {
            self.entered.send(()).expect("signal stalled probe");
            self.release
                .lock()
                .expect("release lock")
                .recv()
                .expect("release stalled probe");
            Ok(None)
        }

        fn statuses(&self) -> Vec<RunnerConnectionInfo> {
            Vec::new()
        }

        fn daemon_api_get(&self, _runner_id: &str, _path: &str) -> Result<Value> {
            unreachable!("local lookup must not query the daemon")
        }

        fn refresh_run(
            &self,
            _: &ObservationStore,
            _: &RunRecord,
        ) -> Result<super::runner_evidence::MirroredRunRefresh> {
            unreachable!("local lookup must not reconcile runner evidence")
        }

        fn mirrored_runner_job_identity(&self, _run: &RunRecord) -> Option<(String, String)> {
            None
        }

        fn download_remote_artifact(
            &self,
            _path: &str,
            _output: Option<std::path::PathBuf>,
        ) -> Result<RemoteArtifactDownloadInfo> {
            unreachable!("local lookup must not hydrate artifacts")
        }
    }

    let _provider_lock = runner_evidence::runner_evidence_test_lock()
        .lock()
        .expect("provider test lock");

    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let store = ObservationStore::open_initialized().expect("store");
        let label = "runner-exec-review-homeboy-lab-terminal-job";
        let run = RunRecord {
            id: "terminal-lab-review-mirror".to_string(),
            kind: "runner-exec".to_string(),
            started_at: "2026-07-28T00:00:00Z".to_string(),
            finished_at: Some("2026-07-28T00:01:00Z".to_string()),
            status: RunStatus::Fail.as_str().to_string(),
            metadata_json: serde_json::json!({
                "requested_run_id": label,
                "lab": {
                    "runner": { "id": "homeboy-lab" },
                    "remote_job": { "id": "terminal-job" }
                }
            }),
            ..Default::default()
        };
        store
            .import_run(&run)
            .expect("persist terminal Lab review run");
        let artifact_path = _home.path().join("terminal-evidence.json");
        std::fs::write(&artifact_path, br#"{"ok":true}"#).expect("artifact bytes");
        store
            .record_artifact(&run.id, "terminal_evidence", &artifact_path)
            .expect("persist terminal evidence");

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        runner_evidence::register_runner_evidence_provider(Box::new(StalledProvider {
            entered: entered_tx,
            release: Mutex::new(release_rx),
        }));
        let stalled = std::thread::spawn(|| {
            runner_evidence::with_runner_evidence(|provider| {
                provider.mirror_connected_runner_run("unrelated-run")
            })
        });
        entered_rx.recv().expect("secondary probe is stalled");

        let resolved = require_run(&store, label).expect("durable local terminal record");
        assert_eq!(resolved.id, run.id);
        assert_eq!(resolved.status, RunStatus::Fail.as_str());
        let artifacts = list_artifacts_for_run(&store, &run.id)
            .expect("durable artifact reader must not wait for the runner");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].kind, "terminal_evidence");
        assert!(
            !stalled.is_finished(),
            "lookup completed before probe release"
        );

        release_tx.send(()).expect("release secondary probe");
        stalled
            .join()
            .expect("secondary probe joined")
            .expect("secondary probe result");
        runner_evidence::clear_runner_evidence_provider();
    });
}

#[test]
fn list_artifacts_for_run_resolves_requested_run_id_alias() {
    with_isolated_home(|home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
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
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
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

fn lab_labeled_run(id: &str, kind: &str, label: &str, job_id: &str) -> RunRecord {
    RunRecord {
        id: id.to_string(),
        kind: kind.to_string(),
        component_id: Some("homeboy".to_string()),
        started_at: "2026-07-18T00:00:00Z".to_string(),
        status: "running".to_string(),
        command: Some(format!("homeboy {kind} --run-id {label}")),
        cwd: Some("/tmp/homeboy-fixture".to_string()),
        rig_id: Some("studio".to_string()),
        metadata_json: serde_json::json!({
            "requested_run_id": label,
            "lab": {
                "runner": { "id": "homeboy-lab" },
                "remote_job": { "id": job_id }
            }
        }),
        ..Default::default()
    }
}

#[test]
fn require_run_resolves_lab_label_to_caller_across_mirrors() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let store = ObservationStore::open_initialized().expect("store");
        let caller = lab_labeled_run("caller", "fuzz", "shared-label", "job-1");
        store.upsert_imported_run(&caller).expect("caller");
        for id in ["mirror-a", "mirror-b", "mirror-c"] {
            store
                .upsert_imported_run(&lab_labeled_run(id, "runner-exec", "shared-label", "job-1"))
                .expect("mirror");
        }

        let resolved = require_run(&store, "shared-label").expect("canonical caller");
        assert_eq!(resolved.id, caller.id);
    });
}

#[test]
fn require_run_keeps_unrelated_lab_label_collision_ambiguous() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let store = ObservationStore::open_initialized().expect("store");
        let caller = lab_labeled_run("caller", "fuzz", "shared-label", "job-1");
        let collision = lab_labeled_run("collision", "fuzz", "shared-label", "job-2");
        store.upsert_imported_run(&caller).expect("caller");
        for id in ["mirror-a", "mirror-b", "mirror-c"] {
            store
                .upsert_imported_run(&lab_labeled_run(id, "runner-exec", "shared-label", "job-1"))
                .expect("mirror");
        }
        store.upsert_imported_run(&collision).expect("collision");

        let err = require_run(&store, "shared-label").expect_err("ambiguous label");
        assert!(err.message.contains("2 persisted runs match"));
        let joined = err.details["tried"]
            .as_array()
            .expect("disambiguation entries")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains(&caller.id));
        assert!(joined.contains(&collision.id));
        assert!(!joined.contains("mirror-a"));
        assert!(!joined.contains("mirror-b"));
        assert!(!joined.contains("mirror-c"));
    });
}
