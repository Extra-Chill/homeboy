//! `daemon serve`: one daemon generation's life over its listener.
//!
//! [`serve_locked`] runs the generation in order: claim the lease, recover the
//! store left by the previous generation, start the background helpers and
//! connection workers, accept connections until a stop, drain, record how the
//! daemon stopped, and hand pending work to a replacement when one is due.

use super::*;

/// Workers that serve accepted connections. Dispatch never blocks on them.
const CONNECTION_WORKERS: usize = 16;

/// How long a requested stop lets in-flight requests and still-running step
/// children finish before the process exits. Both are crash-safe: a request's
/// durable state is already written, and a live step child is re-adopted by
/// the successor. Waiting the whole drain deadline for them made every
/// controller daemon restart with a long-running Cook step or a held
/// long-poll request cost the full ten seconds (`homeboy upgrade` tail).
const REQUESTED_STOP_GRACE: Duration = Duration::from_secs(1);

/// Poll cadence for the bounded shutdown waits below.
const SHUTDOWN_POLL: Duration = Duration::from_millis(20);

pub(super) fn serve_locked<R>(
    listener: TcpListener,
    analysis_runner: R,
    owner_lock: DaemonOwnerLock,
    request_limit: Option<usize>,
    shutdown: Option<Arc<AtomicBool>>,
    service_manager: Option<DaemonServiceManager>,
) -> Result<DaemonState>
where
    R: AnalysisJobRunner,
{
    // Unit fixtures share a process with unrelated, intentionally panicking tests.
    let production = request_limit.is_none() && !cfg!(test);
    let _fatal_panics = production.then(FatalServePanics::enter);
    if let Some(flag) = shutdown.as_ref() {
        let _ = SERVING_SHUTDOWN.set(Arc::clone(flag));
        listener.set_nonblocking(true).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("configure daemon listener".to_string()),
            )
        })?;
    }
    let shutdown_set = || {
        shutdown
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    };
    let local_addr = listener.local_addr().map_err(|e| {
        Error::internal_io(e.to_string(), Some("read daemon local address".to_string()))
    })?;
    // A CLI-launched serve enforces its own lifetime; it needs the shutdown flag
    // to end itself through the same drain SIGTERM uses.
    let owned_lifetime = match shutdown.as_ref() {
        Some(_) => Some(lifetime::owned_idle_lifetime(service_manager)?),
        None => None,
    };
    let state = write_state(local_addr, service_manager)?;
    // The ledger is shared by every daemon of this Unix user; the scope keeps
    // this daemon's startup sweeps and counts to its own jobs (#16104).
    let scope = daemon_scope()?;
    let job_store =
        JobStore::open_without_reconciliation(paths::observation_db()?).map(|store| {
            store
                .with_daemon_lease(state.lease_id.clone())
                .with_daemon_scope(scope.clone())
        })?;
    recover_at_startup(&job_store, &state.lease_id)?;
    // Direct runner jobs count toward their owner's share and queue ETAs.
    let holders_store = job_store.clone();
    crate::lab_capacity::register_runner_job_holders(std::sync::Arc::new(move || {
        holders_store.lab_runner_job_holders()
    }));
    let _ = daemon_runtime_snapshot();
    refresh_daemon_freshness_snapshot(&job_store);

    // Dropped when this function returns, after the lease is released: an idle
    // self-stop keeps lifecycle commands and admissions out until then.
    let self_stop_guards = lifetime::SelfStopGuards::default();
    let mut helpers = Helpers::default();
    helpers.spawn("health-snapshot-updater", |stop| {
        spawn_health_updater(job_store.clone(), stop)
    });
    helpers.spawn("local-child-reservation-reconciler", |stop| {
        spawn_local_child_reservation_reconciler(job_store.clone(), stop)
    });
    helpers.spawn("completion-notifier", spawn_completion_notifier);
    helpers.spawn("orchestration-reconciler", |stop| {
        spawn_orchestration_reconciler(job_store.clone(), stop)
    });
    helpers.spawn("work-intent-deliverer", |stop| {
        spawn_work_intent_deliverer(job_store.clone(), stop)
    });
    if let (Some(idle), Some(flag)) = (owned_lifetime, shutdown.as_ref()) {
        let observation_db = paths::observation_db()?;
        helpers.spawn("daemon-lifetime", |stop| {
            lifetime::spawn_lifetime_monitor(
                idle,
                PathBuf::from(&state.state_path),
                state.lease_id.clone(),
                observation_db,
                Arc::clone(flag),
                Arc::clone(&self_stop_guards),
                stop,
            )
        });
    }
    let (connection_tx, connection_threads) =
        spawn_connection_workers(&job_store, analysis_runner, local_addr.ip().is_loopback());
    if production {
        helpers.spawn("self-health-watchdog", |stop| {
            spawn_watchdog(local_addr, stop)
        });
    }

    let mut serve_result = accept_until_stop(
        &listener,
        &connection_tx,
        &job_store,
        request_limit,
        shutdown_set,
    );

    // Test-support fault boundary: wedge the drain after a requested stop, as a
    // blocked serving thread does, so the shutdown deadline is observable.
    #[cfg(feature = "test-support")]
    if shutdown_set() {
        let _ = crate::test_support::wait_at_file_gate("HOMEBOY_TEST_DAEMON_SHUTDOWN_DRAIN_GATE");
    }
    let replacing = intake_draining();
    drop(connection_tx);
    let shutdown_requested = replacing || shutdown_set();
    // One budget covers the connection workers and the helpers, so a stop
    // never waits on back-to-back drains.
    let drain_deadline = Instant::now() + HELPER_DRAIN_DEADLINE;
    let grace_deadline = || (Instant::now() + REQUESTED_STOP_GRACE).min(drain_deadline);
    // Another controller sharing this host daemon may restart it while a job
    // it admitted is between child reservation and spawn. Let that spawn land
    // so the successor re-adopts a running child instead of expiring the
    // reservation and failing the job (#18063).
    if shutdown_requested {
        while job_store.has_pending_local_child_spawn() && Instant::now() < drain_deadline {
            std::thread::sleep(SHUTDOWN_POLL);
        }
    }
    let connection_results = if shutdown_requested {
        // A requested stop must not wait on a request still in flight; it ends
        // with the process and durable state is already crash-safe (#15483).
        // Responses already being written get a short grace, not the whole
        // drain budget.
        join_with_deadline(
            connection_threads
                .into_iter()
                .map(|handle| ("connection-worker", handle))
                .collect(),
            grace_deadline(),
        )
    } else {
        // Bounded TCP fixtures need all responses drained before their server
        // thread returns. Production listeners remain alive and simply retain
        // the connection workers until the process is stopped.
        connection_threads
            .into_iter()
            .map(|handle| handle.join())
            .collect()
    };
    for result in connection_results {
        if let Err(error) = result
            .unwrap_or_else(|_| Err(std::io::Error::other("daemon connection thread panicked")))
        {
            if serve_result.is_ok() {
                serve_result = Err(Error::internal_io(
                    error.to_string(),
                    Some("serve daemon connection".to_string()),
                ));
            }
        }
    }
    // A raw step's exit record is written by its supervising daemon thread.
    // Give already-running steps the remainder of the bounded drain budget to
    // publish that record before the process exits. Otherwise a short child
    // can finish during restart with no surviving observer of its exit status.
    // Do not wait on workflow drivers (which can wait indefinitely for input),
    // and do not extend the shared shutdown deadline. A step whose child is
    // still alive is re-adopted by the successor, so it only gets the short
    // grace; a step whose child already exited is publishing its exit record
    // and keeps the remaining drain budget.
    if shutdown_requested {
        let grace_deadline = grace_deadline();
        while let Some(wait) = step_exit_publication_wait(
            job_store
                .active_controller_jobs()
                .iter()
                .filter(|(_, state)| state.job_type == "step")
                .filter_map(|(_, state)| state.checkpoint.as_ref()),
            Instant::now(),
            grace_deadline,
            drain_deadline,
            pid_is_running,
        ) {
            std::thread::sleep(wait);
        }
    }
    helpers.stop(drain_deadline);

    // Termination evidence and the lease live in the state directory. Once it
    // is removed there is no reader left, and writing would recreate it.
    if shutdown_set() && !lifetime::state_dir_removed(Path::new(&state.state_path)) {
        record_requested_stop(&state, &scope, &self_stop_guards)?;
    }
    if replacing && service_manager.is_none() && replacement_is_waited_for(&job_store)? {
        // Release custody before launching ensure-running; it must not observe
        // this daemon's owner lock or listening socket as a competing serve.
        drop(listener);
        drop(self_stop_guards);
        drop(owner_lock);
        lifetime::handoff_pending_admissions(replacing, service_manager, true, launch_replacement)?;
    }
    serve_result.map(|()| state)
}

