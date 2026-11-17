//! End-to-end job cgroup tests (#18418). Each outer test re-runs one inner
//! test in a transient `Delegate=yes` user scope, where the inner test process
//! plays the runner daemon: it moves into its `daemon/` leaf and runs real
//! jobs in real cgroups. Outer tests skip, printing why, when the host cannot
//! delegate cgroup v2 `memory` and `pids` to a user scope.

use super::*;
use crate::resource_metrics::{
    measured_command_output_until_cancelled_with_progress, MeasuredOutput,
};
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;

const INNER_ENV: &str = "HOMEBOY_JOB_CGROUP_E2E";
const HOG_ENV: &str = "HOMEBOY_JOB_CGROUP_E2E_HOG";
const INNER_TIMEOUT: Duration = Duration::from_secs(180);
const MIB: u64 = 1024 * 1024;

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

/// Run `inner` as the only test of this binary inside a delegated scope.
fn in_delegated_scope(inner: &str, env: &[(&str, &str)]) {
    if let Some(reason) = delegated_scope_unavailable() {
        eprintln!("skipping job cgroup end-to-end test {inner}: {reason}");
        return;
    }
    let mut command = scope_command();
    command
        .arg(std::env::current_exe().expect("test binary"))
        .args([
            &format!("resource_metrics::job_cgroup::tests::{inner}"),
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
        "{inner} failed in a delegated scope ({:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
}

/// Inner tests run only inside the scope their outer test creates, where this
/// process becomes the daemon in its `daemon/` leaf.
fn enter_delegated_scope() -> Option<&'static Path> {
    if std::env::var_os(INNER_ENV).is_none() {
        eprintln!("skipping: runs only inside the delegated scope its outer test creates");
        return None;
    }
    let jobs = crate::resource_metrics::initialize_job_cgroups()
        .expect("a delegated scope initializes runner job cgroups");
    assert_eq!(jobs_dir(), Some(jobs.as_path()));
    jobs_dir()
}

fn run_job(command: &mut Command, is_cancelled: impl FnMut() -> bool) -> MeasuredOutput {
    measured_command_output_until_cancelled_with_progress(
        command,
        is_cancelled,
        None,
        false,
        None,
        None,
        Some(1),
        None,
    )
    .expect("runner job completes")
}

/// Live processes whose command line names `marker`.
fn survivors(marker: &str) -> Vec<String> {
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

fn alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            let (_, rest) = stat.rsplit_once(") ")?;
            Some(!rest.starts_with('Z'))
        })
        .unwrap_or(false)
}

fn assert_job_cgroups_removed(jobs: &Path) {
    let left: Vec<_> = fs::read_dir(jobs)
        .expect("jobs cgroup")
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .collect();
    assert!(left.is_empty(), "job cgroups outlived their jobs: {left:?}");
}

fn unique_sleep() -> String {
    format!(
        "300.{}{}",
        std::process::id(),
        uuid::Uuid::new_v4().as_u128() % 100_000
    )
}

#[test]
fn the_daemon_runs_in_a_leaf_and_jobs_carry_the_host_budget() {
    in_delegated_scope(
        "delegated_daemon_leaf_and_jobs_budget",
        &[
            (homeboy_runner_contract::RSS_LIMIT_ENV, "268435456"),
            (homeboy_runner_contract::PROCESS_COUNT_LIMIT_ENV, "48"),
        ],
    );
}

