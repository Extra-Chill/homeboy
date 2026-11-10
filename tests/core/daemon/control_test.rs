use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::process::Command;
use std::sync::{mpsc, Arc, Barrier, Mutex};
use std::time::Duration;

use super::{
    artifact_content_url, can_recover_startup_attempt, ensure_running_with_operations,
    fetch_artifact_to_path, observe_startup_lease,
    terminate_token_owned_startup_process_with_operations,
};
use crate::api_jobs::{JobStatus, JobStore};
use crate::build_identity::BuildIdentity;
use crate::daemon::{
    DaemonFreshnessReport, DaemonRuntimeSnapshot, DaemonStaleReasonCode, DaemonState, DaemonStatus,
};
use crate::process::{SIGNAL_KILL, SIGNAL_TERMINATE};
use crate::test_support::with_isolated_home;

#[cfg(unix)]
#[test]
fn detached_daemon_owner_does_not_inherit_the_launcher_session() {
    // HomeGuard holders set the keep-in-process-group test variable process
    // wide, which makes `detach_from_launcher_session` skip the very detach
    // this test proves. Hold the home lock and clear it for this test (#15470).
    let _home = crate::test_support::HomeGuard::new();
    let _keep = crate::test_support::EnvVarGuard::unset(
        crate::test_support::TEST_KEEP_DAEMON_IN_PROCESS_GROUP_ENV,
    );
    let mut command = Command::new("sh");
    command.args(["-c", "printf '%s\\n' \"$$\"; sleep 30"]);
    super::detach_from_launcher_session(&mut command);
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("child");
    let mut stdout = String::new();
    BufReader::new(child.stdout.take().expect("child stdout"))
        .read_line(&mut stdout)
        .expect("read child pid");
    let pid = stdout.trim().parse::<libc::pid_t>().expect("child pid");

    // A session leader has a session ID equal to its PID, so the child can no
    // longer receive SSH-launcher session teardown signals.
    assert_eq!(unsafe { libc::getsid(pid) }, pid);
    assert_eq!(unsafe { libc::kill(-pid, libc::SIGTERM) }, 0);
    child.wait().expect("reap child");
}

#[derive(Default)]
struct FakeEnsureState {
    daemon: Option<super::DaemonStartResult>,
    starts: usize,
}

#[cfg(unix)]
#[test]
fn stale_start_cleanup_waits_for_cook_generation_admission() {
    with_isolated_home(|_| {
        let cook_preflight = super::super::acquire_daemon_admission_lock()
            .expect("Cook preflight acquires generation admission");
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        std::thread::spawn(move || {
            attempt_tx.send(()).expect("report startup cleanup attempt");
            super::stop_stale_daemon_for_start().expect("stale startup cleanup");
            finished_tx
                .send(())
                .expect("report startup cleanup completion");
        });

        attempt_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("startup cleanup attempts admission");
        assert!(
            finished_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "stale startup cleanup must wait for Cook admission"
        );

        drop(cook_preflight);
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stale startup cleanup proceeds after Cook admission");
    });
}

#[test]
fn artifact_content_url_builds_encoded_daemon_byte_alias() {
    let url = artifact_content_url(
        "http://127.0.0.1:7421/base?ignored=true",
        "run 1",
        "report/summary.json",
    )
    .expect("url");

    assert_eq!(
        url,
        "http://127.0.0.1:7421/runs/run%201/artifacts/report%2Fsummary.json/content"
    );
}

/// A store over the shared ledger stamped with `lease_id` and `scope`.
fn attest_store(lease_id: &str, scope: crate::api_jobs::DaemonScope) -> JobStore {
    JobStore::open_without_reconciliation(crate::paths::observation_db().expect("jobs path"))
        .expect("store")
        .with_daemon_lease(lease_id.to_string())
        .with_daemon_scope(scope)
}

fn running_job(store: &JobStore) -> uuid::Uuid {
    let job = store.create("runner.exec");
    store.start(job.id).expect("start job");
    job.id
}

fn ledger_status(job_id: uuid::Uuid) -> JobStatus {
    JobStore::open_without_reconciliation(crate::paths::observation_db().expect("jobs path"))
        .expect("store")
        .get(job_id)
        .expect("job")
        .status
}

