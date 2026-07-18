use super::*;
use crate::{
    Runner, RunnerActiveJobState, RunnerExecMode, RunnerExecOutput, RunnerKind, RunnerRequiredTool,
    RunnerSessionState, RunnerStaleDaemonWarning, RunnerStatusReport,
};
use homeboy_core::build_identity;
use homeboy_core::server::RunnerSettings;
use homeboy_core::Result;
use homeboy_upgrade::upgrade::current_version;
use homeboy_upgrade::upgrade::ExtensionUpgradeEntry;
use homeboy_upgrade::upgrade::InstallMethod;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

thread_local! {
    static LOCAL_VERSION_OVERRIDE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Returns the thread-local local-version override, if one is active.
pub(super) fn local_version_override() -> Option<String> {
    LOCAL_VERSION_OVERRIDE.with(|cell| cell.borrow().clone())
}

/// RAII guard that pins the local/current homeboy version for the current
/// thread, restoring the previous value on drop. Used by runner-upgrade
/// tests so their hardcoded fixture versions stay comparable against a
/// stable "local" version regardless of the live crate version.
pub(super) struct LocalVersionGuard {
    previous: Option<String>,
}

impl LocalVersionGuard {
    fn set(version: &str) -> Self {
        let previous =
            LOCAL_VERSION_OVERRIDE.with(|cell| cell.borrow_mut().replace(version.to_string()));
        Self { previous }
    }
}

impl Drop for LocalVersionGuard {
    fn drop(&mut self) {
        LOCAL_VERSION_OVERRIDE.with(|cell| *cell.borrow_mut() = self.previous.take());
    }
}

/// Pins the local version low enough that no fixture runner version below the
/// live crate version is treated as drift. Tests that mock a runner reporting
/// versions like `0.228.x` use this so the version-drift guard added in #5566
/// does not spuriously skip them once the crate version climbs higher.
pub(super) fn pin_local_version_for_fixtures() -> LocalVersionGuard {
    LocalVersionGuard::set("0.0.0")
}

pub(super) fn extension_update(extension_id: &str, source_revision: &str) -> ExtensionUpgradeEntry {
    ExtensionUpgradeEntry {
        extension_id: extension_id.to_string(),
        old_version: "1.0.0".to_string(),
        new_version: "1.0.0".to_string(),
        linked: true,
        source_path: Some(format!(
            "/Users/user/Developer/homeboy-extensions/{extension_id}"
        )),
        git_root: Some("/Users/user/Developer/homeboy-extensions".to_string()),
        source_url: Some("https://github.com/Extra-Chill/homeboy-extensions.git".to_string()),
        source_revision: Some(source_revision.to_string()),
        source_update: Default::default(),
    }
}

pub(super) fn runner_status(runner_id: &str) -> Result<RunnerStatusReport> {
    Ok(RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected: false,
        state: RunnerSessionState::Disconnected,
        session: None,
        stale_daemon: None,
        daemon_freshness: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_jobs: Vec::new(),
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::NotQueried,
        active_job_source: None,
        active_job_error: None,
        active_job_recovery_evidence: None,
        session_path: "/tmp/homeboy-runner-session.json".to_string(),
    })
}

pub(super) fn stale_runner_status(runner_id: &str) -> Result<RunnerStatusReport> {
    Ok(RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected: true,
        state: RunnerSessionState::Connected,
        session: None,
        stale_daemon: Some(RunnerStaleDaemonWarning::new(
            runner_id,
            "0.228.4".to_string(),
            "0.228.5".to_string(),
            None,
            None,
        )),
        daemon_freshness: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_jobs: Vec::new(),
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::NotQueried,
        active_job_source: None,
        active_job_error: None,
        active_job_recovery_evidence: None,
        session_path: "/tmp/homeboy-runner-session.json".to_string(),
    })
}

pub(super) fn ssh_runner(id: &str, homeboy_path: Option<&str>) -> Runner {
    Runner {
        id: id.to_string(),
        kind: RunnerKind::Ssh,
        server_id: Some(format!("{id}-server")),
        workspace_root: Some("/home/user/workspace".to_string()),
        settings: RunnerSettings {
            homeboy_path: homeboy_path.map(str::to_string),
            ..Default::default()
        },
        env: HashMap::new(),
        secret_env: HashMap::new(),
        resources: HashMap::new(),
        policy: Default::default(),
    }
}

