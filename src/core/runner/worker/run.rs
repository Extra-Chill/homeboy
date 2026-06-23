use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde_json::json;

use crate::core::api_jobs::RemoteRunnerJobResult;
use crate::core::error::{Error, Result};

use super::super::execution::{exec_worker_local_until_cancelled, RunnerExecOptions};
use super::broker::{
    append_progress, cancelled_job_snapshot, claim_job, finish_job, start_claim_heartbeat,
};
use super::result::{
    cancelled_output, claimed_output, log_worker_event, remote_runner_result_from_exec_output,
    reverse_worker_capability_preflight,
};
use super::types::{ReverseRunnerWorkerOptions, ReverseRunnerWorkerOutput};

const BROKER_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub fn run_reverse_worker(
    options: ReverseRunnerWorkerOptions,
) -> Result<(ReverseRunnerWorkerOutput, i32)> {
    let stop = Arc::new(AtomicBool::new(false));
    if options.loop_mode {
        crate::core::process::install_shutdown_handler(stop.clone(), "runner worker")?;
    }
    run_reverse_worker_with_stop(options, stop)
}

fn run_reverse_worker_with_stop(
    options: ReverseRunnerWorkerOptions,
    stop: Arc<AtomicBool>,
) -> Result<(ReverseRunnerWorkerOutput, i32)> {
    if options.runner_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            "reverse runner worker requires a runner id",
            None,
            None,
        ));
    }
    if options.loop_mode {
        run_loop(options, stop, thread::sleep)
    } else {
        run_once_output(options, 1, 0, 0, false)
    }
}

pub(super) fn run_loop(
    options: ReverseRunnerWorkerOptions,
    stop: Arc<AtomicBool>,
    mut sleep: impl FnMut(Duration),
) -> Result<(ReverseRunnerWorkerOutput, i32)> {
    let mut iterations = 0;
    let mut jobs_claimed = 0;
    let mut broker_failures = 0;
    let mut idle_backoff = Duration::from_millis(options.idle_backoff_ms.max(1));
    let max_idle_backoff = Duration::from_millis(options.max_idle_backoff_ms.max(1));
    let broker_failure_backoff = Duration::from_millis(options.broker_failure_backoff_ms.max(1));
    let mut last_job = None;
    let mut last_exit_code = None;
    let mut last_claim = None;
    let mut last_result = None;
    let mut last_error = None;

    log_worker_event(
        &options,
        "started",
        json!({
            "loop": true,
            "idle_backoff_ms": idle_backoff.as_millis(),
            "max_idle_backoff_ms": max_idle_backoff.as_millis(),
            "broker_failure_backoff_ms": broker_failure_backoff.as_millis(),
            "broker_retry_limit": options.broker_retry_limit,
        }),
    );

    while !stop.load(Ordering::SeqCst) {
        iterations += 1;
        match run_once_output(
            options.clone(),
            iterations,
            jobs_claimed,
            broker_failures,
            false,
        ) {
            Ok((output, exit_code)) => {
                broker_failures = 0;
                if output.claimed {
                    jobs_claimed += 1;
                    idle_backoff = Duration::from_millis(options.idle_backoff_ms.max(1));
                    last_claim = output.job.as_ref().map(|job| job.id.to_string());
                    last_result = Some(exit_code);
                    if exit_code == 0 {
                        last_error = None;
                    } else {
                        last_error = Some(format!("job exited with code {exit_code}"));
                    }
                    last_job = output.job;
                    last_exit_code = Some(exit_code);
                    log_worker_event(
                        &options,
                        "job_finished",
                        json!({
                            "iteration": iterations,
                            "job_id": last_claim.as_ref(),
                            "exit_code": exit_code,
                            "jobs_claimed": jobs_claimed,
                        }),
                    );
                } else {
                    log_worker_event(
                        &options,
                        "idle",
                        json!({
                            "iteration": iterations,
                            "sleep_ms": idle_backoff.as_millis(),
                        }),
                    );
                    sleep(idle_backoff);
                    idle_backoff = std::cmp::min(idle_backoff.saturating_mul(2), max_idle_backoff);
                }
            }
            Err(err) => {
                broker_failures = broker_failures.saturating_add(1);
                last_error = Some(err.to_string());
                log_worker_event(
                    &options,
                    "broker_failure",
                    json!({
                        "iteration": iterations,
                        "broker_failures": broker_failures,
                        "retry_limit": options.broker_retry_limit,
                        "sleep_ms": broker_failure_backoff.as_millis(),
                        "error": err.to_string(),
                    }),
                );
                if broker_failures > options.broker_retry_limit {
                    return Err(err);
                }
                sleep(broker_failure_backoff);
            }
        }
    }

    log_worker_event(
        &options,
        "stopped",
        json!({
            "iterations": iterations,
            "jobs_claimed": jobs_claimed,
            "broker_failures": broker_failures,
        }),
    );
    Ok((
        ReverseRunnerWorkerOutput {
            variant: "work",
            command: "runner.work",
            runner_id: options.runner_id,
            broker_url: options.broker_url,
            claimed: jobs_claimed > 0,
            loop_mode: true,
            iterations,
            jobs_claimed,
            broker_failures,
            stopped: true,
            last_claim,
            last_result,
            last_error,
            job: last_job,
            exit_code: last_exit_code,
        },
        0,
    ))
}

