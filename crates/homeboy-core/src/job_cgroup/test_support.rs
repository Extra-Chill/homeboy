//! Shared harness for end-to-end job cgroup tests (#18418, #18449).
//!
//! An outer test re-runs one inner test of the same binary in a transient
//! `Delegate=yes` user scope, where the inner test process plays the daemon: it
//! moves into its `daemon/` leaf and runs real jobs in real cgroups. Outer
//! tests skip, printing why, when the host cannot delegate cgroup v2 `memory`
//! and `pids` to a user scope.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const INNER_ENV: &str = "HOMEBOY_JOB_CGROUP_E2E";
const INNER_TIMEOUT: Duration = Duration::from_secs(180);

fn user_manager_command(program: &str) -> Command {
    let mut command = Command::new(program);
    // The user manager's runtime directory, whatever a test home overrides.
    // SAFETY: getuid cannot fail.
    let runtime_dir = format!("/run/user/{}", unsafe { libc::getuid() });
    if Path::new(&runtime_dir).join("systemd").is_dir() {
        command
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env_remove("DBUS_SESSION_BUS_ADDRESS");
    }
    command
}

fn scope_command() -> Command {
    let mut command = user_manager_command("systemd-run");
    command.args([
        "--user",
        "--scope",
        "--quiet",
        "-p",
        "Delegate=yes",
        // Outer bounds, so a defect cannot reach the host either.
        "-p",
        "TasksMax=2048",
        "-p",
        "MemoryMax=2G",
        "-p",
        "MemorySwapMax=0",
        "--",
    ]);
    command
}

fn output_within(mut command: Command, timeout: Duration) -> Result<std::process::Output, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + timeout;
    while child
        .try_wait()
        .map_err(|error| error.to_string())?
        .is_none()
    {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("timed out after {}s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().map_err(|error| error.to_string())
}

/// Why this host cannot run the end-to-end tests, if it cannot.
fn delegated_scope_unavailable() -> Option<String> {
    let mut probe = scope_command();
    probe.args([
        "sh",
        "-c",
        "cat \"/sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)/cgroup.controllers\"",
    ]);
    match output_within(probe, Duration::from_secs(30)) {
        Err(error) => Some(format!(
            "systemd-run --user --scope is unavailable: {error}"
        )),
        Ok(output) if !output.status.success() => Some(format!(
            "systemd-run --user --scope -p Delegate=yes failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(output) => {
            let controllers = String::from_utf8_lossy(&output.stdout);
            ["memory", "pids"]
                .iter()
                .find(|controller| {
                    !controllers
                        .split_whitespace()
                        .any(|name| name == **controller)
                })
                .map(|controller| {
                    format!("the user manager does not delegate the {controller} controller")
                })
        }
    }
}

/// Run the ignored inner test `test_path` (its full path in this test binary)
/// as the only test inside a delegated scope, or skip with the reason.
pub fn run_in_delegated_scope(test_path: &str, env: &[(&str, &str)]) {
    if let Some(reason) = delegated_scope_unavailable() {
        eprintln!("skipping job cgroup end-to-end test {test_path}: {reason}");
        return;
    }
    let mut command = scope_command();
    command
        .arg(std::env::current_exe().expect("test binary"))
        .args([
            test_path,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(INNER_ENV, "1")
        .envs(env.iter().copied());
    let output = output_within(command, INNER_TIMEOUT).expect("inner test completes");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{test_path} failed in a delegated scope ({:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
}

/// Whether this process is an inner test inside the scope its outer test
/// created. Inner tests skip, printing why, everywhere else.
pub fn inside_delegated_scope() -> bool {
    if std::env::var_os(INNER_ENV).is_none() {
        eprintln!("skipping: runs only inside the delegated scope its outer test creates");
        return false;
    }
    true
}

/// Live processes whose command line names `marker`.
pub fn survivors(marker: &str) -> Vec<String> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().parse::<u32>().is_ok())
        .filter_map(|entry| {
            let cmdline = fs::read(entry.path().join("cmdline")).ok()?;
            let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
            let state = fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
            let zombie = state
                .rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('Z'));
            (cmdline.contains(marker) && !zombie)
                .then(|| format!("{}: {cmdline}", entry.file_name().to_string_lossy()))
        })
        .collect()
}

/// Whether `pid` is a live, non-zombie process.
pub fn alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            let (_, rest) = stat.rsplit_once(") ")?;
            Some(!rest.starts_with('Z'))
        })
        .unwrap_or(false)
}

/// No job cgroup outlived its job.
pub fn assert_job_cgroups_removed(jobs: &Path) {
    let left: Vec<_> = fs::read_dir(jobs)
        .expect("jobs cgroup")
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .collect();
    assert!(left.is_empty(), "job cgroups outlived their jobs: {left:?}");
}

/// A `sleep` duration unique to this test run, to find its survivors by.
pub fn unique_sleep() -> String {
    format!(
        "300.{}{}",
        std::process::id(),
        uuid::Uuid::new_v4().as_u128() % 100_000
    )
}
