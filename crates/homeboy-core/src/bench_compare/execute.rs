//! Base/head materialization and the run-owned lifecycle of runner commands.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{
    diff_routes, evaluate_policy, summarize_routes, BenchCompareFailOn, BenchCompareLifecycle,
    BenchCompareOutput, BenchCompareSideReport, BenchCompareSideResults, BenchCompareStepLifecycle,
    ARTIFACT_FILE, ARTIFACT_SCHEMA,
};
use crate::process::{ProcessContainment, ProcessContainmentCleanupContext};
use crate::{Error, Result};

/// `--head` value selecting the checkout as it is on disk (uncommitted changes
/// included) instead of a committed revision. Cook gates use it: the gate runs
/// in the candidate checkout.
pub const WORKTREE_HEAD: &str = "WORKTREE";

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const TEARDOWN_GRACE: Duration = Duration::from_secs(5);

/// Everything one compare needs, fully resolved by the caller.
pub struct BenchCompareRequest<'a> {
    pub compare_id: String,
    pub component_id: String,
    /// Git toplevel of the component checkout.
    pub repo_root: PathBuf,
    /// Component directory relative to `repo_root` (empty for the root).
    pub component_relative: PathBuf,
    pub base: String,
    pub head: String,
    pub corpus: PathBuf,
    pub runner: Vec<String>,
    pub fail_on: BenchCompareFailOn,
    pub side_timeout: Duration,
    pub artifact_dir: PathBuf,
    /// Set by the caller's signal handlers; a set flag tears down the running
    /// side and finishes the compare as cancelled.
    pub stop: &'a AtomicBool,
}

/// Run both sides, diff them, apply the policy, and write the durable artifact.
///
/// Errors are reserved for problems that prevent a meaningful artifact (an
/// unresolvable revision, an unwritable artifact directory). Runner failures,
/// timeouts, cancellation, and surviving processes are recorded in the
/// artifact and fail its policy instead.
pub fn run_bench_compare(request: &BenchCompareRequest<'_>) -> Result<BenchCompareOutput> {
    let started_at = chrono::Utc::now().to_rfc3339();
    fs::create_dir_all(&request.artifact_dir).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "create bench compare artifact dir {}",
                request.artifact_dir.display()
            )),
        )
    })?;

    let base_sha = resolve_commit(&request.repo_root, &request.base)?;
    let head_sha = if request.head == WORKTREE_HEAD {
        resolve_commit(&request.repo_root, "HEAD").ok()
    } else {
        Some(resolve_commit(&request.repo_root, &request.head)?)
    };

    let mut lifecycle = BenchCompareLifecycle::default();

    let base = run_side(
        request,
        "base",
        &request.base,
        SideSource::Revision(base_sha),
        &mut lifecycle,
    );
    let head = if request.stop.load(Ordering::SeqCst) {
        lifecycle.cancelled = true;
        skipped_side(request, "head", &request.head, "cancelled before head ran")
    } else {
        let source = match &head_sha {
            Some(sha) if request.head != WORKTREE_HEAD => SideSource::Revision(sha.clone()),
            _ => SideSource::Worktree(head_sha.clone()),
        };
        run_side(request, "head", &request.head, source, &mut lifecycle)
    };
    if request.stop.load(Ordering::SeqCst) {
        lifecycle.cancelled = true;
    }

    let (base_report, base_results) = base;
    let (head_report, head_results) = head;
    let routes = match (&base_results, &head_results) {
        (Some(base), Some(head)) => diff_routes(base, head),
        _ => Vec::new(),
    };
    let summary = summarize_routes(&routes);
    let policy = evaluate_policy(
        request.fail_on,
        &summary,
        &base_report,
        &head_report,
        &lifecycle,
    );

    let artifact_path = request.artifact_dir.join(ARTIFACT_FILE);
    let output = BenchCompareOutput {
        status: if policy.passed { "passed" } else { "failed" }.to_string(),
        schema: ARTIFACT_SCHEMA.to_string(),
        compare_id: request.compare_id.clone(),
        component: request.component_id.clone(),
        corpus: request.corpus.display().to_string(),
        started_at,
        finished_at: chrono::Utc::now().to_rfc3339(),
        base: base_report,
        head: head_report,
        summary,
        policy,
        lifecycle,
        routes,
        artifact_dir: request.artifact_dir.display().to_string(),
        artifact_path: artifact_path.display().to_string(),
    };
    let bytes = serde_json::to_vec_pretty(&output)
        .map_err(|error| Error::internal_unexpected(format!("serialize bench compare: {error}")))?;
    fs::write(&artifact_path, bytes).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("write {}", artifact_path.display())),
        )
    })?;
    Ok(output)
}

