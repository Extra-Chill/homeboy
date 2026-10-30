//! Durable custody of a runner exec child (#16681).
//!
//! A runner exec child writes stdout and stderr to files under its job's
//! artifact directory, and a wrapper shell, which stays the workload's parent,
//! records the workload's exit status there. The child leads its own session,
//! so it outlives the daemon that spawned it.
//!
//! The daemon supervising a child holds the directory's custody lock. A daemon
//! that starts and finds a running runner exec job whose custody is free
//! re-adopts the child by PID and kernel start identity — it never spawns it
//! again — and terminalizes the job from the output files and exit record.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::json;
use uuid::Uuid;

use crate::api_jobs::JobStore;
use crate::daemon::runner_exec_driver::RunnerChildIdentity;
use crate::engine::command::{
    capture_file_tail, CommandCaptureMetadata, DEFAULT_CAPTURE_LIMIT_BYTES,
};
use crate::error::{Error, Result};

/// Child stdout lines carrying structured progress, not command output.
pub const PROGRESS_LINE_PREFIX: &str = "HOMEBOY_RUNNER_PROGRESS ";
const STDOUT_FILE: &str = "stdout.log";
const STDERR_FILE: &str = "stderr.log";
const EXIT_FILE: &str = "exit.json";
const REDACTION_FILE: &str = "redaction.json";
const REDACTION_SCHEMA: &str = "homeboy/runner-exec-redaction/v1";
const CUSTODY_FILE: &str = "custody.lock";
const EXIT_SCHEMA: &str = "homeboy/runner-exec-exit/v1";
const SUPERVISE_POLL: Duration = Duration::from_millis(100);
const CANCEL_GRACE: Duration = Duration::from_secs(5);
/// Runs the workload, then records its exit status where any daemon can read it.
const WRAPPER: &str = r#"exit_file=$1; shift; "$@"; status=$?; printf '{"schema":"homeboy/runner-exec-exit/v1","exit_code":%d}' "$status" > "$exit_file"; exit "$status""#;

/// Exclusive supervision of one runner exec job's child. Dropping it after the
/// job's outcome is captured removes the job's output files.
pub struct RunnerExecCustody {
    dir: PathBuf,
    lock: Option<File>,
}

impl RunnerExecCustody {
    /// Take custody before spawning the job's child.
    pub(crate) fn acquire(job_id: Uuid) -> Result<Self> {
        let dir = job_dir(job_id)?;
        fs::create_dir_all(&dir).map_err(|error| io_error(error, &dir))?;
        // The output files may hold secrets until the outcome is redacted.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
                .map_err(|error| io_error(error, &dir))?;
        }
        Self::lock(dir)?.ok_or_else(|| {
            Error::internal_unexpected(format!(
                "runner exec job {job_id} is already supervised by another daemon"
            ))
        })
    }

    /// Custody of a job spawned under this model whose supervisor is gone.
    fn try_acquire(job_id: Uuid) -> Result<Option<Self>> {
        let dir = job_dir(job_id)?;
        if !dir.join(STDOUT_FILE).exists() {
            return Ok(None);
        }
        Self::lock(dir)
    }

    fn lock(dir: PathBuf) -> Result<Option<Self>> {
        let path = dir.join(CUSTODY_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| io_error(error, &path))?;
        Ok(
            super::try_lock_file_exclusive(&file, "runner exec custody")?.then_some(Self {
                dir,
                lock: Some(file),
            }),
        )
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Release custody but keep the files, as a daemon that died would.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn abandon(mut self) {
        self.lock = None;
    }
}

