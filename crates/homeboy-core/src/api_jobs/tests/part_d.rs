#![cfg(test)]

//! The durable job store lives in SQLite: one row per job, one per event, one
//! per live submission key.

use std::fs;
use std::sync::{Arc, Barrier};

use serde_json::{json, Value};

use super::*;
use crate::api_jobs::persistence::{
    lookup_tombstone, open_tombstone_store, tombstone_path, ReplayTombstoneKind,
};

fn sql(path: &std::path::Path) -> rusqlite::Connection {
    open_tombstone_store(path).expect("open sql store")
}

fn count(connection: &rusqlite::Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count rows")
}

/// A `jobs.json` as an older daemon wrote it: produced by the legacy
/// whole-document shape, with active and terminal jobs, events, a live
/// submission key, an already-expired key and compaction evidence.
fn legacy_document(path: &std::path::Path) -> (Value, Vec<Uuid>) {
    let seed = JobStore::open_without_reconciliation(path).expect("seed store");
    let queued = seed.create("queued-work");
    let running = seed.create("runner.exec");
    seed.start(running.id).expect("start running job");
    seed.append_event(
        running.id,
        JobEventKind::Stdout,
        Some("hello".to_string()),
        Some(json!({ "stream": "stdout" })),
    )
    .expect("append stdout");
    let done = seed.create("finished-work");
    seed.start(done.id).expect("start finished job");
    seed.complete(done.id, Some(json!({ "ok": true })))
        .expect("complete finished job");
    let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
    request.metadata = Some(json!({ "submission_key": "agent-task:v1:migrated" }));
    let remote = seed
        .submit_runner_api_fixture(request)
        .expect("submit remote runner job");

    let mut document = durable_snapshot(path);
    assert_eq!(document["jobs"].as_array().expect("jobs").len(), 4);
    document["expired_submission_keys"] = json!({
        "agent-task:v1:long-gone": {
            "fingerprint": "sha256:long-gone",
            "job_id": Uuid::new_v4(),
        }
    });
    document["compaction"] = json!({
        "timestamp_ms": 1,
        "removed_terminal_jobs": 3,
        "retained_terminal_jobs": 1,
        "retained_terminal_bytes": 100,
        "active_jobs": 2,
    });
    (document, vec![queued.id, running.id, done.id, remote.id])
}

#[test]
fn legacy_jobs_json_is_imported_once_then_renamed() {
    let temp = tempfile::tempdir().expect("temp dir");
    let seed_path = temp.path().join("seed").join("jobs.json");
    let (document, ids) = legacy_document(&seed_path);

    let path = temp.path().join("jobs.json");
    install_legacy_store(&path, &document);
    let legacy_bytes = fs::read(&path).expect("legacy bytes");
    assert!(!tombstone_path(&path).exists());

    let store = JobStore::open_without_reconciliation(&path).expect("import legacy store");
    assert!(!path.exists(), "the document is renamed once imported");
    assert_eq!(
        fs::read(path.with_file_name("jobs.json.migrated")).expect("migrated copy"),
        legacy_bytes,
        "the original document is kept verbatim"
    );

    // Every job, event and submission survives the move.
    let imported = durable_snapshot(&path);
    for id in &ids {
        let find = |snapshot: &Value| {
            snapshot["jobs"]
                .as_array()
                .expect("jobs")
                .iter()
                .find(|stored| stored["job"]["id"] == json!(id))
                .cloned()
                .expect("job present")
        };
        assert_eq!(find(&imported), find(&document));
    }
    assert_eq!(imported["submission_keys"], document["submission_keys"]);
    assert_eq!(
        store.get(ids[1]).expect("running job").status,
        JobStatus::Running,
        "opening without reconciliation preserves active work"
    );
    let events = store.events(ids[1]).expect("events");
    assert!(events
        .iter()
        .any(|event| event.message.as_deref() == Some("hello")));

    // The already-expired key became permanent replay evidence.
    let tombstone = lookup_tombstone(
        &path,
        ReplayTombstoneKind::RemoteRunner,
        "agent-task:v1:long-gone",
    )
    .expect("lookup")
    .expect("expired key tombstoned");
    assert_eq!(tombstone.fingerprint, "sha256:long-gone");

    // Rows, not a document.
    let connection = sql(&path);
    assert_eq!(count(&connection, "daemon_jobs"), 4);
    assert_eq!(count(&connection, "daemon_job_submissions"), 1);
    assert!(count(&connection, "daemon_job_events") >= 4);
    drop(connection);

    // A second open never imports again: a stray document is left alone.
    drop(store);
    fs::write(&path, b"{\"jobs\":[]}").expect("stray document");
    let reopened = JobStore::open_without_reconciliation(&path).expect("reopen");
    assert_eq!(reopened.list().len(), 4);
    assert_eq!(
        fs::read(path.with_file_name("jobs.json.migrated")).expect("migrated copy"),
        legacy_bytes
    );
}

