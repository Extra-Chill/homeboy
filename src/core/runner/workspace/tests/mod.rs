mod deterministic;
mod git;
mod snapshot;

use std::path::Path;
use std::process::Command;

/// Run a git command in `path`, asserting success. Shared test helper.
fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Create a git repo with a single committed file then dirty the working tree.
fn dirty_git_repo() -> tempfile::TempDir {
    let source = tempfile::tempdir().expect("source tempdir");
    git(source.path(), &["init"]);
    git(source.path(), &["config", "user.email", "test@example.com"]);
    git(source.path(), &["config", "user.name", "Test User"]);
    std::fs::write(source.path().join("file.txt"), "base\n").expect("write base");
    git(source.path(), &["add", "."]);
    git(source.path(), &["commit", "-m", "base"]);
    std::fs::write(source.path().join("file.txt"), "dirty\n").expect("write dirty file");
    source
}
