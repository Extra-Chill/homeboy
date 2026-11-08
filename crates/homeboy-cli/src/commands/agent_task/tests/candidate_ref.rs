use super::support::*;

#[test]
fn agent_task_status_exposes_applied_candidate_ref_and_clean_git_tree() {
    with_isolated_home(|_| {
        let temp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(temp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.test"]);
        std::fs::write(temp.path().join("file"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        std::fs::write(temp.path().join("file"), "candidate\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "candidate"]);
        let sha = git(&["rev-parse", "HEAD"]);
        let tree = git(&["rev-parse", "HEAD^{tree}"]);
        git(&["reset", "--soft", &base]);
        let run_id = "candidate-ref-attempt";
        let cook_id = "candidate-ref-cook";
        let plan = test_plan();
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["cook_id"] = json!(cook_id);
        })
        .unwrap();
        agent_task_lifecycle::record_promotion(
            run_id,
            json!({
                "status": "applied",
                "target": {"path": temp.path()},
                "candidate": {"base_sha": base, "candidate_sha": sha, "tree_sha": tree},
            }),
        )
        .unwrap();
        let value = status(StatusArgs {
            run_id: run_id.into(),
            exact: true,
            ..Default::default()
        })
        .unwrap()
        .0;
        let reference = value["candidate_ref"].as_str().unwrap();
        assert_eq!(reference, "refs/homeboy/candidates/candidate-ref-cook");
        assert_eq!(value["candidate_sha"], sha);
        let observation = homeboy::core::observation::ObservationStore::open_initialized().unwrap();
        let shown = crate::commands::runs::show_run_in_store(&observation, run_id)
            .unwrap()
            .0;
        let shown = serde_json::to_value(shown).unwrap();
        assert_eq!(shown["payload"]["run"]["candidate_ref"], reference);
        assert_eq!(shown["payload"]["run"]["candidate_sha"], sha);
        assert_eq!(git(&["rev-parse", reference]), sha);
        assert_eq!(
            git(&["rev-parse", &format!("{reference}^{{tree}}")]),
            git(&["write-tree"])
        );
        assert_eq!(git(&["rev-parse", "HEAD"]), base);
        let checkout = temp.path().join("checkout");
        git(&["worktree", "add", checkout.to_str().unwrap(), reference]);
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
    });
}