/// #17429: `--attest-absent` names exact jobs. A job with a recorded child
/// (startup recovery proves its state) or of another daemon scope refuses the
/// whole set; a valid set is terminalized together, with the attestation
/// persisted on each job.
#[test]
fn attest_absent_refuses_any_unprovable_job_and_terminalizes_a_valid_set_atomically() {
    with_isolated_home(|_| {
        let scope = super::super::daemon_scope().expect("scope");
        let dead = attest_store("lease-dead", scope);
        let first = running_job(&dead);
        let second = running_job(&dead);
        let with_child = dead.create("runner.exec").id;
        dead.start_with_child_identity(with_child, 999_999, "2026-01-01T00:00:00Z".into())
            .expect("child identity");
        let foreign = running_job(&attest_store(
            "lease-dead",
            crate::api_jobs::DaemonScope::from_state_dir(std::path::Path::new("/other/scope")),
        ));

        for (refused, reason) in [
            (with_child, "recorded child identity"),
            (foreign, "another daemon scope"),
        ] {
            let error = super::attest_absent(&[first, refused]).expect_err("refused set");
            assert!(error.message.contains(reason), "{}", error.message);
            assert_eq!(
                ledger_status(first),
                JobStatus::Running,
                "nothing is mutated"
            );
            assert_eq!(ledger_status(refused), JobStatus::Running);
        }

        let attested = super::attest_absent(&[second, first]).expect("valid set");
        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(attested.attested_job_ids, expected);
        assert_eq!(attested.live_lease_id, None);
        assert_eq!(ledger_status(first), JobStatus::Failed);
        assert_eq!(ledger_status(second), JobStatus::Failed);
        let events = attest_store("lease-dead", super::super::daemon_scope().expect("scope"))
            .events(first)
            .expect("events");
        assert!(events.iter().any(|event| event
            .data
            .as_ref()
            .is_some_and(|data| data["operator_confirmed_workload_processes_absent"] == true)));
    });
}

/// #17429: with a live daemon, its own generation is never attestable; only
/// other leases in the scope are dead.
#[test]
fn attest_absent_refuses_the_live_generation_while_its_daemon_holds_the_lock() {
    with_isolated_home(|_| {
        let state_path = crate::paths::daemon_state_file().expect("state path");
        let owner = super::super::acquire_daemon_owner_lock_at(&state_path, Duration::ZERO)
            .expect("hold the owner lock as the live daemon");
        let mut daemon = fake_daemon(std::process::id(), "lease-live");
        daemon.state_path = state_path.display().to_string();
        std::fs::write(
            &state_path,
            serde_json::to_vec(&fake_daemon_state(daemon)).expect("state json"),
        )
        .expect("state");
        let scope = super::super::daemon_scope().expect("scope");
        let live = running_job(&attest_store("lease-live", scope.clone()));
        let dead = running_job(&attest_store("lease-dead", scope));

        let error = super::attest_absent(&[live]).expect_err("live generation");
        assert!(
            error.message.contains("live daemon generation"),
            "{}",
            error.message
        );
        assert_eq!(ledger_status(live), JobStatus::Running);

        let attested = super::attest_absent(&[dead]).expect("dead generation");
        assert_eq!(attested.attested_job_ids, vec![dead]);
        assert_eq!(attested.live_lease_id.as_deref(), Some("lease-live"));
        assert_eq!(ledger_status(dead), JobStatus::Failed);
        assert_eq!(ledger_status(live), JobStatus::Running);
        drop(owner);
    });
}

#[test]
fn fetch_artifact_to_path_downloads_daemon_byte_alias() {
    with_isolated_home(|home| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0; 1024];
            let bytes = stream.read(&mut request).expect("request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request
                .starts_with("GET /runs/run-1/artifacts/report%2Fsummary.json/content HTTP/1.1"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nX-Homeboy-Artifact-Sha256: abc123\r\nConnection: close\r\n\r\n{\"ok\":true}",
                )
                .expect("response");
        });
        let output = home.path().join("summary.json");

        let outcome = fetch_artifact_to_path(
            "run-1",
            "report/summary.json",
            Some(format!("http://{addr}")),
            Some(output.clone()),
        )
        .expect("artifact get");

        server.join().expect("server");
        assert_eq!(outcome.content_type.as_deref(), Some("application/json"));
        assert_eq!(outcome.size_bytes, 11);
        assert_eq!(outcome.sha256.as_deref(), Some("abc123"));
        assert_eq!(std::fs::read(&output).expect("output"), br#"{"ok":true}"#);
    });
}

#[test]
fn ensure_running_times_out_when_lifecycle_lock_remains_held() {
    with_isolated_home(|_| {
        let _lock = super::super::acquire_daemon_operation_lock().expect("hold lifecycle lock");

        let err = ensure_running_with_operations(
            Duration::from_millis(10),
            super::super::acquire_daemon_operation_lock_for_ensure,
            || unreachable!("lock acquisition should time out first"),
            |_| unreachable!("lock acquisition should time out first"),
            || unreachable!("lock acquisition should time out first"),
        )
        .expect_err("ensure should time out behind held lock");

        assert!(err.message.contains("timed out"));
        assert!(err.message.contains("ensure-running lifecycle lock"));
    });
}

