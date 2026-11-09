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
