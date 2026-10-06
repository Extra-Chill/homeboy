//! Daemon lifecycle chaos suite (#15553, slice #15555).
//!
//! Each case replays a real incident against the real binary in a hermetic
//! HOME. A case passes only if Homeboy converges on its own: no hand-edited
//! state files and no signals beyond the scenario's own fault.
//!
//! | # | Incident | Where it is covered |
//! |---|----------|---------------------|
//! | 1 | SIGKILL mid checkpointed in-daemon job resumes (#15420) | in-process: `checkpointed_in_daemon_staging_dispatch_is_preserved_for_resume_after_dead_lease` |
//! | 2 | SIGKILL mid uncheckpointed job, one attestation terminalizes (#15556) | in-process: `uncheckpointed_in_daemon_staging_dispatch_still_blocks_dead_lease_recovery` (automatic refusal, then one attestation) |
//! | 3 | Binary replaced under an idle daemon converges (#15403) | `replaced_binary_under_an_idle_daemon_converges` |
//! | 4 | Binary replaced under a busy daemon waits for work | in-process: `daemon::lifetime` tests (`replaced_and_settled`) |
//! | 5 | Foreground `daemon serve`: SIGTERM and stop-by-lease (#15436) | `daemon_serve_lifecycle.rs` |
//! | 6 | Accepted stop with a helper mid-pass exits in bound (#15443) | in-process: `helper_drain_tests` |
//! | 7 | Restart in place over a dead admission owner (#15456) | `restart_in_place_takes_admission_from_a_dead_generation` |
//! | 8 | Dead remote generation with a stale job reconciles (#15556) | in-process: `connection_dead_lease_attestation` tests (`runner reconcile --confirm-workload-processes-absent`) |
//! | 9 | Host-service teardown (whole process group SIGKILL) recovers | `whole_daemon_process_group_killed_recovers_without_edits` |

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

fn homeboy_at(bin: &Path, home: &Path) -> Command {
    let mut command = Command::new(bin);
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1");
    command
}