#[test]
fn ensure_running_returns_stale_live_daemon_without_starting_duplicate() {
    with_isolated_home(|_| {
        let daemon = fake_daemon(4242, "lease-stale");
        let state = Arc::new(Mutex::new(FakeEnsureState {
            daemon: Some(daemon.clone()),
            starts: 0,
        }));
        let read_state = Arc::clone(&state);
        let start_state = Arc::clone(&state);

        let attached = ensure_running_with_operations(
            Duration::from_millis(50),
            super::super::acquire_daemon_operation_lock_for_ensure,
            move || {
                Ok(fake_status(
                    read_state.lock().expect("state").daemon.clone(),
                    false,
                ))
            },
            |pid| pid == daemon.pid,
            move || {
                let mut state = start_state.lock().expect("state");
                state.starts += 1;
                Ok(fake_daemon(4343, "unexpected-replacement"))
            },
        )
        .expect("attach stale live daemon");

        assert_eq!(attached, daemon);
        assert_eq!(state.lock().expect("state").starts, 0);
    });
}

#[test]
fn ensure_running_concurrent_callers_converge_on_same_daemon() {
    with_isolated_home(|_| {
        let daemon = fake_daemon(4242, "lease-shared");
        let state = Arc::new(Mutex::new(FakeEnsureState::default()));
        let barrier = Arc::new(Barrier::new(3));
        let first =
            ensure_with_fake_operations(Arc::clone(&barrier), Arc::clone(&state), daemon.clone());
        let second = ensure_with_fake_operations(Arc::clone(&barrier), Arc::clone(&state), daemon);
        barrier.wait();

        let first = first.join().expect("first thread").expect("first ensure");
        let second = second
            .join()
            .expect("second thread")
            .expect("second ensure");
        assert_eq!(first.pid, second.pid);
        assert_eq!(first.lease_id, second.lease_id);
        assert_eq!(first.address, second.address);
        assert_eq!(state.lock().expect("state").starts, 1);
    });
}

#[test]
fn ensure_running_concurrent_callers_wait_for_slow_startup() {
    with_isolated_home(|_| {
        let daemon = fake_daemon(4242, "lease-slow");
        let state = Arc::new(Mutex::new(FakeEnsureState::default()));
        let barrier = Arc::new(Barrier::new(3));
        let first = ensure_with_slow_fake_operations(
            Arc::clone(&barrier),
            Arc::clone(&state),
            daemon.clone(),
        );
        let second =
            ensure_with_slow_fake_operations(Arc::clone(&barrier), Arc::clone(&state), daemon);
        barrier.wait();

        let first = first.join().expect("first thread").expect("first ensure");
        let second = second
            .join()
            .expect("second thread")
            .expect("second ensure");
        assert_eq!(first, second);
        assert_eq!(state.lock().expect("state").starts, 1);
    });
}

#[test]
fn ensure_running_returns_the_startup_failure() {
    with_isolated_home(|_| {
        let err = ensure_running_with_operations(
            Duration::from_millis(50),
            super::super::acquire_daemon_operation_lock_for_ensure,
            || Ok(fake_status(None, false)),
            |_| false,
            || Err(crate::error::Error::internal_unexpected("startup failed")),
        )
        .expect_err("startup failure must be returned to the caller");

        assert_eq!(err.message, "startup failed");
    });
}

#[test]
fn startup_observation_requires_a_live_attempt_token() {
    let expected = "attempt-token";
    let wrong = fake_status(Some(fake_daemon(4242, "other-lease")), true);
    let result = observe_startup_lease(|| None, expected, 0, || Ok(wrong.clone()), || {})
        .expect("observe startup");
    let observed = result.expect_err("wrong daemon must not be adopted");
    assert_eq!(observed.observed_pid, Some(4242));
    assert_eq!(observed.observed_lease_id.as_deref(), Some("other-lease"));

    let mut stale = fake_status(Some(fake_daemon(4242, "stale-lease")), true);
    stale.state.as_mut().expect("state").startup_token = expected.to_string();
    let result = observe_startup_lease(|| None, expected, 0, || Ok(stale.clone()), || {})
        .expect("observe startup");
    assert!(
        result.is_ok(),
        "the matching token identifies the serve child of this launcher"
    );

    let mut dead = fake_status(Some(fake_daemon(4242, "dead-lease")), false);
    dead.state.as_mut().expect("state").startup_token = expected.to_string();
    let result = observe_startup_lease(|| None, expected, 0, || Ok(dead.clone()), || {})
        .expect("observe startup");
    assert!(result.is_err(), "a stale matching token is not ready");
}

#[test]
fn startup_observation_waits_for_delayed_token_without_provider_work() {
    let expected = "attempt-token";
    let missing = fake_status(None, false);
    let mut ready = fake_status(Some(fake_daemon(4343, "lease-new")), true);
    ready.state.as_mut().expect("state").startup_token = expected.to_string();
    let states = Arc::new(Mutex::new(vec![missing, ready]));
    let read_states = Arc::clone(&states);
    let result = observe_startup_lease(
        || None,
        expected,
        1,
        move || Ok(read_states.lock().expect("states").remove(0)),
        || {},
    )
    .expect("observe delayed startup")
    .expect("matching delayed token is ready");
    assert_eq!(result.lease_id, "lease-new");
}