impl Drop for RunnerExecCustody {
    fn drop(&mut self) {
        if self.lock.is_some() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn job_dir(job_id: Uuid) -> Result<PathBuf> {
    super::step_job::step_job_dir(&job_id.to_string())
}

/// A command that runs `argv` with output to `dir`'s files and its exit status
/// recorded there, leading its own session so it survives the daemon. The
/// caller sets cwd and environment.
pub fn command(dir: &Path, argv: &[String]) -> Result<Command> {
    let open = |name: &str| {
        let path = dir.join(name);
        File::create(&path).map_err(|error| io_error(error, &path))
    };
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(WRAPPER)
        .arg("homeboy-runner-exec")
        .arg(dir.join(EXIT_FILE))
        .args(argv)
        .stdin(Stdio::null())
        .stdout(open(STDOUT_FILE)?)
        .stderr(open(STDERR_FILE)?);
    crate::process::detach_from_caller_session(&mut command);
    Ok(command)
}

/// Spawn a [`command`], capturing the child's identity while it is unreaped.
pub fn spawn(command: &mut Command) -> io::Result<(Child, RunnerChildIdentity)> {
    let child = command.spawn()?;
    let pid = child.id();
    let identity = RunnerChildIdentity {
        pid,
        process_group_id: Some(pid),
        starttime_ticks: crate::process::linux_process_starttime_ticks(pid)
            .ok()
            .flatten(),
    };
    Ok((child, identity))
}

pub fn stdout_path(dir: &Path) -> PathBuf {
    dir.join(STDOUT_FILE)
}

/// Record what a recovering daemon must redact from the output. Secret values
/// never reach disk: only each value's byte length and SHA-256 digest are
/// written, and recovery redacts every output span whose digest matches.
pub fn write_redaction_values(dir: &Path, values: &[String]) -> Result<()> {
    let path = dir.join(REDACTION_FILE);
    let mut digests = values
        .iter()
        .filter(|value| !value.is_empty())
        .map(|value| SecretDigest {
            len: value.len(),
            sha256: sha256_hex(value.as_bytes()),
        })
        .collect::<Vec<_>>();
    digests.sort_by(|a, b| b.len.cmp(&a.len).then_with(|| a.sha256.cmp(&b.sha256)));
    digests.dedup();
    let bytes = serde_json::to_vec(&json!({ "schema": REDACTION_SCHEMA, "secrets": digests }))
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    fs::write(&path, bytes).map_err(|error| io_error(error, &path))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SecretDigest {
    len: usize,
    sha256: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read_redaction_digests(dir: &Path) -> Option<Vec<SecretDigest>> {
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join(REDACTION_FILE)).ok()?).ok()?;
    if record["schema"] != REDACTION_SCHEMA {
        return None;
    }
    serde_json::from_value(record["secrets"].clone()).ok()
}

/// Replace every span of `text` whose SHA-256 matches a recorded secret,
/// longest secret first. Bounded by the output capture limit and only run on
/// the recovery path.
fn redact_digests(text: &str, digests: &[SecretDigest], replacement: &str) -> String {
    if digests.is_empty() {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut redacted = String::with_capacity(text.len());
    let mut index = 0;
    'scan: while index < bytes.len() {
        for digest in digests {
            let end = index + digest.len;
            if end <= bytes.len()
                && text.is_char_boundary(end)
                && sha256_hex(&bytes[index..end]) == digest.sha256
            {
                redacted.push_str(replacement);
                index = end;
                continue 'scan;
            }
        }
        let ch = text[index..]
            .chars()
            .next()
            .expect("index is a char boundary");
        redacted.push(ch);
        index += ch.len_utf8();
    }
    redacted
}

/// Raw bounded stdout and stderr with their capture metadata.
pub fn read_output(dir: &Path) -> Result<(Vec<u8>, Vec<u8>, CommandCaptureMetadata)> {
    let read = |name: &str| {
        let path = dir.join(name);
        capture_file_tail(&path, DEFAULT_CAPTURE_LIMIT_BYTES)
            .map_err(|error| io_error(error, &path))
    };
    let (stdout, stdout_capture) = read(STDOUT_FILE)?;
    let (stderr, stderr_capture) = read(STDERR_FILE)?;
    Ok((
        stdout,
        stderr,
        CommandCaptureMetadata {
            stdout: stdout_capture,
            stderr: stderr_capture,
        },
    ))
}

fn recorded_exit_code(dir: &Path) -> Option<i32> {
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join(EXIT_FILE)).ok()?).ok()?;
    (record["schema"] == EXIT_SCHEMA)
        .then(|| record["exit_code"].as_i64())
        .flatten()
        .and_then(|code| i32::try_from(code).ok())
}

/// Re-adopt every running runner exec child whose supervising daemon is gone.
pub(crate) fn recover_runner_exec_jobs(store: &JobStore) {
    for (job_id, pid, ticks) in store.foreign_runner_exec_children() {
        let Ok(Some(custody)) = RunnerExecCustody::try_acquire(job_id) else {
            continue;
        };
        if store.adopt_runner_exec_job(job_id).is_err() {
            continue;
        }
        let store = store.clone();
        std::thread::spawn(move || supervise_adopted(&store, job_id, pid, ticks, custody));
    }
}