enum SideSource {
    /// Materialize this commit as a detached git worktree owned by the run.
    Revision(String),
    /// Use the checkout on disk; the optional value is its `HEAD` commit.
    Worktree(Option<String>),
}

fn resolve_commit(repo_root: &Path, revision: &str) -> Result<String> {
    let spec = format!("{revision}^{{commit}}");
    crate::git::run_git(
        repo_root,
        &["rev-parse", "--verify", "--quiet", &spec],
        "resolve bench compare revision",
    )
    .map(|sha| sha.trim().to_string())
    .map_err(|_| {
        Error::validation_invalid_argument(
            "revision",
            format!(
                "cannot resolve `{revision}` to a commit in {}",
                repo_root.display()
            ),
            Some(revision.to_string()),
            Some(vec![
                "Pass a commit SHA, branch, or tag that exists in the component checkout."
                    .to_string(),
                "Fetch the revision first when the checkout is shallow or does not have it."
                    .to_string(),
            ]),
        )
    })
}

/// A private clone removed when dropped. No run shares a mutable ref namespace
/// with another run or changes the source repository's worktree registry.
pub(super) struct MaterializedWorktree {
    pub(super) path: PathBuf,
    _holder: tempfile::TempDir,
}

impl MaterializedWorktree {
    pub(super) fn create(repo_root: &Path, sha: &str, side: &str) -> Result<Self> {
        let holder = tempfile::Builder::new()
            .prefix(&format!("homeboy-bench-compare-{side}-"))
            .tempdir()
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some("create worktree dir".to_string()))
            })?;
        let path = holder.path().join("checkout");
        let path_arg = path.to_string_lossy().to_string();
        // Never mutate the source repository's refs, index or worktree registry.
        // --no-hardlinks also prevents workloads from modifying shared objects.
        crate::git::run_git(
            repo_root,
            &[
                "clone",
                "--no-hardlinks",
                "--no-checkout",
                "--quiet",
                "--",
                &repo_root.to_string_lossy(),
                &path_arg,
            ],
            "clone isolated bench compare repository",
        )?;
        crate::git::run_git(
            &path,
            &["checkout", "--detach", "--quiet", sha],
            "materialize bench compare revision",
        )?;
        Ok(Self {
            path,
            _holder: holder,
        })
    }

    pub(super) fn remove(&self) -> bool {
        fs::remove_dir_all(&self.path).is_ok()
    }
}

impl Drop for MaterializedWorktree {
    fn drop(&mut self) {
        if self.path.exists() {
            self.remove();
        }
    }
}