#[test]
fn startup_observation_stops_as_soon_as_the_child_exits() {
    // #17445: a `serve` that died on bind used to be polled for the whole
    // observation budget, re-reading status on every poll.
    let missing = fake_status(None, false);
    let mut reads = 0;
    let mut sleeps = 0;
    let observed = observe_startup_lease(
        || Some("exit status: 1".to_string()),
        "attempt-token",
        100,
        || {
            reads += 1;
            Ok(missing.clone())
        },
        || sleeps += 1,
    )
    .expect("observe startup")
    .expect_err("an exited child never admits work");
    assert_eq!((reads, sleeps), (1, 0), "one status read, no polling");
    assert_eq!(observed.child_exit.as_deref(), Some("exit status: 1"));
}

#[test]
fn startup_observation_admits_a_lease_published_before_the_child_exited() {
    let expected = "attempt-token";
    let mut ready = fake_status(Some(fake_daemon(4343, "lease-new")), true);
    ready.state.as_mut().expect("state").startup_token = expected.to_string();
    let result = observe_startup_lease(
        || Some("exit status: 0".to_string()),
        expected,
        100,
        || Ok(ready.clone()),
        || {},
    )
    .expect("observe startup")
    .expect("an admitting lease wins over a later-sampled exit");
    assert_eq!(result.lease_id, "lease-new");
}

#[test]
fn startup_error_names_the_child_exit_instead_of_its_consequences() {
    let exited = super::StartupLeaseObservation {
        observed_pid: None,
        observed_lease_id: None,
        observed_token: None,
        observed_running: Some(false),
        observed_fresh: Some(false),
        observed_reachable: Some(false),
        child_exit: Some("exit status: 1".to_string()),
    };
    let error = super::startup_timeout_error(4343, "attempt-token", exited, Vec::new(), None);
    let message = error.to_string();
    assert!(
        message.contains("the daemon process exited before publishing its lease (exit status: 1)"),
        "{message}"
    );
    assert!(!message.contains("stale"), "{message}");
    assert!(!message.contains("not reachable"), "{message}");
    assert_eq!(error.details["child_exit"], "exit status: 1");
}

#[test]
fn startup_cleanup_bounds_signals_and_revalidates_ownership_before_sigkill() {
    let pid = 4343;
    let token = "attempt-token";

    let mut cooperative_signals = Vec::new();
    let signal = terminate_token_owned_startup_process_with_operations(
        pid,
        token,
        Duration::from_millis(1),
        |_, _| Ok(true),
        |_, signal| {
            cooperative_signals.push(signal);
            Ok(())
        },
        |_, _| true,
    )
    .expect("cooperative cleanup");
    assert_eq!(signal, "SIGTERM");
    assert_eq!(cooperative_signals, vec![SIGNAL_TERMINATE]);

    let mut escalation_ownership = VecDeque::from([true, true]);
    let mut escalation_waits = VecDeque::from([false, true]);
    let mut escalation_signals = Vec::new();
    let signal = terminate_token_owned_startup_process_with_operations(
        pid,
        token,
        Duration::from_millis(1),
        |_, _| Ok(escalation_ownership.pop_front().expect("ownership probe")),
        |_, signal| {
            escalation_signals.push(signal);
            Ok(())
        },
        |_, _| escalation_waits.pop_front().expect("exit probe"),
    )
    .expect("escalated cleanup");
    assert_eq!(signal, "SIGKILL");
    assert_eq!(escalation_signals, vec![SIGNAL_TERMINATE, SIGNAL_KILL]);

    let mut lost_ownership = VecDeque::from([true, false]);
    let mut lost_signals = Vec::new();
    let error = terminate_token_owned_startup_process_with_operations(
        pid,
        token,
        Duration::from_millis(1),
        |_, _| Ok(lost_ownership.pop_front().expect("ownership probe")),
        |_, signal| {
            lost_signals.push(signal);
            Ok(())
        },
        |_, _| false,
    )
    .expect_err("ownership loss must refuse SIGKILL");
    assert!(error
        .to_string()
        .contains("lost daemon startup-token ownership"));
    assert_eq!(lost_signals, vec![SIGNAL_TERMINATE]);

    let mut survival_waits = VecDeque::from([false, false]);
    let error = terminate_token_owned_startup_process_with_operations(
        pid,
        token,
        Duration::from_millis(1),
        |_, _| Ok(true),
        |_, _| Ok(()),
        |_, _| survival_waits.pop_front().expect("exit probe"),
    )
    .expect_err("SIGKILL survival must fail");
    assert!(error
        .to_string()
        .contains("survived bounded SIGTERM-to-SIGKILL escalation"));
}

