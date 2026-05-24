use super::*;
use std::process::Command;

#[test]
fn is_workdir_clean_returns_true_for_clean_repo() {
    use std::fs;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let path = temp_dir.path();

    Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("Failed to init git repo");

    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(path)
        .output()
        .expect("Failed to configure git email");

    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(path)
        .output()
        .expect("Failed to configure git name");

    fs::write(path.join("test.txt"), "content").expect("Failed to write file");
    Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .expect("Failed to git add");

    Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(path)
        .output()
        .expect("Failed to commit");

    assert!(is_workdir_clean(path), "Expected clean repo to return true");
}

#[test]
fn is_workdir_clean_returns_false_for_dirty_repo() {
    use std::fs;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let path = temp_dir.path();

    Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("Failed to init git repo");

    fs::write(path.join("untracked.txt"), "content").expect("Failed to write file");

    assert!(
        !is_workdir_clean(path),
        "Expected dirty repo to return false"
    );
}

#[test]
fn is_workdir_clean_returns_false_for_invalid_path() {
    let path = std::path::Path::new("/nonexistent/path/that/does/not/exist");
    assert!(
        !is_workdir_clean(path),
        "Expected invalid path to return false"
    );
}

fn init_repo_with_initial_commit() -> (tempfile::TempDir, String) {
    use std::fs;
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().to_string_lossy().to_string();

    Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&path)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&path)
        .output()
        .unwrap();
    fs::write(dir.path().join("README.md"), "initial\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(&path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", "initial"])
        .current_dir(&path)
        .output()
        .unwrap();

    (dir, path)
}

#[test]
fn changes_at_path_only_discovers_portable_component_id() {
    use std::fs;
    let (dir, path) = init_repo_with_initial_commit();
    fs::write(
        dir.path().join("homeboy.json"),
        r#"{"id":"portable-changes"}"#,
    )
    .unwrap();

    let out = changes_at(None, None, false, Some(&path)).expect("changes_at with --path");

    assert_eq!(out.component_id, "portable-changes");
    assert_eq!(out.path, path);
    assert!(out.success);
}

#[test]
fn changes_at_component_and_path_trusts_both_inputs() {
    let (_dir, path) = init_repo_with_initial_commit();

    let out = changes_at(Some("explicit-changes"), None, false, Some(&path))
        .expect("changes_at with component and --path");

    assert_eq!(out.component_id, "explicit-changes");
    assert_eq!(out.path, path);
    assert!(out.success);
}

#[test]
fn test_remote_tag_commit() {
    let (_dir, path) = init_repo_with_initial_commit();
    let remote = tempfile::TempDir::new().expect("remote tempdir");

    Command::new("git")
        .args(["init", "--bare", "-q"])
        .current_dir(remote.path())
        .output()
        .expect("git init bare remote");
    Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            remote.path().to_str().expect("remote path"),
        ])
        .current_dir(&path)
        .output()
        .expect("git remote add");
    Command::new("git")
        .args(["tag", "v1.2.3"])
        .current_dir(&path)
        .output()
        .expect("git tag");
    Command::new("git")
        .args(["push", "origin", "v1.2.3"])
        .current_dir(&path)
        .output()
        .expect("git push tag");

    let head = get_head_commit(&path).expect("head commit");
    let remote_tag = remote_tag_commit(&path, "v1.2.3").expect("remote tag lookup");

    assert_eq!(remote_tag.as_deref(), Some(head.as_str()));
    assert_eq!(remote_tag_commit(&path, "v9.9.9").unwrap(), None);
}

#[test]
fn rebase_against_self_is_a_noop_success() {
    let (_dir, path) = init_repo_with_initial_commit();

    let out = rebase_at(
        None,
        RebaseOptions {
            onto: Some("HEAD".to_string()),
            ..Default::default()
        },
        Some(&path),
    )
    .expect("rebase_at");

    assert!(out.success, "rebase HEAD should succeed: {:?}", out.stderr);
    assert_eq!(out.action, "rebase");
    assert_eq!(out.path, path);
}

#[test]
fn rebase_abort_outside_of_rebase_is_an_error() {
    let (_dir, path) = init_repo_with_initial_commit();

    let out = rebase_at(
        None,
        RebaseOptions {
            abort: true,
            ..Default::default()
        },
        Some(&path),
    )
    .expect("rebase_at returns Ok with failed GitOutput");

    assert!(!out.success);
    assert_ne!(out.exit_code, 0);
    assert!(
        out.stderr.contains("rebase") || out.stderr.contains("No rebase"),
        "expected stderr to mention rebase: {:?}",
        out.stderr
    );
}

