use super::{cook_pull_request_refs, resolve_cook_destination};
use crate::cli_surface::{Cli, Commands};
use clap::Parser;
use std::process::Command;

#[cfg(unix)]
#[test]
fn cook_pr_refs_resolve_for_linked_worktree_and_repo_preview_with_fake_gh() {
    let root = tempfile::tempdir().expect("fixture root");
    let repository = root.path().join("repository");
    std::fs::create_dir(&repository).expect("repository directory");
    let run_git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&repository)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run_git(&["init", "-q", "--initial-branch=main"]);
    run_git(&["config", "user.email", "test@example.com"]);
    run_git(&["config", "user.name", "Test"]);
    std::fs::write(repository.join("tracked"), "base\n").expect("tracked file");
    run_git(&["add", "tracked"]);
    run_git(&["commit", "-qm", "base"]);
    run_git(&["branch", "release"]);
    let linked = root.path().join("linked");
    run_git(&[
        "worktree",
        "add",
        "-qb",
        "feature",
        linked.to_str().unwrap(),
    ]);

    run_git(&[
        "-C",
        linked.to_str().unwrap(),
        "commit",
        "--allow-empty",
        "-qm",
        "PR work",
    ]);
    let pr_head = homeboy::core::git::run_git(&linked, &["rev-parse", "HEAD"], "PR head").unwrap();
    let bin = tempfile::tempdir().expect("fake gh bin");
    let gh = bin.path().join("gh");
    homeboy_test_support::write_executable(
        &gh,
        "#!/bin/sh\nprintf '{\"baseRefName\":\"release\",\"headRefName\":\"feature\"}'\n",
        0o755,
    )
    .expect("fake gh");

    crate::test_support::with_isolated_home(|_| {
        // PATH is process-global: stage the fake `gh` only while the
        // isolated-home guard holds the shared environment lock, so a
        // concurrently restoring test cannot swap the real `gh` back in.
        let _path = homeboy::core::test_support::EnvVarGuard::set(
            "PATH",
            format!(
                "{}:{}",
                bin.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
        assert_eq!(
            cook_pull_request_refs(Some(&linked), "https://github.com/example/repo/pull/1895")
                .expect("canonical PR refs"),
            ("release".to_string(), "feature".to_string())
        );
        let parse = |extra: &[&str]| {
            let mut argv = vec![
                "homeboy",
                "agent-task",
                "cook",
                "--preview",
                "--repo",
                repository.to_str().unwrap(),
                "--task-url",
                "https://github.com/example/repo/pull/1895",
                "--prompt",
                "fix it",
            ];
            argv.extend_from_slice(extra);
            let cli = Cli::try_parse_from(argv).expect("parse Cook");
            let Commands::AgentTask(command) = cli.command else {
                panic!("agent-task")
            };
            let super::super::AgentTaskCommand::Cook(args) = command.command else {
                panic!("cook")
            };
            *args
        };
        // A destination handle may not exist yet (including after issue ->
        // PR routing). It must not shadow the resolved repository checkout.
        let mut pending = parse(&["--workspace", "repository@not-created"]);
        assert_eq!(
            super::cook_pull_request_checkout(&pending).unwrap(),
            Some(repository.clone())
        );
        super::resolve_cook_pull_request(&mut pending).expect("pending workspace PR refs");
        assert_eq!(pending.base.as_deref(), Some("release"));
        assert_eq!(pending.head.as_deref(), Some("feature"));

        pending.dispatch.cwd = Some(root.path().join("missing").display().to_string());
        pending.dispatch.repo = Some("unresolved-repository".into());
        assert_eq!(super::cook_pull_request_checkout(&pending).unwrap(), None);
        super::resolve_cook_pull_request(&mut pending).expect("PR refs without a checkout");
        assert_eq!(
            cook_pull_request_refs(Some(&root.path().join("missing")), "1895")
                .expect("invalid cwd is never passed to spawn"),
            ("release".into(), "feature".into())
        );

        homeboy::core::worktree::record_active_for_test("repository@existing", &linked);
        pending.dispatch.workspace = Some("repository@existing".into());
        assert_eq!(
            super::cook_pull_request_checkout(&pending).unwrap(),
            Some(linked.clone())
        );

        for extra in [
            vec![],
            vec![
                "--head",
                "feature",
                "--base",
                "release",
                "--to-worktree",
                "repository@feature",
            ],
        ] {
            let (resolved, preview) = {
                let mut input = parse(&extra);
                match crate::commands::agent_task::run::resolve_cook_repository(&mut input) {
                    Ok(repository) => crate::commands::agent_task::run::resolve_cook_destination(
                        input,
                        repository.as_ref(),
                    )
                    .and_then(|args| {
                        crate::commands::agent_task::run::provision_cook_destination(
                            &args,
                            repository.as_ref(),
                            true,
                        )
                        .map(|provision| (args, provision))
                    }),
                    Err(error) => Err(error),
                }
            }
            .expect("preview PR destination");
            assert_eq!(preview["action"], "planned_create");
            assert_eq!(preview["handle"], "repository@feature");
            assert_eq!(preview["intent"]["base"], "release");
            assert_eq!(preview["intent"]["head"], "feature");
            assert_eq!(resolved.to_worktree.as_deref(), Some("repository@feature"));
            assert_eq!(resolved.base.as_deref(), Some("release"));
            assert_eq!(resolved.head.as_deref(), Some("feature"));
        }
        run_git(&["branch", "fix/issue-7-repo"]);
        run_git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/example/repo.git",
        ]);
        std::fs::write(&gh, "#!/bin/sh\ncase \"$*\" in api*) printf '[{\"state\":\"OPEN\",\"url\":\"https://github.com/example/repo/pull/1895\"}]';; *list*) printf '[{\"state\":\"OPEN\",\"url\":\"https://github.com/example/repo/pull/1895\"}]';; *) printf '{\"baseRefName\":\"release\",\"headRefName\":\"feature\"}';; esac\n").unwrap();
        let mut issue = parse(&["--workspace", "repository@not-created"]);
        issue.dispatch.task_url = Some("https://github.com/example/repo/issues/7".into());
        let resolved = {
            let mut input = issue.clone();
            let repository = crate::commands::agent_task::run::resolve_cook_repository(&mut input)
                .expect("repository");
            resolve_cook_destination(input, repository.as_ref())
        }
        .expect("issue adopts canonical PR");
        assert_eq!(
            resolved.dispatch.task_url.as_deref(),
            Some("https://github.com/example/repo/pull/1895")
        );
        assert_eq!(resolved.head.as_deref(), Some("feature"));
        assert_eq!(resolved.base.as_deref(), Some("release"));
        assert_eq!(
            resolved.dispatch.workspace.as_deref(),
            Some("repository@not-created")
        );
        assert_eq!(
            homeboy::core::git::run_git(&linked, &["rev-parse", "HEAD"], "preserved PR head")
                .unwrap(),
            pr_head
        );
        // A TLS timeout is transient: the lookup retries and still adopts the PR.
        let flaked = linked.join("gh-flaked");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\nif [ ! -f {flag} ]; then touch {flag}; echo 'net/http: TLS handshake timeout' >&2; exit 1; fi\nprintf '[{{\"state\":\"OPEN\",\"url\":\"https://github.com/example/repo/pull/1895\"}}]'\n",
                flag = flaked.display()
            ),
        )
        .unwrap();
        let mut input = issue.clone();
        crate::commands::agent_task::run::resolve_cook_issue(&mut input)
            .expect("transient lookup failure is retried");
        assert_eq!(
            input.dispatch.task_url.as_deref(),
            Some("https://github.com/example/repo/pull/1895")
        );
        std::fs::write(
            &gh,
            "#!/bin/sh\necho 'gh: Not Found (HTTP 404)' >&2\nexit 1\n",
        )
        .unwrap();
        let error = {
            let mut input = issue;
            let repository = crate::commands::agent_task::run::resolve_cook_repository(&mut input)
                .expect("repository");
            resolve_cook_destination(input, repository.as_ref())
        }
        .expect_err("lookup fails closed");
        assert!(error
            .message
            .contains("could not resolve issue's linked PRs"));
        std::fs::write(
            &gh,
            "#!/bin/sh\nprintf '{\"baseRefName\":\"release\",\"headRefName\":\"feature\"}'\n",
        )
        .unwrap();
        for extra in [
            vec!["--head", "other"],
            vec!["--base", "other"],
            vec!["--to-worktree", "repo@other"],
        ] {
            let error = {
                let mut input = parse(&extra);
                match crate::commands::agent_task::run::resolve_cook_repository(&mut input) {
                    Ok(repository) => resolve_cook_destination(input, repository.as_ref()),
                    Err(error) => Err(error),
                }
            }
            .expect_err("mismatched PR refs");
            assert!(
                error
                    .message
                    .contains("must match the existing pull request"),
                "{error:?}"
            );
        }
        // A pull request in another repository is task context: its base
        // and head never become this checkout's destination.
        let mut input = parse(&[
            "--to-worktree",
            "repository@work",
            "--base",
            "trunk",
            "--head",
            "work",
        ]);
        input.dispatch.task_url = Some("https://github.com/example/other/pull/2951".into());
        let resolved = {
            let repository = crate::commands::agent_task::run::resolve_cook_repository(&mut input)
                .expect("repository");
            resolve_cook_destination(input, repository.as_ref())
        }
        .expect("cross-repository task PR resolves");
        assert_eq!(
            (resolved.base.as_deref(), resolved.head.as_deref()),
            (Some("trunk"), Some("work"))
        );
    });
}
