use super::{
    failure_fingerprint, run_gate_command_with_timeout, AgentTaskGateEnvironmentPolicy,
    AgentTaskGateRevealPolicy, AgentTaskGateStatus, AgentTaskGateVisibility,
};
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[test]
fn javascript_run_totals_and_failure_durations_are_normalized() {
    for (base, candidate) in [
        (
            "Test Files 2 failed | 81 passed (83)",
            "Test Files 2 failed | 82 passed (84)",
        ),
        (
            "Tests 2 failed | 1204 passed (1206)",
            "Tests 2 failed | 1205 passed (1207)",
        ),
    ] {
        assert_eq!(
            super::normalize_failure_record(base),
            super::normalize_failure_record(candidate)
        );
    }
    for marker in ["×", "✕"] {
        assert_eq!(
            super::failure_records(&format!("{marker} keeps hero fluid (12 ms)"), ""),
            vec!["test:keeps hero fluid"],
        );
    }
}

#[test]
fn matching_baseline_failure_is_distinct_from_a_new_failure() {
    let baseline = failure_fingerprint(1, "test alpha ... FAILED\n", "", &[]);
    let matching_candidate = failure_fingerprint(1, "test alpha ... FAILED\n", "", &[]);
    let regressed_candidate =
        failure_fingerprint(1, "test alpha ... FAILED\ntest beta ... FAILED\n", "", &[]);

    assert_eq!(baseline, matching_candidate);
    assert_ne!(baseline, regressed_candidate);
}

#[test]
fn failure_fingerprint_distinguishes_exit_codes_and_ignores_volatile_reordered_output() {
    let baseline = failure_fingerprint(
        1,
        "error E42 at 2026-08-04T12:00:00Z\nworker 0xabc failed\n",
        "",
        &[],
    );
    let reordered = failure_fingerprint(
        1,
        "worker 0xdef failed\nerror E42 at 2027-01-01T00:00:00Z\n",
        "",
        &[],
    );
    let divergent_exit = failure_fingerprint(
        2,
        "worker 0xdef failed\nerror E42 at 2027-01-01T00:00:00Z\n",
        "",
        &[],
    );

    assert_eq!(baseline, reordered);
    assert_ne!(baseline, divergent_exit);
}

fn cargo_failure_output(
    root: &str,
    tmp: &str,
    thread_id: u32,
    seconds: &str,
    filtered: u32,
    hash: &str,
    failing: &[&str],
) -> (String, String) {
    let mut stdout = String::from("\nrunning 724 tests\n");
    for name in failing {
        stdout.push_str(&format!("test {name} ... FAILED\n"));
    }
    stdout.push_str("test agent_task_gate::ok_one ... ok\n\nfailures:\n\n");
    for name in failing {
        stdout.push_str(&format!(
                "---- {name} stdout ----\n\nthread '{name}' ({thread_id}) panicked at crates/homeboy-agents/src/agent_task_gate.rs:120:9:\nfixture missing at {tmp}/fixture.json\nnote: run with `RUST_BACKTRACE=1`\n\n"
            ));
    }
    stdout.push_str("failures:\n");
    for name in failing {
        stdout.push_str(&format!("    {name}\n"));
    }
    stdout.push_str(&format!(
            "\ntest result: FAILED. 720 passed; {} failed; 1 ignored; 0 measured; {filtered} filtered out; finished in {seconds}\n",
            failing.len()
        ));
    let stderr = format!(
            "   Compiling homeboy-agents v0.1.0 ({root}/crates/homeboy-agents)\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 2m 03s\n     Running unittests src/lib.rs ({root}/target/debug/deps/homeboy_agents-{hash})\nerror: test failed, to rerun pass `-p homeboy-agents --lib`\n"
        );
    (stdout, stderr)
}

const FOUR_FAILURES: &[&str] = &[
    "agent_task_gate::tests::alpha",
    "agent_task_gate::tests::beta",
    "agent_task_gate::tests::gamma",
    "agent_task_gate::tests::delta",
];