#[test]
fn migration_of_a_reconciling_open_matches_the_legacy_semantics() {
    let temp = tempfile::tempdir().expect("temp dir");
    let seed_path = temp.path().join("seed").join("jobs.json");
    let (document, ids) = legacy_document(&seed_path);
    let path = temp.path().join("jobs.json");
    install_legacy_store(&path, &document);

    let store = JobStore::open(&path).expect("restart over a legacy document");
    let restarted = store.get(ids[1]).expect("running job");
    assert_eq!(restarted.status, JobStatus::Failed);
    assert_eq!(
        restarted.stale_reason.as_deref(),
        Some("control plane lost before the job reached a terminal status")
    );
    assert_eq!(
        store.get(ids[2]).expect("finished job").status,
        JobStatus::Succeeded
    );
}

#[test]
fn corrupt_legacy_document_is_quarantined_not_imported() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    fs::write(&path, b"{\"jobs\": [").expect("corrupt document");

    let store = JobStore::open(&path).expect("quarantined");
    assert!(store.list().is_empty());
    let names = fs::read_dir(temp.path())
        .expect("read dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(names
        .iter()
        .any(|name| name.starts_with("jobs.json.corrupt-")));
    assert!(!names.iter().any(|name| name == "jobs.json.migrated"));
    assert!(sql_store_has_state(&path));
}

fn sql_store_has_state(path: &std::path::Path) -> bool {
    JobStore::store_generation_at_path(path)
        .expect("generation")
        .is_some()
}

#[test]
fn appending_an_event_writes_only_that_jobs_rows() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path).expect("store");
    let first = store.create("first");
    let second = store.create("second");
    let third = store.create("third");
    for job in [&first, &second, &third] {
        store.start(job.id).expect("start");
        store
            .append_event(job.id, JobEventKind::Stdout, Some("seed".to_string()), None)
            .expect("seed event");
    }

    // Record every row write from here on.
    let connection = sql(&path);
    connection
        .execute_batch(
            "CREATE TABLE row_writes (what TEXT NOT NULL, job_id TEXT NOT NULL);
             CREATE TRIGGER job_inserted AFTER INSERT ON daemon_jobs
                 BEGIN INSERT INTO row_writes VALUES ('job', NEW.job_id); END;
             CREATE TRIGGER job_updated AFTER UPDATE ON daemon_jobs
                 BEGIN INSERT INTO row_writes VALUES ('job', NEW.job_id); END;
             CREATE TRIGGER job_deleted AFTER DELETE ON daemon_jobs
                 BEGIN INSERT INTO row_writes VALUES ('job-deleted', OLD.job_id); END;
             CREATE TRIGGER event_inserted AFTER INSERT ON daemon_job_events
                 BEGIN INSERT INTO row_writes VALUES ('event', NEW.job_id); END;
             CREATE TRIGGER event_deleted AFTER DELETE ON daemon_job_events
                 BEGIN INSERT INTO row_writes VALUES ('event-deleted', OLD.job_id); END;",
        )
        .expect("install write log");
    let before = durable_snapshot(&path);

    store
        .append_event(
            second.id,
            JobEventKind::Stdout,
            Some("one more".to_string()),
            None,
        )
        .expect("append");

    let writes = connection
        .prepare("SELECT what, job_id FROM row_writes ORDER BY rowid")
        .expect("prepare")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("rows");
    assert_eq!(
        writes,
        vec![
            ("job".to_string(), second.id.to_string()),
            ("event".to_string(), second.id.to_string()),
        ],
        "one job row (its event count) and one new event row; nothing else"
    );

    // The other jobs are byte-for-byte what they were.
    let after = durable_snapshot(&path);
    for untouched in [first.id, third.id] {
        let find = |snapshot: &Value| {
            snapshot["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|stored| stored["job"]["id"] == json!(untouched))
                .cloned()
                .unwrap()
        };
        assert_eq!(find(&before), find(&after));
    }
    assert_eq!(
        store.events(second.id).expect("events").len(),
        before["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stored| stored["job"]["id"] == json!(second.id))
            .unwrap()["events"]
            .as_array()
            .unwrap()
            .len()
            + 1
    );
}

#[test]
fn a_mutation_that_changes_nothing_commits_nothing() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path).expect("store");
    store.create("idle");
    let generation = JobStore::store_generation_at_path(&path).expect("generation");

    store.durable_transaction(|_| Ok(())).expect("no-op");
    JobStore::open_without_reconciliation(&path).expect("reopen");

    assert_eq!(
        JobStore::store_generation_at_path(&path).expect("generation"),
        generation
    );
}

