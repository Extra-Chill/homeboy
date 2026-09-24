use super::args::{ComponentPathArgs, IssueArgs, IssueCommand, PrArgs, PrCommand};
use super::{run, GitArgs, GitCommand, GitCommandOutput, PatchCommand};
use clap::Parser;
use std::path::Path;
use std::process::Command;

#[derive(Parser)]
struct TestCli {
    #[command(subcommand)]
    command: GitCommand,
}

#[test]
fn push_override_flags_parse() {
    let cli = TestCli::try_parse_from([
        "git",
        "push",
        "homeboy",
        "--remote-url",
        "https://github.com/Extra-Chill/homeboy",
        "--token",
        "secret-token",
        "--refspec",
        "HEAD:refs/heads/autofix",
        "--strip-extraheader",
    ])
    .expect("push flags parse");

    match cli.command {
        GitCommand::Push {
            component_id,
            remote_url,
            token,
            refspec,
            strip_extraheader,
            ..
        } => {
            assert_eq!(component_id.as_deref(), Some("homeboy"));
            assert_eq!(
                remote_url.as_deref(),
                Some("https://github.com/Extra-Chill/homeboy")
            );
            assert_eq!(token.as_deref(), Some("secret-token"));
            assert_eq!(refspec.as_deref(), Some("HEAD:refs/heads/autofix"));
            assert!(strip_extraheader);
        }
        _ => panic!("expected push command"),
    }
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
fn pr_readiness_flags_parse() {
    let cli = TestCli::try_parse_from([
        "git",
        "pr",
        "readiness",
        "homeboy",
        "--number",
        "5805",
        "--path",
        "/tmp/homeboy",
    ])
    .expect("pr readiness flags parse");

    match cli.command {
        GitCommand::Pr(PrArgs {
            command:
                PrCommand::Readiness {
                    component_id,
                    number,
                    path_args: ComponentPathArgs { path },
                },
        }) => {
            assert_eq!(component_id, "homeboy");
            assert_eq!(number, 5805);
            assert_eq!(path.as_deref(), Some("/tmp/homeboy"));
        }
        _ => panic!("expected pr readiness command"),
    }
}

#[test]
fn pr_ready_flags_parse() {
    let cli = TestCli::try_parse_from([
        "git",
        "pr",
        "ready",
        "homeboy",
        "--number",
        "5805",
        "--path",
        "/tmp/homeboy",
    ])
    .expect("pr ready flags parse");

    match cli.command {
        GitCommand::Pr(PrArgs {
            command:
                PrCommand::Ready {
                    component_id,
                    number,
                    path_args: ComponentPathArgs { path },
                },
        }) => {
            assert_eq!(component_id, "homeboy");
            assert_eq!(number, 5805);
            assert_eq!(path.as_deref(), Some("/tmp/homeboy"));
        }
        _ => panic!("expected pr ready command"),
    }
}

#[test]
fn issue_find_path_flag_parses() {
    let cli =
        TestCli::try_parse_from(["git", "issue", "find", "homeboy", "--path", "/tmp/homeboy"])
            .expect("issue find flags parse");

    match cli.command {
        GitCommand::Issue(IssueArgs {
            command:
                IssueCommand::Find {
                    component_id,
                    path_args: ComponentPathArgs { path },
                    ..
                },
        }) => {
            assert_eq!(component_id, "homeboy");
            assert_eq!(path.as_deref(), Some("/tmp/homeboy"));
        }
        _ => panic!("expected issue find command"),
    }
}

#[test]
fn operation_owned_patch_commands_parse() {
    for (action, restore) in [("preserve", false), ("restore", true)] {
        let cli = TestCli::try_parse_from([
            "git",
            "patch",
            action,
            "cook-123",
            "--path",
            "/tmp/worktree",
        ])
        .expect("patch command parses");

        let GitCommand::Patch { command } = cli.command else {
            panic!("expected patch command");
        };
        let (operation_id, path, parsed_restore) = match command {
            PatchCommand::Preserve { operation_id, path } => (operation_id, path, false),
            PatchCommand::Restore { operation_id, path } => (operation_id, path, true),
        };
        assert_eq!(operation_id, "cook-123");
        assert_eq!(path.as_deref(), Some("/tmp/worktree"));
        assert_eq!(parsed_restore, restore);
    }
}

#[test]
fn subtree_command_parses_explicit_preview_and_branch_only_flags() {
    let cli = TestCli::try_parse_from([
        "git",
        "subtree",
        "fixture",
        "origin/main",
        "--preview",
        "--branch-only",
        "--path",
        "/tmp/nested-component",
    ])
    .expect("subtree command parses");

    match cli.command {
        GitCommand::Subtree {
            component_id,
            source_ref,
            apply,
            preview,
            branch_only,
            version,
            path,
        } => {
            assert_eq!(component_id, "fixture");
            assert_eq!(source_ref, "origin/main");
            assert!(!apply);
            assert!(preview);
            assert!(branch_only);
            assert!(version.is_none());
            assert_eq!(path.as_deref(), Some("/tmp/nested-component"));
        }
        _ => panic!("expected subtree command"),
    }
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

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn rev(dir: &Path, reference: &str) -> String {
    let output = Command::new("git")
        .args(["rev-parse", reference])
        .current_dir(dir)
        .output()
        .expect("rev-parse");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