pub(super) fn git_source_checkout() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("README.md"), "test\n").expect("write readme");
    run_git(dir.path(), &["init"]);
    run_git(
        dir.path(),
        &["config", "user.email", "homeboy@example.test"],
    );
    run_git(dir.path(), &["config", "user.name", "Homeboy Test"]);
    // Disable commit signing for the throwaway checkout. CI/dev environments
    // that set `commit.gpgsign = true` globally would otherwise fail the
    // commit (or leave the tree in a state where `rev-parse`/`status`
    // misbehave), causing `source_checkout_build_identity` to return `None`
    // and panic the caller's `.unwrap()`.
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["config", "tag.gpgsign", "false"]);
    run_git(dir.path(), &["add", "README.md"]);
    run_git(
        dir.path(),
        &["commit", "--no-gpg-sign", "-m", "Initial commit"],
    );
    dir
}

pub(super) struct RemoteSourceFixture {
    root: tempfile::TempDir,
    seed: PathBuf,
    origin: PathBuf,
}

pub(super) fn remote_source_fixture() -> RemoteSourceFixture {
    let root = tempfile::tempdir().expect("tempdir");
    let seed = root.path().join("seed");
    let origin = root.path().join("origin.git");

    std::fs::create_dir_all(&seed).expect("mkdir seed");
    std::fs::write(seed.join("README.md"), "initial\n").expect("write readme");
    run_git(&seed, &["init", "-b", "main"]);
    configure_test_git_identity(&seed);
    run_git(&seed, &["add", "README.md"]);
    run_git(&seed, &["commit", "--no-gpg-sign", "-m", "Initial commit"]);
    run_git(
        root.path(),
        &[
            "clone",
            "--bare",
            seed.to_str().unwrap(),
            origin.to_str().unwrap(),
        ],
    );
    run_git(
        &seed,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );

    RemoteSourceFixture { root, seed, origin }
}

pub(super) fn configure_test_git_identity(path: &Path) {
    run_git(path, &["config", "user.email", "homeboy@example.test"]);
    run_git(path, &["config", "user.name", "Homeboy Test"]);
    run_git(path, &["config", "commit.gpgsign", "false"]);
    run_git(path, &["config", "tag.gpgsign", "false"]);
}

pub(super) fn add_remote_commit(seed: &Path, message: &str) -> String {
    let file = seed.join("README.md");
    let mut content = std::fs::read_to_string(&file).expect("read readme");
    content.push_str(message);
    content.push('\n');
    std::fs::write(&file, content).expect("write readme");
    run_git(seed, &["add", "README.md"]);
    run_git(seed, &["commit", "--no-gpg-sign", "-m", message]);
    run_git(seed, &["push", "origin", "main"]);
    git_stdout(seed, &["rev-parse", "HEAD"])
}

pub(super) fn run_source_prepare_script(path: &Path) {
    let output = Command::new("sh")
        .arg("-lc")
        .arg(runner_source_checkout_prepare_script())
        .current_dir(path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run source prepare script");
    assert!(
        output.status.success(),
        "source prepare script failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(super) fn run_git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        // Isolate from ambient global/system git config so the throwaway
        // checkout behaves deterministically regardless of the host
        // environment (e.g. dubious-ownership safe.directory checks or
        // global signing settings).
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn exec_output(
    runner_id: &str,
    argv: Vec<String>,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
) -> RunnerExecOutput {
    RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: runner_id.to_string(),
        dry_run: false,
        mode: RunnerExecMode::DiagnosticSsh,
        argv,
        remote_cwd: "/home/user/workspace".to_string(),
        exit_code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        source_snapshot: None,
        job: None,
        runner_job: None,
        job_id: None,
        job_events: None,
        mirror_run_id: None,
        patch: None,
        mutation_artifacts: None,
        artifacts: Vec::new(),
        promoted_outputs: Vec::new(),
        structured_summaries: Vec::new(),
        metrics: None,
        capture: None,
        execution_record: None,
        runner_result: None,
        handoff: None,
        diagnostics: None,
    }
}

mod part_a;
mod part_b;
mod part_c;
