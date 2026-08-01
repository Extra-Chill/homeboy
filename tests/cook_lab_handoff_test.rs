use std::process::Output;

use homeboy_core::test_support::{HermeticTestContext, TestBinary};

/// Run the fixture binary through the shared hermetic harness.
///
/// Every case in this file asserts that a contradictory invocation is rejected
/// during argument validation, *before* any worktree or provider resolution.
///
/// `HermeticTestContext::command` owns the complete isolation contract — HOME,
/// XDG config and data roots, artifact root, runtime and temp dirs — and strips
/// the Lab transport variables. Overriding only HOME on a hand-rolled `Command`
/// leaves that leak open (#10717, #10718). These cases run with a default
/// operator context; the transport variables are injected explicitly only by
/// the case that asserts they cannot change a validation outcome (#10917).
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
fn contradictory_cook_arguments_survive_controller_transport_context() {
    // The routing bail-outs skip *routing*, not validation. Injecting the exact
    // transport context that used to suppress these rejections must not change
    // the outcome: a contradictory flag combination is contradictory wherever
    // the process runs.
    //
    // Before #10917 this context sent both invocations past validation into
    // real worktree and provider work, where they blocked indefinitely instead
    // of failing fast.
    let cook = |args: &[&str]| {
        let context = HermeticTestContext::new();
        context
            .controller_runtime_command(TestBinary::HomeboyFixture)
            .env(
                homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV,
                r#"{"runner_id":"homeboy-lab"}"#,
            )
            .args(args)
            .output()
            .expect("run homeboy")
    };

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
        detach_stdout.contains("cannot detach after handoff with --placement local"),
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
