//! Shared git command helpers for stack verbs.

use std::process::Command;

use crate::error::{Error, Result};

pub(crate) fn run_git(path: &str, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|e| Error::git_command_failed(format!("git {}: {}", args.join(" "), e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn test_run_git() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("git init");

        let out = run_git(dir.path().to_str().unwrap(), &["status", "--porcelain=v1"])
            .expect("run git status");
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
    }
}
