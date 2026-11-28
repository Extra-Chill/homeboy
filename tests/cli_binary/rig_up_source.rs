use std::fs;
use std::path::Path;
use std::process::Output;

use homeboy::core::test_support::{HermeticTestContext, TestBinary};
use serde_json::json;

#[test]
fn same_id_packages_run_from_the_selected_source_without_reinstalling() {
    let context = HermeticTestContext::new();
    let first = context.root().join("worktree-a");
    let second = context.root().join("worktree-b");
    write_package(&first, "a");
    write_package(&second, "b");

    assert_success(rig(&context, &["install", "worktree-b"]));
    assert_success(rig(
        &context,
        &["up", "source-fixture", "--source", "worktree-a"],
    ));
    assert_eq!(fs::read_to_string(first.join("ran")).unwrap(), "a");
    assert!(!second.join("ran").exists());

    assert_success(rig(&context, &["up", "source-fixture"]));
    assert_eq!(fs::read_to_string(second.join("ran")).unwrap(), "b");
    fs::remove_file(second.join("ran")).unwrap();

    assert_success(rig(
        &context,
        &["up", "source-fixture", "--source", "worktree-b"],
    ));
    assert_eq!(fs::read_to_string(second.join("ran")).unwrap(), "b");
}

#[test]
fn explicit_source_does_not_fall_back_to_an_installed_rig() {
    let context = HermeticTestContext::new();
    let installed = context.root().join("installed");
    write_package(&installed, "installed");
    assert_success(rig(&context, &["install", "installed"]));

    let wrong_package = context.root().join("wrong-package");
    fs::create_dir(&wrong_package).unwrap();
    fs::write(wrong_package.join("rig.json"), r#"{"id":"another-rig"}"#).unwrap();
    for source in ["wrong-package", "missing-package"] {
        let output = rig(&context, &["up", "source-fixture", "--source", source]);
        assert!(
            !output.status.success(),
            "unexpected success for {source}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    assert!(!installed.join("ran").exists());
}

#[test]
fn explicit_source_rejects_runner_placement_and_dry_run() {
    let context = HermeticTestContext::new();
    let package = context.root().join("local");
    write_package(&package, "local");

    for incompatible in [
        &["--runner", "fixture-runner"][..],
        &["--placement", "lab"],
        &["--dry-run"],
    ] {
        let output = context
            .command(TestBinary::HomeboyFixture)
            .current_dir(context.root())
            .args(["rig", "up", "source-fixture", "--source", "local"])
            .args(incompatible)
            .output()
            .expect("run rig command");
        assert!(
            !output.status.success(),
            "unexpected success with {incompatible:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    assert!(!package.join("ran").exists());
}

fn write_package(root: &Path, marker: &str) {
    let rig_dir = root.join("rigs/source-fixture");
    fs::create_dir_all(&rig_dir).unwrap();
    let base = json!({
        "pipeline": {"up": [{
            "kind": "command",
            "cwd": "${package.root}",
            "command": format!("printf '%s' '{marker}' > ran"),
        }]},
    });
    fs::write(root.join("base.json"), base.to_string()).unwrap();
    fs::write(
        rig_dir.join("rig.json"),
        r#"{"id":"source-fixture","extends":"../../base.json"}"#,
    )
    .unwrap();
}

fn rig(context: &HermeticTestContext, args: &[&str]) -> Output {
    context
        .command(TestBinary::HomeboyFixture)
        .current_dir(context.root())
        .args(["--placement", "local", "rig"])
        .args(args)
        .output()
        .expect("run rig command")
}

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