/// Whether a requested stop should keep waiting for running step jobs to
/// publish their exit, returning the next poll interval when it should.
///
/// A running step whose supervised child has already exited is mid-publication
/// and is waited for until `drain_deadline`. A running step whose child is
/// alive is waited for only until `grace_deadline`: the successor re-adopts it.
pub(super) fn step_exit_publication_wait<'a>(
    checkpoints: impl IntoIterator<Item = &'a serde_json::Value>,
    now: Instant,
    grace_deadline: Instant,
    drain_deadline: Instant,
    child_alive: impl Fn(u32) -> bool,
) -> Option<Duration> {
    if now >= drain_deadline {
        return None;
    }
    let mut live_child = false;
    for checkpoint in checkpoints {
        if checkpoint["phase"] != "running" {
            continue;
        }
        match checkpoint
            .pointer("/child/pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
        {
            Some(pid) if child_alive(pid) => live_child = true,
            // Exited (or unidentified) child: its exit record is pending.
            _ => return Some(SHUTDOWN_POLL),
        }
    }
    (live_child && now < grace_deadline).then_some(SHUTDOWN_POLL)
}

/// Background threads that live as long as `serve`, each stopped through its
/// own channel and joined against the shared drain deadline.
#[derive(Default)]
struct Helpers {
    stops: Vec<mpsc::Sender<()>>,
    threads: Vec<(&'static str, std::thread::JoinHandle<()>)>,
}

impl Helpers {
    fn spawn(
        &mut self,
        name: &'static str,
        start: impl FnOnce(mpsc::Receiver<()>) -> std::thread::JoinHandle<()>,
    ) {
        let (stop, stopped) = mpsc::channel();
        self.stops.push(stop);
        self.threads.push((name, start(stopped)));
    }

    fn stop(self, deadline: Instant) {
        for stop in &self.stops {
            let _ = stop.send(());
        }
        join_with_deadline(self.threads, deadline);
    }
}

/// Recover what the previous generation left in the store before serving.
fn recover_at_startup(job_store: &JobStore, lease_id: &str) -> Result<()> {
    // A restart cannot resume the thread that owned a pre-spawn reservation.
    // Expire only reservations that never recorded a child identity.
    job_store.reconcile_expired_local_child_reservations()?;
    job_store.reconcile_expired_admissions()?;
    recover_dead_generations(job_store, lease_id)?;
    reconcile_pending_workspace_owner_releases();
    reconcile_terminal_workspace_owner_leases(job_store);
    if let Some(stopped_lease) = read_termination_evidence()
        .ok()
        .flatten()
        .filter(|evidence| evidence.stop_requested)
        .and_then(|evidence| evidence.lease_id)
    {
        let _ = job_store.rearm_recovery_after_requested_stop(&stopped_lease);
    }
    job_store.adopt_scopeless_controller_jobs()?;
    recover_controller_jobs(job_store);
    // Built-in cadences (`homeboy.retention`) are registered unless the
    // operator opted out; a failure here must not keep the daemon down.
    if let Err(error) = workflow_cadence::ensure_builtin_cadences(workflow_cadence::now_ms()) {
        eprintln!(
            "built-in workflow cadences were not registered: {}",
            error.message
        );
    }
    Ok(())
}

/// Keep the `/health` freshness snapshot current off the request path: soon
/// after a revision bump, and at least every five seconds.
fn spawn_health_updater(
    job_store: JobStore,
    stop: mpsc::Receiver<()>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut revision = job_store.health_revision.load(Ordering::Acquire);
        let mut refreshed = Instant::now();
        while stop.recv_timeout(Duration::from_millis(100)) == Err(mpsc::RecvTimeoutError::Timeout)
        {
            let current = job_store.health_revision.load(Ordering::Acquire);
            if (current != revision && refreshed.elapsed() >= Duration::from_secs(1))
                || refreshed.elapsed() >= Duration::from_secs(5)
            {
                refresh_daemon_freshness_snapshot(&job_store);
                revision = current;
                refreshed = Instant::now();
            }
        }
    })
}

