use std::collections::BTreeMap;
use std::path::Path;

use homeboy_core::test_support::with_isolated_home;
use homeboy_rig::{run_status, run_up, run_up_with_refs, RigSpec};
use homeboy_test_support::git::output as git;

#[test]
fn local_pull_head_runs_in_detached_worktree_and_tracks_new_heads() {
    with_isolated_home(|home| {
        let remote = home.path().join("remote");
        std::fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "-b", "main"]);
        git(&remote, &["config", "user.name", "Test"]);
        git(&remote, &["config", "user.email", "test@example.test"]);
        std::fs::write(remote.join("payload"), "first").unwrap();
        git(&remote, &["add", "."]);
        git(&remote, &["commit", "-m", "first head"]);
        let first = git(&remote, &["rev-parse", "HEAD"]);
        git(&remote, &["update-ref", "refs/pull/42/head", &first]);
        let checkout = home.path().join("checkout");
        let repository_url = format!("file://{}", remote.display());
        git(
            home.path(),
            &["clone", &repository_url, checkout.to_str().unwrap()],
        );
        let registry = home.path().join(".config/homeboy/components");
        std::fs::create_dir_all(&registry).unwrap();
        std::fs::write(
            registry.join("registered-app.json"),
            serde_json::json!({"local_path": checkout}).to_string(),
        )
        .unwrap();
        let output = home.path().join("observed");
        let rig: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "pr-test",
            "components": {"app": {"component_id": "registered-app"}},
            "pipeline": {"up": [{"kind": "command", "command": format!("git rev-parse HEAD > '{}'", output.display()), "cwd": "${components.app.path}"}]}
        })).unwrap();
        let refs = BTreeMap::from([("app".to_string(), "42".to_string())]);
        let one = run_up_with_refs(&rig, &refs).unwrap();
        assert!(one.success, "{one:?}");
        let record = &one.component_refs["app"];
        assert_eq!(record.head_sha, first);
        assert_eq!(record.pr.as_deref(), Some("42"));
        assert_eq!(std::fs::read_to_string(&output).unwrap().trim(), first);
        assert_eq!(
            git(
                Path::new(&record.worktree_path),
                &["rev-parse", "--abbrev-ref", "HEAD"]
            ),
            "HEAD"
        );
        let status = run_status(&rig).unwrap();
        assert_eq!(status.component_refs, one.component_refs);
        assert_eq!(status.components[0].path, record.worktree_path);
        assert_eq!(
            serde_json::to_value(&one).unwrap()["component_refs"]["app"]["head_sha"],
            first
        );
        let url_refs = BTreeMap::from([("app".to_string(), format!("{repository_url}/pull/42"))]);
        let from_url = run_up_with_refs(&rig, &url_refs).unwrap();
        assert_eq!(
            from_url.component_refs["app"].worktree_path,
            record.worktree_path
        );
        let two = run_up_with_refs(&rig, &refs).unwrap();
        assert_eq!(two.component_refs, one.component_refs);
        // A fork pushes a commit that is not on any branch in the base repo.
        let fork = home.path().join("fork");
        git(
            home.path(),
            &["clone", &repository_url, fork.to_str().unwrap()],
        );
        git(&fork, &["config", "user.name", "Fork author"]);
        git(&fork, &["config", "user.email", "fork@example.test"]);
        std::fs::write(fork.join("payload"), "second").unwrap();
        git(&fork, &["commit", "-am", "new fork PR head"]);
        let second = git(&fork, &["rev-parse", "HEAD"]);
        git(&fork, &["push", "origin", "HEAD:refs/pull/42/head"]);
        let three = run_up_with_refs(&rig, &refs).unwrap();
        assert!(three.success);
        assert_eq!(three.component_refs["app"].head_sha, second);
        assert_ne!(
            three.component_refs["app"].worktree_path,
            record.worktree_path
        );
        assert!(Path::new(&record.worktree_path).exists());
        assert_eq!(std::fs::read_to_string(output).unwrap().trim(), second);
        assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), first);
        let mut pinned = rig.clone();
        pinned.components.get_mut("app").unwrap().default_ref = Some(second.clone());
        assert_eq!(
            run_up(&pinned).unwrap().component_refs["app"].head_sha,
            second
        );
        pinned.components.get_mut("app").unwrap().r#ref = Some("main".into());
        assert_eq!(
            run_up(&pinned).unwrap().component_refs["app"].head_sha,
            first
        );
    });
}
