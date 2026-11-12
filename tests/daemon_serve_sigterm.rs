//! `daemon serve` is what a service manager runs (systemd `ExecStart=homeboy
//! daemon serve`, `Restart=always`). Its stop and restart only work if SIGTERM
//! ends the process; a daemon that catches SIGTERM and keeps running can only be
//! SIGKILLed (#17891).
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use homeboy_core::test_support::{HermeticTestContext, TestBinary};

/// Generous for a loaded CI host, far below a service manager's stop timeout.
const STARTUP_BUDGET: Duration = Duration::from_secs(60);

struct ServeProcess {
    child: Child,
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_serve(context: &HermeticTestContext, envs: &[(&str, String)]) -> ServeProcess {
    let mut command = context.command(TestBinary::HomeboyFixture);
    command
        .args(["daemon", "serve"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in envs {
        command.env(key, value);
    }
    ServeProcess {
        child: command.spawn().expect("spawn daemon serve"),
    }
}

/// Wait until the serving process published its lease, so SIGTERM reaches the
/// running serve loop rather than process startup.
fn wait_for_lease(context: &HermeticTestContext, serve: &mut ServeProcess) {
    let state_path = context.daemon_dir().join("state.json");
    let deadline = Instant::now() + STARTUP_BUDGET;
    loop {
        if let Some(status) = serve.child.try_wait().expect("poll daemon serve") {
            panic!("daemon serve exited before publishing its lease: {status}");
        }
        if lease_pid(&state_path) == Some(serve.child.id()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "daemon serve did not publish its lease within {STARTUP_BUDGET:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn lease_pid(state_path: &Path) -> Option<u32> {
    let body = fs::read(state_path).ok()?;
    let state: serde_json::Value = serde_json::from_slice(&body).ok()?;
    state["pid"].as_u64().map(|pid| pid as u32)
}

fn sigterm(serve: &ServeProcess) {
    let pid = serve.child.id() as libc::pid_t;
    assert_eq!(
        unsafe { libc::kill(pid, libc::SIGTERM) },
        0,
        "deliver SIGTERM"
    );
}

/// How long the process took to exit after SIGTERM, or `None` if it was still
/// running at `budget`.
fn wait_for_exit(serve: &mut ServeProcess, budget: Duration) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < budget {
        if serve.child.try_wait().expect("poll daemon serve").is_some() {
            return Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

#[test]
fn served_daemon_exits_on_sigterm_within_the_grace_period() {
    let context = HermeticTestContext::new();
    let mut serve = spawn_serve(&context, &[]);
    wait_for_lease(&context, &mut serve);

    sigterm(&serve);
    let elapsed = wait_for_exit(&mut serve, homeboy_core::daemon::SHUTDOWN_GRACE)
        .expect("daemon serve kept running after SIGTERM for the whole grace period");
    assert!(elapsed < homeboy_core::daemon::SHUTDOWN_GRACE);
    // A clean drain releases the idle lease.
    assert!(
        lease_pid(&context.daemon_dir().join("state.json")).is_none(),
        "the clean stop released its lease"
    );
}

/// The serving thread wedges after SIGTERM (here at a test-support gate that is
/// never released; in production a blocked dispatch or a held store lock). The
/// caught signal must still end the process at the shutdown deadline, with
/// durable evidence that the requested stop ended there.
#[test]
fn served_daemon_whose_drain_wedges_still_exits_at_the_shutdown_deadline() {
    let context = HermeticTestContext::new();
    let gate = context.root().join("shutdown-drain.gate");
    let grace = Duration::from_millis(1500);
    let mut serve = spawn_serve(
        &context,
        &[
            (
                "HOMEBOY_TEST_DAEMON_SHUTDOWN_DRAIN_GATE",
                gate.display().to_string(),
            ),
            (
                homeboy_core::daemon::SHUTDOWN_GRACE_MS_TEST_ENV,
                grace.as_millis().to_string(),
            ),
        ],
    );
    wait_for_lease(&context, &mut serve);
    let pid = serve.child.id();

    sigterm(&serve);
    // The drain reached the wedge: without the deadline it would sit here for
    // the gate's full 45s timeout.
    let reached = Instant::now() + Duration::from_secs(10);
    while !gate.exists() {
        assert!(
            Instant::now() < reached,
            "serve never reached the wedged drain"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let elapsed = wait_for_exit(&mut serve, grace + Duration::from_secs(10))
        .expect("a wedged drain kept the daemon alive past its shutdown deadline");
    assert!(
        !gate.with_extension("release").exists(),
        "the gate was never released; the deadline ended the process"
    );
    assert!(elapsed < grace + Duration::from_secs(10));

    let evidence: serde_json::Value = serde_json::from_slice(
        &fs::read(context.daemon_dir().join("termination.json")).expect("termination evidence"),
    )
    .expect("termination evidence json");
    assert_eq!(evidence["pid"].as_u64(), Some(u64::from(pid)));
    assert_eq!(evidence["stop_requested"], true);
    assert!(
        evidence["os_evidence"]
            .as_str()
            .is_some_and(|text| text.contains("shutdown deadline")),
        "{evidence}"
    );
    // The lease is kept so the next start's dead-lease recovery owns anything
    // the deadline abandoned.
    assert_eq!(
        lease_pid(&context.daemon_dir().join("state.json")),
        Some(pid)
    );
}
