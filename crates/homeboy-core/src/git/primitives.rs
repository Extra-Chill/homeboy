use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::engine::command;
use crate::error::{Error, GitCommandFailedDetails, Result};

use super::with_remote_tracking_authority_until;

fn git_command_display(args: &[&str]) -> String {
    if args.is_empty() {
        "git".to_string()
    } else {
        format!("git {}", args.join(" "))
    }
}

fn git_cwd_display(git_root: &Path) -> String {
    git_root.to_string_lossy().to_string()
}

fn git_failure_message(context: &str, detail: &str) -> String {
    if detail.trim().is_empty() {
        context.to_string()
    } else {
        format!("{} failed: {}", context, detail.trim())
    }
}

fn ensure_git_success(
    git_root: &Path,
    args: &[&str],
    context: &str,
    output: &std::process::Output,
) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    Err(Error::git_command_failed_with_details(
        git_failure_message(context, &detail),
        GitCommandFailedDetails {
            command: git_command_display(args),
            cwd: git_cwd_display(git_root),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            io_error: None,
        },
    ))
}

/// Clone a git repository to a target directory.
pub fn clone_repo(url: &str, target_dir: &Path) -> Result<()> {
    run_git(
        Path::new("."),
        &["clone", url, &target_dir.to_string_lossy()],
        "git clone",
    )?;
    Ok(())
}

/// Clone a git repository to a target directory and check out a requested ref.
pub fn clone_repo_at_ref(url: &str, target_dir: &Path, revision: Option<&str>) -> Result<()> {
    clone_repo(url, target_dir)?;

    if let Some(revision) = revision {
        run_git(
            target_dir,
            &["checkout", "--quiet", revision],
            "git checkout",
        )?;
    }

    Ok(())
}

/// Clone a git repository and optionally check out a ref, enforcing one deadline
/// across each remote Git stage.
pub fn clone_repo_at_ref_with_timeout(
    url: &str,
    target_dir: &Path,
    revision: Option<&str>,
    timeout: Duration,
) -> Result<()> {
    run_git_with_env_timeout(
        Path::new("."),
        &["clone", url, &target_dir.to_string_lossy()],
        "git clone",
        &[],
        timeout,
    )?;

    if let Some(revision) = revision {
        run_git_with_env_timeout(
            target_dir,
            &["checkout", "--quiet", revision],
            "git checkout",
            &[],
            timeout,
        )?;
    }

    Ok(())
}

/// Pull latest changes in a git repository.
pub fn pull_repo(repo_dir: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let output = fetch_and_merge_upstream_ff_only(repo_dir, deadline)?;
    ensure_git_success(
        repo_dir,
        &["merge", "--ff-only", "FETCH_HEAD"],
        "git merge --ff-only",
        &output,
    )?;
    Ok(())
}

/// Check if a git working directory has no uncommitted changes.
///
/// Uses direct Command execution to properly handle empty output (clean repo).
/// `run_in_optional` returns None for empty stdout, which would incorrectly
/// indicate a dirty repo when used with `.unwrap_or(false)`.
fn is_workdir_clean(path: &Path) -> bool {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output();

    match output {
        Ok(o) if o.status.success() => o.stdout.is_empty(),
        _ => false, // Command failed = assume not clean (conservative)
    }
}

/// Check if a path is either not a git worktree or is a clean git worktree.
pub fn is_workdir_clean_or_not_git(path: &Path) -> bool {
    let inside_tree = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(path)
        .output();

    match inside_tree {
        Ok(output) if output.status.success() => is_workdir_clean(path),
        _ => true,
    }
}

/// Run a git command in a repository and return stdout.
pub fn run_git(git_root: &Path, args: &[&str], context: &str) -> Result<String> {
    run_git_with_env(git_root, args, context, &[])
}

