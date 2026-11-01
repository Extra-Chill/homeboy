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
    for message in ["second", "third"] {
        git_run(root, &["commit", "--quiet", "--allow-empty", "-m", message]);
    }
    repo
}

#[test]
fn sparse_clone_uses_exact_shallow_clone_args() {
    for revision in [None, Some("release"), Some("v1")] {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("clone");
        let target_path = target.to_string_lossy();
        let mut expected_clone = vec![
            "clone",
            "--depth=1",
            "--filter=blob:none",
            "--no-checkout",
            "--sparse",
        ];
        if let Some(revision) = revision {
            expected_clone.extend(["--branch", revision]);
        }
        expected_clone.extend(["https://example.com/repo.git", &target_path]);
        let expected = [
            expected_clone,
            vec!["rev-parse", "--verify", "HEAD"],
            vec!["ls-tree", "-r", "--name-only", "-z", "tip"],
            vec!["show", "tip:extensions/homeboy-extension-root.json"],
            vec![
                "sparse-checkout",
                "set",
                "--cone",
                "--",
                "extensions/wordpress",
                "extensions/shared",
            ],
            vec!["checkout", "--quiet", "HEAD"],
        ];
        let responses = [
            "",
            "tip\n",
            "extensions/homeboy-extension-root.json\0extensions/wordpress/wordpress.json\0",
            r#"{"shared_assets":["shared"]}"#,
            "",
            "",
        ];
        let mut call = 0;
        clone_with_runner(
            "https://example.com/repo.git",
            &target,
            revision,
            "wordpress",
            |cwd, args| {
                assert_eq!(args, expected[call]);
                assert_eq!(cwd, if call == 0 { Path::new(".") } else { &target });
                let response = responses[call].to_string();
                call += 1;
                Ok(response)
            },
        )
        .unwrap();
        assert_eq!(call, expected.len());
    }
}

#[test]
fn sparse_nested_clone_never_checks_out_unrelated_trees() {
    let repo = fixture();
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("clone");
    let bare = temp.path().join("source.git");
    git_run(
        temp.path(),
        &[
            "clone",
            "--bare",
            repo.path().to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    git_run(&bare, &["config", "uploadpack.allowFilter", "true"]);
    let url = format!("file://{}", bare.display());
    clone_with_runner(&url, &target, None, "wordpress", |cwd, args| {
        let result = git::run_git(cwd, args, "test sparse stage");
        assert!(
            !target.join("crates").exists(),
            "unrelated checkout during {args:?}"
        );
        result
    })
    .unwrap();
    assert_eq!(
        git_run(&target, &["rev-parse", "--is-shallow-repository"]).trim(),
        "true"
    );
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
fn sparse_clone_commit_sha_uses_existing_fallback() {
    let repo = fixture();
    let revision = git_run(repo.path(), &["rev-parse", "HEAD~1"]);
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("clone");
    let mut calls = 0;
    clone_with_runner(
        &format!("file://{}", repo.path().display()),
        &target,
        Some(revision.trim()),
        "wordpress",
        |cwd, args| {
            calls += 1;
            assert_eq!(args[0], "clone");
            assert_eq!(&args[5..7], &["--branch", revision.trim()]);
            git::run_git(cwd, args, "test SHA fallback")
        },
    )
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(git_run(&target, &["rev-parse", "HEAD"]), revision);
    assert!(target.join("extensions/wordpress/wordpress.json").is_file());
    assert!(target.join("extensions/shared/helper.sh").is_file());
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
fn sparse_clone_uses_requested_branch_or_tag_tree() {
    let repo = fixture();
    git_run(repo.path(), &["checkout", "-b", "release"]);
    std::fs::write(repo.path().join("extensions/wordpress/release"), "release").unwrap();
    git_run(repo.path(), &["add", "."]);
    git_run(repo.path(), &["commit", "-m", "release"]);
    git_run(repo.path(), &["checkout", "-"]);
    git_run(repo.path(), &["tag", "v1", "release"]);
    for revision in ["release", "v1"] {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("clone");
        clone_extension(
            &format!("file://{}", repo.path().display()),
            &target,
            Some(revision),
            "wordpress",
        )
        .unwrap();
        assert!(target.join("extensions/wordpress/release").is_file());
        assert!(target.join("extensions/shared/helper.sh").is_file());
        assert!(!target.join("crates").exists());
        assert_eq!(
            git_run(&target, &["rev-parse", "--is-shallow-repository"]).trim(),
            "true"
        );
    }
}
