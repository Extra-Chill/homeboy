//! Observation-store foundation tests.
//!
//! These isolate `HOME` / `XDG_DATA_HOME` so the developer's real local DB is
//! never read or written.

use crate::core::observation::store::{
    self, ObservationStore, CURRENT_SCHEMA_VERSION, LAB_OFFLOAD_METADATA_ENV,
    SOURCE_SNAPSHOT_METADATA_ENV,
};
use crate::core::observation::{
    FindingListFilter, NewFindingRecord, NewRunRecord, RunContext, RunListFilter, RunProvenance,
    RunRecord, RunStatus,
};
use crate::test_support::with_isolated_home;
use std::sync::{Arc, Barrier};

struct XdgGuard {
    prior: Option<String>,
}

struct EnvGuard {
    key: &'static str,
    prior: Option<String>,
}

impl XdgGuard {
    fn unset() -> Self {
        let prior = std::env::var("XDG_DATA_HOME").ok();
        std::env::remove_var("XDG_DATA_HOME");
        Self { prior }
    }

    fn set(value: &std::path::Path) -> Self {
        let prior = std::env::var("XDG_DATA_HOME").ok();
        std::env::set_var("XDG_DATA_HOME", value);
        Self { prior }
    }
}

impl Drop for XdgGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
}

impl EnvGuard {
    fn set(key: &'static str, value: String) -> Self {
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

#[test]
fn test_status() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();

        let status = store::status().expect("status");

        assert!(!status.exists);
        assert_eq!(status.schema_version, 0);
        assert_eq!(status.migration_count, 0);
        assert_eq!(status.table_count, 0);
        assert_eq!(
            status.path,
            home.path()
                .join(".local/share/homeboy/homeboy.sqlite")
                .to_string_lossy()
        );
        assert!(
            !std::path::Path::new(&status.path).exists(),
            "read-only status must not create the DB"
        );
    });
}

#[test]
fn test_database_path() {
    with_isolated_home(|home| {
        let data_home = home.path().join("xdg-data");
        let _xdg = XdgGuard::set(&data_home);

        let path = store::database_path().expect("db path");

        assert_eq!(path, data_home.join("homeboy/homeboy.sqlite"));
    });
}

#[test]
fn test_open_initialized() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();

        let store = ObservationStore::open_initialized().expect("init store");
        let status = store.status().expect("status");

        assert!(status.exists);
        assert_eq!(status.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(status.migration_count, 5);
        assert_eq!(status.table_count, 7);
    });
}

#[test]
fn initialization_is_idempotent() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();

        ObservationStore::open_initialized().expect("first init");
        let second = ObservationStore::open_initialized().expect("second init");
        let status = second.status().expect("status");

        assert_eq!(status.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(status.migration_count, 5);
        assert_eq!(status.table_count, 7);
    });
}

#[test]
fn initialization_recovers_when_artifact_type_migration_was_interrupted() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();

        ObservationStore::open_initialized().expect("initial migration");
        let path = store::database_path().expect("db path");
        let connection = rusqlite::Connection::open(&path).expect("open raw db");
        connection
            .execute("DELETE FROM schema_migrations WHERE version = 4", [])
            .expect("remove migration marker");
        drop(connection);

        let reopened = ObservationStore::open_initialized().expect("resume migration");
        let status = reopened.status().expect("status");

        assert_eq!(status.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(status.migration_count, 5);
    });
}

#[test]
fn initialization_is_safe_under_concurrent_setup() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let workers = 4;
        let barrier = Arc::new(Barrier::new(workers));
        let handles = (0..workers)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    ObservationStore::open_initialized()
                        .expect("concurrent init")
                        .status()
                        .expect("status")
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            let status = handle.join().expect("worker joined");
            assert_eq!(status.schema_version, CURRENT_SCHEMA_VERSION);
            assert_eq!(status.migration_count, 5);
        }
    });
}

