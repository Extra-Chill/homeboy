use std::path::Path;

use crate::workspace::snapshot::is_excluded;
use crate::workspace::types::{RunnerWorkspaceMaterializationPlan, DEFAULT_EXCLUDES};
use crate::workspace::util::deterministic_remote_path;
use crate::RunnerWorkspaceSyncMode;

#[test]
fn deterministic_path_stays_under_workspace_root() {
    let path = Path::new("/Users/user/Developer/homeboy@fix-runner-workspace-sync");
    let remote = deterministic_remote_path("/srv/homeboy", path, "snapshot:abc", None);

    assert!(remote.starts_with("/srv/homeboy/_lab_workspaces/homeboy-fix-runner-workspace-sync-"));
}

#[test]
fn run_isolation_token_isolates_runs_and_is_stable_per_run() {
    let path = Path::new("/Users/user/Developer/homeboy");
    let base = deterministic_remote_path("/srv/homeboy", path, "abc123", None);
    let run_a = deterministic_remote_path("/srv/homeboy", path, "abc123", Some("run-a"));
    let run_a_again = deterministic_remote_path("/srv/homeboy", path, "abc123", Some("run-a"));
    let run_b = deterministic_remote_path("/srv/homeboy", path, "abc123", Some("run-b"));
    let blank = deterministic_remote_path("/srv/homeboy", path, "abc123", Some("   "));

    // Two different runs at the same HEAD must not share a remote workspace
    // directory, otherwise leftover untracked artifacts from one run can
    // contaminate the other (#4393). Retries of one run reuse its checkout.
    assert_ne!(run_a, run_b);
    assert_ne!(run_a, base);
    assert_eq!(run_a, run_a_again);
    assert_eq!(base, blank);
    for remote in [&base, &run_a, &run_b] {
        assert!(remote.starts_with("/srv/homeboy/_lab_workspaces/homeboy-"));
    }
}

#[test]
fn materialization_plan_normalizes_root_and_derives_artifact_dir() {
    let path = Path::new("/Users/user/Developer/homeboy@cook-path");
    let remote = deterministic_remote_path("/srv/homeboy/", path, "abc123", Some("run-1"));
    let plan = RunnerWorkspaceMaterializationPlan::from_test_parts(
        "/srv/homeboy/",
        &path.display().to_string(),
        "homeboy@cook-path",
        &remote,
        RunnerWorkspaceSyncMode::Git,
        "abc123",
    );

    assert_eq!(plan.workspace_root, "/srv/homeboy");
    assert_eq!(
        plan.output_paths.artifact_dir,
        format!("{remote}-homeboy-artifacts")
    );
}

#[test]
fn default_excludes_filter_core_metadata_and_secrets() {
    let root = Path::new("/repo");
    let excludes = DEFAULT_EXCLUDES
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();

    assert!(is_excluded(
        root,
        Path::new("/repo/.env.local"),
        &excludes,
        &[]
    ));
    assert!(is_excluded(
        root,
        Path::new("/repo/.homeboy-build/plugin.zip"),
        &excludes,
        &[]
    ));
    assert!(is_excluded(
        root,
        Path::new("/repo/src/__tests__/._index.js"),
        &excludes,
        &[]
    ));
    assert!(!is_excluded(
        root,
        Path::new("/repo/node_modules/pkg/index.js"),
        &excludes,
        &[]
    ));
    assert!(!is_excluded(
        root,
        Path::new("/repo/target/debug/homeboy"),
        &excludes,
        &[]
    ));
    assert!(!is_excluded(
        root,
        Path::new("/repo/src/main.rs"),
        &excludes,
        &[]
    ));
    assert!(!is_excluded(
        root,
        Path::new("/repo/vendor/autoload.php"),
        &excludes,
        &[]
    ));
}