#[test]
fn cherry_pick_picks_a_commit_from_another_branch() {
    use std::fs;
    let (dir, path) = init_repo_with_initial_commit();

    Command::new("git")
        .args(["checkout", "-q", "-b", "side"])
        .current_dir(&path)
        .output()
        .unwrap();
    fs::write(dir.path().join("from-side.txt"), "side\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(&path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", "side commit"])
        .current_dir(&path)
        .output()
        .unwrap();
    let side_sha = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&path)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    Command::new("git")
        .args(["checkout", "-q", "main"])
        .current_dir(&path)
        .output()
        .unwrap();

    let out = cherry_pick_at(
        None,
        CherryPickOptions {
            refs: vec![side_sha.clone()],
            ..Default::default()
        },
        Some(&path),
    )
    .expect("cherry_pick_at");

    assert!(
        out.success,
        "cherry-pick should succeed: stderr={:?}",
        out.stderr
    );
    assert!(
        dir.path().join("from-side.txt").exists(),
        "cherry-picked file should exist on main"
    );
}

#[test]
fn cherry_pick_with_no_refs_and_no_pr_errors() {
    let (_dir, path) = init_repo_with_initial_commit();

    let err = cherry_pick_at(None, CherryPickOptions::default(), Some(&path))
        .expect_err("cherry_pick with empty refs should Err");

    let msg = err.to_string();
    assert!(
        msg.contains("at least one commit ref") || msg.contains("--pr"),
        "expected helpful error, got: {}",
        msg
    );
}

#[test]
fn cherry_pick_abort_outside_of_pick_is_a_failed_output() {
    let (_dir, path) = init_repo_with_initial_commit();

    let out = cherry_pick_at(
        None,
        CherryPickOptions {
            abort: true,
            ..Default::default()
        },
        Some(&path),
    )
    .expect("cherry_pick_at returns Ok with failed GitOutput");

    assert!(!out.success);
    assert_ne!(out.exit_code, 0);
}

#[test]
fn push_options_force_with_lease_includes_flag() {
    let (_dir, path) = init_repo_with_initial_commit();

    let out = push_at(
        None,
        PushOptions {
            tags: false,
            force_with_lease: true,
            ..Default::default()
        },
        Some(&path),
    )
    .expect("push_at");

    assert!(!out.success, "push without remote should fail");
    assert!(
        !out.stderr.contains("unknown option") && !out.stderr.contains("invalid argument"),
        "--force-with-lease should be a known flag, got: {}",
        out.stderr
    );
}

#[test]
fn push_options_remote_url_refspec_and_strip_extraheader_push_to_bare_remote() {
    let (_dir, path) = init_repo_with_initial_commit();
    let remote = tempfile::TempDir::new().expect("bare remote tempdir");
    Command::new("git")
        .args(["init", "-q", "--bare"])
        .current_dir(remote.path())
        .output()
        .expect("git init --bare");

    let remote_url = remote.path().to_string_lossy().to_string();
    let out = push_at(
        None,
        PushOptions {
            remote_url: Some(remote_url.clone()),
            refspec: Some("HEAD:refs/heads/autofix".to_string()),
            strip_extraheader: true,
            ..Default::default()
        },
        Some(&path),
    )
    .expect("push_at");

    assert!(out.success, "push should succeed: stderr={}", out.stderr);
    let verify = Command::new("git")
        .args(["show-ref", "--verify", "refs/heads/autofix"])
        .current_dir(remote.path())
        .output()
        .expect("git show-ref");
    assert!(verify.status.success(), "expected autofix branch on remote");
}

#[test]
fn push_token_requires_github_remote_url() {
    let (_dir, path) = init_repo_with_initial_commit();

    let err = push_at(
        None,
        PushOptions {
            remote_url: Some("https://example.com/owner/repo".to_string()),
            token: Some("secret-token".to_string()),
            ..Default::default()
        },
        Some(&path),
    )
    .expect_err("non-GitHub token push should fail validation");

    assert!(err.to_string().contains("https://github.com/"));
    assert!(!err.to_string().contains("secret-token"));
}

#[test]
fn test_short_head_revision_at() {
    let (_dir, path) = init_repo_with_initial_commit();

    let revision = short_head_revision_at(std::path::Path::new(&path)).expect("short revision");

    assert!(!revision.is_empty());
    assert!(revision.len() <= 12, "unexpected short sha: {revision}");
}