/// Exit for restart when this daemon stops answering its own `/health`.
fn spawn_watchdog(
    mut address: SocketAddr,
    stop: mpsc::Receiver<()>,
) -> std::thread::JoinHandle<()> {
    if address.ip().is_unspecified() {
        address.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    std::thread::spawn(move || {
        let mut failures = 0;
        while stop.recv_timeout(SELF_HEALTH_PROBE_INTERVAL) == Err(mpsc::RecvTimeoutError::Timeout)
        {
            failures = health_failures(failures, probe_self_health(address).unwrap_or(false));
            if failures >= SELF_HEALTH_FAILURE_LIMIT {
                eprintln!("fatal daemon self-health watchdog: /health unanswered on {address} for {failures} consecutive probes; exiting for restart");
                std::process::exit(1);
            }
        }
    })
}

type ConnectionWorker = std::thread::JoinHandle<std::io::Result<()>>;

/// Start the bounded connection workers. Only they hold the queue's receiver,
/// so their death disconnects dispatch.
fn spawn_connection_workers<R>(
    job_store: &JobStore,
    analysis_runner: R,
    loopback_bind: bool,
) -> (mpsc::SyncSender<TcpStream>, Vec<ConnectionWorker>)
where
    R: AnalysisJobRunner,
{
    let (connection_tx, connection_rx) = mpsc::sync_channel::<TcpStream>(64);
    let connection_rx = Arc::new(Mutex::new(connection_rx));
    let workers = (0..CONNECTION_WORKERS)
        .map(|_| {
            let worker_rx = Arc::clone(&connection_rx);
            let worker_store = job_store.clone();
            let worker_runner = analysis_runner.clone();
            std::thread::spawn(move || loop {
                let stream = worker_rx.lock().expect("connection queue lock").recv();
                let Ok(stream) = stream else { return Ok(()) };
                // A malformed or disconnected client owns only this connection;
                // it must never retire the bounded worker that serves later peers.
                let _ =
                    handle_connection(stream, &worker_store, worker_runner.clone(), loopback_bind);
            })
        })
        .collect();
    (connection_tx, workers)
}

/// Accept connections until a requested stop, a finished drain, the request
/// limit, or an accept failure.
fn accept_until_stop(
    listener: &TcpListener,
    connection_tx: &mpsc::SyncSender<TcpStream>,
    job_store: &JobStore,
    request_limit: Option<usize>,
    shutdown_set: impl Fn() -> bool,
) -> Result<()> {
    let mut accepted = 0;
    loop {
        // A requested stop is checked before anything that reads the store, so
        // a slow freshness report never delays it (#17891).
        if shutdown_set() {
            return Ok(());
        }
        if intake_draining()
            && daemon_freshness_report(job_store).is_ok_and(|report| report.active_jobs == 0)
        {
            return Ok(());
        }
        let stream = match listener.accept() {
            // The listener polls non-blocking so shutdown is observed; served
            // connections stay blocking (BSD sockets inherit O_NONBLOCK).
            Ok((stream, _)) => stream.set_nonblocking(false).map(|()| stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                // Wake on the next connection; the timeout bounds how long a
                // shutdown request waits to be observed.
                let mut readable = libc::pollfd {
                    fd: std::os::fd::AsRawFd::as_raw_fd(listener),
                    events: libc::POLLIN,
                    revents: 0,
                };
                unsafe { libc::poll(&mut readable, 1, 100) };
                continue;
            }
            Err(error) => Err(error),
        };
        let stream = stream.map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("accept daemon connection".to_string()),
            )
        })?;
        if shutdown_set() {
            return Ok(());
        }
        // Never block on a full queue: when every worker is busy a blocking
        // send would wedge this loop, and with it every shutdown and drain
        // check (#17891).
        if !dispatch_connection(connection_tx, stream, &shutdown_set)? {
            return Ok(());
        }
        accepted += 1;
        if request_limit.is_some_and(|limit| accepted >= limit) {
            return Ok(());
        }
    }
}

