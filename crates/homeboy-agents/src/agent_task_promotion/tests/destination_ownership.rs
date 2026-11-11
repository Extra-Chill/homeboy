use super::*;
use crate::agent_task_promotion::destination;

#[test]
fn cook_recovery_two_cooks_refuses_foreign_destination_and_preserves_staged_tree() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
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
}

/// A destination follows its Cook's lineage and outlives a terminal owner: a
/// retry publishes through it, a successor Cook takes it over (keeping the
/// owner's staged candidate), and a live owner or foreign run is still refused.
#[test]
fn destination_passes_to_retries_and_to_successors_of_a_terminal_cook() {
    use crate::agent_task_lifecycle as lifecycle;
    homeboy_core::test_support::with_isolated_home(|home| {
        let store = lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
        let repo = home.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "fix/issue-1"]);
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
        durable("cook-a", lifecycle::AgentTaskRunState::Running, None);
        durable(
            "retry-a",
            lifecycle::AgentTaskRunState::Running,
            Some("cook-a"),
        );
        durable("cook-b", lifecycle::AgentTaskRunState::Queued, None);
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
            lifecycle::set_run_state(record, lifecycle::AgentTaskRunState::Failed);
        })
        .unwrap();
        assert!(
            destination::check_run(&repo, Some("cook-b")).is_err(),
            "runs never take over"
        );
        std::fs::write(repo.join("file"), "cook-a candidate\n").unwrap();
        git(&repo, &["add", "file"]);
        destination::check(&repo, Some("cook-b")).unwrap();
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
