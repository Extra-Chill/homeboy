//! Shared harness for end-to-end job cgroup tests (#18418, #18449).
//!
//! An outer test starts a transient job tree unit (`Delegate=yes`,
//! `DelegateSubgroup=holder`), then re-runs one inner test of the same binary
//! in a sibling user scope, where the inner test process plays the daemon: it
//! prepares `jobs/` in the job tree unit and runs real jobs in real cgroups.
//! Outer tests skip, printing why, when the host cannot delegate cgroup v2
//! `memory` and `pids` to a user unit.

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
    command.args(["--user", "--scope", "--quiet", "--"]);
    command
}

/// A transient job tree unit like the runner's `homeboy-runner-jobs.service`,
/// with outer bounds so a defect cannot reach the host either.
fn job_tree_unit_command(unit: &str) -> Command {
    let mut command = user_manager_command("systemd-run");
    command.args([
        "--user",
        "--quiet",
        "--collect",
        &format!("--unit={unit}"),
        "-p",
        "Delegate=yes",
        "-p",
        &format!("DelegateSubgroup={}", super::HOLDER_LEAF),
        "-p",
        "TasksMax=2048",
        "-p",
        "MemoryMax=2G",
        "-p",
        "MemorySwapMax=0",
        "sleep",
        "infinity",
    ]);
    command
}

/// The cgroup directory of a running user unit.
fn unit_cgroup_dir(unit: &str) -> Option<std::path::PathBuf> {
    let mut show = user_manager_command("systemctl");
    show.args(["--user", "show", "--property=ControlGroup", "--value", unit]);
    let output = output_within(show, Duration::from_secs(30)).ok()?;
    let relative = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!relative.is_empty())
        .then(|| Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')))
}

fn stop_unit(unit: &str) {
    let mut stop = user_manager_command("systemctl");
    stop.args(["--user", "stop", unit]);
    let _ = output_within(stop, Duration::from_secs(30));
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

/// Start the job tree unit `unit`, or say why this host cannot run the
/// end-to-end tests.
fn start_job_tree_unit(unit: &str) -> Result<(), String> {
    match output_within(job_tree_unit_command(unit), Duration::from_secs(30)) {
        Err(error) => return Err(format!("systemd-run --user is unavailable: {error}")),
        Ok(output) if !output.status.success() => {
            return Err(format!(
                "systemd-run --user -p Delegate=yes failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
        Ok(_) => {}
    }
    let controllers = unit_cgroup_dir(unit)
        .and_then(|dir| fs::read_to_string(dir.join("cgroup.controllers")).ok())
        .unwrap_or_default();
    match ["memory", "pids"].iter().find(|controller| {
        !controllers
            .split_whitespace()
            .any(|name| name == **controller)
    }) {
        Some(controller) => {
            stop_unit(unit);
            Err(format!(
                "the user manager does not delegate the {controller} controller"
            ))
        }
        None => Ok(()),
    }
}

/// Run the ignored inner test `test_path` (its full path in this test binary)
/// as the only test inside a delegated scope, or skip with the reason.
pub fn run_in_delegated_scope(test_path: &str, env: &[(&str, &str)]) {
    let unit = format!(
        "homeboy-test-jobs-{}.service",
        uuid::Uuid::new_v4().simple()
    );
    if let Err(reason) = start_job_tree_unit(&unit) {
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
        .env(super::JOB_TREE_UNIT_ENV, &unit)
        .envs(env.iter().copied());
    let output = output_within(command, INNER_TIMEOUT);
    stop_unit(&unit);
    let output = output.expect("inner test completes");
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