#[test]
fn startup_recovery_reuses_only_the_authoritative_attempt_identity() {
    let expected = "attempt-token";
    let absent = super::StartupLeaseObservation {
        observed_pid: None,
        observed_lease_id: None,
        observed_token: None,
        observed_running: None,
        observed_fresh: None,
        observed_reachable: None,
        child_exit: None,
    };
    assert!(can_recover_startup_attempt(true, expected, &absent, &[]));

    let stale = super::StartupLeaseObservation {
        observed_pid: Some(4242),
        observed_lease_id: Some("stale-lease".to_string()),
        observed_token: Some(expected.to_string()),
        observed_running: None,
        observed_fresh: None,
        observed_reachable: None,
        child_exit: None,
    };
    assert!(can_recover_startup_attempt(true, expected, &stale, &[]));

    let competing = super::StartupLeaseObservation {
        observed_pid: Some(4343),
        observed_lease_id: Some("other-lease".to_string()),
        observed_token: Some("other-token".to_string()),
        observed_running: None,
        observed_fresh: None,
        observed_reachable: None,
        child_exit: None,
    };
    assert!(!can_recover_startup_attempt(
        true,
        expected,
        &competing,
        &[]
    ));
    assert!(!can_recover_startup_attempt(
        true,
        expected,
        &stale,
        &[
            "retained lease for pid 4242 because its token ownership could not be proven"
                .to_string()
        ],
    ));
}

#[test]
fn startup_timeout_error_reports_failed_admission_without_fabricating_a_token_mismatch() {
    let died = super::StartupLeaseObservation {
        observed_pid: Some(4343),
        observed_lease_id: Some("lease-new".to_string()),
        observed_token: Some("attempt-token".to_string()),
        observed_running: Some(false),
        observed_fresh: Some(true),
        observed_reachable: Some(true),
        child_exit: None,
    };
    let error = super::startup_timeout_error(4343, "attempt-token", died.clone(), Vec::new(), None);
    assert!(
        !error.to_string().contains("token and PID did not match"),
        "the common timeout cause must not be blamed on a token/PID mismatch"
    );
    assert!(
        error
            .to_string()
            .contains("the daemon process was not running"),
        "the report must name the failing admission condition: {error}"
    );
    assert_eq!(
        error.details["admission"],
        serde_json::json!({
            "running": false,
            "fresh": true,
            "reachable": true,
            "startup_token_mismatch": false,
        })
    );

    let unreachable = super::StartupLeaseObservation {
        observed_pid: Some(4343),
        observed_lease_id: Some("lease-new".to_string()),
        observed_token: Some("attempt-token".to_string()),
        observed_running: Some(true),
        observed_fresh: Some(true),
        observed_reachable: Some(false),
        child_exit: None,
    };
    let error = super::startup_timeout_error(4343, "attempt-token", unreachable, Vec::new(), None);
    assert!(
        error
            .to_string()
            .contains("the daemon API was not reachable"),
        "a live-but-unreachable daemon is reported for what it is: {error}"
    );

    let competing = super::StartupLeaseObservation {
        observed_pid: Some(4343),
        observed_lease_id: Some("other-lease".to_string()),
        observed_token: Some("other-token".to_string()),
        observed_running: Some(true),
        observed_fresh: Some(true),
        observed_reachable: Some(true),
        child_exit: None,
    };
    let error = super::startup_timeout_error(4343, "attempt-token", competing, Vec::new(), None);
    assert!(
        error.to_string().contains("different startup token"),
        "an actually observed competing token is still named: {error}"
    );
}

#[test]
fn startup_timeout_error_carries_the_daemon_log_tail() {
    let died = super::StartupLeaseObservation {
        observed_pid: Some(4343),
        observed_lease_id: None,
        observed_token: None,
        observed_running: Some(false),
        observed_fresh: Some(true),
        observed_reachable: Some(false),
        child_exit: None,
    };
    let error = super::startup_timeout_error(
        4343,
        "attempt-token",
        died.clone(),
        Vec::new(),
        Some("error: daemon bind failed".to_string()),
    );
    assert_eq!(
        error.details["daemon_log"],
        serde_json::json!("error: daemon bind failed")
    );
    let error = super::startup_timeout_error(4343, "attempt-token", died, Vec::new(), None);
    assert_eq!(error.details["daemon_log"], serde_json::Value::Null);
}