/// Copy tracked and non-ignored untracked files, preserving deletions. The
/// workload can mutate its candidate snapshot without touching another run.
fn overlay_worktree(source: &Path, destination: &Path) -> Result<()> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(source)
        .output()
        .map_err(|error| Error::internal_io(error.to_string(), None))?;
    if !output.status.success() {
        return Err(Error::internal_unexpected("list candidate snapshot files"));
    }
    for name in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        #[cfg(unix)]
        let name = {
            use std::os::unix::ffi::OsStrExt;
            Path::new(std::ffi::OsStr::from_bytes(name)).to_path_buf()
        };
        #[cfg(not(unix))]
        let name = PathBuf::from(String::from_utf8_lossy(name).as_ref());
        let from = source.join(&name);
        let to = destination.join(&name);
        let copy = || -> std::io::Result<()> {
            // Remove existing symlinks rather than following them during copy.
            if to
                .symlink_metadata()
                .is_ok_and(|metadata| !metadata.is_dir())
            {
                fs::remove_file(&to)?;
            }
            let metadata = match from.symlink_metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            #[cfg(unix)]
            if metadata.file_type().is_symlink() {
                return std::os::unix::fs::symlink(fs::read_link(&from)?, &to);
            }
            if metadata.is_file() {
                fs::copy(&from, &to)?;
            }
            Ok(())
        };
        copy().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("snapshot {}", from.display())),
            )
        })?;
    }
    Ok(())
}

fn side_dirs(request: &BenchCompareRequest<'_>, side: &str) -> (PathBuf, PathBuf) {
    let artifact_dir = request.artifact_dir.join(side);
    let results_path = artifact_dir.join("results.json");
    (artifact_dir, results_path)
}

fn skipped_side(
    request: &BenchCompareRequest<'_>,
    side: &str,
    requested_ref: &str,
    reason: &str,
) -> (BenchCompareSideReport, Option<BenchCompareSideResults>) {
    let (artifact_dir, results_path) = side_dirs(request, side);
    (
        BenchCompareSideReport {
            side: side.to_string(),
            requested_ref: requested_ref.to_string(),
            source_kind: "skipped".to_string(),
            results_path: results_path.display().to_string(),
            artifact_dir: artifact_dir.display().to_string(),
            error: Some(reason.to_string()),
            ..Default::default()
        },
        None,
    )
}

fn run_side(
    request: &BenchCompareRequest<'_>,
    side: &str,
    requested_ref: &str,
    source: SideSource,
    lifecycle: &mut BenchCompareLifecycle,
) -> (BenchCompareSideReport, Option<BenchCompareSideResults>) {
    let (artifact_dir, results_path) = side_dirs(request, side);
    let mut report = BenchCompareSideReport {
        side: side.to_string(),
        requested_ref: requested_ref.to_string(),
        results_path: results_path.display().to_string(),
        artifact_dir: artifact_dir.display().to_string(),
        ..Default::default()
    };
    if let Err(error) = fs::create_dir_all(&artifact_dir) {
        report.error = Some(format!("create {}: {error}", artifact_dir.display()));
        return (report, None);
    }

    // A reused output directory must not allow stale results to mask a skipped evaluator.
    if results_path.exists() {
        if let Err(error) = fs::remove_file(&results_path) {
            report.error = Some(format!("remove stale results: {error}"));
            return (report, None);
        }
    }
    let mut worktree = None;
    let checkout_root = match source {
        SideSource::Revision(sha) => {
            report.source_kind = "git_worktree".to_string();
            report.sha = Some(sha.clone());
            match MaterializedWorktree::create(&request.repo_root, &sha, side) {
                Ok(materialized) => {
                    lifecycle.worktrees_materialized += 1;
                    let root = materialized.path.clone();
                    worktree = Some(materialized);
                    root
                }
                Err(error) => {
                    report.error = Some(format!("materialize {sha}: {}", error.message));
                    return (report, None);
                }
            }
        }
        SideSource::Worktree(sha) => {
            report.source_kind = "worktree_snapshot".to_string();
            report.sha = sha;
            report.dirty = !crate::git::is_workdir_clean_or_not_git(&request.repo_root);
            match MaterializedWorktree::create(&request.repo_root, "HEAD", side).and_then(
                |materialized| {
                    overlay_worktree(&request.repo_root, &materialized.path)?;
                    Ok(materialized)
                },
            ) {
                Ok(materialized) => {
                    lifecycle.worktrees_materialized += 1;
                    let root = materialized.path.clone();
                    worktree = Some(materialized);
                    root
                }
                Err(error) => {
                    report.error = Some(format!("snapshot candidate: {}", error.message));
                    return (report, None);
                }
            }
        }
    };
    let source_dir = checkout_root.join(&request.component_relative);

    let env = side_env(
        request,
        side,
        &report,
        &source_dir,
        &artifact_dir,
        &results_path,
    );
    let deadline = Instant::now() + request.side_timeout;
    let mut passed = true;
    for (index, command) in request.runner.iter().enumerate() {
        let step = run_contained_step(
            command,
            index,
            &source_dir,
            &artifact_dir,
            &env,
            deadline,
            request.stop,
        );
        lifecycle.contained_steps += 1;
        if step.teardown_forced {
            lifecycle.forced_teardowns += 1;
        }
        if step.leftover_processes {
            lifecycle.leftover_process_scopes += 1;
        }
        if step.termination == "cancelled" {
            lifecycle.cancelled = true;
        }
        let succeeded = step.termination == "exited" && step.exit_code == Some(0);
        if !succeeded {
            report.error = Some(format!(
                "runner step {} `{}` {}{}",
                index + 1,
                command,
                step.termination,
                step.exit_code
                    .map(|code| format!(" with exit code {code}"))
                    .unwrap_or_default()
            ));
        }
        report.steps.push(step);
        if !succeeded {
            passed = false;
            break;
        }
    }

    if let Some(materialized) = worktree.take() {
        if materialized.remove() {
            lifecycle.worktrees_removed += 1;
        }
        drop(materialized);
    }

    if !passed {
        return (report, None);
    }
    match load_side_results(&results_path) {
        Ok(results) => {
            report.passed = true;
            report.route_count = results.routes.len();
            (report, Some(results))
        }
        Err(error) => {
            report.error = Some(error);
            (report, None)
        }
    }
}

