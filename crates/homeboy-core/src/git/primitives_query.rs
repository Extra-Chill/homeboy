//! Read-only git query primitives: resolving refs, reading HEAD SHAs, porcelain
//! status, remotes, and repository roots.
//!
//! Split out of `primitives.rs` to keep mutating operations (clone, stage,
//! commit, branch updates) separate from pure reads.

use std::path::{Path, PathBuf};
use std::process::Command;

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

pub fn current_branch(git_root: &Path) -> Option<String> {
    output_optional(git_root, &["branch", "--show-current"])
}

pub fn remote_origin_url(git_root: &Path) -> Option<String> {
    remote_url(git_root, "origin")
}

/// Get the short HEAD revision from a git directory.
pub fn short_head_revision(dir: &Path) -> Option<String> {
    head_sha_short(dir)
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
