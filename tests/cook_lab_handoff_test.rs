use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use homeboy_core::observation::{NewRunRecord, ObservationStore, RunStatus};
use homeboy_core::test_support::{bounded_output, HermeticTestContext, TestBinary};

struct DelayedUnavailableDaemon {
    address: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl DelayedUnavailableDaemon {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake SSH daemon endpoint");
        listener
            .set_nonblocking(true)
            .expect("make fake daemon nonblocking");
        let address = listener
            .local_addr()
            .expect("fake daemon address")
            .to_string();
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        worker_requests.fetch_add(1, Ordering::SeqCst);
                        let mut request = [0; 1024];
                        let _ = stream.read(&mut request);
                        // Keep an interrupted owner observable long enough to
                        // replace its lease without relying on a real network.
                        thread::sleep(Duration::from_millis(25));
                        let _ = stream.write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept fake SSH daemon endpoint: {error}"),
                }
            }
        });
        Self {
            address,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn wait_for_request(&self, previous: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.requests.load(Ordering::SeqCst) <= previous {
            assert!(
                Instant::now() < deadline,
                "recovery did not reach fake daemon endpoint"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for DelayedUnavailableDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(&self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("fake SSH daemon endpoint exits");
        }
    }
}

fn expire_recovery_owner(store: &ObservationStore, owner_id: &str) {
    let mut owner = store
        .get_run(owner_id)
        .expect("read recovery owner")
        .expect("recovery owner exists");
    owner.metadata_json["lease_expires_at_ms"] = serde_json::json!(0);
    store
        .update_run_metadata(owner_id, owner.metadata_json)
        .expect("interrupt recovery owner lease");
}

fn wait_for_detached_process_cleanup(pid: u64) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        #[cfg(unix)]
        let still_running = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
        #[cfg(not(unix))]
        let still_running = false;
        if !still_running {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "detached Cook process {pid} did not exit after cleanup"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// Run the fixture binary through the shared hermetic harness.
///
/// Every case in this file asserts what an invocation settles *before* any
/// worktree or provider resolution: a contradictory combination is rejected,
/// and a locally-detached Cook is handed off (#11476).
///
/// `HermeticTestContext::command` owns the complete isolation contract — HOME,
/// XDG config and data roots, artifact root, runtime and temp dirs — and strips
/// the Lab transport variables. Overriding only HOME on a hand-rolled `Command`
/// leaves that leak open (#10717, #10718). These cases run with a default
/// operator context; the transport variables are injected explicitly only by
/// the case that asserts they cannot change a validation outcome (#10917).
///
/// `controller_runtime_command` adds the controller-runtime pins on top of that
/// contract. Every `agent-task cook` invocation is sealed into an immutable
/// controller runtime by `delegate_agent_task_cook_to_pinned_runtime` *before*
/// routing reaches validation, so a case that survives clap pays a cold pin —
/// hash, 666 MB copy, re-exec — against a fresh `HOME`. The pins are immutable
/// and content-addressed, so sharing them costs no isolation (#10687, #11185).
///
/// `bounded_output` replaces `Command::output`, which waits on pipe EOF with no
/// ceiling and cannot be interrupted by libtest (#10687).
fn homeboy(args: &[&str]) -> Output {
    let context = HermeticTestContext::new();
    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command.args(args);
    bounded_output(command)
}

#[test]
fn cook_handoff_subprocesses_do_not_inherit_controller_transport() {
    // Pins the isolation the rejections below depend on. If these fixtures ever
    // stop stripping the controller transport variables, the routing bail-out
    // above `run_split_placement_cook` swallows every rejection this file
    // asserts, and the suite hangs instead of failing (#10917).
    let context = HermeticTestContext::new();
    let command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    let removed_env = command
        .get_envs()
        .filter_map(|(key, value)| value.is_none().then(|| key.to_string_lossy().into_owned()))
        .collect::<std::collections::BTreeSet<_>>();

    assert!(
        removed_env.contains(homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV),
        "cook handoff fixtures must not inherit Lab offload transport"
    );
    assert!(
        removed_env.contains(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV),
        "cook handoff fixtures must not inherit the controller source snapshot"
    );
}

#[test]
fn contradictory_cook_arguments_survive_controller_transport_context() {
    // The routing bail-outs skip *routing*, not validation. Injecting the exact
    // transport context that used to suppress these rejections must not change
    // the outcome: a contradictory flag combination is contradictory wherever
    // the process runs.
    //
    // Before #10917 this context sent both invocations past validation into
    // real worktree and provider work, where they blocked indefinitely instead
    // of failing fast.
    // #11185 moved this case onto the shared controller-runtime pins; #11191
    // reverted it while rebasing an unrelated broker fixture, restoring a cold
    // 666 MB pin per invocation on a gate that was already at its ceiling.
    let cook = |args: &[&str]| {
        let context = HermeticTestContext::new();
        let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
        command
            .env(
                homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV,
                r#"{"runner_id":"homeboy-lab"}"#,
            )
            .args(args);
        bounded_output(command)
    };

    // Local detach is served on a controller (#11476), but not here: this
    // process IS the runner's owned execution of one attempt. It has no
    // controller lifecycle to hand off, so detaching would orphan work the
    // runner believes it owns. The rejection must still land before any
    // worktree or provider resolution.
    let detach = cook(&[
        "--placement",
        "local",
        "--detach-after-handoff",
        "agent-task",
        "cook",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "missing@worktree",
        "--verify",
        "true",
    ]);
    assert!(!detach.status.success());
    let detach_stdout = String::from_utf8_lossy(&detach.stdout);
    assert!(
        detach_stdout.contains(
            "cannot detach after handoff with --placement local inside a runner-owned execution"
        ),
        "{detach_stdout}"
    );
    assert!(
        !detach_stdout.contains("worktree provider"),
        "{detach_stdout}"
    );

    let queue_only = cook(&[
        "agent-task",
        "cook",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "missing@worktree",
        "--verify",
        "true",
        "--queue-only",
    ]);
    assert!(!queue_only.status.success());
    let queue_only_stdout = String::from_utf8_lossy(&queue_only.stdout);
    assert!(
        queue_only_stdout.contains("cannot queue its controller-owned lifecycle"),
        "{queue_only_stdout}"
    );
    assert!(
        !queue_only_stdout.contains("worktree provider"),
        "{queue_only_stdout}"
    );
}

#[test]
fn cook_rejects_invalid_controller_transport_before_worktree_resolution() {
    // `--runner` implies Lab placement, so combining it with an explicit
    // `--placement` is contradictory and rejected at argument parsing (see
    // `runner_and_placement_are_mutually_exclusive`). That rejection happens
    // before any worktree provider resolution.
    let output = homeboy(&[
        "--placement",
        "local",
        "--runner",
        "homeboy-lab",
        "agent-task",
        "cook",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "missing@worktree",
        "--verify",
        "true",
    ]);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("'--placement <PLACEMENT>' cannot be used with '--runner <RUNNER_ID>'"));
    assert!(!stdout.contains("worktree provider"));
    assert!(!stderr.contains("worktree provider"));
}

/// `--placement local --detach-after-handoff` is served, not rejected (#11476).
///
/// Detached admission validates the destination before it can claim durable
/// ownership. A missing repository is therefore a pre-acceptance rejection,
/// not a synthetic handoff to a child that cannot resume it.
#[test]
fn cook_detaches_local_placement_instead_of_rejecting_it() {
    let context = HermeticTestContext::new();
    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command
        // Bound the launcher's wait so an unresolvable destination reports an
        // honest handoff state promptly instead of holding the default budget.
        .env("HOMEBOY_COOK_DETACH_HANDOFF_TIMEOUT_MS", "5000")
        .args([
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "missing@worktree",
            "--verify",
            "true",
        ]);
    let output = bounded_output(command);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("--repo <repo> is required"), "{stdout}");
}

/// Historical runner recovery is a distinct owner, never an admission gate for
/// an accepted Cook. This invokes the actual detached Cook command path rather
/// than a scheduler helper so its output and durable handoff remain observable.
#[test]
fn detached_cook_admission_is_bounded_with_a_hundred_unavailable_recovery_records() {
    let _env_guard = homeboy_core::test_support::home_env_guard();
    let context = HermeticTestContext::new();
    std::env::set_var("HOME", context.home());
    std::env::set_var("XDG_CONFIG_HOME", context.root().join(".config"));
    std::env::set_var("XDG_DATA_HOME", context.root().join("data"));
    std::env::set_var("HOMEBOY_ARTIFACT_ROOT", context.artifact_dir());
    std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", context.runtime_dir());
    std::env::set_var("TMPDIR", context.temp_dir());
    std::env::set_var("HOMEBOY_CONTROLLER_ID", "cook-admission-fixture");
    let (_checkout_guard, checkout) =
        homeboy_core::test_support::shared_committed_git_repo_fixture("cook-admission-source");
    let task_worktree = context.root().join("cook-admission-worktree");
    homeboy_core::test_support::run_git_fixture_command(
        &checkout,
        &[
            "worktree",
            "add",
            "-b",
            "cook-admission-worktree",
            task_worktree.to_str().expect("task worktree path"),
        ],
    );
    let provider = context.root().join("fixture-provider.sh");
    std::fs::write(
        &provider,
        "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '%s\\n' '{\"schema\":\"homeboy/agent-task-outcome/v1\",\"status\":\"succeeded\",\"summary\":\"fixture provider completed\"}'\n",
    )
    .expect("write fixture provider");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755))
            .expect("make fixture provider executable");
    }
    let database = context.data_dir().join("homeboy.sqlite");
    let store = ObservationStore::open_initialized_at(&database).expect("open fixture store");
    for index in 0..100 {
        store
            .start_run_with_id(
                NewRunRecord::builder("runner_execution")
                    .cwd_path(Path::new("/workspace"))
                    .metadata(serde_json::json!({
                        "kind": "runner_exec",
                        "runner_id": "unavailable-runner",
                        "runner_job_id": format!("stale-job-{index}"),
                    }))
                    .build(),
                format!("stale-runner-exec-{index}"),
            )
            .expect("seed stale runner execution");
    }
    let daemon = DelayedUnavailableDaemon::start();
    let session_path = context
        .config_dir()
        .join("runner-sessions/unavailable-runner/cook-admission-fixture.json");
    std::fs::create_dir_all(session_path.parent().expect("session directory"))
        .expect("create direct SSH session directory");
    std::fs::write(
        session_path,
        serde_json::json!({
            "runner_id": "unavailable-runner",
            "mode": "direct_ssh",
            "role": "controller",
            "server_id": "unavailable-runner",
            "controller_id": "cook-admission-fixture",
            "broker_url": null,
            "remote_daemon_address": daemon.address,
            "local_port": daemon.address.rsplit(':').next().expect("fake daemon port").parse::<u16>().expect("parse fake daemon port"),
            "local_url": format!("http://{}", daemon.address),
            "tunnel_pid": null,
            "tunnel_process_start_identity": null,
            "remote_daemon_pid": 4242,
            "remote_daemon_lease_id": "unavailable-fixture-lease",
            "homeboy_version": "test",
            "homeboy_build_identity": null,
            "connected_at": "2026-01-01T00:00:00Z",
            "worker_identity": null,
            "worker_pid": null,
            "last_seen_at": null,
            "leaseless_recovery_evidence": null,
        })
        .to_string(),
    )
    .expect("persist direct SSH runner session");

    // Interrupt an owner before Cook admission. Its replacement remains a
    // separate, inspectable recovery owner while Cook is handed off.
    let first = homeboy::runner::schedule_terminal_runner_exec_recovery()
        .expect("schedule first recovery")
        .expect("first recovery owner");
    let before_handoff_requests = daemon.requests.load(Ordering::SeqCst);
    let first_owner_id = first.owner_id.clone();
    let first_owner_token = first.owner_token.clone();
    let interrupted = thread::spawn(move || {
        homeboy::runner::run_scheduled_terminal_runner_exec_recovery(
            &first_owner_id,
            &first_owner_token,
        )
        .expect("interrupted recovery returns")
    });
    daemon.wait_for_request(before_handoff_requests);
    expire_recovery_owner(&store, &first.owner_id);
    let before_handoff = homeboy::runner::schedule_terminal_runner_exec_recovery()
        .expect("take over interrupted recovery")
        .expect("replacement owner");
    interrupted.join().expect("interrupted recovery joins");

    let mut command = context.command(TestBinary::HomeboyFixture);
    let output_path = context.root().join("cook-admission-handoff.json");
    let stderr_path = context.root().join("cook-admission.stderr");
    command
        .env("HOMEBOY_CONTROLLER_ID", "cook-admission-fixture")
        // The fixture executable is its own immutable runtime. This isolates
        // the admission budget from pinning a multi-hundred-megabyte debug bin.
        .env(
            "HOMEBOY_COOK_PINNED_CONTROLLER_RUNTIME",
            context.binary_path(TestBinary::HomeboyFixture),
        )
        .env("HOMEBOY_COOK_DETACH_HANDOFF_TIMEOUT_MS", "5000")
        .args([
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "cook",
            "--run-id",
            "cook-admission-11156",
            "--prompt",
            "admit despite stale runner records",
            "--cwd",
            checkout.to_str().expect("checkout path"),
            "--to-worktree",
            task_worktree.to_str().expect("task worktree path"),
            "--backend",
            "fixture",
            "--provider-command",
            provider.to_str().expect("provider path"),
            "--verify",
            "true",
            "--max-attempts",
            "1",
            "--no-finalize",
        ]);
    let started = Instant::now();
    let output = command
        .stdout(Stdio::from(
            std::fs::File::create(&output_path).expect("create Cook output file"),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(&stderr_path).expect("create Cook stderr file"),
        ))
        .status()
        .expect("run detached Cook admission command");
    let stdout = std::fs::read_to_string(&output_path).expect("read Cook output file");
    let stderr = std::fs::read_to_string(&stderr_path).expect("read Cook stderr file");
    assert!(output.success(), "{stdout}\nstderr={stderr}");
    let handoff_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("Cook handoff is durable output\n{stdout}"));
    let response: serde_json::Value = serde_json::from_str(stdout[handoff_start..].trim())
        .unwrap_or_else(|error| panic!("Cook handoff is JSON: {error}\n{stdout}"));
    let handoff = response["data"].clone();
    assert!(
        started.elapsed() <= Duration::from_secs(6),
        "Cook admission/handoff must settle within the five-second budget plus process-exit allowance: elapsed={:?}, handoff={handoff}",
        started.elapsed()
    );
    assert_eq!(handoff["detached"], true, "{stdout}");
    assert_eq!(handoff["cook_id"], "cook-admission-11156", "{stdout}");
    assert_eq!(handoff["handoff"]["state"], "accepted", "{stdout}");
    assert!(
        handoff["handoff"]["phase_timings_ms"]["essential_validation_and_durable_handoff"]
            .as_u64()
            .is_some(),
        "{stdout}"
    );
    assert_eq!(
        handoff["handoff"]["phase_timings_ms"]["global_recovery"], "deferred_to_detached_owner",
        "{stdout}"
    );
    assert!(
        handoff["run_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("cook-admission-11156-attempt-")),
        "{stdout}"
    );
    assert_eq!(
        handoff["status_command"], "homeboy agent-task status cook-admission-11156",
        "{stdout}"
    );
    let handoff_path = handoff["launcher_log"]
        .as_str()
        .map(Path::new)
        .and_then(Path::parent)
        .expect("launcher session path")
        .join("handoff.json");
    let persisted_handoff: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&handoff_path).expect("read durable Cook handoff"))
            .expect("durable Cook handoff JSON");
    assert_eq!(persisted_handoff, handoff, "{stdout}");
    assert!(
        task_worktree.join(".git").exists(),
        "Cook destination remains owned"
    );
    let owner = store
        .get_run(&before_handoff.owner_id)
        .expect("read observable recovery owner")
        .expect("recovery owner remains observable");
    assert_eq!(owner.status, RunStatus::Running.as_str());

    #[cfg(unix)]
    if let Some(pid) = handoff["pid"].as_u64() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
        // The detached Cook can retain at most two seconds for signal delivery
        // and process reaping; this is separate from the five-second admission
        // bound above.
        wait_for_detached_process_cleanup(pid);
    }

    // Interrupt the still-observable owner after the Cook handoff, then let its
    // replacement finish. The former token must not terminalize either owner.
    let after_handoff_requests = daemon.requests.load(Ordering::SeqCst);
    let owner_id = before_handoff.owner_id.clone();
    let owner_token = before_handoff.owner_token.clone();
    let interrupted = thread::spawn(move || {
        homeboy::runner::run_scheduled_terminal_runner_exec_recovery(&owner_id, &owner_token)
            .expect("post-handoff interrupted recovery returns")
    });
    daemon.wait_for_request(after_handoff_requests);
    expire_recovery_owner(&store, &before_handoff.owner_id);
    let after_handoff = homeboy::runner::schedule_terminal_runner_exec_recovery()
        .expect("take over post-handoff recovery")
        .expect("post-handoff replacement owner");
    assert_ne!(after_handoff.owner_token, before_handoff.owner_token);
    interrupted
        .join()
        .expect("post-handoff interrupted recovery joins");
    homeboy::runner::run_scheduled_terminal_runner_exec_recovery(
        &after_handoff.owner_id,
        &after_handoff.owner_token,
    )
    .expect("replacement recovery completes");
    let owner = store
        .get_run(&after_handoff.owner_id)
        .expect("read terminal recovery owner")
        .expect("terminal recovery owner");
    assert_eq!(owner.status, RunStatus::Pass.as_str());
    assert_eq!(owner.metadata_json["phase"], "deferred");
    assert_eq!(owner.metadata_json["deferred_count"], 100);
    for index in 0..100 {
        let source = store
            .get_run(&format!("stale-runner-exec-{index}"))
            .expect("read source recovery record")
            .expect("source recovery record retained");
        assert_eq!(source.status, RunStatus::Running.as_str());
        assert_eq!(
            source.metadata_json["runner_job_id"],
            format!("stale-job-{index}")
        );
    }
    assert!(
        daemon.requests.load(Ordering::SeqCst) <= 50,
        "shared unavailable runner must not receive serial per-record probes"
    );
}

