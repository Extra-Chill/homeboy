use homeboy::core::test_support::{HermeticTestContext, TestBinary};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn refresh_emits_admitted_run_before_slow_probe_and_keeps_final_artifacts_on_that_run() {
    let context = HermeticTestContext::new();
    let workspace = context.root().join("runner-workspace");
    fs::create_dir_all(&workspace).expect("runner workspace");

    let add = context
        .command(TestBinary::HomeboyFixture)
        .args([
            "runner",
            "add",
            "refresh-fixture",
            "--kind",
            "local",
            "--workspace-root",
        ])
        .arg(&workspace)
        .output()
        .expect("add local runner");
    assert!(
        add.status.success(),
        "add stderr: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let binary = context.root().join("slow-homeboy");
    fs::write(
        &binary,
        "#!/bin/sh\nsleep 2\nprintf '%s\\n' '{\"data\":{\"version\":\"0.383.10\",\"display\":\"homeboy 0.383.10+fixture\",\"git_commit\":\"fixture\",\"git_dirty\":false}}'\n",
    )
    .expect("write slow identity fixture");
    let mut permissions = fs::metadata(&binary)
        .expect("fixture metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions).expect("make identity fixture executable");
    }

    let mut command = context.command(TestBinary::HomeboyFixture);
    command.args([
        "runner",
        "refresh-homeboy",
        "refresh-fixture",
        "--allow-downgrade",
        "--select",
    ]);
    command.arg(&binary);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().expect("start refresh process");
    let stderr = child.stderr.take().expect("refresh stderr");
    let (progress_tx, progress_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stderr).lines();
        if let Some(Ok(line)) = lines.next() {
            let _ = progress_tx.send(line);
        }
    });

    let progress = progress_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("refresh admission progress before slow identity probe");
    assert!(progress.starts_with("HOMEBOY_REFRESH_PROGRESS "));
    let progress: Value = serde_json::from_str(
        progress
            .strip_prefix("HOMEBOY_REFRESH_PROGRESS ")
            .expect("progress prefix"),
    )
    .expect("progress JSON");
    let run_id = progress["run_id"]
        .as_str()
        .expect("admitted run id")
        .to_string();
    assert_eq!(progress["phase"], "select");

    let output = child.wait_with_output().expect("collect refresh output");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("refresh JSON envelope");
    assert_eq!(
        output.status.code(),
        Some(0),
        "refresh stdout: {}\nrefresh stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(envelope["success"], true);
    assert_eq!(envelope["data"]["artifacts"]["run_id"], run_id);

    let artifacts = context
        .command(TestBinary::HomeboyFixture)
        .args(["runs", "refs", "--kind", "runner_refresh_homeboy"])
        .output()
        .expect("inspect refresh artifacts");
    assert!(
        artifacts.status.success(),
        "artifact stderr: {}",
        String::from_utf8_lossy(&artifacts.stderr)
    );
    let artifacts: Value = serde_json::from_slice(&artifacts.stdout).expect("artifacts JSON");
    assert_eq!(artifacts["success"], true);
    assert!(
        artifacts["data"]["payload"]["artifacts"]
            .as_array()
            .is_some_and(|items| { items.iter().any(|artifact| artifact["run_id"] == run_id) }),
        "refresh artifact refs: {artifacts}"
    );
}