#[test]
fn test_record_finding() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");
        let run = store
            .start_run(sample_run("lint", "homeboy"))
            .expect("start run");

        let record = store
            .record_finding(&sample_finding(&run.id, "security", "src/foo.php"))
            .expect("record finding");
        let fetched = store
            .get_finding(&record.id)
            .expect("get finding")
            .expect("finding exists");

        assert_eq!(fetched.message, "Missing security");
        assert_eq!(fetched.fixable, Some(true));
    });
}

#[test]
fn test_record_findings() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");
        let run = store
            .start_run(sample_run("lint", "homeboy"))
            .expect("start run");

        let records = store
            .record_findings(&[
                sample_finding(&run.id, "security", "src/foo.php"),
                sample_finding(&run.id, "i18n", "src/bar.php"),
            ])
            .expect("record findings");

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].rule.as_deref(), Some("security"));
        assert_eq!(records[1].rule.as_deref(), Some("i18n"));
    });
}

#[test]
fn test_list_findings() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");
        let run = store
            .start_run(sample_run("lint", "homeboy"))
            .expect("start run");
        let records = store
            .record_findings(&[
                sample_finding(&run.id, "security", "src/foo.php"),
                sample_finding(&run.id, "i18n", "src/bar.php"),
            ])
            .expect("record findings");

        let all = store
            .list_findings(FindingListFilter {
                run_id: Some(run.id.clone()),
                tool: Some("lint".to_string()),
                ..FindingListFilter::default()
            })
            .expect("list findings");
        let filtered = store
            .list_findings(FindingListFilter {
                run_id: Some(run.id),
                file: Some("src/foo.php".to_string()),
                ..FindingListFilter::default()
            })
            .expect("list file findings");

        assert_eq!(all.len(), 2);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, records[0].id);
    });
}

fn sample_finding(run_id: &str, rule: &str, file: &str) -> NewFindingRecord {
    NewFindingRecord {
        run_id: run_id.to_string(),
        tool: "lint".to_string(),
        rule: Some(rule.to_string()),
        file: Some(file.to_string()),
        line: Some(12),
        severity: Some("error".to_string()),
        fingerprint: Some(format!("{file}::{rule}")),
        message: format!("Missing {rule}"),
        fixable: Some(true),
        metadata_json: serde_json::json!({ "category": rule }),
    }
}

fn sample_run(kind: &str, component_id: &str) -> NewRunRecord {
    NewRunRecord::builder(kind)
        .component_id(component_id)
        .command(format!("homeboy {kind} {component_id}"))
        .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
        .homeboy_version("test-version")
        .git_sha(Some("abc123".to_string()))
        .rig_id("studio")
        .metadata(serde_json::json!({
            "scenario": "fixture",
            "attempt": 1,
        }))
        .build()
}

fn sample_import_run(id: &str) -> RunRecord {
    RunRecord {
        id: id.to_string(),
        kind: "runner-exec".to_string(),
        component_id: Some("homeboy".to_string()),
        started_at: "2026-05-25T00:00:00+00:00".to_string(),
        finished_at: Some("2026-05-25T00:01:00+00:00".to_string()),
        status: "pass".to_string(),
        command: Some("homeboy audit homeboy".to_string()),
        cwd: Some("/home/chubes/Developer/homeboy".to_string()),
        homeboy_version: Some("test-version".to_string()),
        git_sha: Some("abc123".to_string()),
        rig_id: None,
        metadata_json: serde_json::json!({
            "lab": {
                "runner": { "id": "lab" },
                "remote_job_id": "job-123"
            }
        }),
    }
}

#[test]
fn test_start_run() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");

        let started = store
            .start_run(sample_run("bench", "homeboy"))
            .expect("start run");

        assert_eq!(started.kind, "bench");
        assert_eq!(started.component_id.as_deref(), Some("homeboy"));
        assert_eq!(started.status, "running");
        assert!(started.finished_at.is_none());
        assert_eq!(started.metadata_json["scenario"], "fixture");

        let fetched = store
            .get_run(&started.id)
            .expect("get run")
            .expect("run exists");

        assert_eq!(fetched, started);
    });
}

