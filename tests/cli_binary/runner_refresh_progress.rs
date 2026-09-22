use homeboy::core::test_support::{HermeticTestContext, TestBinary};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn refresh_emits_early_heartbeat_and_reuses_terminal_ids_for_success_and_failure() {
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

    let success_binary = context.root().join("slow-homeboy");
    fs::write(
        &success_binary,
        "#!/bin/sh\nsleep 2\nprintf '%s\\n' '{\"data\":{\"version\":\"0.383.10\",\"display\":\"homeboy 0.383.10+fixture\",\"git_commit\":\"fixture\",\"git_dirty\":false}}'\n",
    )
    .expect("write slow identity fixture");
    make_executable(&success_binary);

    let (success, success_status, _) = run_refresh(&context, &success_binary);
    assert_eq!(success_status, Some(0));
    assert_eq!(success["success"], true);
    let success_run_id = terminal_run_id(&success);

    let failure_binary = context.root().join("slow-failing-homeboy");
    fs::write(&failure_binary, "#!/bin/sh\nsleep 2\nexit 23\n")
        .expect("write slow failure fixture");
    make_executable(&failure_binary);
    let (failure, failure_status, _) = run_refresh(&context, &failure_binary);
    assert_ne!(failure_status, Some(0));
    assert_eq!(failure["success"], false);
    let failure_run_id = terminal_run_id(&failure);
    assert_ne!(success_run_id, failure_run_id);

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
    let refs = artifacts["data"]["payload"]["artifacts"]
        .as_array()
        .expect("artifact refs");
    assert!(refs
        .iter()
        .any(|artifact| artifact["run_id"] == success_run_id));
    assert!(refs
        .iter()
        .any(|artifact| artifact["run_id"] == failure_run_id));
}

fn run_refresh(context: &HermeticTestContext, binary: &Path) -> (Value, Option<i32>, Vec<String>) {
    let mut command = context.command(TestBinary::HomeboyFixture);
    command.args([
        "runner",
        "refresh-homeboy",
        "refresh-fixture",
        "--allow-downgrade",
        "--select",
    ]);
    command.arg(binary);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().expect("start refresh process");
    let stderr = child.stderr.take().expect("refresh stderr");
    let (progress_tx, progress_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = progress_tx.send(line);
        }
    });

    let admission = progress_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("refresh admission before slow child completes");
    let heartbeat = progress_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("refresh heartbeat before slow child completes");
    let admission_json: Value = serde_json::from_str(
        admission
            .strip_prefix("HOMEBOY_REFRESH_PROGRESS ")
            .expect("progress prefix"),
    )
    .expect("progress JSON");
    assert_eq!(admission_json["phase"], "refresh");
    assert_eq!(admission_json["requested_mode"], "select");
    assert!(admission_json["run_id"].as_str().is_some());
    assert!(heartbeat.contains("\"heartbeat\":true"));

    let output = child.wait_with_output().expect("collect refresh output");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("refresh JSON envelope");
    let mut progress_lines = vec![admission, heartbeat];
    while let Ok(line) = progress_rx.try_recv() {
        progress_lines.push(line);
    }
    (envelope, output.status.code(), progress_lines)
}

fn terminal_run_id(envelope: &Value) -> String {
    envelope["data"]["artifacts"]["run_id"]
        .as_str()
        .expect("terminal run id")
        .to_string()
}

fn make_executable(binary: &Path) {
    let mut permissions = fs::metadata(binary)
        .expect("fixture metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        fs::set_permissions(binary, permissions).expect("make fixture executable");
    }
}