/// Record a requested stop. It is clean even with work in flight: controller
/// work on detached threads ends with this process and resumes from its
/// checkpoint on the next start, which re-arms its resume (#15936). With jobs
/// still active the lease is kept, so dead-lease recovery also re-admits
/// staged direct work.
fn record_requested_stop(
    state: &DaemonState,
    scope: &crate::api_jobs::DaemonScope,
    self_stop_guards: &lifetime::SelfStopGuards,
) -> Result<()> {
    let active_jobs = JobStore::active_count_at_path(paths::observation_db()?, Some(scope))?;
    write_termination_evidence(&DaemonTerminationEvidence {
        classification: DaemonTerminationClassification::CleanStop,
        observed_at: chrono::Utc::now().to_rfc3339(),
        lease_id: Some(state.lease_id.clone()),
        pid: Some(state.pid),
        binary_identity: Some(state.build_identity.display.clone()),
        active_jobs,
        resource_evidence:
            "unavailable: foreground shutdown does not collect OS resource snapshots".to_string(),
        os_evidence: self_stop_guards
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map_or_else(
                || "graceful shutdown requested by SIGINT or SIGTERM".to_string(),
                |stop| stop.reason.os_evidence(),
            ),
        stop_requested: true,
    })?;
    if active_jobs == 0 {
        remove_lease_if_identity_matches(
            &paths::daemon_state_file()?,
            &DaemonLeaseIdentity::from_state(state),
        )?;
    }
    Ok(())
}