#[test]
fn same_cargo_failure_from_different_workspaces_has_equal_fingerprints() {
    let (base_out, base_err) = cargo_failure_output(
        "/home/chubes/Developer/_lab_workspaces/abc123",
        "/tmp/.homeboy-test-tmp/run.RdkCCB/.tmpdIljrU",
        1175443,
        "113.75s",
        1889,
        "0f3a9b2c4d5e6f70",
        FOUR_FAILURES,
    );
    let (cand_out, cand_err) = cargo_failure_output(
        "/home/other/work/_lab_workspaces/zz-999",
        "/tmp/.homeboy-test-tmp/run.Xy12ab/.tmpQQ99zz",
        42,
        "7.01s",
        12,
        "1234567890abcdef",
        FOUR_FAILURES,
    );
    assert_eq!(
        failure_fingerprint(101, &base_out, &base_err, &[]),
        failure_fingerprint(101, &cand_out, &cand_err, &[])
    );
}

#[test]
fn extra_failing_test_changes_the_fingerprint() {
    let (base_out, base_err) = cargo_failure_output(
        "/home/chubes/Developer/_lab_workspaces/abc123",
        "/tmp/.homeboy-test-tmp/run.RdkCCB/.tmpdIljrU",
        1175443,
        "113.75s",
        1889,
        "0f3a9b2c4d5e6f70",
        FOUR_FAILURES,
    );
    let mut five = FOUR_FAILURES.to_vec();
    five.push("agent_task_gate::tests::epsilon");
    let (cand_out, cand_err) = cargo_failure_output(
        "/home/other/work/_lab_workspaces/zz-999",
        "/tmp/.homeboy-test-tmp/run.Xy12ab/.tmpQQ99zz",
        42,
        "7.01s",
        12,
        "1234567890abcdef",
        &five,
    );
    assert_ne!(
        failure_fingerprint(101, &base_out, &base_err, &[]),
        failure_fingerprint(101, &cand_out, &cand_err, &[])
    );
}

fn clippy_output(root: &str, seconds: &str, line: u32) -> String {
    format!(
            "    Checking homeboy-agents v0.1.0 ({root}/crates/homeboy-agents)\nerror: this `if` has identical blocks\n  --> {root}/crates/homeboy-agents/src/agent_task_gate.rs:{line}:5\n   |\n{line} |     if x {{ a }} else {{ a }}\n   |\nerror[E0425]: cannot find value `foo` in this scope\n  --> crates/homeboy-agents/src/lib.rs:10:1\nerror: could not compile `homeboy-agents` (lib) due to 2 previous errors\n    Finished in {seconds}\n"
        )
}

#[test]
fn same_clippy_error_from_different_workspaces_has_equal_fingerprints() {
    let base = clippy_output("/home/chubes/Developer/_lab_workspaces/abc123", "3.2s", 77);
    let candidate = clippy_output("/srv/ci/workspace", "12.9s", 77);
    let moved = clippy_output("/srv/ci/workspace", "12.9s", 78);
    assert_eq!(
        failure_fingerprint(101, "", &base, &[]),
        failure_fingerprint(101, "", &candidate, &[])
    );
    assert_ne!(
        failure_fingerprint(101, "", &base, &[]),
        failure_fingerprint(101, "", &moved, &[])
    );
}

#[test]
fn same_failure_with_different_exit_code_has_different_fingerprint() {
    let (out, err) = cargo_failure_output(
        "/home/chubes/Developer/_lab_workspaces/abc123",
        "/tmp/.homeboy-test-tmp/run.RdkCCB/.tmpdIljrU",
        1175443,
        "113.75s",
        1889,
        "0f3a9b2c4d5e6f70",
        FOUR_FAILURES,
    );
    assert_ne!(
        failure_fingerprint(101, &out, &err, &[]),
        failure_fingerprint(1, &out, &err, &[])
    );
}

#[test]
fn normalization_masks_volatile_tokens() {
    let line = super::normalize_failure_record(
            "thread 'x' (1175443) panicked at /home/a/_lab_workspaces/id/src/lib.rs:3:1 in 2m 03s (target/debug/deps/foo-0f3a9b2c4d5e6f70)",
        )
        .expect("kept");
    assert_eq!(
        line,
        "thread 'x' panicked at src/lib.rs:3:1 in <duration> (target/debug/deps/foo-<hash>)"
    );
    assert!(super::normalize_failure_record("   Compiling foo v1.2.3").is_none());
}