fn side_env(
    request: &BenchCompareRequest<'_>,
    side: &str,
    report: &BenchCompareSideReport,
    source_dir: &Path,
    artifact_dir: &Path,
    results_path: &Path,
) -> Vec<(&'static str, String)> {
    use super::env;
    vec![
        (env::SIDE, side.to_string()),
        (env::REF, report.requested_ref.clone()),
        (env::SHA, report.sha.clone().unwrap_or_default()),
        (env::CORPUS, request.corpus.display().to_string()),
        (env::RESULTS, results_path.display().to_string()),
        (env::ARTIFACT_DIR, artifact_dir.display().to_string()),
        (env::SOURCE, source_dir.display().to_string()),
        (env::COMPONENT, request.component_id.clone()),
        (env::COMPARE_ID, request.compare_id.clone()),
    ]
}

/// Load and validate the document a side's runner wrote.
pub(super) fn load_side_results(
    path: &Path,
) -> std::result::Result<BenchCompareSideResults, String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "runner did not write results to {} ({error}); write a `{}` document to ${}",
            path.display(),
            super::SIDE_RESULTS_SCHEMA,
            super::env::RESULTS
        )
    })?;
    let results: BenchCompareSideResults = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid results in {}: {error}", path.display()))?;
    if let Some(schema) = results.schema.as_deref() {
        if schema != super::SIDE_RESULTS_SCHEMA {
            return Err(format!(
                "results in {} declare schema `{schema}`; expected `{}`",
                path.display(),
                super::SIDE_RESULTS_SCHEMA
            ));
        }
    }
    if results.routes.is_empty() {
        return Err("runner produced no evaluated routes".to_string());
    }
    let mut identities = std::collections::HashSet::new();
    if results
        .routes
        .iter()
        .any(|route| !identities.insert(&route.route))
    {
        return Err("runner produced duplicate route identities".to_string());
    }
    if let Some(empty) = results
        .routes
        .iter()
        .position(|r| r.route.trim().is_empty())
    {
        return Err(format!(
            "results in {} have an empty route identity at index {empty}",
            path.display()
        ));
    }
    Ok(results)
}