#[test]
fn two_handles_interleave_appends_without_losing_events() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let first = JobStore::open(&path).expect("first handle");
    let second = JobStore::open(&path).expect("second handle");
    let job = first.create("shared");

    // Strict alternation: each handle must pick up the other's commit first.
    for index in 0..10 {
        let (store, label) = if index % 2 == 0 {
            (&first, "a")
        } else {
            (&second, "b")
        };
        store
            .append_event(
                job.id,
                JobEventKind::Stdout,
                Some(format!("{label}{index}")),
                None,
            )
            .expect("alternating append");
    }

    // Free-running concurrent writers.
    let barrier = Arc::new(Barrier::new(2));
    let threads = [(first.clone(), "x"), (second.clone(), "y")]
        .into_iter()
        .map(|(store, label)| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for index in 0..15 {
                    store
                        .append_event(
                            job.id,
                            JobEventKind::Stdout,
                            Some(format!("{label}{index}")),
                            None,
                        )
                        .expect("concurrent append");
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().expect("writer thread");
    }

    let reopened = JobStore::open_without_reconciliation(&path).expect("reopen");
    let events = reopened.events(job.id).expect("events");
    let messages = events
        .iter()
        .filter(|event| event.kind == JobEventKind::Stdout)
        .filter_map(|event| event.message.clone())
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 40, "no append was lost: {messages:?}");
    for label in ["a", "b"] {
        for index in (0..10).filter(|index| (index % 2 == 0) == (label == "a")) {
            assert!(messages.contains(&format!("{label}{index}")));
        }
    }
    for label in ["x", "y"] {
        let own = messages
            .iter()
            .filter(|message| message.starts_with(label))
            .collect::<Vec<_>>();
        assert_eq!(own.len(), 15);
        assert!(
            own.windows(2)
                .all(|pair| pair[0].len() < pair[1].len() || pair[0] < pair[1]),
            "each writer's own events keep their order"
        );
    }
    let sequences = events
        .iter()
        .map(|event| event.sequence)
        .collect::<Vec<_>>();
    let mut unique = sequences.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), sequences.len(), "event sequences are unique");
    assert_eq!(
        count(&sql(&path), "daemon_job_events") as usize,
        events.len(),
        "every event is a row"
    );
}

#[test]
fn restart_recovery_runs_on_the_sql_store() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open(&path).expect("store");
    let finished = store.create("finished");
    store.start(finished.id).expect("start");
    store
        .complete(finished.id, Some(json!({ "ok": true })))
        .expect("complete");
    let interrupted = store.create("interrupted");
    store.start(interrupted.id).expect("start");
    store
        .append_event(
            interrupted.id,
            JobEventKind::Stdout,
            Some("partial output".to_string()),
            None,
        )
        .expect("output before the crash");
    let queued = store.create("remote.queued");
    drop(store);

    assert!(!path.exists(), "no whole-store document is written");
    assert!(tombstone_path(&path).exists());

    let restarted = JobStore::open(&path).expect("restart");
    let recovered = restarted.get(interrupted.id).expect("interrupted job");
    assert_eq!(recovered.status, JobStatus::Failed);
    assert_eq!(
        recovered.stale_reason.as_deref(),
        Some("control plane lost before the job reached a terminal status")
    );
    assert_eq!(
        restarted.get(finished.id).expect("finished job").status,
        JobStatus::Succeeded
    );
    let events = restarted.events(interrupted.id).expect("events");
    assert!(events
        .iter()
        .any(|event| event.message.as_deref() == Some("partial output")));
    assert!(events.iter().any(|event| event.kind == JobEventKind::Error));
    assert_eq!(restarted.get(queued.id).expect("queued job").id, queued.id);

    // Recovery was itself durable: a third process sees the same outcome and
    // does not append the recovery events again.
    let event_count = events.len();
    let again = JobStore::open(&path).expect("second restart");
    assert_eq!(
        again.events(interrupted.id).expect("events").len(),
        event_count
    );
    assert_eq!(
        JobStore::active_count_at_path(&path).expect("active count"),
        0
    );
}

