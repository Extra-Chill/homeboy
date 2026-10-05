//! A foreground `homeboy daemon serve` is how external supervisors (systemd,
//! launchd, containers) run the daemon. It must stop cleanly on SIGTERM, and
//! a lease-bound stop must work against it even though it records no startup
//! token (#15436).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

fn homeboy(home: &Path) -> Command {
    let mut command = Command::new(homeboy_bin());
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1");
    command
}

fn daemon_status(home: &Path) -> serde_json::Value {
    let output_path = home.join("status.json");
    let _ = std::fs::remove_file(&output_path);
    let _ = homeboy(home)
        .args(["daemon", "status", "--output"])
        .arg(&output_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    std::fs::read(&output_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn serve(home: &Path) -> (Child, String) {
    let child = homeboy(home)
        .args(["daemon", "serve", "--addr", "127.0.0.1:0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon serve");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status = daemon_status(home);
        if status["data"]["running"] == true {
            if let Some(lease) = status["data"]["daemon"]["lease_id"].as_str() {
                return (child, lease.to_string());
            }
        }
        assert!(
            Instant::now() < deadline,
            "daemon serve did not come up: {status}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn wait_exit(child: &mut Child, bound: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("poll daemon") {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

#[cfg(unix)]
#[test]
fn sigterm_stops_a_foreground_daemon_cleanly_and_releases_its_lease() {
    let home = tempfile::tempdir().expect("temporary home");
    let (mut child, _lease) = serve(home.path());

    // SAFETY: signalling the child we spawned.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let status = wait_exit(&mut child, Duration::from_secs(30)).unwrap_or_else(|| {
        let _ = child.kill();
        panic!("daemon serve ignored SIGTERM");
    });
    assert!(
        status.success(),
        "graceful SIGTERM exit should be 0: {status}"
    );

    let daemon_dir = home.path().join(".config/homeboy/daemon");
    assert!(
        !daemon_dir.join("state.json").exists(),
        "a clean stop releases the lease"
    );
    let evidence: serde_json::Value = serde_json::from_slice(
        &std::fs::read(daemon_dir.join("termination.json")).expect("termination evidence"),
    )
    .expect("termination evidence JSON");
    assert_eq!(evidence["classification"], "clean_stop", "{evidence}");
}

#[cfg(unix)]
#[test]
fn lease_bound_stop_stops_a_tokenless_foreground_daemon() {
    let home = tempfile::tempdir().expect("temporary home");
    let (mut child, lease) = serve(home.path());

    let output = homeboy(home.path())
        .args(["daemon", "stop", "--lease-id", &lease])
        .output()
        .expect("run daemon stop");
    let stopped = wait_exit(&mut child, Duration::from_secs(30));
    if stopped.is_none() {
        let _ = child.kill();
    }
    assert!(
        output.status.success(),
        "lease-bound stop refused a foreground daemon: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stopped.is_some(),
        "foreground daemon did not exit after a lease-bound stop"
    );
}
