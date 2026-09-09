//! Changed-since git file scoping.
//!
//! Given a repository path and a base git ref, return the set of files that
//! changed on the current branch relative to that ref — including uncommitted
//! staged, unstaged, and untracked files — so lint/test/audit scopes can target
//! only what a change actually touched. Handles shallow CI clones by
//! progressively deepening until the merge base is reachable.
//!
//! This is a std-only git primitive (raw `git` subprocess + `homeboy_error`)
//! shared by the audit engine, the refactor planner, and the extension
//! lint/test scopers. It lives in `homeboy-engine-primitives` so those consumers
//! — including a future `homeboy-code-audit` crate — depend on the slim
//! primitives base rather than all of `homeboy-core` for changed-file scoping.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use homeboy_error::{Error, Result};

use crate::command;
use crate::git_remote_tracking_authority::with_remote_tracking_authority_until;

/// Run a git subcommand in `path`, returning the raw process output.
fn execute_git(path: &str, args: &[&str]) -> std::io::Result<Output> {
    Command::new("git").args(args).current_dir(path).output()
}

/// Run Git until `deadline`, terminating its process group if a transport or
/// credential helper stalls.
fn execute_git_until(path: &str, args: &[&str], deadline: Instant) -> Result<Output> {
    execute_git_until_with_program(path, args, Path::new("git"), || deadline)
}

fn execute_git_until_with_program(
    path: &str,
    args: &[&str],
    program: &Path,
    deadline: impl FnOnce() -> Instant,
) -> Result<Output> {
    let mut process = Command::new(program);
    process
        .args(args)
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command::isolate_process_tree(&mut process);
    let mut child = process
        .spawn()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    let deadline = deadline();
    let mut timed_out = false;
    let output = command::wait_with_bounded_output_until_cancelled(
        &mut child,
        command::DEFAULT_CAPTURE_LIMIT_BYTES,
        || {
            timed_out = Instant::now() >= deadline;
            timed_out
        },
    )
    .map_err(|error| Error::git_command_failed(error.to_string()))?
    .into_output();
    if timed_out {
        return Err(Error::git_command_failed(
            "git shallow-clone fetch deadline exhausted; terminated child process group",
        ));
    }
    Ok(output)
}

/// Get the files that changed on the current branch relative to `git_ref`.
///
/// Combines the committed merge-base diff (`<ref>...HEAD`) with staged,
/// unstaged, and untracked working-tree files, filtered to added/copied/
/// modified/renamed paths (deletions are excluded so scopes only reference
/// existing files). Handles shallow clones by deepening to reach the merge base.
pub fn get_files_changed_since(path: &str, git_ref: &str) -> Result<Vec<String>> {
    // Ensure the ref's ancestry is reachable (handles shallow CI clones).
    ensure_ancestry_for_ref(path, git_ref)?;

    // Triple-dot (merge-base diff) — shows only changes on the current
    // branch, not changes on the ref's branch.
    let output = execute_git(
        path,
        &[
            "diff",
            "--name-only",
            "--diff-filter=ACMR",
            &format!("{}...HEAD", git_ref),
        ],
    )
    .map_err(|e| Error::git_command_failed(e.to_string()))?;

    if output.status.success() {
        let mut files: BTreeSet<String> = parse_diff_output(&output.stdout).into_iter().collect();
        files.extend(get_working_tree_files_for_changed_since(path)?);
        return Ok(files.into_iter().collect());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(Error::git_command_failed(format!(
        "git diff {}...HEAD failed: {}",
        git_ref,
        stderr.trim()
    )))
}

/// Get staged, unstaged, and untracked files that should participate in a
/// changed-since scope before they are committed.
///
/// Uses the same add/copy/modify/rename filter as the committed diff path, so
/// deleted files are not returned to lint/test scopes that need existing files.
fn get_working_tree_files_for_changed_since(path: &str) -> Result<Vec<String>> {
    // Best-effort index refresh keeps stat-only touches from surfacing as dirty.
    let _ = execute_git(path, &["update-index", "--refresh"]);

    let mut files: BTreeSet<String> = BTreeSet::new();

    for args in [
        vec!["diff", "--name-only", "--diff-filter=ACMR"],
        vec!["diff", "--cached", "--name-only", "--diff-filter=ACMR"],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        let output =
            execute_git(path, &args).map_err(|e| Error::git_command_failed(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::git_command_failed(format!(
                "git {} failed: {}",
                args.join(" "),
                stderr.trim()
            )));
        }
        files.extend(parse_diff_output(&output.stdout));
    }

    Ok(files.into_iter().collect())
}

/// Check whether the repo is a shallow clone.
fn is_shallow_repo(path: &str) -> bool {
    execute_git(path, &["rev-parse", "--is-shallow-repository"])
        .ok()
        .and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                None
            }
        })
        .map(|s| s == "true")
        .unwrap_or(false)
}

