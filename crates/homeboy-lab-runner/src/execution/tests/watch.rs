//! End-to-end coverage for the controller following Lab jobs with the Runner
//! API v1 watch cursor (#13881 step 2): route parity between the broker and
//! read-only watch surfaces, cursor-follow event-log equality, and resume
//! after a dropped connection.

use super::super::daemon::{
    fetch_daemon_events, fetch_daemon_watch_resilient_with_endpoint_reload, DaemonWatchFollow,
};
use super::handoff::wait_for_path;
use super::*;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn loopback_broker_daemon() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("daemon listener");
    let daemon_url = format!("http://{}", listener.local_addr().expect("daemon address"));
    std::thread::spawn(move || {
        let _ = homeboy_core::daemon::serve_listener(listener);
    });
    daemon_url
}

fn allow_unauthenticated_loopback_broker() {
    // The in-process daemon's /exec endpoint drives runner processes through
    // the RunnerExecDriver hook; register the runner-side driver so the
    // end-to-end tests here can run the child (production wires this at CLI
    // startup). The fixed binary hash keeps in-process daemon startup off the
    // multi-hundred-MB debug binary hashing path.
    crate::register_runner_daemon_exec_driver();
    std::env::set_var(
        homeboy_core::daemon::DAEMON_BINARY_SHA_OVERRIDE_ENV,
        "sha256:test-fixed-daemon-binary",
    );
    homeboy_core::broker_auth::BrokerAuthStore {
        allow_unauthenticated_loopback: true,
        ..Default::default()
    }
    .save()
    .expect("save loopback broker auth opt-in");
}

fn daemon_client() -> Client {
    Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("daemon client")
}
fn post_broker(client: &Client, daemon_url: &str, path: &str, body: Value) -> Value {
    let response = client
        .post(format!("{daemon_url}{path}"))
        .json(&body)
        .send()
        .expect("broker POST");
    let status = response.status();
    let envelope: Value = response.json().expect("broker envelope");
    assert_eq!(
        envelope["success"], true,
        "broker POST {path} returned {status}: {envelope}"
    );
    envelope["data"]["body"].clone()
}

/// Submit a queued runner job through the broker surface and claim it, so the
/// test can stream events into the daemon's durable log over HTTP.
fn submit_claimed_broker_job(client: &Client, daemon_url: &str) -> (String, String) {
    let submitted = post_broker(
        client,
        daemon_url,
        "/runner/jobs",
        json!({
            "runner_id": "lab",
            "command": ["homeboy", "test"],
            "cwd": "/tmp/watch-parity"
        }),
    );
    let job_id = submitted["job"]["id"]
        .as_str()
        .expect("submitted job id")
        .to_string();
    let claim = post_broker(
        client,
        daemon_url,
        "/runner/jobs/claim",
        json!({ "runner_id": "lab", "lease_ms": 60_000 }),
    );
    let claim_id = claim["claim"]["job"]["claim_id"]
        .as_str()
        .expect("claim id")
        .to_string();
    (job_id, claim_id)
}

fn append_broker_event(
    client: &Client,
    daemon_url: &str,
    job_id: &str,
    claim_id: &str,
    kind: &str,
    message: &str,
) -> u64 {
    let appended = post_broker(
        client,
        daemon_url,
        &format!("/runner/jobs/{job_id}/events"),
        json!({
            "runner_id": "lab",
            "claim_id": claim_id,
            "kind": kind,
            "message": message,
        }),
    );
    appended["event"]["sequence"]
        .as_u64()
        .expect("event sequence")
}

fn read_only_watch(client: &Client, daemon_url: &str, job_id: &str, query: &str) -> Value {
    let response: Value = client
        .get(format!("{daemon_url}/jobs/{job_id}/watch{query}"))
        .send()
        .expect("read-only watch request")
        .json()
        .expect("read-only watch envelope");
    assert_eq!(
        response["success"], true,
        "read-only watch envelope: {response}"
    );
    response["data"]["body"]["response"].clone()
}

fn broker_watch(
    client: &Client,
    daemon_url: &str,
    job_id: &str,
    runner_id: &str,
    after_sequence: u64,
) -> Value {
    let body = post_broker(
        client,
        daemon_url,
        "/runner/jobs/watch",
        json!({
            "schema": homeboy_runner_contract::RUNNER_API_WATCH_REQUEST_SCHEMA,
            "api_version": { "major": 1 },
            "runner_id": runner_id,
            "job_id": job_id,
            "after_sequence": after_sequence,
        }),
    );
    body["response"].clone()
}

