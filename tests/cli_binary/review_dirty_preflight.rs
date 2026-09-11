use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn dirty_umbrella_review_rejects_before_dependency_setup_or_stages() {
    let fixture = tempfile::tempdir().expect("fixture");
    let repository = fixture.path().join("repository");
    std::fs::create_dir_all(&repository).expect("repository");
    run_git(&repository, &["init", "-q", "--initial-branch", "main"]);
    run_git(
        &repository,
        &["config", "user.email", "homeboy@example.test"],
    );
    run_git(&repository, &["config", "user.name", "Homeboy Test"]);
    std::fs::write(repository.join("tracked.txt"), "initial\n").expect("tracked file");
    run_git(&repository, &["add", "tracked.txt"]);
    run_git(&repository, &["commit", "-q", "-m", "fixture"]);
    std::fs::write(repository.join("tracked.txt"), "dirty\n").expect("dirty file");

    let output = Command::new(homeboy_bin())
        .args(["--placement", "local", "review", "fixture", "--path"])
        .arg(&repository)
        .args(["--changed-only", "--summary"])
        .env("HOME", fixture.path())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run homeboy review");

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("Review tests require a clean component checkout"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(combined.contains("Dirty files: tracked.txt"), "{combined}");
    assert!(
        combined.contains("homeboy review --changed-only audit")
            && combined.contains("homeboy review --changed-only lint"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !combined.contains("phase=dependency_setup")
            && !combined.contains("review.audit")
            && !combined.contains("review.lint"),
        "dirty review must reject before setup or stages\nstdout: {stdout}\nstderr: {stderr}"
    );
}

fn run_git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