/// Check whether `git merge-base <ref> HEAD` succeeds (the ref's ancestry
/// is reachable from HEAD).
fn has_merge_base(path: &str, git_ref: &str) -> bool {
    execute_git(path, &["merge-base", git_ref, "HEAD"])
        .ok()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Resolve the default remote of a repository: prefer `origin`, else a sole
/// remote, else fall back to `origin`.
fn resolve_default_remote(path: &str) -> String {
    let remotes = remote_names(path);

    if remotes.iter().any(|remote| remote == "origin") {
        return "origin".to_string();
    }
    if let [only] = remotes.as_slice() {
        return only.clone();
    }
    "origin".to_string()
}

fn remote_names(path: &str) -> Vec<String> {
    execute_git(path, &["remote"])
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn remote_and_ref(path: &str, git_ref: &str) -> (String, String) {
    let remote_ref = git_ref.strip_prefix("refs/remotes/").unwrap_or(git_ref);
    if let Some((remote, reference)) = remote_ref.split_once('/') {
        if !reference.is_empty() && remote_names(path).iter().any(|name| name == remote) {
            return (remote.to_string(), reference.to_string());
        }
    }

    (resolve_default_remote(path), git_ref.to_string())
}

fn fetch_until(path: &str, args: &[&str], deadline: Instant) -> Result<()> {
    let output = execute_git_until(path, args, deadline)?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(Error::git_command_failed(format!(
        "git {} failed: {}",
        args.join(" "),
        stderr.trim()
    )))
}

/// In shallow clones, the merge base between a ref and HEAD may not be
/// reachable. This function progressively deepens the repository until the
/// merge base is available.
///
/// Deepening strategy: 50 → 200 → full unshallow. This matches what CI
/// environments typically need — most PRs have <50 commits, so the first
/// deepen usually suffices.
///
/// Returns an error if the merge base cannot be resolved after all attempts.
///
/// Exposed so callers that resolve the merge base themselves (e.g. core's
/// `resolve_merge_base`) can reuse the same shallow-clone deepening strategy.
pub fn ensure_ancestry_for_ref(path: &str, git_ref: &str) -> Result<()> {
    // Fast path: merge base already reachable (full clone or sufficient depth).
    if has_merge_base(path, git_ref) {
        return Ok(());
    }

    // Only deepen if this is actually a shallow clone. In a full clone,
    // a missing merge base means the ref itself is invalid — deepening won't help.
    if !is_shallow_repo(path) {
        return Err(Error::git_command_failed(format!(
            "Cannot resolve merge base for {git_ref}: ref is not reachable and repository is not shallow (is the ref valid?)"
        )));
    }

    eprintln!("Shallow clone detected — deepening to resolve merge base for {git_ref}");
    let deadline = Instant::now() + Duration::from_secs(30);
    with_remote_tracking_authority_until(
        std::path::Path::new(path),
        "deepen shallow clone",
        deadline,
        |_| {
            // Fetch the ref itself if it's not already present.
            let (remote, reference) = remote_and_ref(path, git_ref);
            let tracking_ref = reference.strip_prefix("refs/heads/").unwrap_or(&reference);
            let refspec = format!("{reference}:refs/remotes/{remote}/{tracking_ref}");
            fetch_until(path, &["fetch", &remote, &refspec, "--depth=50"], deadline)?;

            // Progressive deepening: try increasingly generous depths.
            for depth in &["50", "200"] {
                fetch_until(path, &["fetch", "--deepen", depth], deadline)?;
                if has_merge_base(path, git_ref) {
                    eprintln!("Merge base found after deepening by {depth} commits");
                    return Ok(());
                }
            }

            // Last resort: full unshallow.
            eprintln!("Merge base not found with depth 200, unshallowing repository");
            fetch_until(path, &["fetch", "--unshallow"], deadline)?;

            if has_merge_base(path, git_ref) {
                eprintln!("Merge base found after full unshallow");
                Ok(())
            } else {
                Err(Error::git_command_failed(format!(
                    "Cannot resolve merge base for {git_ref} even after full unshallow — the ref may not exist in the remote"
                )))
            }
        },
    )
}

/// Parse newline-delimited `git diff --name-only` output into a file list.
fn parse_diff_output(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn get_files_changed_since_includes_dirty_and_untracked_files() {
        use std::fs;
        use std::process::Command;
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().to_str().unwrap();

        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .output();
        if init.is_err() || !init.unwrap().status.success() {
            return;
        }
        let _ = Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output();
        let _ = Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(path)
            .output();

        fs::write(dir.path().join("tracked.txt"), "initial\n").expect("write tracked");
        fs::write(dir.path().join("staged.txt"), "initial\n").expect("write staged");
        fs::write(dir.path().join("deleted.txt"), "initial\n").expect("write deleted");
        let _ = Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output();
        let _ = Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(path)
            .output();

        fs::write(dir.path().join("tracked.txt"), "dirty\n").expect("modify tracked");
        fs::write(dir.path().join("staged.txt"), "staged dirty\n").expect("modify staged");
        fs::write(dir.path().join("untracked.txt"), "new\n").expect("write untracked");
        fs::remove_file(dir.path().join("deleted.txt")).expect("delete tracked");
        let _ = Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(path)
            .output();

        let files = get_files_changed_since(path, "HEAD").expect("changed files");

        assert!(
            files.contains(&"tracked.txt".to_string()),
            "unstaged tracked file included: {files:?}"
        );
        assert!(
            files.contains(&"staged.txt".to_string()),
            "staged tracked file included: {files:?}"
        );
        assert!(
            files.contains(&"untracked.txt".to_string()),
            "untracked file included: {files:?}"
        );
        assert!(
            !files.contains(&"deleted.txt".to_string()),
            "deleted file excluded: {files:?}"
        );
    }

    #[test]
    fn remote_qualified_refs_use_their_configured_remote() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().to_str().expect("utf-8 path");
        execute_git(path, &["init", "-q"]).expect("initialize repository");
        execute_git(
            path,
            &["remote", "add", "origin", "https://origin.invalid/repo.git"],
        )
        .expect("configure origin");
        execute_git(
            path,
            &[
                "remote",
                "add",
                "upstream",
                "https://upstream.invalid/repo.git",
            ],
        )
        .expect("configure upstream");

        assert_eq!(
            remote_and_ref(path, "upstream/main"),
            ("upstream".to_string(), "main".to_string())
        );
        assert_eq!(
            remote_and_ref(path, "refs/remotes/upstream/main"),
            ("upstream".to_string(), "main".to_string())
        );
    }

    #[test]
    fn shallow_clone_fetches_remote_qualified_ref_from_its_named_remote() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let source = dir.path().join("source");
        let remote = dir.path().join("remote.git");
        let checkout = dir.path().join("checkout");
        std::fs::create_dir(&source).expect("create source");
        let source_path = source.to_str().expect("utf-8 source path");
        execute_git(source_path, &["init", "-q", "-b", "main"]).expect("initialize source");
        execute_git(source_path, &["config", "user.email", "test@example.com"])
            .expect("configure author email");
        execute_git(source_path, &["config", "user.name", "test"]).expect("configure author name");
        std::fs::write(source.join("base.txt"), "base\n").expect("write base commit");
        execute_git(source_path, &["add", "."]).expect("stage base commit");
        execute_git(source_path, &["commit", "-qm", "base"]).expect("commit base");
        execute_git(source_path, &["switch", "-qc", "feature"]).expect("create feature branch");
        std::fs::write(source.join("feature.txt"), "feature\n").expect("write feature commit");
        execute_git(source_path, &["add", "."]).expect("stage feature commit");
        execute_git(source_path, &["commit", "-qm", "feature"]).expect("commit feature");
        execute_git(
            source_path,
            &[
                "init",
                "--bare",
                "-q",
                remote.to_str().expect("utf-8 remote path"),
            ],
        )
        .expect("initialize remote");
        execute_git(
            source_path,
            &[
                "remote",
                "add",
                "upstream",
                remote.to_str().expect("utf-8 remote path"),
            ],
        )
        .expect("configure upstream remote");
        execute_git(source_path, &["push", "-q", "upstream", "main", "feature"])
            .expect("push source branches");

        let clone = Command::new("git")
            .args([
                "clone",
                "--depth=1",
                "--branch",
                "feature",
                &format!("file://{}", remote.display()),
                checkout.to_str().expect("utf-8 checkout path"),
            ])
            .output()
            .expect("clone shallow checkout");
        assert!(clone.status.success(), "shallow clone must succeed");
        let checkout_path = checkout.to_str().expect("utf-8 checkout path");
        execute_git(checkout_path, &["remote", "rename", "origin", "upstream"])
            .expect("rename checkout remote");
        execute_git(
            checkout_path,
            &["remote", "add", "origin", "file:///missing/origin.git"],
        )
        .expect("configure default remote");

        ensure_ancestry_for_ref(checkout_path, "upstream/main")
            .expect("fetch named remote ref and resolve merge base");
        assert!(has_merge_base(checkout_path, "upstream/main"));
    }

    #[test]
    fn failed_fetch_status_is_returned_as_an_error() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().to_str().expect("utf-8 path");
        execute_git(path, &["init", "-q"]).expect("initialize repository");
        execute_git(
            path,
            &["remote", "add", "origin", "file:///missing/repository.git"],
        )
        .expect("configure missing remote");

        let error = fetch_until(
            path,
            &["fetch", "origin", "main", "--depth=50"],
            Instant::now() + Duration::from_secs(5),
        )
        .expect_err("a failed fetch status must be returned");

        assert!(error
            .message
            .contains("git fetch origin main --depth=50 failed"));
    }

    #[cfg(unix)]
    #[test]
    fn deadline_terminates_a_stalled_git_process_group() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().to_str().expect("utf-8 path");
        let git = dir.path().join("git");
        let ready_file = dir.path().join("helper-ready");
        let release_file = dir.path().join("start-stall");
        let pid_file = dir.path().join("descendant.pid");
        let script = format!(
            "#!/bin/sh\ntouch {}\nwhile [ ! -f {} ]; do sleep 0.01; done\nsleep 30 &\necho $! > {}\nwait\n",
            crate::shell::quote_path(&ready_file.display().to_string()),
            crate::shell::quote_path(&release_file.display().to_string()),
            crate::shell::quote_path(&pid_file.display().to_string())
        );
        std::fs::write(&git, script).expect("write stalled git");
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))
            .expect("make stalled git executable");

        let mut deadline_started = None;
        let error = execute_git_until_with_program(path, &["fetch", "origin"], &git, || {
            wait_for_file(&ready_file);
            std::fs::write(&release_file, "start stalled helper").expect("release stalled helper");
            wait_for_file(&pid_file);
            let started = Instant::now();
            deadline_started = Some(started);
            started + Duration::from_secs(1)
        })
        .expect_err("stalled fetch must exhaust its deadline");

        assert!(deadline_started.expect("deadline started").elapsed() < Duration::from_secs(2));
        assert!(error.message.contains("deadline exhausted"));
        let descendant_pid = std::fs::read_to_string(&pid_file)
            .expect("descendant pid")
            .trim()
            .parse::<u32>()
            .expect("numeric descendant pid");
        assert!(
            !command::process_is_running(descendant_pid),
            "deadline left descendant {descendant_pid} runnable"
        );
    }

    #[cfg(unix)]
    fn wait_for_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "stalled Git helper did not record {}",
                path.display()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