fn run_once_output(
    options: ReverseRunnerWorkerOptions,
    iterations: u64,
    jobs_claimed: u64,
    broker_failures: u32,
    stopped: bool,
) -> Result<(ReverseRunnerWorkerOutput, i32)> {
    if options.broker_url.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "broker_url",
            "reverse runner worker requires a broker URL",
            None,
            None,
        ));
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build broker HTTP client: {err}")))?;
    let claim = claim_job(&client, &options)?;
    let Some(claim) = claim else {
        let loop_mode = options.loop_mode;
        return Ok((
            ReverseRunnerWorkerOutput {
                variant: "work",
                command: "runner.work",
                runner_id: options.runner_id,
                broker_url: options.broker_url,
                claimed: false,
                loop_mode,
                iterations: if loop_mode { iterations } else { 0 },
                jobs_claimed: if loop_mode { jobs_claimed } else { 0 },
                broker_failures: if loop_mode { broker_failures } else { 0 },
                stopped,
                last_claim: None,
                last_result: None,
                last_error: None,
                job: None,
                exit_code: None,
            },
            0,
        ));
    };

    if let Some(job) = cancelled_job_snapshot(
        &client,
        &options.broker_url,
        options.broker_token.as_deref(),
        &claim.job,
    )? {
        return Ok(cancelled_output(
            options,
            iterations,
            jobs_claimed,
            broker_failures,
            stopped,
            job,
        ));
    }

    append_progress(
        &client,
        &options.broker_url,
        options.broker_token.as_deref(),
        &options.runner_id,
        &claim.job,
    )?;
    let _heartbeat = start_claim_heartbeat(&client, &options, &claim.job)?;
    // Remote capability-parity preflight: validate that this runner can satisfy
    // the claimed job's top-level command (and any required paths) before
    // starting execution, so a missing tool fails before remote dispatch instead
    // of mid-run (#5093). The local worker execution path runs this preflight
    // before handing the claimed job directly to the local runtime.
    let capability_preflight = reverse_worker_capability_preflight(&claim.request);
    // Shared finisher so the exec-error and exec-success paths submit their
    // terminal job result through identical broker plumbing (#5091).
    let finish = |result: RemoteRunnerJobResult| {
        finish_job(
            &client,
            &options.broker_url,
            options.broker_token.as_deref(),
            &options.runner_id,
            &claim.job,
            result,
        )
    };
    let mut cancel_seen = false;
    let mut last_cancel_poll = Instant::now();
    let exec_result = exec_worker_local_until_cancelled(
        &options.runner_id,
        RunnerExecOptions {
            cwd: claim.request.cwd.clone(),
            project_id: claim.request.project_id.clone(),
            allow_diagnostic_ssh: false,
            command: claim.request.command.clone(),
            env: claim.request.env.clone(),
            secret_env_names: claim.request.secret_env_names.clone(),
            capture_patch: claim.request.capture_patch,
            raw_exec: false,
            source_snapshot: claim.request.source_snapshot.clone(),
            capability_preflight,
            required_extensions: claim.request.required_extensions(),
            require_paths: claim.request.require_paths.clone(),
            runner_workload: claim.request.runner_workload.clone(),
            detach_after_handoff: false,
        },
        || {
            if cancel_seen || last_cancel_poll.elapsed() < BROKER_CANCEL_POLL_INTERVAL {
                return cancel_seen;
            }
            last_cancel_poll = Instant::now();
            cancel_seen = cancelled_job_snapshot(
                &client,
                &options.broker_url,
                options.broker_token.as_deref(),
                &claim.job,
            )
            .map(|job| job.is_some())
            .unwrap_or(false);
            cancel_seen
        },
    );
    let (exec_output, exit_code) = match exec_result {
        Ok(result) => result,
        Err(err) => {
            if let Some(job) = cancelled_job_snapshot(
                &client,
                &options.broker_url,
                options.broker_token.as_deref(),
                &claim.job,
            )? {
                return Ok(cancelled_output(
                    options,
                    iterations,
                    jobs_claimed,
                    broker_failures,
                    stopped,
                    job,
                ));
            }
            let job = finish(RemoteRunnerJobResult {
                exit_code: 1,
                stdout: None,
                stderr: Some(err.to_string()),
                patch: None,
                mutation_artifacts: None,
                data: Some(json!({
                    "error": err.to_string(),
                })),
                observation_run_ids: Vec::new(),
                artifacts: Vec::new(),
                artifact_refs: Vec::new(),
                metrics: None,
                capture: None,
            })?;
            let exit_code = 1;
            return Ok((
                claimed_output(
                    options,
                    iterations,
                    jobs_claimed,
                    broker_failures,
                    stopped,
                    job,
                    exit_code,
                ),
                exit_code,
            ));
        }
    };
    if let Some(job) = cancelled_job_snapshot(
        &client,
        &options.broker_url,
        options.broker_token.as_deref(),
        &claim.job,
    )? {
        return Ok(cancelled_output(
            options,
            iterations,
            jobs_claimed,
            broker_failures,
            stopped,
            job,
        ));
    }
    let job = finish(remote_runner_result_from_exec_output(
        exec_output,
        exit_code,
        claim.request.runner_workload.clone(),
    ))?;

    Ok((
        claimed_output(
            options,
            iterations,
            jobs_claimed,
            broker_failures,
            stopped,
            job,
            exit_code,
        ),
        exit_code,
    ))
}
