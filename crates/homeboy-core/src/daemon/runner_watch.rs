//! The transport-neutral Runner API v1 `watch` service (#13881 step 2).
//!
//! One authority serves the watch operation on both surfaces: the broker
//! `POST /runner/jobs/watch` route (credential-authorized) and the local
//! read-only `GET /jobs/:id/watch` route (the SSH tunnel is the trust
//! boundary). Request parsing and authorization stay at each route; this
//! module owns everything after them: job resolution, runner-bound ownership,
//! cursor paging, and terminal projection. Both routes therefore return
//! identical `homeboy/runner-api-watch-response/v1` bodies for the same job
//! and cursor.

use uuid::Uuid;

use crate::api_jobs::{JobStatus, JobStore};
use crate::error::Result;
use homeboy_runner_contract::{
    RunnerApiOperationFailure, RunnerApiOperationFailureCode, RunnerApiWatchRequest,
    RunnerApiWatchResponse, RunnerApiWatchTerminalOutcome, RunnerApiWatchedEvent, RUNNER_API_V1,
    RUNNER_API_WATCH_RESPONSE_SCHEMA,
};

/// Serve one watch read against the durable job store.
///
/// `watching_runner` is the identity the route resolved for the reader:
/// `Some(runner_id)` for a credential-bound broker read (the credential's
/// paired runner, or the runner the request names), and `None` for a trusted
/// local read through the tunnel, which carries no broker credential and owns
/// no runner-bound restriction — the same trust rule the neighbouring local
/// job reads use.
///
/// Operation-level conditions (unknown job, a runner without standing on the
/// job) are typed `RunnerApiOperationFailure` values in the versioned
/// response; store failures are `Err` and remain an HTTP concern at each
/// route.
pub(crate) fn watch_job(
    job_store: &JobStore,
    request: &RunnerApiWatchRequest,
    watching_runner: Option<&str>,
) -> Result<RunnerApiWatchResponse> {
    let job_id_not_found = |request: &RunnerApiWatchRequest, job_id: String| {
        watch_failure(
            request,
            RunnerApiOperationFailureCode::JobNotFound,
            format!("remote runner job not found: {job_id}"),
        )
    };
    let Ok(job_id) = Uuid::parse_str(&request.job_id) else {
        return Ok(job_id_not_found(request, request.job_id.clone()));
    };
    let Ok(job) = job_store.get(job_id) else {
        return Ok(job_id_not_found(request, job_id.to_string()));
    };
    if let Some(watching_runner) = watching_runner {
        if job
            .target_runner_id
            .as_deref()
            .is_some_and(|target| target != watching_runner)
        {
            return Ok(watch_failure(
                request,
                RunnerApiOperationFailureCode::RunnerNotAuthorized,
                format!("remote runner job is not owned by runner {watching_runner}"),
            ));
        }
    }

    // Exclusive lower bound: only events with `sequence > after_sequence` are
    // returned, ascending, so a resume sees every later event exactly once.
    let mut later_events = job_store
        .events(job_id)?
        .into_iter()
        .filter(|event| event.sequence > request.after_sequence)
        .collect::<Vec<_>>();
    later_events.sort_by_key(|event| event.sequence);
    let later_event_count = later_events.len();
    let page = match request.limit {
        Some(limit) => {
            let keep = (limit as usize).min(later_events.len());
            later_events.truncate(keep);
            later_events
        }
        None => later_events,
    };
    let next_sequence = page
        .last()
        .map(|event| event.sequence)
        .unwrap_or(request.after_sequence);
    // A terminal job is reported terminal only on the page that reaches the
    // end of its log, so a client that stops watching at `terminal: true`
    // never misses events left behind a `limit`.
    let page_reaches_end = page.len() == later_event_count;
    let terminal = job.status.is_terminal() && page_reaches_end;
    let terminal_outcome = match job.status {
        _ if !terminal => None,
        JobStatus::Succeeded => Some(RunnerApiWatchTerminalOutcome::Succeeded),
        JobStatus::Failed => Some(RunnerApiWatchTerminalOutcome::Failed),
        JobStatus::Cancelled => Some(RunnerApiWatchTerminalOutcome::Cancelled),
        JobStatus::Queued | JobStatus::Running => None,
    };
    Ok(RunnerApiWatchResponse {
        schema: RUNNER_API_WATCH_RESPONSE_SCHEMA.to_string(),
        api_version: RUNNER_API_V1,
        job_id: request.job_id.clone(),
        events: page
            .into_iter()
            .map(|event| RunnerApiWatchedEvent {
                sequence: event.sequence,
                kind: serde_json::to_value(event.kind)
                    .expect("serialize job event kind")
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                timestamp_ms: event.timestamp_ms,
                message: event.message,
                data: event.data,
            })
            .collect(),
        next_sequence,
        terminal,
        terminal_outcome,
        failure: None,
    })
}