fn event_sequences(events: &[homeboy_core::api_jobs::JobEvent]) -> Vec<u64> {
    events.iter().map(|event| event.sequence).collect()
}

/// Releases a blocked child workload when the test ends, even on failure.
struct ReleaseBlockedWorkload(std::path::PathBuf);

impl Drop for ReleaseBlockedWorkload {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, "release");
    }
}

#[test]
fn watch_routes_return_identical_bodies_from_zero_and_middle_cursors() {
    homeboy_core::test_support::with_isolated_home(|_| {
        allow_unauthenticated_loopback_broker();
        let daemon_url = loopback_broker_daemon();
        let client = daemon_client();
        let (job_id, claim_id) = submit_claimed_broker_job(&client, &daemon_url);
        append_broker_event(&client, &daemon_url, &job_id, &claim_id, "progress", "one");
        let second = append_broker_event(&client, &daemon_url, &job_id, &claim_id, "stdout", "two");
        append_broker_event(
            &client,
            &daemon_url,
            &job_id,
            &claim_id,
            "progress",
            "three",
        );

        let full_log = fetch_daemon_events(&client, &daemon_url, &job_id).expect("full log");
        for (query, cursor) in [
            ("", 0),
            (format!("?after_sequence={second}").as_str(), second),
        ] {
            let local = read_only_watch(&client, &daemon_url, &job_id, query);
            let broker = broker_watch(&client, &daemon_url, &job_id, "lab", cursor);
            assert_eq!(local, broker, "watch routes must agree for cursor {cursor}");
            let seen = local["events"]
                .as_array()
                .expect("watched events")
                .iter()
                .map(|event| event["sequence"].as_u64().expect("sequence"))
                .collect::<Vec<_>>();
            let expected: Vec<u64> = event_sequences(&full_log)
                .into_iter()
                .filter(|sequence| *sequence > cursor)
                .collect();
            assert_eq!(seen, expected, "cursor {cursor}");
            assert_eq!(local["next_sequence"], broker["next_sequence"]);
        }
    });
}

#[test]
fn watch_follow_reproduces_the_daemon_event_log_and_reports_promotion_once() {
    homeboy_core::test_support::with_isolated_home(|_| {
        allow_unauthenticated_loopback_broker();
        let run_id = "cook-13881-watch-follow";
        homeboy_agents::agent_task_lifecycle::record_lab_offload_phase(
            run_id,
            "lab",
            "dispatching",
            None,
            None,
            None,
            None,
        )
        .expect("persist controller proxy before daemon acceptance");
        let workspace = tempfile::tempdir().expect("workspace");
        let started = workspace.path().join("started");
        let release = workspace.path().join("release");
        let _release_on_drop = ReleaseBlockedWorkload(release.clone());
        let daemon_url = loopback_broker_daemon();

        let frame = |message: &str| {
            format!(
                "printf 'HOMEBOY_RUNNER_PROGRESS {{\"schema\":\"homeboy/runner-progress/v1\",\"phase\":\"promotion\",\"metadata\":{{\"promotion\":{{\"schema\":\"homeboy/promotion-progress-frame/v1\",\"message\":\"{message}\"}}}}}}}}\\n'"
            )
        };
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "{}; echo stdout-one; {}; echo stdout-two; printf started > \"$1\"; while [ ! -e \"$2\" ]; do sleep 0.01; done",
                frame("promotion progress: applying patch"),
                frame("promotion progress: gate running"),
            ),
            "sh".to_string(),
            started.display().to_string(),
            release.display().to_string(),
        ];

        // The foreground follow blocks until the job is terminal, so release
        // the workload from another thread once it has started.
        let releaser = {
            let started = started.clone();
            let release = release.clone();
            std::thread::spawn(move || {
                wait_for_path(&started, "watched workload start");
                std::fs::write(&release, "release").expect("release workload");
            })
        };
        let (output, exit_code) = exec_via_daemon(
            &ssh_runner(),
            &daemon_url,
            None,
            workspace.path().display().to_string(),
            None,
            command,
            Default::default(),
            Vec::new(),
            false,
            None,
            None,
            Vec::new(),
            Vec::new(),
            None,
            Some(run_id.to_string()),
            false,
            // run_id_owns_generic_exec, detach_after_handoff, mirror_evidence,
            // print_handoff_output: follow the job to its terminal state.
            false,
            false,
            false,
            None,
        )
        .expect("watched direct-daemon handoff");
        releaser.join().expect("releaser thread");

        assert_eq!(exit_code, 0, "stderr: {}", output.stderr);
        assert_eq!(
            output.job.as_ref().map(|job| job.status),
            Some(JobStatus::Succeeded)
        );

        // The follow loop's terminal event log must equal the daemon's full
        // log exactly: same sequences, same order, no duplicates.
        let client = daemon_client();
        let job_id = output.job_id.as_deref().expect("accepted job id");
        let full_log = fetch_daemon_events(&client, &daemon_url, job_id).expect("full log");
        let followed = output.job_events.as_deref().expect("followed event log");
        assert_eq!(event_sequences(&followed), event_sequences(&full_log));

        // Every promotion frame is reported exactly once each, in log order.
        let record = homeboy_agents::agent_task_lifecycle::reconcile_status(run_id)
            .expect("controller record");
        let reported = record
            .metadata
            .get("promotion_progress_frames")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let reported_sequences: Vec<u64> = reported
            .iter()
            .filter_map(|frame| frame["sequence"].as_u64())
            .collect();
        let daemon_promotion_sequences: Vec<u64> = full_log
            .iter()
            .filter(|event| {
                event
                    .data
                    .as_ref()
                    .and_then(|data| data.pointer("/metadata/promotion/schema"))
                    .and_then(Value::as_str)
                    == Some("homeboy/promotion-progress-frame/v1")
            })
            .map(|event| event.sequence)
            .collect();
        assert_eq!(reported_sequences, daemon_promotion_sequences);
    });
}