/// Run one runner command inside a run-owned containment boundary and tear
/// down everything it started, whatever way it ends.
pub(super) fn run_contained_step(
    command_text: &str,
    index: usize,
    source_dir: &Path,
    artifact_dir: &Path,
    env: &[(&'static str, String)],
    deadline: Instant,
    stop: &AtomicBool,
) -> BenchCompareStepLifecycle {
    let started = Instant::now();
    let mut step = BenchCompareStepLifecycle {
        command: command_text.to_string(),
        ..Default::default()
    };

    let log = |suffix: &str| artifact_dir.join(format!("step-{}.{suffix}.log", index + 1));
    let stdout = fs::File::create(log("stdout"));
    let stderr = fs::File::create(log("stderr"));
    let (stdout, stderr) = match (stdout, stderr) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        (Err(error), _) | (_, Err(error)) => {
            step.termination = "spawn_failed".to_string();
            step.detail = Some(format!("create runner logs: {error}"));
            return step;
        }
    };

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let mut command = Command::new(shell);
    command
        .arg("-lc")
        .arg(command_text)
        .current_dir(source_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (key, value) in env {
        command.env(key, value);
    }

    let mut containment = match ProcessContainment::prepare(&mut command) {
        Ok(containment) => containment,
        Err(error) => {
            step.termination = "spawn_failed".to_string();
            step.detail = Some(format!("prepare containment: {}", error.message));
            return step;
        }
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            step.termination = "spawn_failed".to_string();
            step.detail = Some(format!("spawn `sh -c`: {error}"));
            return step;
        }
    };
    if let Err(error) = containment.attach(&child) {
        let _ = child.kill();
        let _ = child.wait();
        step.termination = "spawn_failed".to_string();
        step.detail = Some(format!("attach containment: {}", error.message));
        return step;
    }
    step.process_group_id = containment.process_group_id();

    let mut termination = "exited";
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                step.detail = Some(format!("wait for runner: {error}"));
                termination = "wait_failed";
                break None;
            }
        }
        if stop.load(Ordering::SeqCst) {
            termination = "cancelled";
            break None;
        }
        if Instant::now() >= deadline {
            termination = "timed_out";
            break None;
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let exit = match exit {
        Some(status) => Some(status),
        None => {
            // The leader is still running: stop the whole boundary, then reap.
            if let Ok(cleanup) = containment.cleanup_with_grace(
                TEARDOWN_GRACE,
                ProcessContainmentCleanupContext::LeaderMayBeRunning,
            ) {
                step.teardown_forced |= cleanup.forced;
            }
            let _ = child.kill();
            child.wait().ok()
        }
    };
    step.termination = termination.to_string();
    step.exit_code = exit.and_then(|status| status.code());

    // The leader is reaped. Anything it left behind — backgrounded servers,
    // sites, browsers — still belongs to this run and is torn down here.
    let cleanup_complete = match containment.cleanup_with_grace(
        TEARDOWN_GRACE,
        ProcessContainmentCleanupContext::LeaderReaped,
    ) {
        Ok(cleanup) => {
            step.teardown_forced |= cleanup.forced;
            if let Some(detail) = cleanup.detail {
                step.detail = Some(detail);
            }
            cleanup.complete
        }
        Err(error) => {
            step.detail = Some(format!("teardown: {}", error.message));
            false
        }
    };
    let group_alive = step.process_group_id.is_some_and(|pgid| {
        homeboy_engine_primitives::command::process_group_is_running(pgid).unwrap_or(true)
    });
    step.leftover_processes = group_alive || !cleanup_complete;
    step.duration_ms = started.elapsed().as_millis() as u64;
    step
}