#[test]
#[ignore = "inner half of the_daemon_runs_in_a_leaf_and_jobs_carry_the_host_budget"]
fn delegated_daemon_leaf_and_jobs_budget() {
    let Some(jobs) = enter_delegated_scope() else {
        return;
    };
    let own = own_cgroup_dir().expect("own cgroup");
    assert_eq!(own.file_name(), Some(DAEMON_LEAF.as_ref()), "{own:?}");
    let service = own.parent().expect("service cgroup");
    assert_eq!(jobs, service.join(JOBS_DIR));
    assert_eq!(
        fs::read_to_string(service.join("cgroup.procs"))
            .unwrap()
            .trim(),
        "",
        "the service cgroup is an inner node"
    );
    assert_eq!(
        fs::read_to_string(own.join("cgroup.procs")).unwrap().trim(),
        std::process::id().to_string()
    );
    for cgroup in [service, jobs] {
        let enabled = fs::read_to_string(cgroup.join("cgroup.subtree_control")).unwrap();
        assert!(
            enabled.contains("memory") && enabled.contains("pids"),
            "{enabled}"
        );
    }
    assert_eq!(
        fs::read_to_string(jobs.join("memory.max")).unwrap().trim(),
        "268435456"
    );
    if let Ok(swap) = fs::read_to_string(jobs.join("memory.swap.max")) {
        assert_eq!(swap.trim(), "0");
    }

    let job = JobCgroup::create(Some("fixture-job"), 48, 2)
        .expect("create job cgroup")
        .expect("delegated");
    assert_eq!(job.dir(), jobs.join("fixture-job"));
    assert_eq!(
        fs::read_to_string(job.dir().join("pids.max"))
            .unwrap()
            .trim(),
        "48"
    );
    assert_eq!(
        fs::read_to_string(job.dir().join("memory.oom.group"))
            .unwrap()
            .trim(),
        "1"
    );
    assert_eq!(
        fs::read_to_string(jobs.join("pids.max")).unwrap().trim(),
        "96"
    );
    job.stop().expect("stop empty job cgroup");
    assert!(!job.dir().exists());
}

#[test]
fn a_job_that_forks_into_fresh_sessions_is_stopped_by_pids_max_with_zero_survivors() {
    in_delegated_scope(
        "delegated_setsid_fork_bomb",
        &[(homeboy_runner_contract::PROCESS_COUNT_LIMIT_ENV, "32")],
    );
}

#[test]
#[ignore = "inner half of a_job_that_forks_into_fresh_sessions_is_stopped_by_pids_max_with_zero_survivors"]
fn delegated_setsid_fork_bomb() {
    let Some(jobs) = enter_delegated_scope() else {
        return;
    };
    let temp = tempfile::tempdir().expect("tempdir");
    let bomb = temp.path().join("bomb.sh");
    let sleep = unique_sleep();
    // Every instance re-execs itself twice, each in a fresh session (and so a
    // fresh process group) that a process-group kill cannot reach.
    fs::write(
        &bomb,
        "#!/bin/sh\nsetsid \"$0\" 2>/dev/null &\nsetsid \"$0\" 2>/dev/null &\nsleep \"$BOMB_SLEEP\"\n",
    )
    .unwrap();
    fs::set_permissions(&bomb, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut command = Command::new(&bomb);
    command.env("BOMB_SLEEP", &sleep);

    let started = Instant::now();
    let output = run_job(&mut command, || false);

    assert!(
        started.elapsed() < Duration::from_secs(60),
        "pids.max stopped the job"
    );
    assert!(!output.output.status.success());
    let metrics = output.metrics;
    assert_eq!(metrics.isolation.as_deref(), Some("cgroup"));
    assert_eq!(metrics.source, "linux_cgroup_v2");
    let violation = metrics.guard_violation.expect("guard violation");
    assert_eq!(violation.reason, "process_count_limit_exceeded");
    assert_eq!(violation.process_count_limit, 32);
    assert!(violation.process_count >= 32, "{violation:?}");
    assert!(
        violation
            .top_processes
            .iter()
            .any(|process| process.name == "bomb.sh" && process.count > 1),
        "top processes come from cgroup.procs: {:?}",
        violation.top_processes
    );
    assert_eq!(metrics.cgroup_memory.scope, "job_cgroup");
    assert_eq!(metrics.cgroup_memory.status, "observed");
    let marker = bomb.display().to_string();
    assert_eq!(survivors(&marker), Vec::<String>::new());
    assert_eq!(survivors(&sleep), Vec::<String>::new());
    assert_job_cgroups_removed(jobs);
}

#[test]
fn a_job_past_its_memory_budget_is_oom_killed_as_a_group_and_the_daemon_survives() {
    in_delegated_scope(
        "delegated_memory_budget_oom",
        &[(homeboy_runner_contract::RSS_LIMIT_ENV, "134217728")],
    );
}

#[test]
#[ignore = "inner half of a_job_past_its_memory_budget_is_oom_killed_as_a_group_and_the_daemon_survives"]
fn delegated_memory_budget_oom() {
    let Some(jobs) = enter_delegated_scope() else {
        return;
    };
    let sleep = unique_sleep();
    let hog = std::env::current_exe().expect("test binary");
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "sleep \"$1\" & exec \"$2\" resource_metrics::job_cgroup::tests::delegated_memory_hog --exact --ignored --test-threads=1",
            "sh",
        ])
        .arg(&sleep)
        .arg(&hog)
        .env(HOG_ENV, "1");

    let output = run_job(&mut command, || false);

    assert_eq!(output.output.status.signal(), Some(libc::SIGKILL));
    let metrics = output.metrics;
    assert_eq!(metrics.isolation.as_deref(), Some("cgroup"));
    let violation = metrics.guard_violation.expect("guard violation");
    assert_eq!(violation.reason, "memory_limit_exceeded");
    assert_eq!(violation.rss_limit_bytes, 128 * MIB);
    assert!(metrics
        .cgroup_memory
        .oom_kill_delta
        .is_some_and(|delta| delta >= 1));
    assert_eq!(metrics.cgroup_memory.scope, "job_cgroup");
    assert_eq!(metrics.cgroup_memory.memory_limit_bytes, Some(128 * MIB));
    // memory.oom.group took the job's idle sibling down with the allocator.
    assert_eq!(survivors(&sleep), Vec::<String>::new());
    // The daemon was outside the OOM domain and still runs in its leaf.
    assert_eq!(
        own_cgroup_dir().expect("own cgroup").file_name(),
        Some(DAEMON_LEAF.as_ref())
    );
    assert_job_cgroups_removed(jobs);
}