fn root_is_live(pid: u32, ticks: u64) -> bool {
    match crate::process::linux_process_starttime_ticks(pid) {
        Ok(Some(actual)) => actual == ticks,
        Ok(None) => false,
        Err(_) => true,
    }
}

/// Supervise a child this daemon did not spawn and cannot reap.
fn supervise_adopted(
    store: &JobStore,
    job_id: Uuid,
    pid: u32,
    ticks: u64,
    custody: RunnerExecCustody,
) {
    let job = store.handle(job_id);
    let observed_live = root_is_live(pid, ticks);
    while root_is_live(pid, ticks) {
        if job.is_cancelled() {
            let _ = crate::process::terminate_isolated_process_group_with_grace(pid, CANCEL_GRACE);
            return;
        }
        std::thread::sleep(SUPERVISE_POLL);
    }
    if observed_live {
        let _ = crate::engine::command::terminate_remaining_process_group(pid);
    }
    let _ = terminalize(store, job_id, custody.dir());
}

fn terminalize(store: &JobStore, job_id: Uuid, dir: &Path) -> Result<()> {
    let job = store.handle(job_id);
    if job.is_terminal() {
        return Ok(());
    }
    let fail = |message: &str, reason: &str| {
        store
            .fail_with_data(job_id, message, Some(json!({ "reason": reason })))
            .map(|_| ())
    };
    let Some(exit_code) = recorded_exit_code(dir) else {
        return fail(
            "runner exec child exited while no daemon supervised it and left no exit record",
            "runner_exec_exit_unrecorded",
        );
    };
    let Some(secrets) = read_redaction_digests(dir) else {
        return fail(
            "runner exec output cannot be redacted: its redaction record is missing",
            "runner_exec_redaction_unrecorded",
        );
    };
    let policy = crate::redaction::RedactionPolicy::default();
    let redact = |bytes: Vec<u8>| {
        let text = String::from_utf8_lossy(&bytes)
            .split_inclusive('\n')
            .filter(|line| !line.starts_with(PROGRESS_LINE_PREFIX))
            .collect::<String>();
        policy.redact_string(&redact_digests(&text, &secrets, policy.replacement()))
    };
    let (stdout, stderr, capture) = read_output(dir)?;
    let (stdout, stderr) = (redact(stdout), redact(stderr));
    if !stdout.is_empty() {
        job.stdout(stdout.clone())?;
    }
    if !stderr.is_empty() {
        job.stderr(stderr.clone())?;
    }
    let result = json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "capture": capture,
        "recovery": "runner_exec_child_readopted",
    });
    if exit_code == 0 {
        store.complete(job_id, Some(result))?;
    } else {
        job.result(result)?;
        store.fail_with_data(
            job_id,
            format!("runner exec child exited with status {exit_code}"),
            Some(json!({ "reason": "runner_exec_exit_nonzero", "exit_code": exit_code })),
        )?;
    }
    Ok(())
}

