//! A throwaway Git index for computing trees without touching a checkout's
//! real index.
//!
//! The file lives in the repository's own Git directory, on the same
//! filesystem as the objects it describes, so a full shared temporary
//! filesystem cannot fail tree computation for an unrelated checkout.

use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// A private index file in `repo`'s per-worktree Git directory, removed on drop.
pub fn private_index(repo: &Path) -> Result<tempfile::NamedTempFile> {
    let output = Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .current_dir(repo)
        .output()
        .map_err(|error| io_error(error, repo))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "resolve the Git directory of {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let git_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    tempfile::Builder::new()
        .prefix("homeboy-index-")
        .tempfile_in(git_dir)
        .map_err(|error| io_error(error, repo))
}

fn io_error(error: std::io::Error, repo: &Path) -> Error {
    Error::internal_io(
        error.to_string(),
        Some(format!("create a private Git index for {}", repo.display())),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_private_index_lives_in_the_repository_git_dir() {
        let repo = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(status.success());
        let index = super::private_index(repo.path()).unwrap();
        let git_dir = repo.path().canonicalize().unwrap().join(".git");
        assert_eq!(
            index.path().parent().unwrap().canonicalize().unwrap(),
            git_dir
        );
        let path = index.path().to_path_buf();
        drop(index);
        assert!(!path.exists());
    }
}