mod run_context_tests {
    use super::*;

    #[test]
    fn start_run_records_subprocess_source_snapshot_metadata() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("init store");
            let snapshot = serde_json::json!({
                "runner_id": "lab",
                "remote_path": "/srv/homeboy/repo",
                "dirty": true,
                "sync_mode": "snapshot",
                "snapshot_hash": "sha256:dirty",
                "synced_at": "2026-05-16T00:00:00Z",
                "sync_excludes": ["node_modules/"]
            });

            let _env = EnvGuard::set(SOURCE_SNAPSHOT_METADATA_ENV, snapshot.to_string());
            let run = store
                .start_run(sample_run("test", "homeboy"))
                .expect("start run");

            assert_eq!(run.metadata_json["source_snapshot"], snapshot);
        });
    }

    #[test]
    fn start_run_records_subprocess_lab_offload_metadata() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("init store");
            let lab = serde_json::json!({
                "source": "automatic",
                "status": "fallback",
                "runner_id": "lab",
                "remote_workspace": null,
                "fallback_reason": "runner connect timed out after 3s"
            });

            let _env = EnvGuard::set(LAB_OFFLOAD_METADATA_ENV, lab.to_string());
            let run = store
                .start_run(sample_run("test", "homeboy"))
                .expect("start run");

            assert_eq!(run.metadata_json["lab_offload"], lab);
        });
    }

    #[test]
    fn start_run_prefers_typed_context_over_subprocess_environment() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("init store");
            let env_snapshot = serde_json::json!({ "runner_id": "env" });
            let explicit_snapshot = serde_json::json!({ "runner_id": "typed" });
            let explicit_lab = serde_json::json!({ "status": "fallback", "source": "typed" });
            let _env_snapshot =
                EnvGuard::set(SOURCE_SNAPSHOT_METADATA_ENV, env_snapshot.to_string());
            let _env_lab = EnvGuard::set(LAB_OFFLOAD_METADATA_ENV, "{not json".to_string());

            let run = store
                .start_run(
                    sample_run("test", "homeboy").with_run_context(RunContext::from_provenance(
                        RunProvenance::default()
                            .with_source_snapshot(explicit_snapshot.clone())
                            .with_lab_offload(explicit_lab.clone()),
                    )),
                )
                .expect("start run");

            assert_eq!(run.metadata_json["source_snapshot"], explicit_snapshot);
            assert_eq!(run.metadata_json["lab_offload"], explicit_lab);
        });
    }

    #[test]
    fn malformed_subprocess_environment_does_not_pollute_typed_context() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("init store");
            let _env_snapshot =
                EnvGuard::set(SOURCE_SNAPSHOT_METADATA_ENV, "{not json".to_string());
            let _env_lab = EnvGuard::set(LAB_OFFLOAD_METADATA_ENV, "{not json".to_string());

            let run = store
                .start_run_with_context(
                    sample_run("test", "homeboy"),
                    RunContext::from_provenance(
                        RunProvenance::default()
                            .with_artifact_mirror(serde_json::json!({ "mirror": "typed" })),
                    ),
                )
                .expect("start run");

            assert!(run.metadata_json.get("source_snapshot").is_none());
            assert!(run.metadata_json.get("lab_offload").is_none());
            assert_eq!(run.metadata_json["artifact_mirror"]["mirror"], "typed");
        });
    }
}

#[test]
fn import_run_is_idempotent_for_existing_mirrored_run_id() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let first = ObservationStore::open_initialized().expect("first store");
        let second = ObservationStore::open_initialized().expect("second store");
        let run = sample_import_run("runner-run-123");

        first.import_run(&run).expect("first import");
        second.import_run(&run).expect("duplicate import");

        assert_eq!(second.get_run(&run.id).expect("get run"), Some(run));
    });
}

#[test]
fn import_run_rejects_conflicting_existing_record() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = sample_import_run("runner-run-456");
        let mut conflicting = run.clone();
        conflicting.status = "fail".to_string();

        store.import_run(&run).expect("first import");
        let err = store
            .import_run(&conflicting)
            .expect_err("conflicting duplicate should fail");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("existing run record conflicts"));
    });
}