#[test]
#[ignore = "allocator run as a job by delegated_memory_budget_oom"]
fn delegated_memory_hog() {
    if std::env::var_os(HOG_ENV).is_none() {
        eprintln!("skipping: allocates until its job cgroup is OOM-killed");
        return;
    }
    let mut chunks = Vec::new();
    // The scope's own MemoryMax bounds this even if the job budget did not.
    for _ in 0..2048 {
        chunks.push(vec![1_u8; MIB as usize]);
    }
    panic!("allocated {} MiB without an OOM kill", chunks.len());
}

#[test]
fn cancelling_a_job_kills_descendants_that_left_its_process_group() {
    in_delegated_scope("delegated_cancel_kills_escaped_descendants", &[]);
}

#[test]
#[ignore = "inner half of cancelling_a_job_kills_descendants_that_left_its_process_group"]
fn delegated_cancel_kills_escaped_descendants() {
    let Some(jobs) = enter_delegated_scope() else {
        return;
    };
    let temp = tempfile::tempdir().expect("tempdir");
    let job_id = uuid::Uuid::new_v4().to_string();
    let dir = temp.path().join(&job_id);
    fs::create_dir(&dir).unwrap();
    let session_pid = temp.path().join("session.pid");
    let orphan_pid = temp.path().join("orphan.pid");
    let sleep = unique_sleep();
    let script = format!(
        "setsid sh -c 'trap \"\" TERM; echo $$ > {session}; exec sleep {sleep}' & \
         sh -c 'sleep {sleep} & echo $! > {orphan}' ; wait",
        session = session_pid.display(),
        orphan = orphan_pid.display(),
    );
    let mut command = homeboy_core::daemon::runner_exec_job::command(
        &dir,
        &["sh".to_string(), "-c".to_string(), script],
    )
    .expect("durable job command");
    let ready = |path: &Path| {
        fs::read_to_string(path)
            .ok()
            .and_then(|pid| pid.trim().parse::<u32>().ok())
    };
    let mut named_for_job = false;

    let output = measured_command_output_until_cancelled_with_progress(
        &mut command,
        || {
            named_for_job |= jobs.join(&job_id).is_dir();
            ready(&session_pid).is_some() && ready(&orphan_pid).is_some()
        },
        None,
        false,
        None,
        None,
        Some(1),
        Some(&dir),
    )
    .expect("cancelled job completes");

    assert!(!output.output.status.success());
    assert_eq!(output.metrics.isolation.as_deref(), Some("cgroup"));
    for path in [&session_pid, &orphan_pid] {
        let pid = ready(path).expect("descendant pid");
        assert!(!alive(pid), "cancel left escaped descendant {pid}");
    }
    assert_eq!(survivors(&sleep), Vec::<String>::new());
    assert!(
        named_for_job,
        "a durable job's cgroup is named for its job id"
    );
    assert_job_cgroups_removed(jobs);
}
