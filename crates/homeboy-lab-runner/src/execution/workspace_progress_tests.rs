use super::request::*;
use std::process::Command;
use std::time::{Duration, Instant};

// Run the real exec-request path in a fresh process so stderr and the deadline
// are observed at the invocation boundary, not through an in-process observer.
#[test]
fn stalled_workspace_transfer_reports_progress_and_deadline() {
    let started = Instant::now();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "execution::workspace_progress_tests::stalled_sync_child",
            "--nocapture",
        ])
        .env("HOMEBOY_STALLED_SYNC_CHILD", "1")
        .env(
            "HOMEBOY_LAB_ISOLATED_TEST",
            "execution::workspace_progress_tests::stalled_sync_child",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(started.elapsed() < Duration::from_secs(15));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("sync: scanning"), "{stderr}");
    assert!(stderr.contains("sync: transferring 0/"), "{stderr}");
    assert!(stderr.contains("active for"), "heartbeat missing: {stderr}");
    assert!(
        stderr.contains("transferring 0/") && stderr.contains("timed out"),
        "{stderr}"
    );
    assert!(!stderr.contains("sync: done"), "{stderr}");
}

#[test]
fn stalled_sync_child() {
    if std::env::var_os("HOMEBOY_STALLED_SYNC_CHILD").is_none() {
        return;
    }
    crate::test_support::with_isolated_home(|_| {
        use sha2::Digest;
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let bin = fixture.path().join("bin");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&bin).unwrap();
        // Larger than the pipe buffer: the second SSH call never reads stdin.
        let bytes = vec![b'x'; 8 * 1024 * 1024];
        let digest = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
        std::fs::write(source.join("payload"), bytes).unwrap();
        let marker = fixture.path().join("planned");
        let response = serde_json::json!({
            "lab_contract_version": homeboy_lab_contract::LAB_CONTRACT_VERSION,
            "available_bytes": 1_000_000_000_000u64,
            "available_inodes": 1_000_000_000u64,
            "total_bytes": 1_000_000_000_000u64,
            "tips": [[]], "missing": [digest],
        });
        homeboy_test_support::write_executable(bin.join("ssh"), format!(
            "#!/bin/sh\nif [ ! -f '{}' ]; then cat >/dev/null; touch '{}'; printf '%s' '{}'; else sleep 60; fi\n",
            marker.display(), marker.display(), response,
        ), 0o755).unwrap();
        homeboy_core::server::create(
            r#"{"id":"stalled-sync","host":"sync.invalid","user":"test"}"#,
            false,
        )
        .unwrap();
        crate::create(&serde_json::json!({"id":"stalled-sync", "kind":"ssh", "server_id":"stalled-sync", "workspace_root":fixture.path().join("runner"), "homeboy_path":"/runner/homeboy"}).to_string(), false).unwrap();
        let path = std::env::var("PATH").unwrap();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));
        let mut request =
            RunnerExecRequest::new("stalled-sync", super::RunnerExecOptions::default());
        request.options.command = vec!["true".into()];
        request.sync_workspace = Some(source.display().to_string());
        request.workspace_sync_timeout = Duration::from_secs(12);
        let started = Instant::now();
        let error = exec_request(request).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(13));
        assert_eq!(error.details["workspace_sync"]["timed_out"], true);
        assert!(error.message.contains("transferring 0/"), "{error:?}");
        assert!(
            error.details["workspace_sync"]["timeout_ms"]
                .as_u64()
                .unwrap()
                > 0
        );
        eprintln!("{}", error.message);
        std::env::set_var("PATH", path);
    });
}