#[test]
fn test_finish_run() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");
        let started = store
            .start_run(sample_run("bench", "homeboy"))
            .expect("start run");

        let finished = store
            .finish_run(
                &started.id,
                RunStatus::Pass,
                Some(serde_json::json!({ "scenario": "fixture", "ok": true })),
            )
            .expect("finish run");
        let fetched = store
            .get_run(&started.id)
            .expect("get run")
            .expect("run exists");

        assert_eq!(finished.status, "pass");
        assert!(finished.finished_at.is_some());
        assert_eq!(finished.metadata_json["ok"], true);
        assert_eq!(fetched, finished);
    });
}

#[test]
fn test_list_runs() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");

        let bench = store
            .start_run(sample_run("bench", "homeboy"))
            .expect("start bench");
        store
            .finish_run(&bench.id, RunStatus::Pass, None)
            .expect("finish bench");

        let mut trace = sample_run("trace", "homeboy");
        trace.rig_id = Some("other-rig".to_string());
        let trace = store.start_run(trace).expect("start trace");
        store
            .finish_run(&trace.id, RunStatus::Fail, None)
            .expect("finish trace");

        let filtered = store
            .list_runs(RunListFilter {
                kind: Some("bench".to_string()),
                component_id: Some("homeboy".to_string()),
                status: Some("pass".to_string()),
                rig_id: Some("studio".to_string()),
                limit: Some(10),
            })
            .expect("list filtered");

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, bench.id);
        assert_eq!(filtered[0].status, "pass");

        let missing = store
            .list_runs(RunListFilter {
                status: Some("error".to_string()),
                ..RunListFilter::default()
            })
            .expect("list missing");
        assert!(missing.is_empty());
    });
}

#[test]
fn test_latest_run() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");

        let old = store
            .start_run(sample_run("lint", "homeboy"))
            .expect("start old");
        store
            .finish_run(&old.id, RunStatus::Pass, None)
            .expect("finish old");
        let latest = store
            .start_run(sample_run("lint", "homeboy"))
            .expect("start latest");
        store
            .finish_run(&latest.id, RunStatus::Fail, None)
            .expect("finish latest");
        let other_kind = store
            .start_run(sample_run("bench", "homeboy"))
            .expect("start bench");

        let selected = store
            .latest_run(RunListFilter {
                kind: Some("lint".to_string()),
                component_id: Some("homeboy".to_string()),
                ..RunListFilter::default()
            })
            .expect("latest run")
            .expect("run exists");
        let missing = store
            .latest_run(RunListFilter {
                status: Some("stale".to_string()),
                ..RunListFilter::default()
            })
            .expect("missing latest");

        assert_eq!(selected.id, latest.id);
        assert_ne!(selected.id, old.id);
        assert_ne!(selected.id, other_kind.id);
        assert!(missing.is_none());
    });
}

#[test]
fn test_record_artifact() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");
        let run = store
            .start_run(sample_run("trace", "homeboy"))
            .expect("start run");
        let artifact_path = home.path().join("trace-results.json");
        std::fs::write(&artifact_path, br#"{"status":"pass"}"#).expect("write artifact");

        let artifact = store
            .record_artifact(&run.id, "trace-results", &artifact_path)
            .expect("record artifact");
        let artifacts = store.list_artifacts(&run.id).expect("list artifacts");

        assert_eq!(artifacts, vec![artifact.clone()]);
        assert_eq!(artifact.run_id, run.id);
        assert_eq!(artifact.kind, "trace-results");
        assert_eq!(artifact.artifact_type, "file");
        assert_ne!(artifact.path, artifact_path.to_string_lossy());
        assert!(std::path::PathBuf::from(&artifact.path).is_file());
        assert_eq!(
            std::fs::read_to_string(&artifact.path).expect("read persisted artifact"),
            "{\"status\":\"pass\"}"
        );
        assert_eq!(artifact.url, None);
        assert_eq!(artifact.size_bytes, Some(17));
        assert_eq!(artifact.mime.as_deref(), Some("application/json"));
        assert_eq!(
            artifact.sha256.as_deref(),
            Some("117367705c6e7ef5d779dd71de15a95ee62339e1ef635f08246f8e1ec99167e2")
        );
    });
}

