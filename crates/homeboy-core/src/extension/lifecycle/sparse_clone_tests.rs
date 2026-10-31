use super::sparse_clone::{clone_extension, clone_with_runner};
use homeboy_core::{error::Error, git};
use std::path::Path;

fn git_run(cwd: &Path, args: &[&str]) -> String {
    git::run_git(cwd, args, "test fixture").unwrap()
}

fn fixture() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git_run(root, &["init", "--quiet"]);
    git_run(root, &["config", "user.name", "Test"]);
    git_run(root, &["config", "user.email", "test@example.com"]);
    git_run(root, &["config", "uploadpack.allowFilter", "true"]);
    git_run(root, &["config", "uploadpack.allowAnyFilter", "true"]);
    for (path, data) in [
        (
            "extensions/homeboy-extension-root.json",
            r#"{"shared_assets":["shared"]}"#,
        ),
        (
            "extensions/wordpress/wordpress.json",
            r#"{"id":"wordpress","name":"WordPress","version":"1.0.0"}"#,
        ),
        ("extensions/shared/helper.sh", "shared asset"),
        ("extensions/other/other.json", "{}"),
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
    std::fs::create_dir_all(root.join("crates/unrelated/src")).unwrap();
    std::fs::write(
        root.join("crates/unrelated/src/large"),
        vec![b'x'; 2 * 1024 * 1024],
    )
    .unwrap();
    git_run(root, &["add", "."]);
    git_run(root, &["commit", "--quiet", "-m", "fixture"]);
    repo
}

#[test]
fn sparse_nested_clone_never_checks_out_unrelated_trees() {
    let repo = fixture();
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("clone");
    let url = format!("file://{}", repo.path().display());
    clone_with_runner(&url, &target, None, "wordpress", |cwd, args| {
        let result = git::run_git(cwd, args, "test sparse stage");
        assert!(
            !target.join("crates").exists(),
            "unrelated checkout during {args:?}"
        );
        result
    })
    .unwrap();
    assert!(target.join("extensions/wordpress/wordpress.json").is_file());
    assert!(target.join("extensions/shared/helper.sh").is_file());
    assert!(target
        .join("extensions/homeboy-extension-root.json")
        .is_file());
    assert!(!target.join("extensions/other").exists());
    let patterns = std::fs::read_to_string(target.join(".git/info/sparse-checkout")).unwrap();
    assert!(patterns.contains("/extensions/wordpress/"));
    assert!(patterns.contains("/extensions/shared/"));
    assert_eq!(
        git_run(&target, &["config", "remote.origin.promisor"]).trim(),
        "true"
    );
}

#[test]
fn sparse_setup_failure_retries_full_blobless_clone() {
    let repo = fixture();
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("clone");
    let mut failed = false;
    clone_with_runner(
        &format!("file://{}", repo.path().display()),
        &target,
        None,
        "wordpress",
        |cwd, args| {
            if args.first() == Some(&"sparse-checkout") {
                failed = true;
                return Err(Error::internal_io(
                    "simulated unsupported sparse checkout",
                    None,
                ));
            }
            git::run_git(cwd, args, "test fallback")
        },
    )
    .unwrap();
    assert!(failed);
    assert!(target.join("crates/unrelated/src/large").is_file());
    assert!(target.join("extensions/shared/helper.sh").is_file());
    assert_eq!(
        git_run(&target, &["config", "remote.origin.partialclonefilter"]).trim(),
        "blob:none"
    );
}

#[test]
fn sparse_clone_uses_requested_nondefault_branch_tree() {
    let repo = fixture();
    git_run(repo.path(), &["checkout", "-b", "release"]);
    std::fs::write(repo.path().join("extensions/wordpress/release"), "release").unwrap();
    git_run(repo.path(), &["add", "."]);
    git_run(repo.path(), &["commit", "-m", "release"]);
    git_run(repo.path(), &["checkout", "-"]);
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("clone");
    clone_extension(
        &format!("file://{}", repo.path().display()),
        &target,
        Some("release"),
        "wordpress",
    )
    .unwrap();
    assert!(target.join("extensions/wordpress/release").is_file());
    assert!(!target.join("crates").exists());
}
