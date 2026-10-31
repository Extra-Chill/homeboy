//! Real-process Cook execution-intent chaos (#15804 S8b PR 3, stage A).
//! Lab admission followed by the public, confirmed local placement action gives
//! us a real admission intent without a remote runner. No lifecycle/SQL writes
//! are used to arrange or recover a crash. SQL below is read-only evidence.
#![cfg(unix)]

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore;
use homeboy_core::test_support::{bounded_output, HermeticTestContext, TestBinary};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

const COOK: &str = "execution-chaos";

#[derive(Clone, Copy)]
enum Fault {
    BeforeDelivery,
    Child,
    Group,
    Stop,
}

/// Teardown for every daemon generation a case starts, even on failure.
struct Teardown<'a> {
    context: &'a HermeticTestContext,
}

impl Drop for Teardown<'_> {
    fn drop(&mut self) {
        // Open all fixture gates, then stop the current tokenized daemon.
        for gate in ["delivery", "child"] {
            let _ = std::fs::write(self.context.root().join(gate).with_extension("release"), "");
        }
        let mut stop = command(self.context);
        stop.args(["daemon", "stop"]);
        let _ = bounded_output(stop);
        if std::thread::panicking() {
            // Supervised daemon stderr is only published as termination
            // evidence, which the stop above has just written.
            eprintln!(
                "daemon termination evidence: {}",
                std::fs::read_to_string(self.context.daemon_dir().join("termination.json"))
                    .unwrap_or_default()
            );
        }
    }
}

fn command(context: &HermeticTestContext) -> Command {
    let mut command = context.command(TestBinary::HomeboyFixture);
    command.env("HOMEBOY_TEST_SUBPROCESS_BUDGET_SECS", "15");
    // Admission freezes every ambient HOMEBOY_* variable into the step
    // environment. Keep the operator's (e.g. a session notifier) out of it.
    let explicit: Vec<_> = command
        .get_envs()
        .map(|(name, _)| name.to_owned())
        .collect();
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("HOMEBOY_") && !explicit.contains(&name) {
            command.env_remove(name);
        }
    }
    command
}

fn run(context: &HermeticTestContext, args: &[&str]) -> Value {
    let mut cmd = command(context);
    cmd.args(args);
    let output = bounded_output(cmd);
    assert!(
        output.status.success(),
        "{args:?}: stdout={} stderr={}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        evidence(context),
    );
    serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
}

/// Start a tokenized daemon generation through the public `daemon start`.
///
/// `bounded_output` reaps the whole process tree of its command, and hermetic
/// daemons deliberately stay in their launcher's process group, so the
/// launcher gets a private group (never libtest's) and a bounded wait of its
/// own. Returns the serving daemon's pid.
fn start_daemon(context: &HermeticTestContext, delivery_gate: bool) -> i32 {
    let stdout = context.root().join("daemon-start.stdout");
    let stderr = context.root().join("daemon-start.stderr");
    let mut start = command(context);
    start
        .args(["daemon", "start"])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()));
    if delivery_gate {
        start.env(
            "HOMEBOY_TEST_WORK_INTENT_DELIVERY_GATE",
            context.root().join("delivery"),
        );
    }
    let mut launcher = start.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = launcher.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = launcher.kill();
            let _ = launcher.wait();
            panic!("daemon start did not return\n{}", evidence(context));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let started: Value =
        serde_json::from_slice(&std::fs::read(&stdout).unwrap_or_default()).unwrap_or(Value::Null);
    assert!(
        status.success() && started["data"]["pid"].is_u64(),
        "daemon start: stdout={started} stderr={}\n{}",
        std::fs::read_to_string(&stderr).unwrap_or_default(),
        evidence(context),
    );
    started["data"]["pid"].as_u64().unwrap() as i32
}

/// A killed daemon group can be orphaned onto this test binary when it is a
/// child subreaper; the production probe reads such a zombie as exited.
fn alive(pid: i32) -> bool {
    homeboy_core::process::pid_is_running(pid as u32)
}

fn sql_rows(path: &Path, sql: &str) -> Vec<Value> {
    let Ok(connection) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Vec::new();
    };
    connection.busy_timeout(Duration::from_secs(1)).unwrap();
    let Ok(mut statement) = connection.prepare(sql) else {
        return Vec::new();
    };
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|raw| serde_json::from_str(&raw.unwrap()).unwrap())
        .collect()
}

/// The daemon job store shares the observation database.
fn job_db(context: &HermeticTestContext) -> PathBuf {
    homeboy_core::paths::observation_db_in_root(&context.data_dir())
}

fn intents(context: &HermeticTestContext) -> Vec<Value> {
    sql_rows(
        &homeboy_core::paths::observation_db_in_root(&context.data_dir()),
        "SELECT json_object('id',id,'state',state,'payload',json(payload_json),'receipt',json(receipt_json)) FROM control_plane_work_intents WHERE kind = 'agent-task-execution-step' ORDER BY id",
    )
}

