use super::*;
use crate::agent_task_promotion::destination;

#[test]
fn cook_recovery_two_cooks_refuses_foreign_destination_and_preserves_staged_tree() {
    homeboy_core::test_support::with_isolated_home(|_| {
        seed_recipe("cook-a");
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &[
                "config",
                "remote.origin.url",
                "https://example.test/repo.git",
            ],
        );
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        std::fs::write(repo.join("file"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        git(
            &repo,
            &["worktree", "add", "-b", "cook-a", a.to_str().unwrap()],
        );
        git(
            &repo,
            &["worktree", "add", "-b", "cook-b", b.to_str().unwrap()],
        );
        destination::record(&a, "cook-a").unwrap();
        destination::record(&b, "cook-b").unwrap();
        let base = git_head(&b, "HEAD");
        git(&repo, &["update-ref", "refs/remotes/origin/trunk", &base]);
        std::fs::write(b.join("file"), "foreign candidate\n").unwrap();
        git(&b, &["add", "file"]);
        git(&b, &["commit", "-qm", "Cook B candidate"]);
        let candidate = crate::agent_task::AgentTaskCandidate {
            base_sha: base.clone(),
            candidate_sha: git_head(&b, "HEAD"),
            tree_sha: git_head(&b, "HEAD^{tree}"),
        };
        assert!(crate::agent_task_promotion::materialize_candidate_tree(&a, &candidate).is_err());
        std::fs::write(a.join("file"), "unpublished candidate\n").unwrap();
        git(&a, &["add", "file"]);
        let tree = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(&a)
                .arg("write-tree")
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let head = git_head(&a, "HEAD");
        let error = destination::before_reset(&a, "cook-b").unwrap_err();
        assert!(error.message.contains("destination belongs to Cook cook-a"));
        assert!(destination::check_run(&a, Some("cook-b")).is_err());
        let staged = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(&a)
                .arg("write-tree")
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        assert_eq!(staged, tree);
        assert_eq!(git_head(&a, "HEAD"), head);
        destination::before_reset(&a, "cook-a").unwrap();
        git(&a, &["reset", "--hard", "HEAD"]);
        assert_eq!(git_head(&a, "refs/homeboy/candidates/cook-a^{tree}"), tree);
        assert_eq!(std::fs::read_to_string(a.join("file")).unwrap(), "base\n");
        assert!(destination::check(&b, Some("cook-b")).is_ok());
        assert_eq!(git_head(&repo, "refs/remotes/origin/trunk"), base);
        git(&a, &["checkout", "-b", "unexpected"]);
        assert!(destination::before_reset(&a, "cook-a").is_err());
    });
}

/// A destination follows its Cook's lineage and outlives a terminal owner: a
/// retry publishes through it, a successor Cook takes it over (keeping the
/// owner's staged candidate), and a live owner or foreign run is still refused.
#[test]
fn destination_passes_to_retries_and_to_successors_of_a_terminal_cook() {
    use crate::agent_task_lifecycle as lifecycle;
    for title in [Some("fix: preserve candidate"), Some(""), Some("   "), None] {
        homeboy_core::test_support::with_isolated_home(|home| {
            if let Some(title) = title {
                seed_recipe_with_title("cook-a", title);
            }
            let store = lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
            let repo = home.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q", "-b", "fix/issue-1"]);
            git(
                &repo,
                &[
                    "config",
                    "remote.origin.url",
                    "https://example.test/repo.git",
                ],
            );
            git(&repo, &["config", "user.name", "Test"]);
            git(&repo, &["config", "user.email", "test@example.com"]);
            std::fs::write(repo.join("file"), "base\n").unwrap();
            git(&repo, &["add", "."]);
            git(&repo, &["commit", "-qm", "base"]);
            let durable = |id: &str, state, retry_of: Option<&str>| {
                lifecycle::record_detached_cook_handoff_parent_in_store(&store, id).unwrap();
                lifecycle::rewrite_record_for_test(id, |record| {
                    lifecycle::set_run_state(record, state);
                    if let Some(parent) = retry_of {
                        record.metadata["retry_of"] = serde_json::json!(parent);
                    }
                })
                .unwrap();
            };
            durable(
                "cook-a",
                homeboy_lifecycle_contract::RunExecutionState::Running,
                None,
            );
            durable(
                "retry-a",
                homeboy_lifecycle_contract::RunExecutionState::Running,
                Some("cook-a"),
            );
            durable(
                "cook-b",
                homeboy_lifecycle_contract::RunExecutionState::Queued,
                None,
            );
            destination::record(&repo, "cook-a").unwrap();

            assert!(
                destination::check(&repo, Some("cook-b")).is_err(),
                "live owner"
            );
            assert!(
                destination::check_run(&repo, Some("retry-a")).is_ok(),
                "retry lineage"
            );
            assert!(
                destination::check_run(&repo, Some("cook-b")).is_err(),
                "foreign run"
            );

            lifecycle::rewrite_record_for_test("cook-a", |record| {
                lifecycle::set_run_state(
                    record,
                    homeboy_lifecycle_contract::RunExecutionState::Failed,
                );
            })
            .unwrap();
            assert!(
                destination::check_run(&repo, Some("cook-b")).is_err(),
                "runs never take over"
            );
            std::fs::write(repo.join("file"), "cook-a candidate\n").unwrap();
            git(&repo, &["add", "file"]);
            let staged_tree = || {
                let output = Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .arg("write-tree")
                    .output()
                    .unwrap();
                assert!(output.status.success());
                String::from_utf8(output.stdout).unwrap().trim().to_string()
            };
            let tree = staged_tree();
            let head = git_head(&repo, "HEAD");
            destination::check(&repo, Some("cook-b")).unwrap();
            assert_eq!(
                git_head(&repo, "refs/homeboy/candidates/cook-a^{tree}"),
                tree
            );
            assert_eq!(git_head(&repo, "HEAD"), head);
            assert_eq!(staged_tree(), tree);
            let subject = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["log", "-1", "--format=%s", "refs/homeboy/candidates/cook-a"])
                .output()
                .unwrap();
            assert!(subject.status.success());
            assert_eq!(
                String::from_utf8(subject.stdout).unwrap().trim(),
                title
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or("base")
            );
            assert_eq!(
                std::fs::read_to_string(repo.join("file")).unwrap(),
                "cook-a candidate\n"
            );
            git(&repo, &["show", "refs/homeboy/candidates/cook-a:file"]);
            assert!(
                destination::check(&repo, Some("cook-a")).is_err(),
                "ownership moved"
            );
            assert!(destination::check(&repo, Some("cook-b")).is_ok());
        });
    }
}

