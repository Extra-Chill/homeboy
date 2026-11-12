use super::super::AgentTaskPromotionStatus;
use super::*;

#[test]
fn adoption_recovers_staged_and_unstaged_candidate_trees_before_gating() {
    let _home = gate_home();
    for staged in [true, false] {
        let (temp, repo, base, candidate) = adopted_commit_repo();
        git(&repo, &["reset", "--soft", &base]);
        if !staged {
            git(&repo, &["reset"]);
        }
        let options = adopted_commit_options(
            &temp,
            &repo,
            base,
            candidate.clone(),
            VerifyGateOptions {
                verify: vec!["true".into()],
                ..Default::default()
            },
        );
        let mut provider = FakePromotionWorkspaceProvider {
            workspace_path: Some(repo.clone()),
            ..Default::default()
        };
        let report = promote_with_provider(options, &mut provider).unwrap();
        assert_eq!(report.status, AgentTaskPromotionStatus::Applied);
        assert!(!provider.verify_calls.is_empty());
        assert_eq!(provider.verify_worktrees_clean, vec![true]);
        assert_eq!(git_head(&repo, "HEAD"), candidate);
    }
}

#[test]
fn recovery_preserves_unrelated_dirty_bytes_and_dry_runs() {
    let (temp, repo, base, candidate) = adopted_commit_repo();
    git(&repo, &["reset", "--soft", &base]);
    let recover = |dry_run| {
        super::super::committed_changes::recover_candidate_destination(
            &repo,
            &candidate,
            Some(&base),
            dry_run,
        )
        .unwrap()
    };
    assert!(recover(true));
    assert_eq!(git_head(&repo, "HEAD"), base);
    std::fs::write(repo.join("user.txt"), "keep me").unwrap();
    assert!(!recover(false));
    assert_eq!(git_head(&repo, "HEAD"), base);
    assert_eq!(
        std::fs::read_to_string(repo.join("user.txt")).unwrap(),
        "keep me"
    );
    drop(temp);
}