#[test]
fn retention_deletes_rows_and_keeps_replay_tombstones() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_with_retention(&path, 100, 1).expect("store");
    let mut keys = Vec::new();
    for index in 0..4 {
        let key = format!("agent-task:v1:retained-{index}");
        let mut request = remote_runner_request("homeboy-lab", Some("extrachill"));
        request.metadata = Some(json!({ "submission_key": key }));
        let job = store.submit_runner_api_fixture(request).expect("submit");
        store
            .durable_transaction(|inner| {
                inner.jobs.get_mut(&job.id).expect("job").job.status = JobStatus::Succeeded;
                Ok(())
            })
            .expect("terminalize");
        keys.push((key, job.id));
    }

    let connection = sql(&path);
    assert_eq!(
        count(&connection, "daemon_jobs"),
        1,
        "one terminal job kept"
    );
    assert_eq!(count(&connection, "daemon_job_submissions"), 1);
    let orphan_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM daemon_job_events
             WHERE job_id NOT IN (SELECT job_id FROM daemon_jobs)",
            [],
            |row| row.get(0),
        )
        .expect("count orphans");
    assert_eq!(orphan_events, 0, "deleted jobs take their events with them");
    for (key, job_id) in &keys[..3] {
        let tombstone = lookup_tombstone(&path, ReplayTombstoneKind::RemoteRunner, key)
            .expect("lookup")
            .expect("compacted key is tombstoned");
        assert_eq!(&tombstone.job_id, job_id);
    }
    assert!(
        lookup_tombstone(&path, ReplayTombstoneKind::RemoteRunner, &keys[3].0)
            .expect("lookup")
            .is_none(),
        "the retained job's key is still live, not tombstoned"
    );
}

#[test]
fn a_failed_commit_leaves_the_database_and_the_next_transaction_intact() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path).expect("store");
    let job = store.create("durable");
    let before = durable_snapshot(&path);

    store.fail_next_durable_writes(1);
    assert!(store
        .append_event(job.id, JobEventKind::Stdout, Some("lost".to_string()), None)
        .is_err());
    assert_eq!(durable_snapshot(&path), before, "nothing was committed");
    assert!(!store
        .events(job.id)
        .expect("events")
        .iter()
        .any(|event| event.message.as_deref() == Some("lost")));

    store
        .append_event(job.id, JobEventKind::Stdout, Some("kept".to_string()), None)
        .expect("the store keeps working after a failed commit");
    let messages = JobStore::open_without_reconciliation(&path)
        .expect("reopen")
        .events(job.id)
        .expect("events")
        .into_iter()
        .filter_map(|event| event.message)
        .collect::<Vec<_>>();
    assert_eq!(messages, ["job queued", "kept"]);
}

#[test]
fn opening_and_reading_an_unchanged_store_leaves_the_database_file_untouched() {
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("jobs.json");
    let store = JobStore::open_without_reconciliation(&path).expect("store");
    store.create("idle");
    let database = tombstone_path(&path);
    let committed = fs::read(&database).expect("database bytes");

    store
        .durable_transaction(|_| Ok(()))
        .expect("no-op transaction");
    JobStore::open_without_reconciliation(&path).expect("reopen");
    JobStore::active_count_at_path(&path).expect("active count");
    JobStore::retained_owner_report_at_path(&path).expect("owner report");
    store
        .reconcile_controller_completions()
        .expect("completions");
    lookup_tombstone(&path, ReplayTombstoneKind::Controller, "absent").expect("lookup");

    assert_eq!(
        fs::read(&database).expect("database bytes"),
        committed,
        "a status read or no-op transaction must not rewrite the store"
    );
}
