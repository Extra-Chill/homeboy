//! End-to-end check through a real loopback daemon that a job held in the
//! daemon's capacity queue does not spend the observer's execution budget,
//! while an admitted job that overruns it still times out (#19108).
//!
//! `HOMEBOY_RUNNER_PRESSURE_SAMPLE` holds admission: the in-process daemon
//! queues the submitted job as `pending_admission_reason: pressure` until the
//! sample clears.

use super::super::daemon::{fetch_daemon_job, follow_daemon_job_to_terminal, DaemonWaitBudget};
use super::handoff::{wait_for_path, ReleaseBlockedWorkload, BLOCKED_WORKLOAD_LOOP};
use super::watch::{allow_unauthenticated_loopback_broker, daemon_client, loopback_broker_daemon};
use super::*;
use homeboy_core::api_jobs::JobStatus;
use std::time::{Duration, Instant};

/// Pressure far above every default threshold, so the daemon queues the job.
const SATURATED_SAMPLE: &str = r#"{"memory":10000}"#;

/// Submit a direct daemon job the way the gate transport does: the handoff
/// returns once the daemon accepts it, queued or not.
fn submit_detached(daemon_url: &str, cwd: &std::path::Path, command: Vec<String>) -> String {
    let (output, _) = exec_via_daemon(
        &ssh_runner(),
        daemon_url,
        None,
        cwd.display().to_string(),
        None,
        command,
        Default::default(),
        Default::default(),
        false,
        None,
        None,
        Vec::new(),
        Vec::new(),
        None,
        None,
        // run_id_owns_generic_exec, detach_after_handoff, mirror_evidence,
        // print_handoff_output.
        false,
        true,
        false,
        false,
        None,
    )
    .expect("detached direct daemon handoff");
    output.job_id.expect("accepted job id")
}

#[test]
fn queued_capacity_wait_spares_the_execution_budget_and_admitted_overrun_still_times_out() {
    crate::test_support::with_isolated_home(|_| {
        allow_unauthenticated_loopback_broker();
        let daemon_url = loopback_broker_daemon();
        let client = daemon_client();
        let workspace = tempfile::tempdir().expect("workspace");
        let execution = Duration::from_secs(2);
        let budget =
            DaemonWaitBudget::execution(execution).with_queue_bound(Duration::from_secs(120));

        // Case 1: held queued past the execution budget, then admitted and run
        // to success. The wait ends with the job's own terminal status.
        let mut pressure = Some(EnvVarGuard::set(
            homeboy_core::runner_pressure::SAMPLE_ENV,
            SATURATED_SAMPLE,
        ));
        let queued_job = submit_detached(
            &daemon_url,
            workspace.path(),
            vec!["sh".to_string(), "-c".to_string(), "exit 0".to_string()],
        );
        let mut first_queued: Option<Instant> = None;
        let mut queued_for = Duration::ZERO;
        let (terminal, _) = follow_daemon_job_to_terminal(
            daemon_url.clone(),
            &queued_job,
            budget,
            &|| false,
            |endpoint| {
                let job = fetch_daemon_job(&client, endpoint, &queued_job)?;
                if job.status == JobStatus::Queued {
                    let since = *first_queued.get_or_insert_with(Instant::now);
                    queued_for = since.elapsed();
                    // Release capacity only once the queue outlasted the
                    // whole execution budget.
                    if queued_for > execution + Duration::from_secs(1) {
                        pressure.take();
                    }
                }
                Ok(job)
            },
            || Ok(None),
        )
        .expect("a gate queued past its execution budget still runs to success");
        drop(pressure);
        assert_eq!(terminal.status, JobStatus::Succeeded);
        assert!(
            queued_for > execution,
            "the job must have waited in the capacity queue longer than the execution budget, waited {queued_for:?}"
        );

        // Case 2: admitted at once and running past the execution budget. The
        // wait times out as an overrun, not as a lost observation.
        let started = workspace.path().join("started");
        let release = workspace.path().join("release");
        let _release_on_drop = ReleaseBlockedWorkload(release.clone());
        let running_job = submit_detached(
            &daemon_url,
            workspace.path(),
            vec![
                "sh".to_string(),
                "-c".to_string(),
                BLOCKED_WORKLOAD_LOOP.to_string(),
                "sh".to_string(),
                started.display().to_string(),
                release.display().to_string(),
            ],
        );
        wait_for_path(&started, "admitted workload start");
        let error = follow_daemon_job_to_terminal(
            daemon_url.clone(),
            &running_job,
            budget,
            &|| false,
            |endpoint| fetch_daemon_job(&client, endpoint, &running_job),
            || Ok(None),
        )
        .expect_err("an admitted gate past its execution budget times out");
        assert!(
            error.details.get("daemon_terminal_wait_timeout").is_some(),
            "{error:?}"
        );
        assert!(!crate::execution::daemon_observation_was_lost(&error));
        std::fs::write(&release, "release").expect("release workload");
    });
}