#[test]
fn test_record_directory_artifact() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");
        let run = store
            .start_run(sample_run("bench", "homeboy"))
            .expect("start run");
        let artifact_path = home.path().join("visual-comparisons");
        std::fs::create_dir_all(artifact_path.join("nested")).expect("mkdir artifact");
        std::fs::write(artifact_path.join("summary.json"), br#"{"status":"skip"}"#)
            .expect("write artifact");
        std::fs::write(artifact_path.join("nested/detail.txt"), "detail").expect("write nested");

        let artifact = store
            .record_directory_artifact(&run.id, "bench_artifact", &artifact_path)
            .expect("record directory artifact");
        let artifacts = store.list_artifacts(&run.id).expect("list artifacts");

        assert_eq!(artifacts, vec![artifact.clone()]);
        assert_eq!(artifact.run_id, run.id);
        assert_eq!(artifact.kind, "bench_artifact");
        assert_eq!(artifact.artifact_type, "directory");
        assert_ne!(artifact.path, artifact_path.to_string_lossy());
        let persisted = std::path::PathBuf::from(&artifact.path);
        assert!(persisted.is_dir());
        assert_eq!(
            std::fs::read_to_string(persisted.join("summary.json")).expect("read persisted"),
            "{\"status\":\"skip\"}"
        );
        assert_eq!(
            std::fs::read_to_string(persisted.join("nested/detail.txt")).expect("read nested"),
            "detail"
        );
        assert_eq!(artifact.url, None);
        assert_eq!(artifact.size_bytes, None);
        assert_eq!(artifact.mime, None);
        assert_eq!(artifact.sha256, None);
    });
}

#[test]
fn test_record_url_artifact() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");
        let run = store
            .start_run(sample_run("bench", "homeboy"))
            .expect("start run");

        let artifact = store
            .record_url_artifact(&run.id, "frontend_url", "https://example.test/")
            .expect("record URL artifact");
        let artifacts = store.list_artifacts(&run.id).expect("list artifacts");

        assert_eq!(artifacts, vec![artifact.clone()]);
        assert_eq!(artifact.kind, "frontend_url");
        assert_eq!(artifact.artifact_type, "url");
        assert_eq!(artifact.path, "https://example.test/");
        assert_eq!(artifact.url.as_deref(), Some("https://example.test/"));
        assert_eq!(artifact.sha256, None);
        assert_eq!(artifact.size_bytes, None);
        assert_eq!(artifact.mime, None);
    });
}

#[test]
fn test_list_artifacts() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");
        let run = store
            .start_run(sample_run("trace", "homeboy"))
            .expect("start run");
        let first_path = home.path().join("first.json");
        let second_path = home.path().join("second.log");
        std::fs::write(&first_path, b"first").expect("write first");
        std::fs::write(&second_path, b"second").expect("write second");

        let first = store
            .record_artifact(&run.id, "first", &first_path)
            .expect("record first");
        let second = store
            .record_artifact(&run.id, "second", &second_path)
            .expect("record second");

        let artifacts = store.list_artifacts(&run.id).expect("list artifacts");
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].id, first.id);
        assert_eq!(artifacts[1].id, second.id);
    });
}

#[test]
fn missing_artifact_file_returns_clear_error() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("init store");
        let run = store
            .start_run(sample_run("bench", "homeboy"))
            .expect("start run");
        let missing = home.path().join("missing.json");

        let err = store
            .record_artifact(&run.id, "missing", &missing)
            .expect_err("missing artifact should fail");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("artifact file not found"));
        assert!(err.details.to_string().contains("missing.json"));
    });
}
