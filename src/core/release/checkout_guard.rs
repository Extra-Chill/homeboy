use std::collections::BTreeSet;

use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::git;

#[derive(Debug, Clone)]
pub(super) struct ReleaseCheckoutGuard {
    path: String,
    original_ref: OriginalRef,
    original_head: String,
    original_untracked: BTreeSet<String>,
}

#[derive(Debug, Clone)]
enum OriginalRef {
    Branch(String),
    Detached,
}

impl ReleaseCheckoutGuard {
    pub(super) fn capture(component: &Component) -> Result<Option<Self>> {
        if !git::is_git_repo(&component.local_path) {
            return Ok(None);
        }

        let changes = git::get_uncommitted_changes(&component.local_path)?;
        if !changes.staged.is_empty() || !changes.unstaged.is_empty() {
            return Err(dirty_checkout_error(
                changes
                    .staged
                    .iter()
                    .chain(changes.unstaged.iter())
                    .cloned()
                    .collect(),
            ));
        }

        let original_ref = current_ref(&component.local_path)?;
        let original_head = git_stdout(&component.local_path, &["rev-parse", "HEAD"])?;
        let original_untracked = changes.untracked.into_iter().collect();

        Ok(Some(Self {
            path: component.local_path.clone(),
            original_ref,
            original_head,
            original_untracked,
        }))
    }

    pub(super) fn restore_after_failure(&self) -> Result<()> {
        abort_in_progress_operations(&self.path);
        run_git_checked(&self.path, &["reset", "--hard"])?;
        remove_new_untracked(&self.path, &self.original_untracked)?;

        match &self.original_ref {
            OriginalRef::Branch(branch) => {
                run_git_checked(&self.path, &["checkout", "-q", branch])?
            }
            OriginalRef::Detached => {
                run_git_checked(&self.path, &["checkout", "-q", &self.original_head])?
            }
        }

        run_git_checked(&self.path, &["reset", "--hard", &self.original_head])?;
        remove_new_untracked(&self.path, &self.original_untracked)?;
        Ok(())
    }
}

fn current_ref(path: &str) -> Result<OriginalRef> {
    let output = git_output(path, &["symbolic-ref", "--short", "HEAD"])?;
    if output.status.success() {
        return Ok(OriginalRef::Branch(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ));
    }
    Ok(OriginalRef::Detached)
}

fn abort_in_progress_operations(path: &str) {
    for args in [
        &["merge", "--abort"][..],
        &["rebase", "--abort"][..],
        &["cherry-pick", "--abort"][..],
    ] {
        let _ = git_output(path, args);
    }
}

fn remove_new_untracked(path: &str, original_untracked: &BTreeSet<String>) -> Result<()> {
    let changes = git::get_uncommitted_changes(path)?;
    for file in changes.untracked {
        if original_untracked.contains(&file) {
            continue;
        }
        run_git_checked(path, &["clean", "-fd", "--", &file])?;
    }
    Ok(())
}

fn dirty_checkout_error(files: Vec<String>) -> Error {
    Error::validation_invalid_argument(
        "working_tree",
        "Uncommitted tracked changes detected before release checkout guard captured state",
        None,
        Some(vec![
            "Commit, stash, or discard tracked changes before releasing".to_string(),
            format!(
                "Dirty tracked files ({}): {}{}",
                files.len(),
                files
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                if files.len() > 10 { ", ..." } else { "" }
            ),
        ]),
    )
}

fn git_stdout(path: &str, args: &[&str]) -> Result<String> {
    let output = git_output(path, args)?;
    if !output.status.success() {
        return Err(git_failure(args, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_git_checked(path: &str, args: &[&str]) -> Result<()> {
    let output = git_output(path, args)?;
    if output.status.success() {
        return Ok(());
    }
    Err(git_failure(args, &output))
}

fn git_output(path: &str, args: &[&str]) -> Result<std::process::Output> {
    git::execute_git_for_release(path, args).map_err(|e| Error::git_command_failed(e.to_string()))
}

fn git_failure(args: &[&str], output: &std::process::Output) -> Error {
    Error::git_command_failed(format!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::ReleaseCheckoutGuard;
    use crate::core::component::Component;

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
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

    fn run_git_allow_failure(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git")
    }

    fn component(dir: &std::path::Path) -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        }
    }

    fn init_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_git(dir, &["init", "-q", "--initial-branch", "main"]);
        run_git(dir, &["config", "user.email", "homeboy@example.com"]);
        run_git(dir, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(dir.join("file.txt"), "main\n").expect("write file");
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "Initial commit"]);
        temp
    }

    #[test]
    fn restore_after_failure_returns_to_original_branch_and_removes_new_untracked() {
        let temp = init_repo();
        let dir = temp.path();
        run_git(dir, &["checkout", "-q", "-b", "release-local"]);
        let original_head = git_stdout_for_test(dir, &["rev-parse", "HEAD"]);
        let guard = ReleaseCheckoutGuard::capture(&component(dir))
            .expect("capture")
            .expect("git repo");

        run_git(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("generated.txt"), "release output\n").expect("write generated");
        std::fs::write(dir.join("file.txt"), "mutated\n").expect("mutate tracked");

        guard.restore_after_failure().expect("restore");

        assert_eq!(
            git_stdout_for_test(dir, &["branch", "--show-current"]),
            "release-local"
        );
        assert_eq!(
            git_stdout_for_test(dir, &["rev-parse", "HEAD"]),
            original_head
        );
        assert_eq!(git_stdout_for_test(dir, &["status", "--porcelain=v1"]), "");
        assert!(!dir.join("generated.txt").exists());
    }

    #[test]
    fn restore_after_failure_aborts_merge_conflicts() {
        let temp = init_repo();
        let dir = temp.path();
        run_git(dir, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(dir.join("file.txt"), "feature\n").expect("write feature");
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "Feature"]);
        let feature_head = git_stdout_for_test(dir, &["rev-parse", "HEAD"]);
        let guard = ReleaseCheckoutGuard::capture(&component(dir))
            .expect("capture")
            .expect("git repo");

        run_git(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("file.txt"), "main conflict\n").expect("write main");
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "Main conflict"]);
        run_git(dir, &["checkout", "-q", "feature"]);
        let merge = run_git_allow_failure(dir, &["merge", "main"]);
        assert!(!merge.status.success(), "merge should conflict");
        assert!(git_stdout_for_test(dir, &["status", "--porcelain=v1"]).contains("UU file.txt"));

        guard.restore_after_failure().expect("restore");

        assert_eq!(
            git_stdout_for_test(dir, &["branch", "--show-current"]),
            "feature"
        );
        assert_eq!(
            git_stdout_for_test(dir, &["rev-parse", "HEAD"]),
            feature_head
        );
        assert_eq!(git_stdout_for_test(dir, &["status", "--porcelain=v1"]), "");
        assert_eq!(
            std::fs::read_to_string(dir.join("file.txt")).unwrap(),
            "feature\n"
        );
    }

    #[test]
    fn capture_refuses_dirty_tracked_files() {
        let temp = init_repo();
        let dir = temp.path();
        std::fs::write(dir.join("file.txt"), "dirty\n").expect("dirty tracked");

        let err = ReleaseCheckoutGuard::capture(&component(dir)).expect_err("dirty checkout fails");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("Uncommitted tracked changes"));
    }

    fn git_stdout_for_test(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
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
}
