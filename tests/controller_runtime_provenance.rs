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
    let foreign_revision = String::from_utf8(foreign_revision.stdout)
        .expect("foreign revision is UTF-8")
        .trim()
        .to_string();

    let plan = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/agent_task_smoke_plan.json");
    let mut submit = context.controller_runtime_command(TestBinary::HomeboyFixture);
    submit.current_dir(foreign.path()).args([
        "agent-task",
        "submit",
        "--plan",
        &format!("@{}", plan.display()),
        "--run-id",
        "foreign-runtime-provenance",
    ]);
    let submitted = bounded_output(submit);
    assert!(
        submitted.status.success(),
        "submit failed: {}",
        String::from_utf8_lossy(&submitted.stderr)
    );

    let mut status = context.controller_runtime_command(TestBinary::HomeboyFixture);
    status.current_dir(foreign.path()).args([
        "agent-task",
        "status",
        "foreign-runtime-provenance",
        "--full",
    ]);
    let status = bounded_output(status);
    assert!(
        status.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status is structured JSON");
    let source = &status["data"]["metadata"]["controller_runtime"]["originating"]["source"];

    assert_eq!(
        source["repository"],
        "https://github.com/Extra-Chill/homeboy"
    );
    assert_eq!(source["verification"], "build_metadata");
    assert_ne!(source["repository"], foreign_origin);
    assert_ne!(source["revision"], foreign_revision);
    assert_ne!(source["verification"], "observed_from_process_cwd");
    assert!(
        status["data"]["metadata"]["controller_runtime"]["originating"]["sha256"]
            .as_str()
            .is_some_and(|digest| !digest.is_empty()),
        "immutable executable hash evidence is retained"
    );
    assert!(
        status["data"]["metadata"]["controller_runtime"]["originating"]["build_identity"]
            .as_str()
            .is_some_and(|identity| !identity.is_empty()),
        "build identity evidence is retained"
    );
}
