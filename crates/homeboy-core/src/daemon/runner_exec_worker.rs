//! Runs one admitted runner execution on the daemon's capacity-queued worker:
//! stamp the child's identity, spawn it under a heartbeat-only stall watchdog,
//! and turn its output into the job's durable result.

use super::*;
use crate::runner_execution_envelope::PathMaterializationPlan;
use crate::source_snapshot::SourceSnapshot;

const HEARTBEAT_ONLY_STALL_POLL: Duration = Duration::from_secs(1);

/// Separates transport liveness from child work. A wrapper heartbeat is useful
/// evidence that the supervisor is alive, but it cannot extend a child's lease.
#[derive(Debug)]
struct ExecLiveness {
    started: Instant,
    last_semantic_progress: Instant,
    last_cpu_ms: Option<u64>,
    heartbeat_count: u64,
    last_heartbeat: Option<serde_json::Value>,
    child: Option<serde_json::Value>,
    stalled: bool,
}

impl ExecLiveness {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            last_semantic_progress: now,
            last_cpu_ms: None,
            heartbeat_count: 0,
            last_heartbeat: None,
            child: None,
            stalled: false,
        }
    }

    fn record_child(&mut self, pid: u32, process_group_id: Option<u32>, identity: String) {
        self.child = Some(json!({
            "pid": pid,
            "process_group_id": process_group_id,
            "identity": identity,
        }));
    }

    fn observe(&mut self, data: &serde_json::Value, now: Instant) -> bool {
        if self.stalled {
            return false;
        }
        let phase = data.get("phase").and_then(serde_json::Value::as_str);
        // Child protocols may legitimately use a heartbeat phase. The resource
        // wrapper is identified by the process snapshot it emits.
        let wrapper_heartbeat =
            phase == Some("heartbeat") && data.pointer("/process/root_pid").is_some();
        let cpu_ms = data
            .pointer("/process/resources/cpu_user_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
            .saturating_add(
                data.pointer("/process/resources/cpu_system_ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
            );
        let cpu_advanced = self.last_cpu_ms.is_some_and(|previous| cpu_ms > previous);
        if !wrapper_heartbeat || cpu_advanced {
            self.last_semantic_progress = now;
        }
        if wrapper_heartbeat {
            self.last_cpu_ms = Some(cpu_ms);
            self.heartbeat_count = self.heartbeat_count.saturating_add(1);
            self.last_heartbeat = Some(data.clone());
        }
        true
    }

    fn evidence(&self, timeout: Duration) -> serde_json::Value {
        json!({
            "classification": "heartbeat_only_stall",
            "timeout_ms": timeout.as_millis(),
            "started_elapsed_ms": self.started.elapsed().as_millis(),
            "last_semantic_progress_age_ms": self.last_semantic_progress.elapsed().as_millis(),
            "last_cpu_ms": self.last_cpu_ms,
            "heartbeat_count": self.heartbeat_count,
            "last_heartbeat": self.last_heartbeat,
            "child": self.child,
        })
    }

    fn select_stall(&mut self, now: Instant, timeout: Duration) -> bool {
        if self.stalled || now.duration_since(self.last_semantic_progress) < timeout {
            return false;
        }
        self.stalled = true;
        true
    }
}

fn heartbeat_only_stall_reason(timeout: Duration) -> String {
    format!(
        "heartbeat_only_stall: no child output, semantic progress, or advancing CPU for {}s; cancellation requested so the daemon can converge",
        timeout.as_secs()
    )
}

/// Everything the worker needs, resolved at admission. The workspace owner
/// lease travels with the claim store it was registered through.
pub(super) struct RunnerExecWork {
    pub(super) plan: runner_exec_driver::PreparedDaemonExec,
    pub(super) controller_run_id: Option<String>,
    pub(super) controller_attempt_id: Option<String>,
    pub(super) accepted_handoff_id: Option<String>,
    pub(super) workspace_owner: Option<PendingOwnerRegistration>,
    pub(super) job_store: JobStore,
    pub(super) baseline: Option<patch_capture::BaselineCapture>,
    pub(super) source_snapshot: Option<SourceSnapshot>,
    pub(super) path_materialization_plan: Option<PathMaterializationPlan>,
}

impl RunnerExecWork {
    pub(super) fn run(self, job: crate::api_jobs::JobHandle) -> Result<serde_json::Value> {
        let Self {
            mut plan,
            controller_run_id,
            controller_attempt_id,
            accepted_handoff_id,
            workspace_owner,
            job_store,
            baseline,
            source_snapshot,
            path_materialization_plan,
        } = self;
        if let Some(limit) = plan.concurrency_limit {
            plan.env.insert(
                "HOMEBOY_LAB_CONCURRENCY_LIMIT".to_string(),
                limit.to_string(),
            );
        }
        if let Some((claim_store, lease)) = &workspace_owner {
            if !claim_store.validate_owner(lease, workspace_claim_now_ms())? {
                return Err(Error::validation_invalid_argument(
                    "workspace_owner_lease",
                    "direct workspace owner lease is no longer live before process spawn",
                    Some(job.job_id().to_string()),
                    None,
                ));
            }
        }
        job.progress(json!({ "phase": "local_child_reservation_lookup_started" }))?;
        let reservation_id = match job.local_child_reservation_id() {
            Ok(reservation_id) => reservation_id,
            Err(error) => {
                let _ = job.progress(json!({
                    "phase": "local_child_reservation_lookup_failed",
                    "error": error.to_string(),
                }));
                return Err(error);
            }
        };
        let execution_context = crate::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon_with_dispatch_metadata(
            controller_run_id.as_deref(),
            controller_attempt_id.as_deref(),
            accepted_handoff_id.as_deref(),
            &plan.runner_id,
            &job.job_id().to_string(),
            plan.command.first().map(String::as_str).unwrap_or_default(),
            &reservation_id,
        )?;
        let execution_context_evidence = execution_context.evidence_record()?;
        // Durable progress makes the same receipt resolvable after a
        // controller restart without consulting process environment.
        job.progress(json!({
            "phase": "runner_job_execution_context_verified",
            "execution_context": execution_context_evidence,
        }))?;
        plan.env.insert(
            crate::runner_job_execution_context::RUNNER_CHILD_RESERVATION_ENV.to_string(),
            reservation_id,
        );
        // Runner-local supervisors need the durable job identity to
        // attach their child-service ledger to this daemon owner.
        plan.env.insert(
            crate::runner_job_execution_context::RUNNER_JOB_ID_ENV.to_string(),
            job.job_id().to_string(),
        );
        plan.env.insert(
            crate::runner_job_execution_context::RUNNER_JOB_EXECUTION_CONTEXT_ID_ENV.to_string(),
            execution_context.id().to_string(),
        );
        // The child authenticates against this daemon's store, whose
        // format is build-specific. Stamp the build so a skewed child
        // refuses with a named cause instead of "job not found".
        plan.env.insert(
            crate::runner_job_execution_context::DAEMON_CHILD_BUILD_IDENTITY_ENV.to_string(),
            crate::build_identity::current().display,
        );
        let liveness = Arc::new(Mutex::new(ExecLiveness::new(Instant::now())));
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        // The heartbeat inherits the registration's authority rather
        // than resolving its own on the renewal thread.
        let _workspace_owner_renewer = workspace_owner.map(|(claim_store, lease)| {
            DirectWorkspaceOwnerRenewer::start(
                job.job_id(),
                lease,
                job_store.clone(),
                claim_store,
                Arc::clone(&cancellation_requested),
            )
        });
        let stall_evidence = Arc::new(Mutex::new(None));
        let progress_job = job.clone();
        let progress_liveness = Arc::clone(&liveness);
        let progress_sink = Arc::new(move |data| {
            // Select a stall before accepting progress so a late wrapper
            // heartbeat cannot race a selected cancellation back to life.
            if !progress_liveness
                .lock()
                .expect("runner exec liveness lock")
                .observe(&data, Instant::now())
            {
                return Err(Error::internal_unexpected(
                    "runner exec progress arrived after heartbeat-only stall cancellation",
                ));
            }
            progress_job.progress(data).map(|_| ())
        });
        let started_job = job.clone();
        let started_liveness = Arc::clone(&liveness);
        let child_started = Arc::new(move |identity: runner_exec_driver::RunnerChildIdentity| {
            // The execution owner captured this identity before the
            // root could be reaped. Re-probing `/proc` here raced the
            // Linux supervisor reaping a short-lived command.
            let pid = identity.pid;
            let process_group_id = identity.process_group_id;
            let discriminator = match identity.starttime_ticks {
                Some(ticks) => {
                    crate::api_jobs::LocalChildStartDiscriminator::LinuxProcStatStarttimeTicks {
                        ticks,
                    }
                }
                None if cfg!(target_os = "linux") => {
                    return Err(Error::internal_unexpected(
                        "runner child Linux start identity was not captured at spawn",
                    ))
                }
                None => crate::api_jobs::LocalChildStartDiscriminator::Unsupported {
                    evidence: "Linux /proc starttime identity is unsupported on this platform"
                        .to_string(),
                },
            };
            started_liveness
                .lock()
                .expect("runner exec liveness lock")
                .record_child(pid, process_group_id, format!("{discriminator:?}"));
            started_job.start_with_reserved_child_identity(pid, process_group_id, discriminator)?;
            Ok(())
        });
        let cancel_job = job.clone();
        let watchdog_liveness = Arc::clone(&liveness);
        let watchdog_cancellation = Arc::clone(&cancellation_requested);
        let watchdog_evidence = Arc::clone(&stall_evidence);
        let watchdog_job = job.clone();
        let stall_timeout = plan.heartbeat_only_stall.timeout();
        let (watchdog_stop, watchdog_stopped) = mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            while watchdog_stopped
                .recv_timeout(HEARTBEAT_ONLY_STALL_POLL)
                .is_err()
            {
                let Some(timeout) = stall_timeout else {
                    return;
                };
                let evidence = {
                    let mut liveness = watchdog_liveness.lock().expect("runner exec liveness lock");
                    liveness
                        .select_stall(Instant::now(), timeout)
                        .then(|| liveness.evidence(timeout))
                };
                if let Some(evidence) = evidence {
                    // Latch before requesting cancellation. The worker
                    // remains active until its process tree confirms reap.
                    watchdog_cancellation.store(true, Ordering::SeqCst);
                    *watchdog_evidence
                        .lock()
                        .expect("runner exec stall evidence lock") = Some(evidence.clone());
                    let _ = watchdog_job.progress(json!({
                        "phase": "heartbeat_only_stall_cancellation_requested",
                        "evidence": evidence,
                    }));
                    return;
                }
            }
        });
        // Held until the outcome is captured; a restarted daemon re-adopts
        // a child whose custody was released by its daemon's death.
        let custody = runner_exec_job::RunnerExecCustody::acquire(job.job_id())?;
        plan.output_dir = Some(custody.dir().to_path_buf());
        job.progress(json!({ "phase": "local_child_driver_execution_started" }))?;
        job.progress(json!({ "phase": "local_child_command_spawn_attempted" }))?;
        let driver_cancellation = Arc::clone(&cancellation_requested);
        // Timed in memory: the submitter already owns this execution's
        // one run, and the controller links the published spans to it.
        let mut timer = homeboy_engine_primitives::phase_timing::PhaseTimer::with_version(env!(
            "CARGO_PKG_VERSION"
        ));
        let execute_started = std::time::Instant::now();
        let process_output = runner_exec_driver::execute_exec(
            &plan,
            &execution_context,
            Box::new(move || {
                driver_cancellation.load(Ordering::SeqCst) || cancel_job.is_cancelled()
            }),
            Some(progress_sink),
            true,
            Some(child_started),
        );
        if process_output
            .as_ref()
            .is_ok_and(|output| output.exit_code == 0)
        {
            timer.record_ok(
                "stage.lab.observe.runner.execute",
                execute_started.elapsed(),
            );
        } else {
            timer.record_failed(
                "stage.lab.observe.runner.execute",
                execute_started.elapsed(),
            );
        }
        if let Err(error) = job.progress(json!({ "phase_spans": timer.into_report() })) {
            crate::log_status!("observe", "runner timing publication failed: {}", error);
        }
        let _ = watchdog_stop.send(());
        let _ = watchdog.join();
        let process_output = match process_output {
            Ok(output) => output,
            Err(mut error) if cancellation_requested.load(Ordering::SeqCst) => {
                let evidence = stall_evidence
                    .lock()
                    .expect("runner exec stall evidence lock")
                    .clone();
                let _ = job.progress(json!({
                    "phase": "heartbeat_only_stall_reap_unconfirmed",
                    "evidence": evidence,
                    "reap_error": error.to_string(),
                }));
                error.details["retain_active"] = serde_json::Value::Bool(true);
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let stdout = process_output.stdout.clone();
        let stderr = process_output.stderr.clone();
        let exit_code = process_output.exit_code;
        let metrics = process_output.metrics.clone();
        let capture = process_output.capture.clone();
        let extension_env_provenance = process_output.extension_env_provenance.clone();
        let diagnostic_hints = process_output.diagnostic_hints.clone();
        if cancellation_requested.load(Ordering::SeqCst) {
            let evidence = stall_evidence
                .lock()
                .expect("runner exec stall evidence lock")
                .clone();
            job.progress(json!({
                "phase": "heartbeat_only_stall_reap_confirmed",
                "evidence": evidence,
                "reap": { "confirmed": true, "exit_code": exit_code },
            }))?;
            job_store.cancel(
                job.job_id(),
                heartbeat_only_stall_reason(stall_timeout.expect("stall selected")),
            )?;
        }
        let mut result = json!({
            "runner_id": plan.runner_id,
            "cwd": plan.cwd,
            "command": plan.command,
            "exit_code": exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "source_snapshot": source_snapshot,
            "path_materialization_plan": path_materialization_plan,
            "metrics": metrics,
            "capture": capture,
        });
        if job.is_cancelled() {
            let _ = job.progress(json!({
                "phase": "cancelled",
                "exit_code": exit_code,
                "metrics": metrics,
            }));
            result["status"] = json!(JobStatus::Cancelled);
            return Ok(result);
        }
        if !stdout.is_empty() {
            job.stdout(stdout.clone())?;
        }
        if !stderr.is_empty() {
            job.stderr(stderr.clone())?;
        }
        job.progress(json!({
            "phase": "finished",
            "exit_code": exit_code,
            "metrics": metrics,
        }))?;
        let patch = baseline
            .map(|baseline| {
                capture_patch_report(
                    job.job_id(),
                    &plan.runner_id,
                    &plan.cwd,
                    &plan.command,
                    source_snapshot.as_ref(),
                    &baseline,
                    exit_code,
                )
            })
            .transpose()?;
        result["patch"] = json!(patch);
        result["extension_env_providers"] = json!(extension_env_provenance);
        if !diagnostic_hints.is_empty() {
            result["diagnostic_hints"] = json!(diagnostic_hints);
        }
        if exit_code != 0 {
            job.result(result.clone())?;
            let message = metrics
                .as_ref()
                .and_then(homeboy_runner_contract::find_resource_guard_stop)
                .map(|stop| stop.violation.operator_message())
                .unwrap_or_else(|| "Remote command failed".to_string());
            return Err(Error::remote_command_failed_with_details(
                message,
                RemoteCommandFailedDetails {
                    command: plan.command.join(" "),
                    exit_code,
                    stdout,
                    stderr,
                    target: TargetDetails {
                        project_id: None,
                        server_id: Some(plan.runner_id.clone()),
                        host: None,
                    },
                },
            ));
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_only_liveness_stalls_after_the_bounded_window() {
        let start = Instant::now();
        let mut liveness = ExecLiveness::new(start);
        let heartbeat = json!({
            "phase": "heartbeat",
            "process": { "root_pid": 1, "resources": { "cpu_user_ms": 10, "cpu_system_ms": 2 } }
        });

        assert!(liveness.observe(&heartbeat, start + Duration::from_secs(1)));
        assert!(liveness.observe(&heartbeat, start + Duration::from_secs(9)));
        assert!(liveness.select_stall(start + Duration::from_secs(10), Duration::from_secs(10)));
        assert!(!liveness.observe(&heartbeat, start + Duration::from_secs(11)));
        assert!(
            heartbeat_only_stall_reason(Duration::from_secs(10)).contains("heartbeat_only_stall")
        );
    }

    #[test]
    fn heartbeat_only_stall_evidence_records_child_and_wrapper_observations() {
        let start = Instant::now();
        let mut liveness = ExecLiveness::new(start);
        liveness.record_child(42, Some(42), "starttime:7".to_string());
        liveness.observe(
            &json!({
                "phase": "heartbeat",
                "process": { "root_pid": 42, "resources": { "cpu_user_ms": 7, "cpu_system_ms": 3 } }
            }),
            start + Duration::from_secs(1),
        );

        let evidence = liveness.evidence(Duration::from_secs(10));
        assert_eq!(evidence["classification"], "heartbeat_only_stall");
        assert_eq!(evidence["child"]["pid"], 42);
        assert_eq!(evidence["heartbeat_count"], 1);
        assert_eq!(evidence["last_cpu_ms"], 10);
    }

    #[test]
    fn heartbeat_only_stall_policy_has_safe_default_and_explicit_disable() {
        let default = crate::server::HeartbeatOnlyStallPolicy::default();
        assert_eq!(default.timeout(), Some(Duration::from_secs(15 * 60)));
        assert_eq!(
            crate::server::HeartbeatOnlyStallPolicy { timeout_seconds: 0 }.timeout(),
            None
        );
        assert_eq!(
            crate::server::HeartbeatOnlyStallPolicy { timeout_seconds: 7 }.timeout(),
            Some(Duration::from_secs(7))
        );
    }

    #[test]
    fn semantic_output_and_advancing_cpu_protect_long_running_work() {
        let start = Instant::now();
        let mut liveness = ExecLiveness::new(start);
        let heartbeat = |cpu| {
            json!({
                "phase": "heartbeat",
                "process": { "root_pid": 1, "resources": { "cpu_user_ms": cpu, "cpu_system_ms": 0 } }
            })
        };

        assert!(liveness.observe(&heartbeat(10), start + Duration::from_secs(9)));
        assert!(liveness.observe(&heartbeat(11), start + Duration::from_secs(18)));
        assert!(!liveness.select_stall(start + Duration::from_secs(20), Duration::from_secs(10)));
        assert!(liveness.observe(
            &json!({ "phase": "child_output", "message": "working" }),
            start + Duration::from_secs(29)
        ));
        assert!(!liveness.select_stall(start + Duration::from_secs(30), Duration::from_secs(10)));
    }

    #[test]
    fn selected_stall_wins_the_progress_cancellation_race() {
        let start = Instant::now();
        let mut liveness = ExecLiveness::new(start);
        assert!(liveness.select_stall(start + Duration::from_secs(10), Duration::from_secs(10)));
        assert!(liveness.stalled);
        assert!(!liveness.observe(
            &json!({ "phase": "child_output" }),
            start + Duration::from_secs(10)
        ));
        assert!(!liveness.select_stall(start + Duration::from_secs(11), Duration::from_secs(10)));
    }

    #[test]
    fn stalled_cancellation_converges_active_job_accounting() {
        let store = JobStore::default();
        let job = store.create("runner.exec");
        store.start(job.id).expect("start job");
        let cancelled = store
            .cancel(job.id, heartbeat_only_stall_reason(Duration::from_secs(10)))
            .expect("cancel stalled job");

        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert!(cancelled
            .stale_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("heartbeat_only_stall")));
        assert!(
            store.active_runner_jobs().is_empty(),
            "cancelled jobs cannot pin the daemon"
        );
    }
}