fn seed_recipe(cook: &str) {
    seed_recipe_with_title(cook, "fix: preserve candidate");
}

fn seed_recipe_with_title(cook: &str, title: &str) {
    use crate::agent_task_service::*;
    let options = CookRequest {
        identity: CookIdentity {
            cook_id: cook.into(),
            initial_run_id: cook.into(),
            initial_plan: crate::agent_task_scheduler::AgentTaskPlan::new(
                "candidate-test",
                vec![serde_json::from_value(serde_json::json!({
                    "schema": crate::agent_task::AGENT_TASK_REQUEST_SCHEMA,
                    "task_id": "candidate", "executor": {"backend": "test"},
                    "instructions": "change", "workspace": {"root": "/tmp"}, "inputs": {}
                }))
                .unwrap()],
            ),
        },
        workspace: CookWorkspace {
            to_worktree: "fixture@candidate".into(),
            source_worktree_path: None,
            task_base_sha: None,
            source_refs: vec![],
        },
        provider_transport: CookProviderTransport {
            provider_invocation: None,
            attempt_dispatcher: None,
        },
        gates: Default::default(),
        retry_policy: CookRetryPolicy { max_attempts: 1 },
        finalization: CookFinalization {
            publish_incomplete_draft: false,
            tracker_note: true,
            no_finalize: true,
            draft_pr: false,
            base: "main".into(),
            head: Some("cook/head".into()),
            title: title.into(),
            commit_message: "fix: preserve candidate".into(),
            protected_branches: vec![],
        },
        harvest_context:
            crate::agent_task_scheduler::HarvestExecutionContext::from_current_process().unwrap(),
    };
    persist_initial_recipe(&options).unwrap();
}
