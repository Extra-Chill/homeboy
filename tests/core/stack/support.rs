use std::fs;
use std::process::Command;

use tempfile::TempDir;

pub(crate) fn init_repo() -> (TempDir, String) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().to_string_lossy().to_string();
    git(&path, &["init", "-q", "-b", "main"]);
    git(&path, &["config", "user.email", "test@test.com"]);
    git(&path, &["config", "user.name", "Test"]);
    fs::write(dir.path().join("README.md"), "initial\n").unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "-q", "-m", "initial"]);
    (dir, path)
}

pub(crate) fn git(path: &str, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap_or_else(|e| panic!("git {:?}: {}", args, e));
    assert!(
        out.status.success(),
        "git {:?} failed: {} / {}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

pub(crate) fn commit_file(dir: &TempDir, path: &str, file: &str, body: &str, msg: &str) -> String {
    fs::write(dir.path().join(file), body).unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", msg]);
    rev_parse(path, "HEAD")
}

pub(crate) fn rev_parse(path: &str, rev: &str) -> String {
    let out = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git rev-parse {rev} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
