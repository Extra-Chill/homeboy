use super::{run, GitArgs, GitCommand, GitCommandOutput};
use clap::Parser;
use std::path::Path;
use std::process::Command;

#[derive(Parser)]
struct TestCli {
    #[command(subcommand)]
    command: GitCommand,
}

#[test]
fn push_token_requires_remote_url_at_parse_time() {
    let err = match TestCli::try_parse_from(["git", "push", "homeboy", "--token", "secret-token"]) {
        Ok(_) => panic!("--token should require --remote-url"),
        Err(err) => err,
    };

    assert!(err.to_string().contains("--remote-url"));
}

#[test]
fn subtree_cli_publishes_from_nested_component_and_previews_without_mutation() {
    homeboy::core::test_support::with_isolated_home(|_| {
        let root = tempfile::tempdir().expect("source repository");
        let remote = tempfile::tempdir().expect("destination repository");
        // Pin the bare remote's default branch explicitly: subtree publish
        // only ever creates/updates `refs/heads/main`, so if the host's
        // ambient `init.defaultBranch` differs (e.g. `master`), HEAD is left
        // pointing at a ref that never receives a commit and the final
        // `git clone` below silently checks out an empty tree.
        git(remote.path(), &["init", "--bare", "-q", "-b", "main"]);
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        git(root.path(), &["config", "user.name", "Test"]);
        std::fs::create_dir_all(root.path().join("packages/example")).expect("component");
        std::fs::create_dir_all(root.path().join("packages/other")).expect("sibling");
        std::fs::write(root.path().join("packages/example/file"), "one").expect("file");
        std::fs::write(root.path().join("packages/other/file"), "sibling").expect("sibling file");
        std::fs::write(
            root.path().join("packages/example/homeboy.json"),
            serde_json::json!({
                "id": "fixture",
                "release": {"subtree": [{
                    "prefix": "packages/example",
                    "remote": remote.path().display().to_string(),
                    "branch": "main",
                    "tag": false
                }]}
            })
            .to_string(),
        )
        .expect("portable config");
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "release one"]);

        let nested = root.path().join("packages/example");
        let first = run_cli([
            "git",
            "subtree",
            "fixture",
            "HEAD",
            "--apply",
            "--branch-only",
            "--path",
            nested.to_str().unwrap(),
        ]);
        assert!(matches!(first, GitCommandOutput::Subtree(_)));

        std::fs::write(root.path().join("packages/example/file"), "two").expect("updated file");
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "release two"]);
        let second = run_cli([
            "git",
            "subtree",
            "fixture",
            "HEAD",
            "--apply",
            "--branch-only",
            "--path",
            nested.to_str().unwrap(),
        ]);
        let GitCommandOutput::Subtree(evidence) = second else {
            panic!("expected subtree output");
        };
        assert_eq!(evidence[0].branch_action, "update");
        assert_eq!(
            evidence[0].source_repo_root,
            root.path().display().to_string()
        );

        std::fs::write(root.path().join("packages/example/file"), "preview").expect("preview file");
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "release preview"]);
        let before = rev(remote.path(), "main");
        let preview = run_cli([
            "git",
            "subtree",
            "fixture",
            "HEAD",
            "--preview",
            "--branch-only",
            "--path",
            nested.to_str().unwrap(),
        ]);
        let GitCommandOutput::Subtree(evidence) = preview else {
            panic!("expected subtree preview output");
        };
        assert!(evidence[0].preview);

        // Serialize through the real command-output serializer. Matching on the
        // enum alone cannot catch a payload that is not an object: this
        // serializer tags every variant by inserting `variant`, so a bare
        // sequence fails at runtime while every in-process assertion passes.
        let serialized = serde_json::to_value(GitCommandOutput::Subtree(evidence))
            .expect("subtree output serializes through the command envelope");
        assert_eq!(serialized["variant"], "subtree");
        assert!(
            serialized["publications"]
                .as_array()
                .is_some_and(|publications| !publications.is_empty()),
            "publications are carried as an array inside the tagged object: {serialized}"
        );
        assert_eq!(rev(remote.path(), "main"), before);

        let clone = tempfile::tempdir().expect("destination clone");
        git(
            clone.path(),
            &["clone", remote.path().to_str().unwrap(), "."],
        );
        assert!(clone.path().join("file").is_file());
        assert!(!clone.path().join("packages/other/file").exists());
    });
}

fn run_cli<const N: usize>(args: [&str; N]) -> GitCommandOutput {
    let cli = TestCli::try_parse_from(args).expect("CLI parse");
    let (output, exit_code) = run(GitArgs {
        command: cli.command,
    })
    .expect("CLI operation");
    assert_eq!(exit_code, 0);
    output
}

use homeboy_test_support::git::run as git;

fn rev(dir: &Path, reference: &str) -> String {
    let output = Command::new("git")
        .args(["rev-parse", reference])
        .current_dir(dir)
        .output()
        .expect("rev-parse");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