#[test]
fn locked_start_operations_publish_once_and_converge_concurrent_callers() {
    with_isolated_home(|_| {
        let daemon = fake_daemon(4242, "lease-published");
        let state = Arc::new(Mutex::new(FakeEnsureState::default()));
        let barrier = Arc::new(Barrier::new(3));
        let first = locked_start_with_fake_operations(
            Arc::clone(&barrier),
            Arc::clone(&state),
            daemon.clone(),
        );
        let second =
            locked_start_with_fake_operations(Arc::clone(&barrier), Arc::clone(&state), daemon);
        barrier.wait();
        let first = first.join().expect("first start").expect("first result");
        let second = second.join().expect("second start").expect("second result");
        assert_eq!(first.pid, second.pid);
        assert_eq!(first.lease_id, second.lease_id);
        assert_eq!(state.lock().expect("state").starts, 1);
    });
}

fn ensure_with_fake_operations(
    barrier: Arc<Barrier>,
    state: Arc<Mutex<FakeEnsureState>>,
    daemon: super::DaemonStartResult,
) -> std::thread::JoinHandle<crate::error::Result<super::DaemonStartResult>> {
    std::thread::spawn(move || {
        barrier.wait();
        let read_state = Arc::clone(&state);
        let start_state = Arc::clone(&state);
        let daemon_pid = daemon.pid;
        ensure_running_with_operations(
            Duration::from_secs(1),
            super::super::acquire_daemon_operation_lock_for_ensure,
            move || {
                Ok(fake_status(
                    read_state.lock().expect("state").daemon.clone(),
                    true,
                ))
            },
            move |pid| pid == daemon_pid,
            move || {
                let mut state = start_state.lock().expect("state");
                state.starts += 1;
                state.daemon = Some(daemon.clone());
                Ok(daemon)
            },
        )
    })
}

fn ensure_with_slow_fake_operations(
    barrier: Arc<Barrier>,
    state: Arc<Mutex<FakeEnsureState>>,
    daemon: super::DaemonStartResult,
) -> std::thread::JoinHandle<crate::error::Result<super::DaemonStartResult>> {
    std::thread::spawn(move || {
        barrier.wait();
        let read_state = Arc::clone(&state);
        let start_state = Arc::clone(&state);
        let daemon_pid = daemon.pid;
        ensure_running_with_operations(
            Duration::from_secs(1),
            super::super::acquire_daemon_operation_lock_for_ensure,
            move || {
                Ok(fake_status(
                    read_state.lock().expect("state").daemon.clone(),
                    true,
                ))
            },
            move |pid| pid == daemon_pid,
            move || {
                std::thread::sleep(Duration::from_millis(100));
                let mut state = start_state.lock().expect("state");
                state.starts += 1;
                state.daemon = Some(daemon.clone());
                Ok(daemon)
            },
        )
    })
}

fn locked_start_with_fake_operations(
    barrier: Arc<Barrier>,
    state: Arc<Mutex<FakeEnsureState>>,
    daemon: super::DaemonStartResult,
) -> std::thread::JoinHandle<crate::error::Result<super::DaemonStartResult>> {
    std::thread::spawn(move || {
        barrier.wait();
        let read_state = Arc::clone(&state);
        let start_state = Arc::clone(&state);
        let daemon_pid = daemon.pid;
        super::ensure_running_with_operations(
            Duration::from_secs(1),
            super::super::acquire_daemon_operation_lock_for_ensure,
            move || {
                Ok(fake_status(
                    read_state.lock().expect("state").daemon.clone(),
                    false,
                ))
            },
            move |pid| pid == daemon_pid,
            move || {
                let publish_state = Arc::clone(&start_state);
                super::start_or_return_live_with_operations(
                    || {
                        Ok(fake_status(
                            start_state.lock().expect("state").daemon.clone(),
                            false,
                        ))
                    },
                    || Ok(Some(())),
                    || Ok(()),
                    move || {
                        let mut state = publish_state.lock().expect("state");
                        state.starts += 1;
                        state.daemon = Some(daemon.clone());
                        Ok(daemon)
                    },
                )
            },
        )
    })
}

fn fake_daemon(pid: u32, lease_id: &str) -> super::DaemonStartResult {
    super::DaemonStartResult {
        pid,
        address: "127.0.0.1:49152".to_string(),
        state_path: "/fake/daemon-state.json".to_string(),
        lease_id: lease_id.to_string(),
    }
}

fn fake_status(daemon: Option<super::DaemonStartResult>, fresh: bool) -> DaemonStatus {
    let stale_reason_code = (!fresh).then_some(DaemonStaleReasonCode::VersionMismatch);
    // The fixture models a live daemon whenever a lease is present, matching
    // the real projection where a version-mismatched lease keeps its live PID.
    let mut status = DaemonStatus {
        running: daemon.is_some(),
        fresh,
        reachable: true,
        freshness: DaemonFreshnessReport {
            fresh,
            stale_reason_code,
            restartable: !fresh,
            lease_id: daemon.as_ref().map(|daemon| daemon.lease_id.clone()),
            pid: daemon.as_ref().map(|daemon| daemon.pid),
            recovery_evidence: None,
            ownership_evidence: None,
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs: 0,
            termination_evidence: None,
            repair_plan: Vec::new(),
        },
        stale_reason: (!fresh).then(|| "simulated stale daemon".to_string()),
        replacement_blocked: !fresh,
        replacement_blocked_reason: (!fresh)
            .then(|| "the recorded evidence authorizes no replacement".to_string()),
        summary: String::new(),
        state: daemon.map(fake_daemon_state),
        state_path: "/fake/daemon-state.json".to_string(),
        state_identity: "sha256:fake".to_string(),
        owner_lock: crate::daemon::DaemonOwnerLockProbe::Free,
        competing_serve: None,
        active_job_recovery_evidence: Vec::new(),
        termination_evidence: None,
    };
    status.summary = status.render_summary();
    status
}

