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
    /// Executables required by the workload, checked before either side starts.
    pub required_tools: Vec<String>,
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
    // Use the same contained login shell for preflight and workload execution.
    let preflight_dir = request.artifact_dir.join("preflight");
    fs::create_dir_all(&preflight_dir).map_err(|e| {
        Error::internal_io(e.to_string(), Some("create preflight logs".to_string()))
    })?;
    let tools = request.required_tools.iter().map(|tool| {
        format!("command -v {} >/dev/null 2>&1 || {{ echo 'missing required tool:' {} >&2; exit 127; }}", shell_quote(tool), shell_quote(tool))
    }).collect::<Vec<_>>().join("; ");
    let preflight = run_contained_step(
        if tools.is_empty() { ":" } else { &tools },
        0,
        &request.repo_root,
        &preflight_dir,
        &[],
        Instant::now() + request.side_timeout,
        request.stop,
    );
    let preflight_error = if request.runner.is_empty() {
        Some("missing evaluator: no runner commands declared".to_string())
    } else if preflight.termination != "exited"
        || preflight.exit_code != Some(0)
        || preflight.leftover_processes
    {
        Some(format!(
            "tool preflight failed for {:?}: {} {:?}; see {}",
            request.required_tools,
            preflight.termination,
            preflight.exit_code,
            preflight_dir.display()
        ))
    } else {
        None
    };

    let base = if let Some(reason) = &preflight_error {
        skipped_side(request, "base", &request.base, reason)
    } else {
        run_side(
            request,
            "base",
            &request.base,
            SideSource::Revision(base_sha),
            &mut lifecycle,
        )
    };
    let head = if let Some(reason) = &preflight_error {
        skipped_side(request, "head", &request.head, reason)
    } else if request.stop.load(Ordering::SeqCst) {
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
    /// Materialize this commit in a private clone owned by the run.
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

/// A private clone removed when dropped; no shared worktree/ref bookkeeping.
struct MaterializedWorktree {
    path: PathBuf,
    _holder: tempfile::TempDir,
}

impl MaterializedWorktree {
    fn create(repo_root: &Path, sha: &str, side: &str) -> Result<Self> {
        let holder = tempfile::Builder::new()
            .prefix(&format!("homeboy-bench-compare-{side}-"))
            .tempdir()
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some("create worktree dir".to_string()))
            })?;
        let path = holder.path().join("checkout");
        let path_arg = path.to_string_lossy().to_string();
        // Never register worktrees or mutate refs in the component repository.
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
            "clone bench compare repository",
        )?;
        crate::git::run_git(
            &path,
            &["checkout", "--detach", "--quiet", sha],
            "checkout compare revision",
        )?;
        Ok(Self {
            path,
            _holder: holder,
        })
    }

    fn remove(&self) -> bool {
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// Copy the candidate, including untracked files and deletions, but never its
// git administration directory: commands must mutate only the owned clone.
fn copy_worktree(source: &Path, target: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(target)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    copy_candidate_files(source, target)
}

fn copy_candidate_files(source: &Path, target: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let destination = target.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            fs::create_dir(&destination)?;
            copy_candidate_files(&entry.path(), &destination)?;
        } else if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, destination)?;
            #[cfg(not(unix))]
            fs::copy(entry.path(), destination)?;
        } else {
            fs::copy(entry.path(), destination)?;
        }
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

    let mut worktree;
    let checkout_root = match source {
        SideSource::Revision(sha) => {
            report.source_kind = "git_clone".to_string();
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
            report.source_kind = "worktree".to_string();
            report.sha = sha;
            report.dirty = !crate::git::is_workdir_clean_or_not_git(&request.repo_root);
            match MaterializedWorktree::create(
                &request.repo_root,
                report.sha.as_deref().unwrap_or("HEAD"),
                side,
            ) {
                Ok(materialized) => {
                    if let Err(error) = copy_worktree(&request.repo_root, &materialized.path) {
                        report.error = Some(format!("snapshot candidate: {error}"));
                        return (report, None);
                    }
                    lifecycle.worktrees_materialized += 1;
                    let root = materialized.path.clone();
                    worktree = Some(materialized);
                    root
                }
                Err(error) => {
                    report.error = Some(error.message);
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
    // A reused output directory must not let stale evaluator output pass.
    if results_path.exists() {
        if let Err(error) = fs::remove_file(&results_path) {
            report.error = Some(format!("remove stale results: {error}"));
            return (report, None);
        }
    }
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
    let document: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid results in {}: {error}", path.display()))?;
    if document.get("passed").and_then(|v| v.as_bool()) == Some(false)
        || document.get("skipped").and_then(|v| v.as_bool()) == Some(true)
    {
        return Err(format!(
            "evaluator failed or skipped: {}",
            document.get("reason").unwrap_or(&serde_json::Value::Null)
        ));
    }
    for field in ["status", "evaluation_status"] {
        if let Some(status) = document.get(field).and_then(|value| value.as_str()) {
            if !matches!(status, "passed" | "success" | "completed") {
                return Err(format!(
                    "evaluator {field} is `{status}`: {}",
                    document.get("reason").unwrap_or(&serde_json::Value::Null)
                ));
            }
        }
    }
    let results: BenchCompareSideResults = serde_json::from_value(document)
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
        return Err("missing evaluator output: results contain no routes".to_string());
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

    let mut command =
        Command::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()));
    command
        .arg("-lc")
        // Profiles may cd; restore the owned checkout before running commands.
        .arg(format!(
            "cd {} && {{\n{}\n}}",
            shell_quote(&source_dir.to_string_lossy()),
            command_text
        ))
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