/// The failure value of the watch operation: the versioned response envelope
/// with an empty event page and the typed operation failure carried in-band.
pub(crate) fn watch_failure(
    request: &RunnerApiWatchRequest,
    code: RunnerApiOperationFailureCode,
    message: impl Into<String>,
) -> RunnerApiWatchResponse {
    RunnerApiWatchResponse {
        schema: RUNNER_API_WATCH_RESPONSE_SCHEMA.to_string(),
        api_version: request.api_version,
        job_id: request.job_id.clone(),
        events: Vec::new(),
        next_sequence: request.after_sequence,
        terminal: false,
        terminal_outcome: None,
        failure: Some(RunnerApiOperationFailure {
            code,
            message: message.into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_jobs::JobEventKind;
    use crate::daemon::remote_runner;
    use crate::daemon::remote_runner::BrokerAuthContext;
    use crate::test_support::HomeGuard;
    use homeboy_runner_contract::{
        RunnerApiOperationFailureCode, RUNNER_API_V1, RUNNER_API_WATCH_REQUEST_SCHEMA,
        RUNNER_API_WATCH_RESPONSE_SCHEMA,
    };
    use serde_json::json;

    fn watch_request(job_id: &str, after_sequence: u64) -> RunnerApiWatchRequest {
        RunnerApiWatchRequest {
            schema: RUNNER_API_WATCH_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "homeboy-lab".to_string(),
            job_id: job_id.to_string(),
            after_sequence,
            limit: None,
        }
    }

    fn accepted_job_id(store: &JobStore) -> String {
        let job = store.create("runner.exec");
        store.start(job.id).expect("start the accepted test job");
        job.id.to_string()
    }

    fn append(store: &JobStore, job_id: &str, kind: JobEventKind, message: &str) -> u64 {
        store
            .append_event(
                Uuid::parse_str(job_id).expect("valid job id"),
                kind,
                Some(message.to_string()),
                None,
            )
            .expect("append event")
            .sequence
    }

    fn page_sequences(response: &RunnerApiWatchResponse) -> Vec<u64> {
        response.events.iter().map(|event| event.sequence).collect()
    }

    fn store_log_sequences(store: &JobStore, job_id: &str) -> Vec<u64> {
        store
            .events(Uuid::parse_str(job_id).expect("valid job id"))
            .expect("job events")
            .into_iter()
            .map(|event| event.sequence)
            .collect()
    }

    fn query_cursor(query: &str) -> u64 {
        query
            .split("after_sequence=")
            .nth(1)
            .and_then(|value| value.split('&').next())
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    fn failure_code(response: &RunnerApiWatchResponse) -> serde_json::Value {
        let failure = response.failure.as_ref().expect("typed watch failure");
        serde_json::to_value(failure.code).expect("failure code")
    }

    /// Route parity (#13881 step 2): the broker route and the read-only local
    /// route return identical watch bodies for the same job and cursor, both
    /// from cursor 0 and from a middle cursor.
    #[test]
    fn broker_and_read_only_routes_return_identical_watch_bodies() {
        use crate::http_api::{handle_with_jobs, HttpApiRequest, HttpMethod};

        let _home = HomeGuard::new();
        let store = JobStore::default();
        let submit = remote_runner::route(
            "POST",
            "/runner/jobs",
            Some(json!({
                "runner_id": "homeboy-lab",
                "command": ["homeboy", "test"],
                "cwd": "/tmp/x"
            })),
            &store,
            &BrokerAuthContext::trusted_local(),
        );
        assert_eq!(submit.status_code, 200, "submit body: {}", submit.body);
        let job_id = submit.body["body"]["job"]["id"]
            .as_str()
            .expect("job id")
            .to_string();
        append(&store, &job_id, JobEventKind::Progress, "one");
        let second = append(&store, &job_id, JobEventKind::Stdout, "two");
        append(&store, &job_id, JobEventKind::Progress, "three");

        let body_for = |query: &str| {
            let read_only = handle_with_jobs(
                HttpApiRequest {
                    method: HttpMethod::Get,
                    path: format!("/jobs/{job_id}/watch{query}"),
                    body: None,
                },
                &store,
            )
            .expect("read-only watch");
            assert_eq!(read_only.status, 200);
            let broker = remote_runner::route(
                "POST",
                "/runner/jobs/watch",
                Some(json!({
                    "schema": RUNNER_API_WATCH_REQUEST_SCHEMA,
                    "api_version": { "major": 1 },
                    "runner_id": "homeboy-lab",
                    "job_id": job_id,
                    "after_sequence": query_cursor(query),
                })),
                &store,
                &BrokerAuthContext::trusted_local(),
            );
            assert_eq!(broker.status_code, 200, "broker body: {}", broker.body);
            (
                read_only.body["response"].clone(),
                broker.body["body"]["response"].clone(),
            )
        };

        let (from_zero_read_only, from_zero_broker) = body_for("");
        assert_eq!(from_zero_read_only, from_zero_broker);
        assert_eq!(
            from_zero_read_only["events"]
                .as_array()
                .expect("events")
                .len(),
            store_log_sequences(&store, &job_id).len()
        );
        let middle = format!("?after_sequence={second}");
        let (from_middle_read_only, from_middle_broker) = body_for(&middle);
        assert_eq!(from_middle_read_only, from_middle_broker);
        assert_eq!(
            from_middle_read_only["next_sequence"],
            from_zero_broker["next_sequence"]
        );
    }

    #[test]
    fn watch_pages_the_log_from_zero_and_middle_cursors_identically() {
        let _home = HomeGuard::new();
        let store = JobStore::default();
        let job_id = accepted_job_id(&store);
        append(&store, &job_id, JobEventKind::Progress, "one");
        let second = append(&store, &job_id, JobEventKind::Stdout, "two");
        append(&store, &job_id, JobEventKind::Progress, "three");

        let from_zero = watch_job(&store, &watch_request(&job_id, 0), None).expect("watch");
        let from_middle = watch_job(&store, &watch_request(&job_id, second), None).expect("watch");
        assert_eq!(
            page_sequences(&from_zero),
            store_log_sequences(&store, &job_id)
        );
        assert_eq!(page_sequences(&from_middle), vec![second + 1]);
        assert_eq!(from_zero.next_sequence, second + 1);
        assert_eq!(from_middle.next_sequence, second + 1);
        assert!(!from_zero.terminal);
        assert!(from_zero.failure.is_none());
    }

    #[test]
    fn watch_reports_a_terminal_job_only_at_the_end_of_its_log() {
        let _home = HomeGuard::new();
        let store = JobStore::default();
        let job_id = accepted_job_id(&store);
        let first = append(&store, &job_id, JobEventKind::Progress, "one");
        append(&store, &job_id, JobEventKind::Stdout, "two");
        store
            .complete(Uuid::parse_str(&job_id).expect("valid job id"), None)
            .expect("complete the test job");

        let paged = watch_job(
            &store,
            &RunnerApiWatchRequest {
                limit: Some(1),
                ..watch_request(&job_id, 0)
            },
            None,
        )
        .expect("watch");
        assert_eq!(
            page_sequences(&paged),
            vec![store_log_sequences(&store, &job_id)[0]]
        );
        assert!(!paged.terminal);
        assert!(paged.terminal_outcome.is_none());

        let caught_up = watch_job(&store, &watch_request(&job_id, first), None).expect("watch");
        assert!(caught_up.terminal);
        assert_eq!(
            caught_up.terminal_outcome,
            Some(RunnerApiWatchTerminalOutcome::Succeeded)
        );
    }

    #[test]
    fn watch_rejects_a_foreign_runner_and_an_unknown_job_in_band() {
        let _home = HomeGuard::new();
        let store = JobStore::default();
        let submit = remote_runner::route(
            "POST",
            "/runner/jobs",
            Some(json!({
                "runner_id": "homeboy-lab",
                "command": ["homeboy", "test"],
                "cwd": "/tmp/x"
            })),
            &store,
            &BrokerAuthContext::trusted_local(),
        );
        assert_eq!(submit.status_code, 200, "submit body: {}", submit.body);
        let job_id = submit.body["body"]["job"]["id"]
            .as_str()
            .expect("job id")
            .to_string();
        append(&store, &job_id, JobEventKind::Progress, "one");

        let foreign =
            watch_job(&store, &watch_request(&job_id, 0), Some("other-runner")).expect("watch");
        assert_eq!(
            failure_code(&foreign),
            serde_json::to_value(RunnerApiOperationFailureCode::RunnerNotAuthorized)
                .expect("unauthorized code")
        );

        let missing =
            watch_job(&store, &watch_request(&Uuid::new_v4().to_string(), 0), None).expect("watch");
        assert_eq!(
            failure_code(&missing),
            serde_json::to_value(RunnerApiOperationFailureCode::JobNotFound).expect("missing code")
        );
        assert!(missing.events.is_empty());
        assert_eq!(missing.next_sequence, 0);
        assert!(!missing.terminal);
        let encoded = serde_json::to_value(&missing).expect("serialize watch response");
        assert_eq!(encoded["schema"], RUNNER_API_WATCH_RESPONSE_SCHEMA);
        assert_eq!(encoded["job_id"], json!(missing.job_id));
        assert_eq!(encoded["terminal"], json!(false));
    }
}
