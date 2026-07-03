use std::path::Path;

use crate::core::runner::workspace::snapshot::is_excluded;
use crate::core::runner::workspace::types::{RunnerWorkspaceMaterializationPlan, DEFAULT_EXCLUDES};
use crate::core::runner::workspace::util::deterministic_remote_path;
use crate::core::runner::RunnerWorkspaceSyncMode;

#[test]
fn deterministic_path_stays_under_workspace_root() {
    let path = Path::new("/Users/user/Developer/homeboy@fix-runner-workspace-sync");
    let remote = deterministic_remote_path("/srv/homeboy", path, "snapshot:abc", None);

    assert!(remote.starts_with("/srv/homeboy/_lab_workspaces/homeboy-fix-runner-workspace-sync-"));
}

#[test]
fn run_isolation_token_yields_distinct_remote_paths_for_same_head() {
    let path = Path::new("/Users/user/Developer/homeboy");
    let base = deterministic_remote_path("/srv/homeboy", path, "abc123", None);
    let run_a = deterministic_remote_path("/srv/homeboy", path, "abc123", Some("run-a"));
    let run_b = deterministic_remote_path("/srv/homeboy", path, "abc123", Some("run-b"));

    // Two different runs at the same HEAD must not share a remote workspace
    // directory, otherwise leftover untracked artifacts from one run can
    // contaminate the other (#4393).
    assert_ne!(run_a, run_b);
    assert_ne!(run_a, base);
    assert_ne!(run_b, base);
    // All paths still stay under the deterministic workspace root.
    for remote in [&base, &run_a, &run_b] {
        assert!(remote.starts_with("/srv/homeboy/_lab_workspaces/homeboy-"));
    }
}

#[test]
fn run_isolation_token_is_stable_for_same_run() {
    let path = Path::new("/Users/user/Developer/homeboy");
    let first = deterministic_remote_path("/srv/homeboy", path, "abc123", Some("run-a"));
    let second = deterministic_remote_path("/srv/homeboy", path, "abc123", Some("run-a"));

    // The same run id must deterministically resolve to the same workspace
    // so retries/streaming of one run reuse its own isolated checkout.
    assert_eq!(first, second);
}

#[test]
fn blank_run_isolation_token_does_not_change_remote_path() {
    let path = Path::new("/Users/user/Developer/homeboy");
    let base = deterministic_remote_path("/srv/homeboy", path, "abc123", None);
    let blank = deterministic_remote_path("/srv/homeboy", path, "abc123", Some("   "));

    assert_eq!(base, blank);
}

#[test]
fn materialization_plan_records_explicit_remote_path_contract() {
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
    assert_eq!(plan.local_basename, "homeboy@cook-path");
    assert_eq!(plan.remote_path, remote);
    assert_eq!(plan.sync_mode, RunnerWorkspaceSyncMode::Git);
    assert_eq!(plan.identity, "abc123");
    assert_eq!(
        plan.path_strategy,
        "workspace_root_lab_workspaces_sanitized_basename_identity_digest"
    );
    assert_eq!(
        plan.output_paths.artifact_dir,
        format!("{remote}-homeboy-artifacts")
    );
}

#[test]
fn default_excludes_filter_generated_outputs_and_secrets() {
    let root = Path::new("/repo");
    let excludes = DEFAULT_EXCLUDES
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();

    assert!(is_excluded(
        root,
        Path::new("/repo/node_modules/pkg/index.js"),
        &excludes,
        &[]
    ));
    assert!(is_excluded(
        root,
        Path::new("/repo/.env.local"),
        &excludes,
        &[]
    ));
    assert!(is_excluded(
        root,
        Path::new("/repo/target/debug/homeboy"),
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