fn jobs(context: &HermeticTestContext) -> Vec<Value> {
    sql_rows(
        &job_db(context),
        "SELECT body FROM daemon_jobs ORDER BY created_at_ms",
    )
}

fn deliveries(context: &HermeticTestContext) -> Vec<Value> {
    sql_rows(&job_db(context), "SELECT json_object('job_id',job_id,'receipt',json(receipt),'payload',payload,'attempts',attempts,'last_error',last_error) FROM step_terminal_deliveries")
}

fn evidence(context: &HermeticTestContext) -> String {
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let mut status = command(context);
    status.args(["daemon", "status"]);
    let status = bounded_output(status);
    format!(
        "daemon status={}{}\nrecord={:?}\nintents={}\njobs={}\ndeliveries={}\ndaemon stderr={}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr),
        store.read_record(COOK),
        json!(intents(context)),
        json!(jobs(context)),
        json!(deliveries(context)),
        std::fs::read_to_string(context.root().join("daemon.stderr")).unwrap_or_default(),
    )
}

fn wait(context: &HermeticTestContext, what: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(40);
    while !predicate() {
        assert!(Instant::now() < deadline, "{what}\n{}", evidence(context));
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn release(context: &HermeticTestContext, gate: &str) {
    std::fs::write(context.root().join(gate).with_extension("release"), "").unwrap();
}

fn signal(pid: i32) {
    // SAFETY: exact fixture PID, or its explicitly isolated process group.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0, "kill {pid}");
}

fn crash_case(fault: Fault) {
    let context = HermeticTestContext::new();
    let (_repo_guard, checkout) =
        homeboy_core::test_support::shared_committed_git_repo_fixture("execution-chaos");
    std::fs::create_dir_all(checkout.join("docs")).unwrap();
    std::fs::write(checkout.join("docs/agent-task-smoke.md"), "before\n").unwrap();
    homeboy_core::test_support::run_git_fixture_command(&checkout, &["add", "."]);
    homeboy_core::test_support::run_git_fixture_command(&checkout, &["commit", "-m", "seed patch"]);
    run(
        &context,
        &[
            "component",
            "create",
            "--local-path",
            checkout.to_str().unwrap(),
        ],
    );
    run(
        &context,
        &[
            "worktree",
            "create",
            "execution-chaos",
            "--branch",
            "task",
            "--from",
            "HEAD",
        ],
    );
    let worktree = checkout.parent().unwrap().join("execution-chaos@task");
    std::fs::write(
        context.config_dir().join("homeboy.json"),
        r#"{"retention":{"reconstructable_artifact_reserve_bytes":0}}"#,
    )
    .unwrap();

    let _teardown = Teardown { context: &context };
    let daemon = start_daemon(&context, true);
    wait(&context, "daemon deliverer is gated", || {
        context.root().join("delivery").exists()
    });

    let ledger = context.root().join("provider-executions");
    let mut admit = command(&context);
    admit
        .env(
            "HOMEBOY_TEST_EXECUTION_STEP_GATE",
            context.root().join("child"),
        )
        .env("HOMEBOY_FIXTURE_PROVIDER_EXECUTIONS_FILE", &ledger)
        .args([
            "--placement",
            "lab",
            "agent-task",
            "cook",
            "--run-id",
            COOK,
            "--repo",
            "execution-chaos",
            "--backend",
            "fixture",
            "--model",
            "fixture-model",
            "--prompt",
            "make the deterministic fixture patch",
            "--cwd",
            worktree.to_str().unwrap(),
            "--to-worktree",
            worktree.to_str().unwrap(),
            "--verify",
            "true",
            "--max-attempts",
            "1",
            "--no-finalize",
        ]);
    let output = bounded_output(admit);
    assert!(
        output.status.success(),
        "admit: {} {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        evidence(&context)
    );
    run(
        &context,
        &[
            "--placement",
            "local",
            "agent-task",
            "placement-update",
            COOK,
            "--confirm",
        ],
    );
    let committed = intents(&context);
    assert_eq!(
        committed.len(),
        2,
        "Lab admission and local replacement\n{}",
        evidence(&context)
    );
    assert!(committed.iter().all(|row| row["state"] == "pending"));
    assert!(
        jobs(&context).is_empty(),
        "nothing submitted before delivery"
    );
    assert!(!ledger.exists(), "no provider work before delivery");

    let mut held_child = None;
    if matches!(fault, Fault::BeforeDelivery) {
        signal(daemon);
        wait(&context, "killed daemon is gone", || !alive(daemon));
        release(&context, "delivery");
        release(&context, "child");
        start_daemon(&context, false);
    } else {
        release(&context, "delivery");
        wait(
            &context,
            "exact execution-step child held before publication",
            || context.root().join("child").exists(),
        );
        let child: i32 = std::fs::read_to_string(context.root().join("child"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            !ledger.exists(),
            "held child has not executed provider work"
        );
        match fault {
            Fault::Child => {
                signal(child);
                wait(
                    &context,
                    "consumer commits exactly one recovery failure",
                    || {
                        let store = AgentTaskLifecycleStore::new(context.path_roots());
                        store.read_record(COOK).unwrap().metadata["execution_scheduling"]
                            ["recovery_failures"]
                            == 1
                    },
                );
                release(&context, "child");
            }
            Fault::Group => {
                // Step sessions intentionally survive the daemon's process
                // group. Restart must adopt, not duplicate, that exact child.
                // SAFETY: getpgid only reads process-group membership.
                let group = unsafe { libc::getpgid(daemon) };
                assert!(
                    group > 1 && group != unsafe { libc::getpgid(0) },
                    "isolated daemon group"
                );
                signal(-group);
                wait(&context, "killed daemon group is gone", || !alive(daemon));
                assert!(
                    alive(child),
                    "the held step child is outside the daemon group"
                );
                start_daemon(&context, false);
                release(&context, "child");
                held_child = Some(child);
            }
            Fault::Stop => {
                run(&context, &["daemon", "stop"]);
                wait(&context, "stopped daemon is gone", || !alive(daemon));
                assert!(
                    alive(child),
                    "daemon stop leaves the held step child running"
                );
                start_daemon(&context, false);
                release(&context, "child");
                held_child = Some(child);
            }
            Fault::BeforeDelivery => unreachable!(),
        }
    }

    let store = AgentTaskLifecycleStore::new(context.path_roots());
    wait(
        &context,
        "Cook and both durable delivery stores settle",
        || {
            let Ok(index) = store.read_cook_index(COOK) else {
                return false;
            };
            let Ok(record) = store.read_record(&index.latest_run_id) else {
                return false;
            };
            record.state().is_terminal()
                && intents(&context)
                    .iter()
                    .all(|row| row["state"] == "submitted")
                && !deliveries(&context).is_empty()
                && deliveries(&context)
                    .iter()
                    .all(|row| row["receipt"].is_object())
        },
    );
    let index = store.read_cook_index(COOK).unwrap();
    let attempt = store.read_record(&index.latest_run_id).unwrap();
    assert_eq!(
        serde_json::to_value(attempt.state()).unwrap(),
        "succeeded",
        "{}",
        evidence(&context)
    );
    assert_eq!(
        index.attempts.len(),
        1,
        "one initial Cook attempt: {index:?}"
    );
    let executions = std::fs::read_to_string(&ledger).unwrap();
    assert_eq!(
        executions.lines().count(),
        1,
        "provider ledger: {executions}\n{}",
        evidence(&context)
    );
    let parent = store.read_record(COOK).unwrap();
    assert_eq!(
        serde_json::to_value(parent.state()).unwrap(),
        "succeeded",
        "{}",
        evidence(&context)
    );
    if let Some(child) = held_child {
        // The exact child held across the fault is the one that published.
        assert_eq!(
            parent.metadata["execution_step_owner"]["pid"],
            child,
            "{}",
            evidence(&context)
        );
    }
    assert_eq!(
        parent.metadata["execution_scheduling"]["recovery_failures"],
        u64::from(matches!(fault, Fault::Child)),
        "{}",
        evidence(&context)
    );
    let submissions = sql_rows(&job_db(&context), "SELECT json_object('key',key,'job_id',job_id) FROM daemon_job_submissions WHERE kind = 'controller' ORDER BY key");
    let execution_intents = intents(&context);
    assert_eq!(
        jobs(&context).len(),
        execution_intents.len(),
        "no unkeyed step job\n{}",
        evidence(&context)
    );
    for intent in &execution_intents {
        let matching: Vec<_> = submissions
            .iter()
            .filter(|row| row["key"] == intent["id"])
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "one keyed job per intent\n{}\nsubmissions={submissions:?}",
            evidence(&context)
        );
        assert_eq!(intent["receipt"]["job_id"], matching[0]["job_id"]);
    }
    if matches!(fault, Fault::Child) {
        // The consumer classified the SIGKILLed child as a crash (signal
        // exit, no publication) of the exact local admission step.
        let failure = parent.metadata["execution_step_failure"]["message"]
            .as_str()
            .unwrap_or_default();
        assert!(
            failure.contains("agent-task-exec:execution-chaos:2:0 ended before publication")
                && failure.contains("exit None"),
            "{failure}\n{}",
            evidence(&context)
        );
        let recovered: Vec<_> = deliveries(&context)
            .into_iter()
            .filter(|row| row["receipt"]["disposition"] == "recovered")
            .collect();
        assert_eq!(
            recovered.len(),
            1,
            "one crash terminal consumed\n{}",
            evidence(&context)
        );
    }
}

#[test]
fn committed_execution_intent_survives_daemon_sigkill_before_delivery() {
    crash_case(Fault::BeforeDelivery);
}

#[test]
fn killed_execution_step_child_recovers_once() {
    crash_case(Fault::Child);
}

#[test]
fn execution_step_survives_daemon_process_group_sigkill() {
    crash_case(Fault::Group);
}

#[test]
fn execution_step_survives_daemon_stop_start() {
    crash_case(Fault::Stop);
}
