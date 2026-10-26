use super::git;
use crate::workspace::{sync_workspace, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions};
use std::fs;
use std::path::Path;

fn checkout(path: &Path) -> tempfile::TempDir {
    let remote = tempfile::tempdir().unwrap();
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test"]);
    git(remote.path(), &["init", "--bare", "-b", "main"]);
    git(
        path,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "initial"]);
    git(path, &["push", "-u", "origin", "main"]);
    remote
}

#[test]
fn cook_snapshot_git_reuses_dependency_published_by_review_git_sync() {
    crate::test_support::with_isolated_home(|_| {
        let parent = tempfile::tempdir().unwrap();
        let candidate = parent.path().join("candidate");
        let dependency = parent.path().join("shared-runtime");
        let runner_root = tempfile::tempdir().unwrap();
        for (path, manifest) in [
            (
                &candidate,
                serde_json::json!({"id":"candidate", "extensions":{"wordpress":{"settings":{"validation_dependencies":["shared-runtime"]}}}}),
            ),
            (&dependency, serde_json::json!({"id":"shared-runtime"})),
        ] {
            fs::create_dir(path).unwrap();
            fs::write(path.join("homeboy.json"), manifest.to_string()).unwrap();
            fs::write(path.join("main.php"), "<?php\n").unwrap();
        }
        let _candidate_remote = checkout(&candidate);
        let _dependency_remote = checkout(&dependency);
        crate::create(&serde_json::json!({"id":"dependency-paths", "kind":"local", "workspace_root":runner_root.path()}).to_string(), false).unwrap();
        let options = RunnerWorkspaceSyncOptions {
            path: candidate.display().to_string(),
            mode: RunnerWorkspaceSyncMode::Git,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            validation_dependency_ids: None,
            run_isolation_token: None,
        };
        let (review, _) = sync_workspace("dependency-paths", options.clone()).unwrap();
        let dependency_path = &review.validation_dependencies[0].remote_path;
        // Exercise replacement as well as reuse of the review-created sibling.
        fs::write(Path::new(dependency_path).join("main.php"), "old output").unwrap();
        for _ in 0..2 {
            let (cook, _) = sync_workspace(
                "dependency-paths",
                RunnerWorkspaceSyncOptions {
                    mode: RunnerWorkspaceSyncMode::SnapshotGit,
                    run_isolation_token: Some("lab-gate-regression".into()),
                    ..options.clone()
                },
            )
            .unwrap();
            assert_eq!(cook.validation_dependencies.len(), 1);
            assert_eq!(
                &cook.validation_dependencies[0].remote_path,
                dependency_path
            );
            assert_eq!(
                fs::read_to_string(Path::new(dependency_path).join("main.php")).unwrap(),
                "<?php\n"
            );
            assert!(Path::new(&cook.remote_path).join(".git").exists());
            assert!(!Path::new(dependency_path).join(".git").exists());
        }
    });
}