fn json_output(bin: &Path, home: &Path, args: &[&str]) -> serde_json::Value {
    let output_path = home.join(format!("chaos-{}.json", uuid::Uuid::new_v4()));
    let _ = homeboy_at(bin, home)
        .args(args)
        .arg("--output")
        .arg(&output_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let value = std::fs::read(&output_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(serde_json::Value::Null);
    let _ = std::fs::remove_file(&output_path);
    value
}

fn status(bin: &Path, home: &Path) -> serde_json::Value {
    json_output(bin, home, &["daemon", "status"])
}

fn pid_alive(pid: i64) -> bool {
    // SAFETY: signal 0 only checks existence.
    if pid <= 0 || unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        return false;
    }
    // The test binary is a child subreaper, so killed daemons linger as
    // zombies until it reaps them. A zombie is not running.
    !std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit(')')
                .next()
                .map(|rest| rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

/// Wait until `predicate` holds for the daemon status, or panic with the last
/// status seen.
fn wait_status(
    bin: &Path,
    home: &Path,
    bound: Duration,
    what: &str,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Instant::now() + bound;
    loop {
        let current = status(bin, home);
        if predicate(&current["data"]) {
            return current;
        }
        assert!(Instant::now() < deadline, "{what}; last status: {current}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn serving_and_admitting(data: &serde_json::Value) -> bool {
    data["running"] == true && data["admits_work"] == true && data["fresh"] == true
}

/// Kills every daemon process a test started, even when an assertion fails.
#[derive(Default)]
struct Reaper {
    pids: Vec<i64>,
    children: Vec<Child>,
}

impl Reaper {
    fn track(&mut self, pid: i64) {
        if pid > 0 {
            self.pids.push(pid);
        }
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        for pid in &self.pids {
            // SAFETY: best-effort cleanup of processes this test spawned.
            unsafe {
                libc::kill(*pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

fn parent_pid(pid: i64) -> i64 {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit(')')
                .next()
                .and_then(|rest| rest.split_whitespace().nth(1))
                .and_then(|ppid| ppid.parse().ok())
        })
        .unwrap_or_default()
}

/// Reap these exact PIDs if this process is their (sub)reaper. Never waits on
/// arbitrary children: other tests in the same process own theirs.
fn reap_exact(pids: &[i64]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    for pid in pids.iter().copied().filter(|pid| *pid > 1) {
        loop {
            let mut status = 0;
            // SAFETY: waitpid on a specific PID; ECHILD when not our child.
            let reaped = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
            if reaped != 0 || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn daemon_pid(data: &serde_json::Value) -> i64 {
    data["daemon"]["pid"].as_i64().unwrap_or_default()
}

fn start_on_demand(bin: &Path, home: &Path, reaper: &mut Reaper) -> serde_json::Value {
    let started = homeboy_at(bin, home)
        .args(["daemon", "start"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("daemon start");
    assert!(started.success(), "daemon start failed");
    let current = wait_status(
        bin,
        home,
        Duration::from_secs(60),
        "daemon start",
        serving_and_admitting,
    );
    reaper.track(daemon_pid(&current["data"]));
    current
}

/// Case 3 (#15403, #15427): an upgrade replaces the binary under an idle
/// daemon. The daemon must notice and stop itself; it may not keep serving the
/// deleted image until someone kills it.
#[cfg(target_os = "linux")]
#[test]
fn replaced_binary_under_an_idle_daemon_converges() {
    let home = tempfile::tempdir().expect("home");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let installed = bin_dir.path().join("homeboy");
    std::fs::copy(homeboy_bin(), &installed).expect("install copy");
    let mut reaper = Reaper::default();

    let current = start_on_demand(&installed, home.path(), &mut reaper);
    let old_pid = daemon_pid(&current["data"]);
    assert!(pid_alive(old_pid));

    // Installers stage a new file and rename it over the path.
    let staged = bin_dir.path().join(".homeboy.new");
    std::fs::copy(homeboy_bin(), &staged).expect("stage replacement");
    std::fs::rename(&staged, &installed).expect("replace binary");

    // Idle replaced daemons stop after a short settle window (30s).
    let deadline = Instant::now() + Duration::from_secs(90);
    while pid_alive(old_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        !pid_alive(old_pid),
        "daemon kept serving a replaced binary for 90s"
    );

    // The next command gets a daemon on the installed binary.
    start_on_demand(&installed, home.path(), &mut reaper);
    let _ = homeboy_at(&installed, home.path())
        .args(["daemon", "stop"])
        .status();
}

/// Case 7 (#15456, #15460): a daemon restarted in place (service manager after
/// a crash) must take admission from the dead generation instead of leaving
/// every status probe following the dead lease.
#[cfg(unix)]
#[test]
fn restart_in_place_takes_admission_from_a_dead_generation() {
    let home = tempfile::tempdir().expect("home");
    let bin = homeboy_bin();
    let mut reaper = Reaper::default();

    let serve = |reaper: &mut Reaper| {
        let child = homeboy_at(&bin, home.path())
            .args(["daemon", "serve", "--addr", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("daemon serve");
        let pid = child.id() as i64;
        reaper.children.push(child);
        pid
    };

    let first_pid = serve(&mut reaper);
    let first = wait_status(
        &bin,
        home.path(),
        Duration::from_secs(60),
        "first serve",
        |data| serving_and_admitting(data) && daemon_pid(data) == first_pid,
    );
    let first_lease = first["data"]["daemon"]["lease_id"]
        .as_str()
        .unwrap()
        .to_string();

    // The crash: no clean stop, no lease release.
    // SAFETY: killing the daemon this test spawned.
    unsafe {
        libc::kill(first_pid as libc::pid_t, libc::SIGKILL);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_alive(first_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }

    // The supervisor restarts it in place.
    let second_pid = serve(&mut reaper);
    let second = wait_status(
        &bin,
        home.path(),
        Duration::from_secs(60),
        "restarted daemon must admit work without manual registry edits",
        |data| serving_and_admitting(data) && daemon_pid(data) == second_pid,
    );
    let second_lease = second["data"]["daemon"]["lease_id"].as_str().unwrap();
    assert_ne!(second_lease, first_lease);

    let registry: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.path().join(".config/homeboy/daemon/generations.json"))
            .expect("generation registry"),
    )
    .expect("registry JSON");
    assert_eq!(
        registry["generations"]["admission_owner"], second_lease,
        "admission must move to the live generation"
    );
}

/// Case 9 (wp-coding-agents#659): the host service is stopped with
/// `KillMode=control-group`, so the supervisor and daemon die together with
/// SIGKILL. The next recovery must converge without hand-edited state.
#[cfg(unix)]
#[test]
fn whole_daemon_process_group_killed_recovers_without_edits() {
    let home = tempfile::tempdir().expect("home");
    let bin = homeboy_bin();
    let mut reaper = Reaper::default();

    let current = start_on_demand(&bin, home.path(), &mut reaper);
    let pid = daemon_pid(&current["data"]);
    let supervisor = parent_pid(pid);
    // SAFETY: getpgid/kill on the daemon this test started.
    let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
    assert!(pgid > 0);
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    // In production the init system reaps the killed processes at once. The
    // test binary can be their subreaper (nextest runs each test as its own
    // process), so reap exactly what was killed; otherwise the zombies read
    // as live unleased daemon candidates and recovery rightly refuses.
    reap_exact(&[pid, supervisor]);
    assert!(
        !pid_alive(pid),
        "daemon {pid} (pgid {pgid}) survived group SIGKILL: {}",
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with("State") || line.starts_with("PPid"))
            .collect::<Vec<_>>()
            .join(" ")
    );

    let recovered = homeboy_at(&bin, home.path())
        .args(["daemon", "recover", "--yes"])
        .output()
        .expect("daemon recover");
    assert!(
        recovered.status.success(),
        "recover must converge a killed idle daemon: {}",
        String::from_utf8_lossy(&recovered.stdout)
    );
    let after = wait_status(
        &bin,
        home.path(),
        Duration::from_secs(60),
        "a fresh daemon after recovery",
        serving_and_admitting,
    );
    reaper.track(daemon_pid(&after["data"]));
    let _ = homeboy_at(&bin, home.path())
        .args(["daemon", "stop"])
        .status();
}
