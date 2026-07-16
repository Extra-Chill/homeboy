use std::process::Command;

fn homeboy() -> Command {
    Command::new(env!("CARGO_BIN_EXE_homeboy"))
}

#[test]
fn cook_rejects_invalid_controller_transport_before_worktree_resolution() {
    let output = homeboy()
        .args([
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
        ])
        .output()
        .expect("run homeboy");

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--placement local cannot be combined with --runner"));
    assert!(!stdout.contains("worktree provider"));
}

#[test]
fn cook_rejects_queue_only_before_worktree_resolution() {
    let output = homeboy()
        .args([
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "missing@worktree",
            "--verify",
            "true",
            "--queue-only",
        ])
        .output()
        .expect("run homeboy");

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cannot queue its controller-owned lifecycle"));
    assert!(!stdout.contains("worktree provider"));
}

#[test]
fn cook_help_does_not_advertise_queue_only() {
    let output = homeboy()
        .args(["agent-task", "cook", "--help"])
        .output()
        .expect("run homeboy help");

    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\n      --queue-only\n"));
}