/// Work that only a replacement daemon can take: pending admissions, active
/// workflows, or a Cook admission not yet materialized.
fn replacement_is_waited_for(job_store: &JobStore) -> Result<bool> {
    Ok(job_store.has_pending_admissions()
        || job_store
            .active_controller_jobs()
            .iter()
            .any(|(_, state)| state.job_type == workflow_job::WORKFLOW_JOB_TYPE)
        || crate::observation::ObservationStore::open_readonly_at(paths::observation_db()?)
            .and_then(|store| store.has_pending_unmaterialized_cook_admission())
            .unwrap_or(false))
}

fn launch_replacement() -> Result<()> {
    let executable = lifetime::launchable_executable().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("locate replacement daemon".to_string()),
        )
    })?;
    Command::new(executable)
        .args(["daemon", "ensure-running"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("start replacement daemon".to_string()),
            )
        })?;
    Ok(())
}

#[cfg(test)]
mod shutdown_wait_tests {
    use super::*;

    #[test]
    fn requested_stop_waits_only_for_steps_publishing_an_exit() {
        let now = Instant::now();
        let grace = now + REQUESTED_STOP_GRACE;
        let drain = now + HELPER_DRAIN_DEADLINE;
        let live = json!({"phase": "running", "child": {"pid": 11}});
        let exited = json!({"phase": "running", "child": {"pid": 22}});
        let waiting = json!({"phase": "waiting"});
        let alive = |pid: u32| pid == 11;

        // No running step: return at once.
        assert_eq!(
            step_exit_publication_wait([&waiting], now, grace, drain, alive),
            None
        );
        // A live child gets only the grace, then the successor adopts it.
        assert!(step_exit_publication_wait([&live], now, grace, drain, alive).is_some());
        assert_eq!(
            step_exit_publication_wait([&live], grace, grace, drain, alive),
            None
        );
        // An exited child keeps the remaining drain budget, but never more.
        assert!(step_exit_publication_wait([&live, &exited], grace, grace, drain, alive).is_some());
        assert_eq!(
            step_exit_publication_wait([&exited], drain, grace, drain, alive),
            None
        );
    }
}
