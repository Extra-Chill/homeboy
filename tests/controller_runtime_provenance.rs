use std::process::Command;

use homeboy_core::test_support::{bounded_output, HermeticTestContext, TestBinary};

#[test]
fn foreign_worktree_never_becomes_controller_executable_provenance() {
    let context = HermeticTestContext::new();
    let foreign = tempfile::tempdir().expect("create foreign worktree");
    let foreign_origin = "https://example.test/foreign-task.git";
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(foreign.path())
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet"]);
    git(&["remote", "add", "origin", foreign_origin]);
    std::fs::write(foreign.path().join("task.txt"), "foreign task\n").expect("write task");
    git(&["add", "task.txt"]);
    git(&[
        "-c",
        "user.name=Homeboy Test",
        "-c",
        "user.email=homeboy@example.test",
        "commit",
        "--quiet",
        "-m",
        "foreign task",
    ]);
    let foreign_revision = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(foreign.path())
        .output()
        .expect("read foreign revision");
    let _foreign_revision = String::from_utf8(foreign_revision.stdout)
        .expect("foreign revision is UTF-8")
        .trim()
        .to_string();

    let plan = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/agent_task_smoke_plan.json");
    let mut run = context.command(TestBinary::HomeboyFixture);
    run.current_dir(foreign.path()).args([
        "--placement",
        "local",
        "agent-task",
        "run-plan",
        "--plan",
        &format!("@{}", plan.display()),
        "--record-run-id",
        "foreign-runtime-provenance",
    ]);
    // The fixture's typed-artifact contract fails the run in this bare
    // worktree; the durable record and its provenance are what this pins.
    let ran = bounded_output(run);
    assert!(
        ran.status.code().is_some(),
        "run-plan did not finish: {}",
        String::from_utf8_lossy(&ran.stderr)
    );

    // The CLI's `agent-task status` projection is a bounded operator summary:
    // it carries `runtime.build_identity` but no `metadata`, so the originating
    // source provenance this test exists to pin is not reachable through it at
    // all (the earlier `--full` flag it passed does not exist). Read the
    // durable lifecycle record instead, which is where the controller runtime
    // records that provenance and where the library-level coverage asserts it.
    let record =
        homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots())
            .read_record("foreign-runtime-provenance")
            .expect("durable lifecycle record for the recorded run");
    let runtime = &record.metadata["controller_runtime"];
    assert_eq!(
        runtime["originating"]["build_identity"],
        homeboy_core::build_identity::current().display
    );
    assert!(runtime["originating"]["pinned_executable"].is_null());
    assert!(!context.data_dir().join("controller-runtimes").exists());
}