fn fake_daemon_state(daemon: super::DaemonStartResult) -> DaemonState {
    DaemonState {
        schema: crate::daemon::DAEMON_LEASE_SCHEMA.to_string(),
        lease_id: daemon.lease_id,
        startup_token: "fake-startup-token".to_string(),
        address: daemon.address,
        pid: daemon.pid,
        state_path: daemon.state_path,
        started_at: "2026-01-01T00:00:00Z".to_string(),
        last_seen_at: "2026-01-01T00:00:00Z".to_string(),
        build_identity: BuildIdentity {
            version: "test".to_string(),
            git_commit: None,
            git_dirty: None,
            display: "homeboy test".to_string(),
        },
        service_manager: None,
        executable: crate::daemon::ExecutableIdentity {
            path: "/usr/local/bin/homeboy".into(),
            dev: 0,
            ino: 0,
        },
        runtime_paths: DaemonRuntimeSnapshot {
            pressure: None,
            loaded_at: "2026-01-01T00:00:00Z".to_string(),
            paths: Vec::new(),
        },
    }
}

mod startup_cleanup_contract {
    use super::*;

    #[test]
    fn exit_race_is_not_escalated_to_sigkill() {
        let mut signals = Vec::new();
        let outcome = super::super::terminate_token_owned_startup_process_with_outcome(
            4343,
            "attempt-token",
            Duration::from_millis(1),
            |_, _| Ok(true),
            |_| false,
            |_, signal| {
                signals.push(signal);
                Ok(())
            },
            |_, _| false,
        )
        .expect("exit race is cleanup evidence, not a cleanup error");

        assert_eq!(
            outcome,
            super::super::StartupCleanupOutcome::ExitedBeforeEscalation
        );
        assert_eq!(signals, vec![SIGNAL_TERMINATE]);
    }

    #[test]
    fn live_ownership_loss_is_distinct_and_never_signals_the_reused_pid() {
        let mut signals = Vec::new();
        let mut ownership = VecDeque::from([true, false]);
        let outcome = super::super::terminate_token_owned_startup_process_with_outcome(
            4343,
            "attempt-token",
            Duration::from_millis(1),
            |_, _| Ok(ownership.pop_front().expect("ownership probe")),
            |_| true,
            |_, signal| {
                signals.push(signal);
                Ok(())
            },
            |_, _| false,
        )
        .expect("ownership loss is cleanup evidence, not a cleanup error");

        assert_eq!(outcome, super::super::StartupCleanupOutcome::OwnershipLost);
        assert_eq!(signals, vec![SIGNAL_TERMINATE]);
    }

    #[test]
    fn cleanup_evidence_does_not_replace_the_primary_startup_error() {
        let observation = super::super::StartupLeaseObservation {
            observed_pid: Some(4343),
            observed_lease_id: Some("lease-new".to_string()),
            observed_token: Some("attempt-token".to_string()),
            observed_running: Some(false),
            observed_fresh: Some(true),
            observed_reachable: Some(true),
            child_exit: None,
        };
        let error = super::super::startup_timeout_error(
            4343,
            "attempt-token",
            observation,
            vec!["startup cleanup observed an exit race; no signal sent".to_string()],
            None,
        );

        assert!(error.message.contains("daemon startup did not reach"));
        assert_eq!(
            error.details["cleanup"][0],
            serde_json::json!("startup cleanup observed an exit race; no signal sent")
        );
        assert_eq!(
            error.details["classification"],
            serde_json::json!("terminal_pre_provider_startup")
        );
    }
}

/// #17387: a held owner lock is a live or starting daemon (a service manager
/// restarting its own, for one) even when no lease says so. No start path may
/// spawn beside it.
#[test]
fn start_refuses_to_spawn_while_a_live_owner_holds_the_lock() {
    let error = super::start_or_return_live_with_operations(
        || Ok(fake_status(None, false)),
        || Ok(None::<()>),
        || unreachable!("nothing to clean up while the owner lock is held"),
        || -> crate::error::Result<super::DaemonStartResult> {
            panic!("a daemon was spawned beside a live owner")
        },
    )
    .expect_err("a held owner lock refuses the start");
    assert!(
        error.message.contains("owner lock is held"),
        "{}",
        error.message
    );
}

