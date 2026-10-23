use super::*;
use crate::evidence::mirror::{incremental_job_snapshot, refresh_snapshot};
use serde_json::Value;
use std::collections::HashMap;

#[test]
fn incremental_poll_counts_routes_and_skips_unchanged_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("observation.db");
    let store = ObservationStore::open_initialized_at(&db).unwrap();
    let mut run = store
        .start_run(NewRunRecord::builder("runner-exec").build())
        .unwrap();
    let runner = ssh_runner();
    let mut job = terminal_runner_job();
    job.status = JobStatus::Running;
    job.finished_at_ms = None;
    let id = job.id.to_string();
    let mut routes = HashMap::<String, usize>::new();
    let event = json!({"sequence": 1, "kind": "progress", "timestamp_ms": 1, "message": "working"});
    let history = (1..=1000)
        .map(|sequence| {
            let mut event = event.clone();
            event["sequence"] = json!(sequence);
            event
        })
        .collect::<Vec<_>>();
    assert_eq!(serde_json::to_vec(&history).unwrap().len(), 71_894);
    assert_eq!(serde_json::to_vec(&history[..32]).unwrap().len(), 2_264);
    let mut bytes = Vec::new();
    for poll in 0..3 {
        let after = run
            .metadata_json
            .pointer("/lab/last_sequence")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let snapshot = incremental_job_snapshot(&id, after, |path| {
            *routes.entry(path.to_string()).or_default() += 1;
            if path == format!("/jobs/{id}") {
                return Ok(json!({"body": {"job": job}}));
            }
            assert_eq!(
                path,
                format!("/jobs/{id}/watch?after_sequence={after}&limit=32")
            );
            let events = if after == 0 {
                json!([event])
            } else {
                json!([])
            };
            bytes.push(serde_json::to_vec(&events).unwrap().len());
            Ok(json!({"body": {"events": events, "terminal": false, "failure": null}}))
        })
        .unwrap();
        let connection = rusqlite::Connection::open(&db).unwrap();
        // Abort every UPDATE after the first poll: this proves unchanged
        // publication doesn't merely write the same bytes again.
        if poll == 1 {
            connection.execute_batch("CREATE TRIGGER no_metadata_write BEFORE UPDATE ON runs BEGIN SELECT RAISE(ABORT, 'unexpected metadata write'); END;").unwrap();
        }
        let refresh = refresh_snapshot(&store, &run, &runner, snapshot, || {
            panic!("running job must not reconcile generations")
        })
        .unwrap();
        assert_eq!(refresh.job.unwrap().id, job.id);
        run = refresh.run;
    }
    assert_eq!(routes[&format!("/jobs/{id}")], 3);
    assert_eq!(routes.values().sum::<usize>(), 6);
    assert!(!routes.contains_key(&format!("/jobs/{id}/events")));
    assert_eq!(bytes[1..], [2, 2]); // Empty event arrays, not a repeated log.
    eprintln!(
        "event-array bytes: 1000-event history=71894, bounded first page=2264, unchanged poll=2"
    );
    assert_eq!(run.metadata_json["lab"]["remote_event_count"], 1);
    assert_eq!(
        run.metadata_json["lab"]["remote_events"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn result_summary_survives_an_empty_incremental_page_without_a_write() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("runs.db");
    let store = ObservationStore::open_initialized_at(&db).unwrap();
    let runner = ssh_runner();
    let run = store
        .start_run(NewRunRecord::builder("runner-exec").build())
        .unwrap();
    let mut job = terminal_runner_job();
    job.status = JobStatus::Running;
    job.finished_at_ms = None;
    let event = JobEvent {
        sequence: 1,
        job_id: job.id,
        kind: JobEventKind::Result,
        timestamp_ms: 1,
        message: None,
        data: Some(json!({"exit_code": 0})),
    };
    let first = refresh_snapshot(
        &store,
        &run,
        &runner,
        (
            homeboy_core::api_jobs::RunnerJobLogSnapshot {
                job: job.clone(),
                events: vec![event],
            },
            false,
        ),
        || Ok(()),
    )
    .unwrap();
    rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TRIGGER no_metadata_write BEFORE UPDATE ON runs BEGIN SELECT RAISE(ABORT, 'unexpected metadata write'); END;").unwrap();
    let second = refresh_snapshot(
        &store,
        &first.run,
        &runner,
        (
            homeboy_core::api_jobs::RunnerJobLogSnapshot {
                job,
                events: vec![],
            },
            false,
        ),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(
        second.run.metadata_json["lab"]["result_summary"]["exit_code"],
        0
    );
}

#[test]
fn bounded_terminal_pages_preserve_result_until_reconciliation() {
    let temp = tempfile::tempdir().unwrap();
    let store = ObservationStore::open_initialized_at(temp.path().join("runs.db")).unwrap();
    let runner = ssh_runner();
    let mut run = store
        .start_run(NewRunRecord::builder("runner-exec").build())
        .unwrap();
    let job = terminal_runner_job();
    let id = job.id.to_string();
    for (sequence, terminal) in [(32, false), (33, true)] {
        let snapshot = incremental_job_snapshot(&id, sequence - 1, |path| {
            if path == format!("/jobs/{id}") { return Ok(json!({"body": {"job": job}})); }
            Ok(json!({"body": {"events": [{"sequence": sequence, "kind": "result", "timestamp_ms": 1, "data": {"exit_code": 0}}], "terminal": terminal}}))
        }).unwrap();
        let refresh = refresh_snapshot(&store, &run, &runner, snapshot, || Ok(())).unwrap();
        let settled = homeboy_core::observation::runs_service::reconcile_owned_stale_running_run_with_snapshot(&store, &refresh.run, refresh.reconciliation_job()).unwrap();
        run = if settled.is_some() {
            store.get_run(&run.id).unwrap().unwrap()
        } else {
            refresh.run
        };
        assert_eq!(run.status, if terminal { "pass" } else { "running" });
        assert_eq!(run.metadata_json["lab"]["result_summary"]["exit_code"], 0);
    }
}
