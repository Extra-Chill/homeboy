//! A gate observer that loses its daemon transport keeps following the same
//! job to its terminal status (#17376).

use super::super::daemon::follow_daemon_job_to_terminal;
use super::*;
use crate::daemon_http_get::{daemon_transport_error, DaemonHttpErrorKind};
use homeboy_core::api_jobs::{Job, JobStatus};
use std::cell::RefCell;
use std::time::Duration;

fn job(status: JobStatus) -> Job {
    let mut job: Job = serde_json::from_value(serde_json::json!({
        "id": "7f0b7a7e-2f0e-4c4e-9a37-7a3e3c1f0001",
        "operation": "runner.exec",
        "status": "running",
        "created_at_ms": 1,
        "updated_at_ms": 1,
        "event_count": 0,
    }))
    .expect("job fixture");
    job.status = status;
    job
}

fn send_failed() -> Error {
    daemon_transport_error(
        DaemonHttpErrorKind::Request,
        "/jobs/x",
        None,
        "query runner daemon",
        "error sending request",
    )
}

fn no_session() -> Error {
    crate::connection::no_current_daemon_session_error("homeboy-lab")
}

#[test]
fn lost_transport_reattaches_and_reads_the_same_jobs_terminal_status() {
    let reads = RefCell::new(Vec::new());
    let reloads = RefCell::new(0);
    let (terminal, endpoint) = follow_daemon_job_to_terminal(
        "http://old".to_string(),
        "x",
        Duration::from_secs(30),
        &|| false,
        |endpoint| {
            reads.borrow_mut().push(endpoint.to_string());
            match reads.borrow().len() {
                1 => Ok(job(JobStatus::Running)),
                2 | 3 => Err(send_failed()),
                _ => Ok(job(JobStatus::Succeeded)),
            }
        },
        || {
            *reloads.borrow_mut() += 1;
            // The first reattach races a concurrent session replace.
            if *reloads.borrow() == 1 {
                Err(no_session())
            } else {
                Ok(Some("http://new".to_string()))
            }
        },
    )
    .expect("the job stays observable across the transport loss");

    assert_eq!(terminal.status, JobStatus::Succeeded);
    assert_eq!(endpoint, "http://new");
    assert_eq!(
        *reads.borrow(),
        ["http://old", "http://old", "http://old", "http://new"]
    );
}

#[test]
fn transport_lost_past_the_budget_is_a_lost_observation_not_a_failure() {
    let error = follow_daemon_job_to_terminal(
        "http://old".to_string(),
        "x",
        Duration::from_millis(600),
        &|| false,
        |_| Err(send_failed()),
        || Ok(None),
    )
    .expect_err("the daemon never came back");

    assert_eq!(error.retryable, Some(true));
    assert!(daemon_observation_was_lost(&error));
}