/// Run Git with an explicit transport environment.
///
/// The inherited process environment remains available, so repository-level
/// credential helpers, URL rewrites, and SSH configuration keep working.
pub fn run_git_with_env(
    git_root: &Path,
    args: &[&str],
    context: &str,
    env: &[(String, String)],
) -> Result<String> {
    let output = run_git_output_with_env(git_root, args, context, env)?;
    ensure_git_success(git_root, args, context, &output)?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Run Git with a deadline, terminating its isolated process group on expiry.
///
/// Remote Git operations can wait indefinitely for a transport or credential
/// helper. Callers use this only for bounded interactive command phases.
pub fn run_git_with_env_timeout(
    git_root: &Path,
    args: &[&str],
    context: &str,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<String> {
    let output = run_git_output_with_env_timeout(git_root, args, context, env, timeout)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(Error::git_command_failed_with_details(
            git_failure_message(context, &detail),
            GitCommandFailedDetails {
                command: git_command_display(args),
                cwd: git_cwd_display(git_root),
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                io_error: None,
            },
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Run Git with a deadline and preserve its exit status and captured output.
///
/// This supports callers that need command-specific diagnostics while still
/// sharing a caller-owned deadline with remote-tracking authority waits.
pub fn run_git_output_with_env_timeout(
    git_root: &Path,
    args: &[&str],
    context: &str,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<std::process::Output> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(git_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    command::isolate_process_tree(&mut command);
    let mut child = command.spawn().map_err(|error| {
        Error::git_command_failed_with_details(
            git_failure_message(context, &error.to_string()),
            GitCommandFailedDetails {
                command: git_command_display(args),
                cwd: git_cwd_display(git_root),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                io_error: Some(error.to_string()),
            },
        )
    })?;
    let started = Instant::now();
    let mut timed_out = false;
    let output = command::wait_with_bounded_output_until_cancelled(
        &mut child,
        command::DEFAULT_CAPTURE_LIMIT_BYTES,
        || {
            timed_out = started.elapsed() >= timeout;
            timed_out
        },
    )
    .map_err(|error| {
        Error::git_command_failed_with_details(
            git_failure_message(context, &error.to_string()),
            GitCommandFailedDetails {
                command: git_command_display(args),
                cwd: git_cwd_display(git_root),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                io_error: Some(error.to_string()),
            },
        )
    })?
    .into_output();
    if timed_out {
        return Err(Error::git_command_failed_with_details(
            format!(
                "{context} timed out after {}s; terminated child process group.",
                timeout.as_secs()
            ),
            GitCommandFailedDetails {
                command: git_command_display(args),
                cwd: git_cwd_display(git_root),
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                io_error: None,
            },
        ));
    }
    Ok(output)
}

/// Fetch remote refs while serializing the shared remote-tracking namespace of
/// linked worktrees. Waiting and Git execution consume one caller-owned budget.
pub fn fetch_remote_tracking_refs_until(
    git_root: &Path,
    args: &[&str],
    context: &str,
    env: &[(String, String)],
    deadline: Instant,
) -> Result<String> {
    debug_assert_eq!(args.first(), Some(&"fetch"));
    with_remote_tracking_authority_until(git_root, context, deadline, |remaining| {
        run_git_with_env_timeout(git_root, args, context, env, remaining)
    })
}

/// Fetch the current repository under remote-tracking authority, then merge its
/// already-fetched upstream ref without allowing Git to fetch a second time.
pub fn fetch_and_merge_upstream_ff_only(
    git_root: &Path,
    deadline: Instant,
) -> Result<std::process::Output> {
    with_remote_tracking_authority_until(git_root, "git pull", deadline, |_| {
        let branch = git_stdout_until(
            git_root,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            "resolve current branch for git pull",
            deadline,
        )?;
        let remote = git_stdout_until(
            git_root,
            &["config", "--get", &format!("branch.{branch}.remote")],
            "git upstream remote",
            deadline,
        )?;
        let upstream_ref = git_stdout_until(
            git_root,
            &["config", "--get", &format!("branch.{branch}.merge")],
            "git upstream ref",
            deadline,
        )?;
        run_git_with_env_timeout(
            git_root,
            &["fetch", &remote, &upstream_ref],
            &format!("git fetch {remote} {upstream_ref}"),
            &[],
            remaining_timeout(deadline)?,
        )?;
        run_git_output_with_env_timeout(
            git_root,
            &["merge", "--ff-only", "FETCH_HEAD"],
            "git merge --ff-only",
            &[],
            remaining_timeout(deadline)?,
        )
    })
}

/// Run a fetch-capable Git operation while holding the linked-worktree
/// remote-tracking authority for its full duration.
pub fn run_git_remote_tracking_operation_until(
    git_root: &Path,
    args: &[&str],
    context: &str,
    deadline: Instant,
) -> Result<std::process::Output> {
    with_remote_tracking_authority_until(git_root, context, deadline, |_| {
        run_git_output_with_env_timeout(git_root, args, context, &[], remaining_timeout(deadline)?)
    })
}

/// Run a git command in a repository and return raw output without treating
/// non-zero exit status as an error.
pub fn run_git_output(
    git_root: &Path,
    args: &[&str],
    context: &str,
) -> Result<std::process::Output> {
    run_git_output_with_env(git_root, args, context, &[])
}

/// Execute Git with component-scoped transport settings without including
/// environment values in the resulting command diagnostics.
pub fn run_git_output_with_env(
    git_root: &Path,
    args: &[&str],
    context: &str,
    env: &[(String, String)],
) -> Result<std::process::Output> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(git_root)
        .stdin(std::process::Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().map_err(|e| {
        Error::git_command_failed_with_details(
            git_failure_message(context, &e.to_string()),
            GitCommandFailedDetails {
                command: git_command_display(args),
                cwd: git_cwd_display(git_root),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                io_error: Some(e.to_string()),
            },
        )
    })
}

/// Stage all changes in a repository.
pub fn stage_all(git_root: &Path) -> Result<()> {
    run_git(git_root, &["add", "-A"], "git add -A")?;
    Ok(())
}

/// Return true when the index contains staged changes.
pub fn has_staged_changes(git_root: &Path) -> Result<bool> {
    let output = run_git_output(git_root, &["diff", "--cached", "--quiet"], "git diff")?;
    Ok(!output.status.success())
}

/// Commit staged changes with an explicit author string.
pub fn commit_staged_with_author(git_root: &Path, message: &str, author: &str) -> Result<()> {
    run_git(
        git_root,
        &["commit", "-m", message, "--author", author],
        "git commit",
    )?;
    Ok(())
}

/// Resolve the git remote name to use for release/deploy operations on a repo.
///
/// Homeboy core is framework-agnostic and must operate on repositories whose
/// remote is not named `origin` (forks, renamed remotes, Enterprise mirrors).
/// Resolution prefers `origin` when it exists (the overwhelmingly common case),
/// then falls back to the repository's sole configured remote, and finally to
/// the literal `"origin"` when nothing can be determined.
pub fn resolve_default_remote(path: &Path) -> String {
    let remotes: Vec<String> = run_git(path, &["remote"], "git remote")
        .ok()
        .map(|out| {
            out.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    if remotes.iter().any(|remote| remote == "origin") {
        return "origin".to_string();
    }
    if let [only] = remotes.as_slice() {
        return only.clone();
    }
    "origin".to_string()
}

/// Resolve the remote-qualified default branch ref of a repository, e.g.
/// `"origin/main"` or `"upstream/trunk"`.
///
/// Reads the resolved remote's `HEAD` symref first; when that is unset (common
/// on fresh clones that never ran `git remote set-head`), probes the well-known
/// default branch names against the resolved remote.
pub fn default_remote_branch(path: &Path) -> Option<String> {
    let remote = resolve_default_remote(path);
    let head_ref = format!("refs/remotes/{remote}/HEAD");

    if let Some(value) = run_git(
        path,
        &["symbolic-ref", "--quiet", "--short", &head_ref],
        "git default remote branch",
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
    {
        return Some(value);
    }

    ["main", "trunk", "master"].iter().find_map(|branch| {
        let candidate = format!("{remote}/{branch}");
        run_git(
            path,
            &["rev-parse", "--verify", "--quiet", &candidate],
            "git rev-parse",
        )
        .is_ok()
        .then_some(candidate)
    })
}

/// Resolve the bare default branch name of a repository, e.g. `"main"`,
/// stripping the remote prefix from [`default_remote_branch`].
pub fn default_branch_name(path: &Path) -> Option<String> {
    default_remote_branch(path).map(|reference| {
        reference
            .split_once('/')
            .map(|(_, branch)| branch.to_string())
            .unwrap_or(reference)
    })
}

/// Update a clean linked repo to the latest remote default-branch revision.
pub fn update_to_remote_default_branch(git_root: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    with_remote_tracking_authority_until(git_root, "update remote default branch", deadline, |_| {
        let remote = resolve_default_remote_until(git_root, deadline)?;
        let remote_branch = default_remote_branch_until(git_root, &remote, deadline)?;
        let (target_remote, target_ref, local_branch) = match remote_branch {
            Some(branch) => {
                let branch_name = branch
                    .rsplit_once('/')
                    .map(|(_, name)| name)
                    .unwrap_or(&branch);
                (
                    remote,
                    format!("refs/heads/{branch_name}"),
                    Some(branch_name.to_string()),
                )
            }
            None => {
                let (remote, reference) = configured_upstream_until(git_root, deadline)?;
                (remote, reference, None)
            }
        };
        run_git_with_env_timeout(
            git_root,
            &["fetch", &target_remote, &target_ref],
            &format!("git fetch {target_remote} {target_ref}"),
            &[],
            remaining_timeout(deadline)?,
        )?;
        if let Some(local_branch) = local_branch {
            if run_git_with_env_timeout(
                git_root,
                &["switch", &local_branch],
                "git switch default branch",
                &[],
                remaining_timeout(deadline)?,
            )
            .is_err()
            {
                run_git_with_env_timeout(
                    git_root,
                    &["switch", "--detach", "FETCH_HEAD"],
                    "git switch detached default branch",
                    &[],
                    remaining_timeout(deadline)?,
                )?;
            }
        }
        run_git_with_env_timeout(
            git_root,
            &["merge", "--ff-only", "FETCH_HEAD"],
            "git merge default branch --ff-only",
            &[],
            remaining_timeout(deadline)?,
        )?;
        Ok(())
    })
}

fn remaining_timeout(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| Error::git_command_failed("git operation deadline exhausted"))
}

fn git_stdout_until(
    git_root: &Path,
    args: &[&str],
    context: &str,
    deadline: Instant,
) -> Result<String> {
    run_git_with_env_timeout(git_root, args, context, &[], remaining_timeout(deadline)?)
        .map(|value| value.trim().to_string())
}

fn resolve_default_remote_until(git_root: &Path, deadline: Instant) -> Result<String> {
    let remotes = git_stdout_until(git_root, &["remote"], "git remote", deadline)?;
    let remotes: Vec<_> = remotes
        .lines()
        .filter(|remote| !remote.is_empty())
        .collect();
    if remotes.contains(&"origin") {
        Ok("origin".to_string())
    } else if let [remote] = remotes.as_slice() {
        Ok((*remote).to_string())
    } else {
        Ok("origin".to_string())
    }
}

fn default_remote_branch_until(
    git_root: &Path,
    remote: &str,
    deadline: Instant,
) -> Result<Option<String>> {
    let head_ref = format!("refs/remotes/{remote}/HEAD");
    let output = run_git_output_with_env_timeout(
        git_root,
        &["symbolic-ref", "--quiet", "--short", &head_ref],
        "git default remote branch",
        &[],
        remaining_timeout(deadline)?,
    )?;
    if output.status.success() {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !value.is_empty() {
            return Ok(Some(value));
        }
    }
    for branch in ["main", "trunk", "master"] {
        let candidate = format!("{remote}/{branch}");
        let output = run_git_output_with_env_timeout(
            git_root,
            &["rev-parse", "--verify", "--quiet", &candidate],
            "git rev-parse",
            &[],
            remaining_timeout(deadline)?,
        )?;
        if output.status.success() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn configured_upstream_until(git_root: &Path, deadline: Instant) -> Result<(String, String)> {
    let branch = git_stdout_until(
        git_root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        "resolve current branch for configured upstream",
        deadline,
    )?;
    let remote = git_stdout_until(
        git_root,
        &["config", "--get", &format!("branch.{branch}.remote")],
        "git upstream remote",
        deadline,
    )?;
    let reference = git_stdout_until(
        git_root,
        &["config", "--get", &format!("branch.{branch}.merge")],
        "git upstream ref",
        deadline,
    )?;
    Ok((remote, reference))
}

/// List all git-tracked markdown files in a directory.
/// Uses `git ls-files` to respect .gitignore and only include tracked/staged files.
/// Returns relative paths from the repository root.
pub(crate) fn list_tracked_markdown_files(path: &Path) -> Result<Vec<String>> {
    let stdout = run_git(
        path,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "*.md",
        ],
        "git ls-files",
    )?;

    Ok(stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

pub fn is_git_repo(path: &str) -> bool {
    command::succeeded_in(path, "git", &["rev-parse", "--git-dir"])
}

/// Report whether `relative` (a repo-relative path) is committed/tracked in the
/// git repository rooted at (or containing) `repo_dir`.
///
/// Returns `false` when the path is gitignored, untracked, or the directory is
/// not a git repository. Uses `git ls-files --error-unmatch`, which only
/// succeeds for paths recorded in the index.
pub fn is_tracked_path(repo_dir: &Path, relative: &str) -> bool {
    command::succeeded_in(
        &repo_dir.to_string_lossy(),
        "git",
        &["ls-files", "--error-unmatch", "--", relative],
    )
}

/// Get the git repository root directory from any path within the repo.
pub fn get_git_root(path: &str) -> Result<String> {
    run_git(
        Path::new(path),
        &["rev-parse", "--show-toplevel"],
        "git root",
    )
    .map(|s| s.trim().to_string())
}

/// Resolve a repository root without allowing Git to outlive an interactive
/// caller's deadline.
pub fn get_git_root_with_timeout(path: &str, timeout: Duration) -> Result<String> {
    run_git_with_env_timeout(
        Path::new(path),
        &["rev-parse", "--show-toplevel"],
        "git root",
        &[],
        timeout,
    )
    .map(|root| root.trim().to_string())
}

/// Normalize a path into a directory suitable for probing git provenance.
///
/// Git commands need a directory to run inside; when the caller hands us a file
/// path (e.g. a toolchain binary or component artifact), probe its parent
/// directory instead. Directories (and files with no parent) are returned
/// unchanged.
pub fn git_probe_path(path: &Path) -> std::path::PathBuf {
    if path.is_file() {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

/// Compute the relative path prefix of a component within a monorepo.
///
/// If `local_path` is a subdirectory of the git root, returns the relative path
/// (e.g. "frontend" for `/repo/frontend`). Returns None if local_path IS the
/// git root (not a monorepo component).
pub fn get_component_path_prefix(local_path: &str) -> Option<String> {
    let git_root = get_git_root(local_path).ok()?;
    let root = std::path::Path::new(&git_root).canonicalize().ok()?;
    let component = std::path::Path::new(local_path).canonicalize().ok()?;

    if root == component {
        return None; // Not a monorepo — component IS the repo root
    }

    component
        .strip_prefix(&root)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::*;
    use crate::git::primitives_query::current_branch;

    use crate::test_support::run_git_command as git;

    #[test]
    fn run_git_failure_includes_command_cwd_exit_stdout_and_stderr() {
        let dir = tempfile::tempdir().expect("tempdir");

        let err = run_git(dir.path(), &["rev-parse", "--show-toplevel"], "git root")
            .expect_err("non-repo git command should fail");

        assert_eq!(err.code.as_str(), "git.command_failed");
        assert_eq!(err.details["command"], "git rev-parse --show-toplevel");
        assert_eq!(err.details["cwd"], dir.path().to_string_lossy().to_string());
        assert!(err.details["exit_code"].as_i64().is_some());
        assert!(err.details["stdout"].as_str().is_some());
        assert!(err.details["stderr"].as_str().is_some());
    }

    #[test]
    fn get_git_root_io_failure_keeps_git_diagnostics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("missing");

        let err = get_git_root(&missing.to_string_lossy())
            .expect_err("missing cwd should report git command failure details");

        assert_eq!(err.code.as_str(), "git.command_failed");
        assert_ne!(err.message, "IO error");
        assert_eq!(err.details["command"], "git rev-parse --show-toplevel");
        assert_eq!(err.details["cwd"], missing.to_string_lossy().to_string());
        assert!(err.details["exit_code"].is_null());
        assert!(err.details["io_error"].as_str().is_some());
        assert_eq!(err.details["stdout"], "");
        assert_eq!(err.details["stderr"], "");
    }

    #[test]
    fn bounded_git_command_terminates_hung_child_process_group() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        let child_pid = dir.path().join("hung-child.pid");
        git(
            dir.path(),
            &[
                "config",
                "alias.hang",
                &format!(
                    "!sh -c 'sleep 10 & echo $! > {}; wait'",
                    child_pid.display()
                ),
            ],
        );

        let started = Instant::now();
        let err = run_git_with_env_timeout(
            dir.path(),
            &["hang"],
            "test hung Git phase",
            &[],
            Duration::from_millis(50),
        )
        .expect_err("hung Git alias should time out");

        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(err.details["stderr"]
            .as_str()
            .is_some_and(|detail| detail.contains("timed out")));
        let pid: i32 = std::fs::read_to_string(&child_pid)
            .expect("hung child records its pid")
            .trim()
            .parse()
            .expect("numeric pid");
        assert!(
            !crate::process::pid_is_running(pid as u32),
            "timeout must terminate every subprocess in the Git process group"
        );
    }

    #[test]
    fn resolve_default_remote_prefers_origin_then_sole_remote() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();
        git(path, &["init", "-q"]);

        // No remotes configured — fall back to the conventional "origin".
        assert_eq!(resolve_default_remote(path), "origin");

        // A single non-origin remote resolves to that remote.
        git(
            path,
            &["remote", "add", "upstream", "https://example.test/x.git"],
        );
        assert_eq!(resolve_default_remote(path), "upstream");

        // Once origin exists it is preferred even alongside other remotes.
        git(
            path,
            &["remote", "add", "origin", "https://example.test/y.git"],
        );
        assert_eq!(resolve_default_remote(path), "origin");
    }

    #[test]
    fn default_branch_resolves_through_a_non_origin_remote() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let remote = tmp.path().join("remote.git");
        let seed = tmp.path().join("seed");
        let clone = tmp.path().join("clone");

        git(
            tmp.path(),
            &["init", "--bare", "-b", "main", remote.to_str().unwrap()],
        );
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        git(&seed, &["config", "user.email", "t@x.test"]);
        git(&seed, &["config", "user.name", "T"]);
        git(&seed, &["config", "commit.gpgsign", "false"]);
        std::fs::write(seed.join("f.txt"), "x").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-q", "-m", "init"]);
        git(&seed, &["push", "-q", "origin", "main"]);

        // Fresh clone records refs/remotes/origin/HEAD; rename it so the remote
        // is deliberately NOT named origin.
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        git(&clone, &["remote", "rename", "origin", "upstream"]);

        assert_eq!(resolve_default_remote(&clone), "upstream");
        assert_eq!(
            default_remote_branch(&clone).as_deref(),
            Some("upstream/main")
        );
        assert_eq!(default_branch_name(&clone).as_deref(), Some("main"));
    }

    #[test]
    fn update_to_remote_default_branch_fast_forwards_from_fetched_tracking_ref() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let remote = tmp.path().join("remote.git");
        let seed = tmp.path().join("seed");
        let checkout = tmp.path().join("checkout");
        git(
            tmp.path(),
            &["init", "--bare", "-b", "main", remote.to_str().unwrap()],
        );
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        git(&seed, &["config", "user.email", "t@x.test"]);
        git(&seed, &["config", "user.name", "T"]);
        std::fs::write(seed.join("f.txt"), "one\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-qm", "initial"]);
        git(&seed, &["push", "-q", "origin", "main"]);
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        let fetches = tmp.path().join("fetches");
        let upload_pack = tmp.path().join("count-upload-pack");
        std::fs::write(
            &upload_pack,
            format!(
                "#!/bin/sh\nprintf fetch >> {}\nexec git upload-pack \"$@\"\n",
                fetches.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&upload_pack, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(
            &checkout,
            &[
                "config",
                "remote.origin.uploadpack",
                upload_pack.to_str().unwrap(),
            ],
        );
        std::fs::write(seed.join("f.txt"), "two\n").unwrap();
        git(&seed, &["commit", "-qam", "advance"]);
        git(&seed, &["push", "-q", "origin", "main"]);
        let expected = run_git(&seed, &["rev-parse", "HEAD"], "seed head").unwrap();

        update_to_remote_default_branch(&checkout).expect("fast forward checkout");

        assert_eq!(
            run_git(&checkout, &["rev-parse", "HEAD"], "checkout head").unwrap(),
            expected
        );
        assert_eq!(
            std::fs::read_to_string(fetches).unwrap().lines().count(),
            1,
            "the update must merge the authority-fetched tracking ref without a second fetch"
        );
    }

    #[test]
    fn pull_repo_fetches_configured_non_default_upstream_with_restricted_refspec() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let origin = tmp.path().join("origin.git");
        let upstream = tmp.path().join("upstream.git");
        let seed = tmp.path().join("seed");
        let checkout = tmp.path().join("checkout");
        git(
            tmp.path(),
            &["init", "--bare", "-b", "main", origin.to_str().unwrap()],
        );
        git(
            tmp.path(),
            &["init", "--bare", "-b", "topic", upstream.to_str().unwrap()],
        );
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        git(&seed, &["config", "user.email", "t@x.test"]);
        git(&seed, &["config", "user.name", "T"]);
        std::fs::write(seed.join("f.txt"), "one\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-qm", "initial"]);
        git(&seed, &["push", "-q", "origin", "main"]);
        git(
            &seed,
            &["remote", "add", "upstream", upstream.to_str().unwrap()],
        );
        git(&seed, &["switch", "-qc", "topic"]);
        std::fs::write(seed.join("f.txt"), "two\n").unwrap();
        git(&seed, &["commit", "-qam", "topic"]);
        git(&seed, &["push", "-q", "upstream", "topic"]);
        let expected = run_git(&seed, &["rev-parse", "HEAD"], "seed head").unwrap();
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        git(
            &checkout,
            &["remote", "add", "upstream", upstream.to_str().unwrap()],
        );
        git(&checkout, &["config", "branch.main.remote", "upstream"]);
        git(
            &checkout,
            &["config", "branch.main.merge", "refs/heads/topic"],
        );
        git(
            &checkout,
            &[
                "config",
                "remote.upstream.fetch",
                "+refs/heads/main:refs/remotes/upstream/main",
            ],
        );
        git(
            &checkout,
            &["remote", "set-url", "origin", "missing-origin.git"],
        );
        let fetches = tmp.path().join("fetches");
        let upload_pack = tmp.path().join("count-upload-pack");
        std::fs::write(
            &upload_pack,
            format!(
                "#!/bin/sh\nprintf fetch >> {}\nexec git upload-pack \"$@\"\n",
                fetches.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&upload_pack, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(
            &checkout,
            &[
                "config",
                "remote.upstream.uploadpack",
                upload_pack.to_str().unwrap(),
            ],
        );

        pull_repo(&checkout).expect("pull configured upstream");

        assert_eq!(
            run_git(&checkout, &["rev-parse", "HEAD"], "checkout head").unwrap(),
            expected
        );
        assert!(
            run_git(
                &checkout,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "refs/remotes/upstream/topic"
                ],
                "upstream tracking ref",
            )
            .is_err(),
            "the restricted fetch refspec must leave the topic without a tracking ref"
        );
        assert_eq!(std::fs::read_to_string(fetches).unwrap().lines().count(), 1);
    }

    #[test]
    fn update_to_remote_default_branch_fallback_merges_without_a_second_fetch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let remote = tmp.path().join("remote.git");
        let seed = tmp.path().join("seed");
        let checkout = tmp.path().join("checkout");
        git(
            tmp.path(),
            &["init", "--bare", "-b", "feature", remote.to_str().unwrap()],
        );
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        git(&seed, &["config", "user.email", "t@x.test"]);
        git(&seed, &["config", "user.name", "T"]);
        std::fs::write(seed.join("f.txt"), "one\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-qm", "initial"]);
        git(&seed, &["push", "-q", "origin", "feature"]);
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        // Preferable origin is unavailable and has no conventional default;
        // the configured non-origin upstream must be selected before fetching.
        git(&checkout, &["remote", "rename", "origin", "upstream"]);
        git(
            &checkout,
            &[
                "remote",
                "add",
                "origin",
                tmp.path().join("missing-origin.git").to_str().unwrap(),
            ],
        );
        let fetches = tmp.path().join("fetches");
        let upload_pack = tmp.path().join("count-upload-pack");
        std::fs::write(
            &upload_pack,
            format!(
                "#!/bin/sh\nprintf fetch >> {}\nexec git upload-pack \"$@\"\n",
                fetches.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&upload_pack, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(
            &checkout,
            &[
                "config",
                "remote.upstream.uploadpack",
                upload_pack.to_str().unwrap(),
            ],
        );
        std::fs::write(seed.join("f.txt"), "two\n").unwrap();
        git(&seed, &["commit", "-qam", "advance"]);
        git(&seed, &["push", "-q", "origin", "feature"]);
        let expected = run_git(&seed, &["rev-parse", "HEAD"], "seed head").unwrap();

        update_to_remote_default_branch(&checkout).expect("fallback update");

        assert_eq!(
            run_git(&checkout, &["rev-parse", "HEAD"], "checkout head").unwrap(),
            expected
        );
        assert_eq!(std::fs::read_to_string(fetches).unwrap().lines().count(), 1);
    }

    #[test]
    fn update_to_remote_default_branch_detaches_when_the_local_default_branch_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let remote = tmp.path().join("remote.git");
        let seed = tmp.path().join("seed");
        let checkout = tmp.path().join("checkout");
        git(
            tmp.path(),
            &["init", "--bare", "-b", "main", remote.to_str().unwrap()],
        );
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        git(&seed, &["config", "user.email", "t@x.test"]);
        git(&seed, &["config", "user.name", "T"]);
        std::fs::write(seed.join("f.txt"), "one\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-qm", "initial"]);
        git(&seed, &["push", "-q", "origin", "main"]);
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        git(&checkout, &["switch", "--detach"]);
        let parked = tmp.path().join("parked-main");
        // Make `main` unavailable in this checkout so the update takes its
        // detached tracking-ref fallback rather than Git's auto-track path.
        git(
            &checkout,
            &["worktree", "add", "-q", parked.to_str().unwrap(), "main"],
        );
        std::fs::write(seed.join("f.txt"), "two\n").unwrap();
        git(&seed, &["commit", "-qam", "advance"]);
        git(&seed, &["push", "-q", "origin", "main"]);
        let expected = run_git(&seed, &["rev-parse", "HEAD"], "seed head").unwrap();

        update_to_remote_default_branch(&checkout).expect("detached fallback update");

        assert_eq!(current_branch(&checkout), None);
        assert_eq!(
            run_git(&checkout, &["rev-parse", "HEAD"], "checkout head").unwrap(),
            expected
        );
    }

    #[test]
    fn update_to_remote_default_branch_waits_for_remote_tracking_authority() {
        let _env_lock = crate::test_support::env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let repository = dir.path();
        git(repository, &["init", "-q", "-b", "main"]);
        git(repository, &["config", "user.email", "t@x.test"]);
        git(repository, &["config", "user.name", "T"]);
        std::fs::write(repository.join("f.txt"), "x\n").unwrap();
        git(repository, &["add", "."]);
        git(repository, &["commit", "-qm", "initial"]);
        git(
            repository,
            &[
                "remote",
                "add",
                "origin",
                dir.path().join("missing.git").to_str().unwrap(),
            ],
        );
        let (locked, ready) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let locked_repository = repository.to_path_buf();
        let holder = thread::spawn(move || {
            with_remote_tracking_authority_until(
                &locked_repository,
                "test lock holder",
                Instant::now() + Duration::from_secs(2),
                |_| {
                    locked.send(()).unwrap();
                    released.recv().expect("release authority");
                    Ok(())
                },
            )
            .unwrap();
        });
        ready.recv().expect("authority acquired");

        let attempted = dir.path().join("contender-attempted-authority");
        let _attempted = crate::test_support::EnvVarGuard::set(
            "HOMEB0Y_REMOTE_TRACKING_FETCH_LOCK_ATTEMPTED",
            &attempted,
        );
        let (done, completed) = mpsc::channel();
        let contender_repository = repository.to_path_buf();
        let contender = thread::spawn(move || {
            done.send(update_to_remote_default_branch(&contender_repository))
                .expect("report contender result");
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !attempted.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(attempted.exists(), "contender did not attempt authority");
        assert!(
            completed.try_recv().is_err(),
            "contender proceeded before authority release"
        );

        release.send(()).expect("release holder");
        holder.join().unwrap();
        let _ = completed
            .recv()
            .expect("contender completes after authority release")
            .expect_err("missing remote fails");
        contender.join().unwrap();
    }

    #[test]
    fn fetch_and_merge_upstream_uses_the_caller_deadline_for_authority_wait() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repository = dir.path();
        git(repository, &["init", "-q", "-b", "main"]);
        git(repository, &["config", "user.email", "t@x.test"]);
        git(repository, &["config", "user.name", "T"]);
        std::fs::write(repository.join("f.txt"), "x\n").unwrap();
        git(repository, &["add", "."]);
        git(repository, &["commit", "-qm", "initial"]);
        git(repository, &["config", "branch.main.remote", "origin"]);
        git(
            repository,
            &["config", "branch.main.merge", "refs/heads/main"],
        );

        let (locked, ready) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let holder_repository = repository.to_path_buf();
        let holder = thread::spawn(move || {
            with_remote_tracking_authority_until(
                &holder_repository,
                "test lock holder",
                Instant::now() + Duration::from_secs(2),
                |_| {
                    locked.send(()).unwrap();
                    released.recv().expect("release authority");
                    Ok(())
                },
            )
            .unwrap();
        });
        ready.recv().expect("authority acquired");

        let started = Instant::now();
        let error = fetch_and_merge_upstream_ff_only(
            repository,
            Instant::now() + Duration::from_millis(50),
        )
        .expect_err("authority wait exhausts the pull deadline");
        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(error.message.contains("deadline"));

        release.send(()).expect("release holder");
        holder.join().unwrap();
    }
}
