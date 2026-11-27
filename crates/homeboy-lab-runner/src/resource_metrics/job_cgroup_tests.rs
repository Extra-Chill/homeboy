//! End-to-end runner job cgroup tests (#18418, #18493). Each outer test
//! re-runs one inner test beside a transient job tree unit through the shared
//! harness in `homeboy_core::job_cgroup::test_support`.

use super::*;
use crate::resource_metrics::{
    durable_command_output_until_cancelled, measured_command_output_until_cancelled_with_progress,
    MeasuredOutput,
};
use homeboy_core::job_cgroup::test_support::{
    alive, assert_job_cgroups_removed, inside_delegated_scope, run_in_delegated_scope, survivors,
    unique_sleep,
};
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const HOG_ENV: &str = "HOMEBOY_JOB_CGROUP_E2E_HOG";
const MIB: u64 = 1024 * 1024;

/// Run `inner` as the only test of this binary inside a delegated scope.
fn in_delegated_scope(inner: &str, env: &[(&str, &str)]) {
    run_in_delegated_scope(
        &format!("resource_metrics::job_cgroup::tests::{inner}"),
        env,
    );
}

/// Inner tests run only inside the scope their outer test creates, where this
/// process becomes the daemon of the job tree unit beside it.
fn enter_delegated_scope() -> Option<&'static Path> {
    if !inside_delegated_scope() {
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

#[test]
fn jobs_live_outside_the_daemon_unit_and_carry_the_host_budget() {
    in_delegated_scope(
        "delegated_job_tree_and_jobs_budget",
        &[
            (homeboy_runner_contract::RSS_LIMIT_ENV, "268435456"),
            (homeboy_runner_contract::PROCESS_COUNT_LIMIT_ENV, "48"),
        ],
    );
}

#[test]
#[ignore = "inner half of jobs_live_outside_the_daemon_unit_and_carry_the_host_budget"]
fn delegated_job_tree_and_jobs_budget() {
    let Some(jobs) = enter_delegated_scope() else {
        return;
    };
    // The job tree is the sibling job tree unit, never the daemon's own unit,
    // so restarting the daemon never meets a cgroup with live jobs (#18493).
    let own = own_cgroup_dir().expect("own cgroup");
    let service = jobs.parent().expect("job tree unit cgroup");
    assert_eq!(service.parent(), own.parent(), "{service:?} beside {own:?}");
    assert_ne!(service, own);
    assert!(!jobs.starts_with(&own), "{jobs:?} inside {own:?}");
    assert_eq!(jobs, service.join(JOBS_DIR));
    assert_eq!(
        fs::read_to_string(service.join("cgroup.procs"))
            .unwrap()
            .trim(),
        "",
        "the job tree unit's root is an inner node"
    );
    assert!(
        !fs::read_to_string(service.join(HOLDER_LEAF).join("cgroup.procs"))
            .unwrap()
            .trim()
            .is_empty(),
        "the job tree unit's process runs in its holder leaf"
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
        "1536"
    );
    assert_eq!(
        fs::read_to_string(job.dir().join("memory.oom.group"))
            .unwrap()
            .trim(),
        "1"
    );
    assert_eq!(
        fs::read_to_string(jobs.join("pids.max")).unwrap().trim(),
        "3072"
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

/// The process limit counts processes: one process with more threads than
/// the limit runs to completion (`pids.max` counts threads).
#[test]
fn leftover_processes_are_reported_at_settle() {
    in_delegated_scope("delegated_leftover_evidence", &[]);
}

#[test]
#[ignore = "inner half of leftover_processes_are_reported_at_settle"]
fn delegated_leftover_evidence() {
    let Some(jobs) = enter_delegated_scope() else {
        return;
    };
    // A sibling job must survive the tested job's teardown.
    let sibling = JobCgroup::create(Some("sibling"), 0, 1).unwrap().unwrap();
    let mut sibling_command = Command::new("sleep");
    sibling_command.arg("60");
    sibling.join_before_exec(&mut sibling_command);
    let mut sibling_child = sibling_command.spawn().unwrap();
    // Durable runs read stdout from the job directory, so the command must
    // be built by `runner_exec_job::command`, as production does.
    let temp = tempfile::tempdir().unwrap();
    let mut command = homeboy_core::daemon::runner_exec_job::command(
        temp.path(),
        &[
            "bash".to_string(),
            "-c".to_string(),
            "setsid nohup sleep 777 >/dev/null 2>&1 </dev/null & echo $!; sleep 0.2".to_string(),
        ],
    )
    .expect("durable job command");
    let output = durable_command_output_until_cancelled(
        &mut command,
        temp.path(),
        || false,
        None,
        false,
        None,
        None,
        Some(1),
    )
    .unwrap();
    assert!(output.output.status.success());
    let stderr = String::from_utf8_lossy(&output.output.stderr);
    let pid = String::from_utf8_lossy(&output.output.stdout)
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(
        stderr.contains("killed 1 leftover process(es) in job cgroup:"),
        "{stderr}"
    );
    assert!(stderr.contains(&format!("{pid} sleep")), "{stderr}");
    assert!(stderr.contains("; use --detach for long-running work"));
    assert!(sibling_child.try_wait().unwrap().is_none());
    sibling.stop().unwrap();
    sibling_child.wait().unwrap();
    assert_job_cgroups_removed(jobs);
}

#[test]
fn a_multithreaded_job_under_its_process_limit_runs() {
    in_delegated_scope(
        "delegated_multithreaded_job",
        &[(homeboy_runner_contract::PROCESS_COUNT_LIMIT_ENV, "8")],
    );
}

#[test]
#[ignore = "inner half of a_multithreaded_job_under_its_process_limit_runs"]
fn delegated_multithreaded_job() {
    let Some(jobs) = enter_delegated_scope() else {
        return;
    };
    let mut command = Command::new("python3");
    command.args([
        "-c",
        "import threading, time\n\
         threads = [threading.Thread(target=time.sleep, args=(2,)) for _ in range(64)]\n\
         [t.start() for t in threads]; [t.join() for t in threads]",
    ]);
    let output = run_job(&mut command, || false);
    assert!(
        output.output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.output.stderr)
    );
    assert_eq!(output.metrics.isolation.as_deref(), Some("cgroup"));
    assert!(
        output.metrics.guard_violation.is_none(),
        "{:?}",
        output.metrics.guard_violation
    );
    // The usage block is the job cgroup's exact `cpu.stat`, read at finish.
    let usage = output.metrics.usage.expect("job usage");
    assert_eq!(usage.source, "job_cgroup");
    assert!(!usage.estimated);
    assert!(usage.cpu_usage_usec > 0);
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
    // The daemon was outside the OOM domain: the job tree is another unit.
    assert!(!jobs.starts_with(own_cgroup_dir().expect("own cgroup")));
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
