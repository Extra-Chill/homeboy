use std::process::Output;

use homeboy_core::test_support::{HermeticTestContext, TestBinary};

/// Run the fixture binary through the shared hermetic harness.
///
/// Every case in this file asserts that a contradictory invocation is rejected
/// during argument validation, *before* any worktree or provider resolution.
/// `route_after_parse` returns early when it detects a Lab-offload or
/// runner-hosted execution context, and that bail-out sits above the validation
/// under test. A single inherited controller-transport variable therefore skips
/// the rejection silently and lets cook proceed into real worktree work, which
/// can block forever. Because `cargo test` runs integration binaries serially,
/// one blocked binary strands every binary ordered after it (#10917).
///
/// `HermeticTestContext::command` owns the complete isolation contract — HOME,
/// XDG config and data roots, artifact root, runtime and temp dirs — and strips
/// the Lab transport variables. Overriding only HOME on a hand-rolled `Command`
/// leaves that leak open (#10717, #10718).
fn homeboy(args: &[&str]) -> Output {
    let context = HermeticTestContext::new();
    context
        .command(TestBinary::HomeboyFixture)
        .args(args)
        .output()
        .expect("run homeboy")
}

#[test]
fn cook_handoff_subprocesses_do_not_inherit_controller_transport() {
    // Pins the isolation the rejections below depend on. If these fixtures ever
    // stop stripping the controller transport variables, the routing bail-out
    // above `run_split_placement_cook` swallows every rejection this file
    // asserts, and the suite hangs instead of failing (#10917).
    let context = HermeticTestContext::new();
    let command = context.command(TestBinary::HomeboyFixture);
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

#[test]
fn cook_rejects_local_detach_before_worktree_resolution() {
    let output = homeboy(&[
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

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cannot detach after handoff with --placement local"));
    assert!(!stdout.contains("worktree provider"));
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
fn cook_help_does_not_advertise_queue_only() {
    let output = homeboy(&["agent-task", "cook", "--help"]);

    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(!help.contains("\n      --queue-only\n"));
    assert!(help.contains("--detach-after-handoff"), "{help}");
    assert!(help.contains("--wait"), "{help}");
}