#[test]
fn watch_follow_resumes_a_dropped_connection_without_losing_events() {
    homeboy_core::test_support::with_isolated_home(|_| {
        allow_unauthenticated_loopback_broker();
        let daemon_url = loopback_broker_daemon();
        let client = daemon_client();
        let (job_id, claim_id) = submit_claimed_broker_job(&client, &daemon_url);
        append_broker_event(&client, &daemon_url, &job_id, &claim_id, "progress", "one");
        let job_uuid = uuid::Uuid::parse_str(&job_id).expect("valid job id");

        // Read the first page, then lose the endpoint.
        let mut follow = DaemonWatchFollow::default();
        let (first, _) = fetch_daemon_watch_resilient_with_endpoint_reload(
            &client,
            &daemon_url,
            &job_id,
            follow.cursor,
            || Ok(None),
        )
        .expect("first watch page");
        follow.absorb(job_uuid, &first).expect("absorb first page");
        assert!(follow.events.len() >= 2, "first page: {first:?}");

        // Events land while the connection is down. The retry goes through a
        // dead endpoint and recover over the reloaded one, repeating the read
        // from the same cursor.
        append_broker_event(
            &client,
            &daemon_url,
            &job_id,
            &claim_id,
            "progress",
            "three",
        );
        let fourth =
            append_broker_event(&client, &daemon_url, &job_id, &claim_id, "stdout", "four");
        let unavailable = std::net::TcpListener::bind("127.0.0.1:0").expect("dead listener");
        let dead_endpoint = format!("http://{}", unavailable.local_addr().expect("dead address"));
        drop(unavailable);
        let reloads = AtomicUsize::new(0);
        let (resumed, endpoint) = fetch_daemon_watch_resilient_with_endpoint_reload(
            &client,
            &dead_endpoint,
            &job_id,
            follow.cursor,
            || {
                reloads.fetch_add(1, Ordering::SeqCst);
                Ok(Some(daemon_url.clone()))
            },
        )
        .expect("resumed watch page");
        assert_eq!(endpoint, daemon_url);
        assert!(reloads.load(Ordering::SeqCst) >= 1);
        follow
            .absorb(job_uuid, &resumed)
            .expect("absorb resumed page");

        // Finish the job, then assemble the terminal log from the cursor.
        post_broker(
            &client,
            &daemon_url,
            &format!("/runner/jobs/{job_id}/finish"),
            json!({
                "runner_id": "lab",
                "claim_id": claim_id,
                "result": { "exit_code": 0 }
            }),
        );
        follow
            .drain_to_end(&client, &daemon_url, &job_id, || Ok(None))
            .expect("terminal event log");
        let events = follow.events.clone();
        let full_log = fetch_daemon_events(&client, &daemon_url, &job_id).expect("full log");
        let followed = event_sequences(&events);
        let expected = event_sequences(&full_log);
        assert_eq!(followed, expected);
        assert!(followed.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(followed.contains(&fourth));
        // No page was counted twice.
        assert_eq!(
            followed.len(),
            followed
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        );
    });
}
