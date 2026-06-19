use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::engine::command;
use crate::core::error::{Error, GitCommandFailedDetails, Result};

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

/// Pull latest changes in a git repository.
pub fn pull_repo(repo_dir: &Path) -> Result<()> {
    run_git(repo_dir, &["pull"], "git pull")?;
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
    let output = run_git_output(git_root, args, context)?;

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

/// Run a git command in a repository and return raw output without treating
/// non-zero exit status as an error.
pub fn run_git_output(
    git_root: &Path,
    args: &[&str],
    context: &str,
) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(git_root)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| {
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

/// Resolve a git revision to its commit/object id, returning None when the ref
/// cannot be resolved.
pub fn rev_parse(git_root: &Path, git_ref: &str) -> Option<String> {
    output_optional(git_root, &["rev-parse", git_ref])
}

/// Run a git command and return stdout bytes when the command succeeds.
pub fn output_optional_bytes(git_root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(git_root)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    output.status.success().then_some(output.stdout)
}

/// Run a git command and return trimmed stdout when the command succeeds and is non-empty.
pub fn output_optional(git_root: &Path, args: &[&str]) -> Option<String> {
    let output = output_optional_bytes(git_root, args)?;
    let value = String::from_utf8_lossy(&output).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Get the full HEAD commit SHA from a git directory.
pub fn head_sha(git_root: &Path) -> Option<String> {
    output_optional(git_root, &["rev-parse", "HEAD"])
}

/// Get the short HEAD commit SHA from a git directory.
pub fn head_sha_short(git_root: &Path) -> Option<String> {
    output_optional(git_root, &["rev-parse", "--short", "HEAD"])
}

/// Get porcelain status bytes from a git directory.
pub fn status_porcelain_bytes(git_root: &Path) -> Option<Vec<u8>> {
    output_optional_bytes(git_root, &["status", "--porcelain=v1", "-z"])
}

/// Get porcelain status text from a git directory.
pub fn status_porcelain(git_root: &Path) -> Option<String> {
    output_optional_bytes(git_root, &["status", "--porcelain=v1"])
        .map(|output| String::from_utf8_lossy(&output).to_string())
}

/// Get a remote URL from a git directory.
pub fn remote_url(git_root: &Path, remote: &str) -> Option<String> {
    output_optional(git_root, &["remote", "get-url", remote])
}

/// Get the git repository root directory from any path within the repo.
pub fn toplevel(git_root: &Path) -> Option<String> {
    output_optional(git_root, &["rev-parse", "--show-toplevel"])
}

/// Get the git repository root directory from any path within the repo.
pub fn repo_root(path: &Path) -> Option<PathBuf> {
    toplevel(path).map(PathBuf::from)
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

pub fn current_branch(git_root: &Path) -> Option<String> {
    output_optional(git_root, &["branch", "--show-current"])
}

pub fn remote_origin_url(git_root: &Path) -> Option<String> {
    remote_url(git_root, "origin")
}

fn default_remote_branch(git_root: &Path) -> Option<String> {
    run_git(
        git_root,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
        "git default remote branch",
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
}

/// Update a clean linked repo to the latest remote default-branch revision.
pub fn update_to_remote_default_branch(git_root: &Path) -> Result<()> {
    let old_branch = current_branch(git_root);
    run_git(git_root, &["fetch", "origin"], "git fetch origin")?;
    let mut detached_default_branch: Option<String> = None;

    if let Some(remote_branch) = default_remote_branch(git_root) {
        let local_branch = remote_branch
            .strip_prefix("origin/")
            .unwrap_or(&remote_branch)
            .to_string();

        if old_branch.as_deref() != Some(local_branch.as_str())
            && run_git(
                git_root,
                &["switch", &local_branch],
                "git switch default branch",
            )
            .is_err()
        {
            run_git(
                git_root,
                &["switch", "--detach", &remote_branch],
                "git switch detached default branch",
            )?;
            detached_default_branch = Some(local_branch);
        }
    }

    if let Some(branch) = detached_default_branch {
        run_git(
            git_root,
            &["pull", "--ff-only", "origin", &branch],
            "git pull detached default branch --ff-only",
        )?;
    } else {
        run_git(git_root, &["pull", "--ff-only"], "git pull --ff-only")?;
    }

    Ok(())
}

/// Get the short HEAD revision from a git directory.
pub fn short_head_revision(dir: &Path) -> Option<String> {
    head_sha_short(dir)
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

pub(crate) fn is_git_repo(path: &str) -> bool {
    command::succeeded_in(path, "git", &["rev-parse", "--git-dir"])
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
/// (e.g. "wordpress" for `/repo/wordpress`). Returns None if local_path IS the
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
    use super::*;
    use std::process::Command;

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run git test fixture command");

        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

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
    fn optional_helpers_return_head_remote_toplevel_and_clean_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "--quiet"]);
        git(
            dir.path(),
            &["remote", "add", "origin", "https://example.test/repo.git"],
        );
        std::fs::write(dir.path().join("README.md"), "hello\n").expect("write fixture file");
        git(dir.path(), &["add", "README.md"]);
        git(
            dir.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );

        assert_eq!(
            Path::new(&toplevel(dir.path()).expect("git toplevel"))
                .canonicalize()
                .expect("canonical git toplevel"),
            dir.path().canonicalize().expect("canonical fixture dir")
        );
        assert_eq!(
            remote_url(dir.path(), "origin").as_deref(),
            Some("https://example.test/repo.git")
        );
        assert!(head_sha(dir.path()).is_some());
        assert!(head_sha_short(dir.path()).is_some());
        assert_eq!(status_porcelain(dir.path()).as_deref(), Some(""));
        assert_eq!(
            status_porcelain_bytes(dir.path()).as_deref(),
            Some(&b""[..])
        );
    }
}