#[test]
fn start_names_the_service_manager_that_owns_a_stale_daemon() {
    let mut status = fake_status(Some(fake_daemon(4242, "lease-managed")), false);
    status.state.as_mut().unwrap().service_manager =
        Some(crate::daemon::DaemonServiceManager::Systemd);
    let error = super::start_or_return_live_with_operations(
        move || Ok(status),
        || Ok(None::<()>),
        || unreachable!("a managed daemon's lease is never cleaned up here"),
        || -> crate::error::Result<super::DaemonStartResult> {
            panic!("a daemon was spawned beside its service manager")
        },
    )
    .expect_err("a service-managed owner refuses the start");
    assert!(
        error.message.contains("service manager systemd"),
        "{}",
        error.message
    );
}

fn managed_status(daemon: Option<super::DaemonStartResult>, fresh: bool) -> DaemonStatus {
    let mut status = fake_status(daemon, fresh);
    if let Some(state) = status.state.as_mut() {
        state.service_manager = Some(crate::daemon::DaemonServiceManager::Systemd);
    }
    status
}

/// #17387: `ensure-running` defers a stale service-managed daemon to its
/// manager instead of stopping it and spawning a Homeboy-owned replacement.
#[test]
fn ensure_running_defers_a_stale_service_managed_daemon_to_its_manager() {
    let managed = managed_status(Some(fake_daemon(4242, "lease-managed")), false);
    assert_eq!(
        super::stale_daemon_action(&managed),
        Some(super::StaleDaemonAction::RestartServiceManaged(
            "lease-managed"
        ))
    );
    let plain = fake_status(Some(fake_daemon(4242, "lease-plain")), false);
    assert_eq!(
        super::stale_daemon_action(&plain),
        Some(super::StaleDaemonAction::RestartInPlace("lease-plain"))
    );
    let fresh = managed_status(Some(fake_daemon(4242, "lease-fresh")), true);
    assert_eq!(super::stale_daemon_action(&fresh), None);
}

/// #17387: restarting a service-managed daemon signals the exact leased
/// daemon once and then only observes; the replacement comes from its
/// manager. The operation has no spawn step at all.
#[test]
fn service_managed_restart_waits_for_the_managers_replacement() {
    let old = fake_daemon(4242, "lease-old");
    let new = fake_daemon(4343, "lease-new");
    let mut reads = VecDeque::from([
        managed_status(Some(old.clone()), false),
        // The old daemon is still draining, then gone, then replaced.
        managed_status(Some(old.clone()), false),
        managed_status(None, false),
        managed_status(Some(new.clone()), true),
    ]);
    let mut signaled = Vec::new();
    let replacement = super::restart_service_managed_with(
        "lease-old",
        DaemonStatus::admits_work,
        || Ok(reads.pop_front().expect("no read past the replacement")),
        |state| {
            signaled.push(state.pid);
            Ok(())
        },
        || {},
        10,
    )
    .expect("the manager's replacement is returned");
    assert_eq!(replacement, new);
    assert_eq!(signaled, vec![old.pid]);
}

#[test]
fn service_managed_restart_fails_instead_of_spawning_when_the_manager_does_not_replace() {
    let old = fake_daemon(4242, "lease-old");
    let mut terminated = 0;
    let error = super::restart_service_managed_with(
        "lease-old",
        DaemonStatus::admits_work,
        || Ok(managed_status(None, false)),
        |_| {
            terminated += 1;
            Ok(())
        },
        || {},
        3,
    );
    // The first read finds no live lease: nothing is signaled.
    assert!(error.is_err());
    assert_eq!(terminated, 0);

    let mut reads = VecDeque::from([managed_status(Some(old), false)]);
    let error = super::restart_service_managed_with(
        "lease-old",
        DaemonStatus::admits_work,
        || {
            Ok(reads
                .pop_front()
                .unwrap_or_else(|| managed_status(None, false)))
        },
        |_| {
            terminated += 1;
            Ok(())
        },
        || {},
        3,
    )
    .expect_err("no replacement is an error, never a spawn");
    assert_eq!(terminated, 1);
    assert!(
        error.message.contains("does not start one beside it"),
        "{}",
        error.message
    );
}

#[test]
fn service_managed_restart_refuses_an_unmanaged_lease() {
    let error = super::restart_service_managed_with(
        "lease-plain",
        DaemonStatus::admits_work,
        || Ok(fake_status(Some(fake_daemon(4242, "lease-plain")), false)),
        |_| panic!("an unmanaged daemon is not restarted through a manager"),
        || {},
        3,
    )
    .expect_err("unmanaged lease is refused");
    assert!(error.message.contains("not owned by a service manager"));
}