#[test]
fn non_tty_client_must_choose_one_lab_observation_mode() {
    // Command::output supplies pipes, matching an interruptible bridge client
    // rather than an interactive terminal. Reject ambiguity before any worktree
    // or provider work can begin.
    let output = homeboy(&[
        "--wait",
        "--detach-after-handoff",
        "agent-task",
        "cook",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "missing@worktree",
        "--verify",
        "true",
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot be used with"), "{stderr}");
    assert!(!stderr.contains("worktree provider"), "{stderr}");
}

#[test]
fn cook_rejects_queue_only_before_worktree_resolution() {
    let output = homeboy(&[
        "agent-task",
        "cook",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "missing@worktree",
        "--verify",
        "true",
        "--queue-only",
    ]);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cannot queue its controller-owned lifecycle"));
    assert!(!stdout.contains("worktree provider"));
}

#[test]
fn required_lab_route_without_a_selected_runner_fails_deterministically() {
    let output = homeboy(&["--placement", "lab", "review", "lint"]);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("required Lab placement has no selected ready runner")
            || stderr.contains("required Lab placement has no selected ready runner"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(!stdout.contains("worktree provider"));
    assert!(!stderr.contains("worktree provider"));
}

#[test]
fn cook_help_does_not_advertise_queue_only() {
    let output = homeboy(&["agent-task", "cook", "--help"]);

    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(!help.contains("\n      --queue-only\n"));
    assert!(help.contains("--detach-after-handoff"), "{help}");
    assert!(help.contains("--wait"), "{help}");
}