fn io_error(error: io::Error, path: &Path) -> Error {
    Error::internal_io(error.to_string(), Some(path.display().to_string()))
}

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn redaction_record_holds_no_secret_value_and_still_redacts() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = ["s3cret".to_string(), "tok-é-long".to_string()];
        write_redaction_values(dir.path(), &secrets).unwrap();
        let on_disk = fs::read_to_string(dir.path().join(REDACTION_FILE)).unwrap();
        for secret in &secrets {
            assert!(
                !on_disk.contains(secret.as_str()),
                "secret value reached disk"
            );
        }
        let digests = read_redaction_digests(dir.path()).unwrap();
        assert_eq!(
            redact_digests("a s3cret b tok-é-long c s3cre", &digests, "[R]"),
            "a [R] b [R] c s3cre"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::api_jobs::{DaemonScope, JobEventKind, JobStatus, LocalChildStartDiscriminator};
    use crate::test_support::with_isolated_home;
    use std::time::Instant;

    fn scope(name: &str) -> DaemonScope {
        DaemonScope::from_state_dir(Path::new(&format!("/state/controllers/{name}")))
    }

    /// A child spawned the way the daemon spawns it, by a daemon that then
    /// died: its custody is released and its store handle is gone.
    fn spawn_then_lose_daemon(path: &Path, script: &str) -> (Uuid, Child) {
        spawn_then_lose_scoped_daemon(path, script, Some(scope("own")))
    }

    fn spawn_then_lose_scoped_daemon(
        path: &Path,
        script: &str,
        daemon_scope: Option<DaemonScope>,
    ) -> (Uuid, Child) {
        let store = JobStore::open_without_reconciliation(path)
            .unwrap()
            .with_daemon_lease("dead-daemon".to_string());
        let store = match daemon_scope {
            Some(daemon_scope) => store.with_daemon_scope(daemon_scope),
            None => store,
        };
        let job = store.create("runner.exec");
        store
            .reserve_local_child_at_with_runner_capacity(
                job.id,
                crate::api_jobs::timestamp_ms(),
                None,
            )
            .unwrap();
        let custody = RunnerExecCustody::acquire(job.id).unwrap();
        write_redaction_values(custody.dir(), &["s3cret".to_string()]).unwrap();
        let argv = ["/bin/sh", "-c", script].map(String::from);
        let (child, identity) = spawn(&mut command(custody.dir(), &argv).unwrap()).unwrap();
        store
            .start_with_reserved_child_identity(
                job.id,
                identity.pid,
                identity.process_group_id,
                LocalChildStartDiscriminator::LinuxProcStatStarttimeTicks {
                    ticks: identity.starttime_ticks.unwrap(),
                },
            )
            .unwrap();
        custody.abandon();
        (job.id, child)
    }

    /// The same daemon scope restarted in place: a new lease, the same scope.
    fn restarted_daemon(path: &Path) -> JobStore {
        JobStore::open_without_reconciliation(path)
            .unwrap()
            .with_daemon_lease("restarted-daemon".to_string())
            .with_daemon_scope(scope("own"))
    }

    fn wait_terminal(store: &JobStore, job_id: Uuid) -> crate::api_jobs::Job {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let job = store.get(job_id).unwrap();
            if job.status.is_terminal() {
                return job;
            }
            assert!(Instant::now() < deadline, "job never terminalized");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn result(store: &JobStore, job_id: Uuid) -> serde_json::Value {
        store
            .events(job_id)
            .unwrap()
            .into_iter()
            .rev()
            .find(|event| event.kind == JobEventKind::Result)
            .and_then(|event| event.data)
            .expect("result event")
    }

    #[test]
    fn restart_mid_run_re_adopts_the_child_without_a_second_spawn() {
        with_isolated_home(|home| {
            let path = home.path().join("jobs.sqlite");
            let counter = home.path().join("spawns");
            let script = format!(
                "echo spawn >> {}; echo 'out s3cret'; sleep 1; echo err >&2; exit 3",
                counter.display()
            );
            let (job_id, mut child) = spawn_then_lose_daemon(&path, &script);
            let store = restarted_daemon(&path);
            // The child's real parent is gone with the daemon; reap it as init would.
            let reaper = std::thread::spawn(move || child.wait());
            recover_runner_exec_jobs(&store);
            let job = wait_terminal(&store, job_id);
            reaper.join().unwrap().unwrap();

            assert_eq!(job.status, JobStatus::Failed);
            let result = result(&store, job_id);
            assert_eq!(result["exit_code"], 3);
            assert_eq!(
                result["stdout"],
                format!(
                    "out {}\n",
                    crate::redaction::RedactionPolicy::default().replacement()
                )
            );
            assert_eq!(result["stderr"], "err\n");
            assert_eq!(
                std::fs::read_to_string(&counter).unwrap().lines().count(),
                1,
                "re-adoption must not spawn the child again"
            );
            assert!(
                !job_dir(job_id).unwrap().exists(),
                "custody files are released"
            );
        });
    }

    #[test]
    fn cancelling_a_re_adopted_job_stops_its_process_tree() {
        with_isolated_home(|home| {
            let path = home.path().join("jobs.sqlite");
            let grandchild = home.path().join("grandchild");
            let script = format!("sleep 60 & echo $! > {}; wait", grandchild.display());
            let (job_id, mut child) = spawn_then_lose_daemon(&path, &script);
            let store = restarted_daemon(&path);
            let reaper = std::thread::spawn(move || child.wait());
            recover_runner_exec_jobs(&store);
            let deadline = Instant::now() + Duration::from_secs(10);
            let grandchild_pid = loop {
                if let Some(pid) = std::fs::read_to_string(&grandchild)
                    .ok()
                    .and_then(|pid| pid.trim().parse::<u32>().ok())
                {
                    break pid;
                }
                assert!(Instant::now() < deadline, "grandchild never started");
                std::thread::sleep(Duration::from_millis(20));
            };

            store.cancel(job_id, "operator cancelled").unwrap();
            reaper.join().unwrap().unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while crate::process::linux_process_starttime_ticks(grandchild_pid)
                .unwrap()
                .is_some()
                && !std::fs::read_to_string(format!("/proc/{grandchild_pid}/stat"))
                    .unwrap_or_default()
                    .contains(") Z ")
            {
                assert!(
                    Instant::now() < deadline,
                    "grandchild survived cancellation"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(store.get(job_id).unwrap().status, JobStatus::Cancelled);
        });
    }

    #[test]
    fn a_child_that_exited_while_no_daemon_ran_gets_its_recorded_status() {
        with_isolated_home(|home| {
            let path = home.path().join("jobs.sqlite");
            let (recorded, mut child) = spawn_then_lose_daemon(&path, "echo done");
            child.wait().unwrap();
            let (unrecorded, mut child) = spawn_then_lose_daemon(&path, "true");
            child.wait().unwrap();
            fs::remove_file(job_dir(unrecorded).unwrap().join(EXIT_FILE)).unwrap();

            let store = restarted_daemon(&path);
            recover_runner_exec_jobs(&store);

            let job = wait_terminal(&store, recorded);
            assert_eq!(job.status, JobStatus::Succeeded);
            assert_eq!(result(&store, recorded)["exit_code"], 0);
            assert_eq!(result(&store, recorded)["stdout"], "done\n");
            let job = wait_terminal(&store, unrecorded);
            assert_eq!(job.status, JobStatus::Failed);
            let error = store
                .events(unrecorded)
                .unwrap()
                .into_iter()
                .find(|event| event.kind == JobEventKind::Error)
                .and_then(|event| event.data)
                .unwrap();
            assert_eq!(error["reason"], "runner_exec_exit_unrecorded");
        });
    }
    /// Another controller's daemon shares the ledger but never adopts this
    /// scope's child, even while its owner is down and custody is free
    /// (#16104). Nor does any daemon adopt a scope-less (pre-scope) child.
    /// The owning scope, restarted with a new lease, adopts it exactly once.
    #[test]
    fn only_the_owning_scope_re_adopts_a_child() {
        with_isolated_home(|home| {
            let path = home.path().join("jobs.sqlite");
            let counter = home.path().join("spawns");
            let script = format!("echo spawn >> {}; sleep 1; exit 0", counter.display());
            let (job_id, mut child) = spawn_then_lose_daemon(&path, &script);
            let (legacy_id, mut legacy_child) =
                spawn_then_lose_scoped_daemon(&path, "sleep 1; exit 0", None);
            let reaper = std::thread::spawn(move || child.wait());
            let legacy_reaper = std::thread::spawn(move || legacy_child.wait());

            let other = JobStore::open_without_reconciliation(&path)
                .unwrap()
                .with_daemon_lease("other-controller-daemon".to_string())
                .with_daemon_scope(scope("other"));
            assert!(other.foreign_runner_exec_children().is_empty());
            recover_runner_exec_jobs(&other);
            assert!(other.adopt_runner_exec_job(job_id).is_err());
            let untouched = other.get(job_id).unwrap();
            assert_eq!(untouched.status, JobStatus::Running);
            assert_eq!(untouched.daemon_lease_id.as_deref(), Some("dead-daemon"));

            let store = restarted_daemon(&path);
            let foreign = store.foreign_runner_exec_children();
            assert_eq!(
                foreign.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(),
                vec![job_id],
                "the owning scope sees only its own child, never the scope-less one"
            );
            recover_runner_exec_jobs(&store);
            recover_runner_exec_jobs(&store);
            let job = wait_terminal(&store, job_id);
            reaper.join().unwrap().unwrap();
            legacy_reaper.join().unwrap().unwrap();

            assert_eq!(job.status, JobStatus::Succeeded);
            assert_eq!(
                std::fs::read_to_string(&counter).unwrap().lines().count(),
                1,
                "re-adoption must not spawn the child again"
            );
            assert_eq!(store.get(legacy_id).unwrap().status, JobStatus::Running);
        });
    }
}