#[test]
fn bounded_baseline_gate_is_cancelled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let report = run_gate_command_with_timeout(
        temp.path(),
        1,
        "sleep 1",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        temp.path(),
        Duration::from_millis(20),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
    .expect("bounded gate report");

    assert_eq!(report.status, AgentTaskGateStatus::Failed);
    assert_eq!(report.exit_code, 124);
    assert!(report.stderr.contains("was cancelled"));
}

#[cfg(unix)]
#[test]
fn bounded_baseline_gate_reaps_background_descendants_before_reader_join() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pid_file = temp.path().join("descendant.pid");
    let report = run_gate_command_with_timeout(
        temp.path(),
        1,
        &format!(
            "sh -c 'trap \"\" TERM; while :; do sleep 1; done' & echo $! > '{}'; wait",
            pid_file.display()
        ),
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        temp.path(),
        Duration::from_millis(20),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
    .expect("bounded gate report");

    assert_eq!(report.exit_code, 124);
    let descendant_pid = std::fs::read_to_string(pid_file)
        .expect("descendant pid")
        .trim()
        .parse::<libc::pid_t>()
        .expect("numeric descendant pid");
    assert!(
        !homeboy_engine_primitives::command::process_is_running(descendant_pid as u32),
        "bounded baseline gate left descendant {descendant_pid} runnable"
    );
}

/// Re-exec target for [`killing_the_gate_controller_reaps_its_background_descendant`].
/// Runs a deterministic gate whose command backgrounds a SIGTERM-ignoring
/// descendant and then blocks on it, standing in for an agent process that
/// is mid-gate (e.g. running `cargo test -p ... --lib`) when it is killed.
/// Only runs under the env var the parent test sets; a bare `cargo test`
/// invocation of the full suite exercises this as a no-op.
#[cfg(unix)]
#[test]
fn gate_process_containment_worker() {
    let Ok(dir) = std::env::var("HOMEBOY_GATE_CONTAINMENT_WORKER_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let pid_file = dir.join("descendant.pid");
    let _ = run_gate_command_with_timeout(
        &dir,
        1,
        &format!(
            "sh -c 'trap \"\" TERM; while :; do sleep 1; done' & echo $! > '{}'; wait",
            pid_file.display()
        ),
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        &dir,
        Duration::from_secs(30),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    );
}

/// The #13649 regression: an agent gets killed (operator `kill -9`, OOM,
/// supervisor teardown) while a deterministic gate it launched is still
/// running. Before containment, the gate's `sh -lc` leader and whatever
/// build/test tooling it spawned were reparented to init and kept running
/// — and kept holding the shared cargo build lock — because nothing was
/// left alive to signal them. This drives that exact scenario through a
/// real OS process (not just an in-process drop) so the assertion is that
/// the reported orphan survives or does not, not that a particular type
/// gets constructed.
#[cfg(unix)]
#[test]
fn killing_the_gate_controller_reaps_its_background_descendant() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pid_file = temp.path().join("descendant.pid");
    let test_binary = std::env::current_exe().expect("test binary path");
    let mut worker = Command::new(&test_binary)
        .args([
            "--exact",
            "agent_task_gate::baseline_tests::gate_process_containment_worker",
        ])
        .env(
            "HOMEBOY_GATE_CONTAINMENT_WORKER_DIR",
            temp.path().as_os_str(),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start gate containment worker");

    let start_deadline = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() {
        assert!(
            Instant::now() < start_deadline,
            "gate containment worker never started its background descendant"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let descendant_pid = std::fs::read_to_string(&pid_file)
        .expect("descendant pid")
        .trim()
        .parse::<libc::pid_t>()
        .expect("numeric descendant pid");
    assert!(
        homeboy_engine_primitives::command::process_is_running(descendant_pid as u32),
        "descendant must still be running for this test to prove anything"
    );

    // The kill an operator or OOM issues: it lands on the agent process
    // (here, the worker), not on the gate's descendants. The worker gets
    // no chance to run its own cleanup — that is the entire point.
    worker.kill().expect("kill gate containment worker");
    worker.wait().expect("reap gate containment worker");

    let reaped_deadline = Instant::now() + Duration::from_secs(10);
    while homeboy_engine_primitives::command::process_is_running(descendant_pid as u32) {
        assert!(
            Instant::now() < reaped_deadline,
            "descendant {descendant_pid} survived its controller being killed"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
