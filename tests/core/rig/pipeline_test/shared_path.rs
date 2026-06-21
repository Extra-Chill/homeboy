//! Shared-path pipeline-step end-to-end tests. Split out of `pipeline_test.rs`
//! to keep the parent test module under the structural god-file threshold.

#![cfg(unix)]

use std::collections::HashMap;
use std::fs;

use crate::core::rig::pipeline::{cleanup_shared_paths, run_pipeline};
use crate::core::rig::spec::{PipelineStep, RigSpec, SharedPathOp, SharedPathSpec};
use crate::core::rig::state::RigState;
use crate::test_support::with_isolated_home;

fn rig_with_shared_path(id: &str, shared: SharedPathSpec, op: SharedPathOp) -> RigSpec {
    let mut pipeline = HashMap::new();
    pipeline.insert(
        "up".to_string(),
        vec![PipelineStep::SharedPath {
            step_id: None,
            depends_on: Vec::new(),
            op,
        }],
    );
    RigSpec {
        id: id.to_string(),
        description: String::new(),
        components: Default::default(),
        services: Default::default(),
        symlinks: Vec::new(),
        shared_paths: vec![shared],
        resources: Default::default(),
        requirements: Default::default(),
        pipeline,
        bench: None,
        fuzz: None,
        bench_workloads: Default::default(),
        trace_workloads: Default::default(),
        fuzz_workloads: Default::default(),
        trace_workload_defaults: Default::default(),
        trace_phase_templates: Default::default(),
        trace_variants: Default::default(),
        trace_profiles: Default::default(),
        trace_experiments: Default::default(),
        trace_guardrails: Default::default(),
        bench_profiles: Default::default(),
        app_launcher: None,
    }
}

fn shared(link: &std::path::Path, target: &std::path::Path) -> SharedPathSpec {
    SharedPathSpec {
        link: link.to_string_lossy().into_owned(),
        target: target.to_string_lossy().into_owned(),
    }
}

#[test]
fn test_shared_path_ensure_creates_missing_symlink_and_records_state() {
    with_isolated_home(|_home| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let target = tmp.path().join("primary-node_modules");
        let link = tmp.path().join("worktree-node_modules");
        fs::create_dir(&target).expect("target dir");

        let rig = rig_with_shared_path(
            "shared-create",
            shared(&link, &target),
            SharedPathOp::Ensure,
        );
        let out = run_pipeline(&rig, "up", true).expect("pipeline");
        assert!(out.is_success(), "outcomes: {:?}", out.steps);
        assert!(link.is_symlink(), "missing path becomes symlink");
        assert_eq!(fs::read_link(&link).expect("read link"), target);

        let state = RigState::load(&rig.id).expect("state");
        let key = link.to_string_lossy().into_owned();
        assert_eq!(
            state.shared_paths.get(&key).unwrap().target,
            target.to_string_lossy()
        );
    });
}

#[test]
fn test_shared_path_ensure_leaves_existing_local_directory_unowned() {
    with_isolated_home(|_home| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let target = tmp.path().join("primary-node_modules");
        let link = tmp.path().join("worktree-node_modules");
        fs::create_dir(&target).expect("target dir");
        fs::create_dir(&link).expect("local deps dir");

        let rig =
            rig_with_shared_path("shared-local", shared(&link, &target), SharedPathOp::Ensure);
        let out = run_pipeline(&rig, "up", true).expect("pipeline");
        assert!(out.is_success(), "existing local directory should pass");
        assert!(link.is_dir());
        assert!(!link.is_symlink());

        let state = RigState::load(&rig.id).expect("state");
        assert!(
            state.shared_paths.is_empty(),
            "local deps are not rig-owned"
        );
    });
}

#[test]
fn test_shared_path_cleanup_removes_only_state_owned_symlink() {
    with_isolated_home(|_home| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let target = tmp.path().join("primary-node_modules");
        let owned_link = tmp.path().join("owned-node_modules");
        fs::create_dir(&target).expect("target dir");

        let rig = rig_with_shared_path(
            "shared-cleanup",
            shared(&owned_link, &target),
            SharedPathOp::Ensure,
        );
        run_pipeline(&rig, "up", true).expect("ensure");
        assert!(owned_link.is_symlink());

        cleanup_shared_paths(&rig).expect("cleanup");
        assert!(!owned_link.exists(), "owned symlink removed");
        let state = RigState::load(&rig.id).expect("state");
        assert!(state.shared_paths.is_empty(), "ownership marker cleared");
    });
}

#[test]
fn test_shared_path_cleanup_does_not_remove_unowned_matching_symlink() {
    with_isolated_home(|_home| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let target = tmp.path().join("primary-node_modules");
        let link = tmp.path().join("worktree-node_modules");
        fs::create_dir(&target).expect("target dir");
        std::os::unix::fs::symlink(&target, &link).expect("preexisting symlink");

        let rig = rig_with_shared_path(
            "shared-unowned",
            shared(&link, &target),
            SharedPathOp::Ensure,
        );
        run_pipeline(&rig, "up", true).expect("ensure sees existing symlink");
        cleanup_shared_paths(&rig).expect("cleanup");
        assert!(link.is_symlink(), "unowned symlink is left alone");
    });
}

#[test]
fn test_shared_path_ensure_refuses_existing_symlink_to_other_target() {
    with_isolated_home(|_home| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let target = tmp.path().join("primary-node_modules");
        let other = tmp.path().join("other-node_modules");
        let link = tmp.path().join("worktree-node_modules");
        fs::create_dir(&target).expect("target dir");
        fs::create_dir(&other).expect("other dir");
        std::os::unix::fs::symlink(&other, &link).expect("preexisting symlink");

        let rig = rig_with_shared_path(
            "shared-wrong-symlink",
            shared(&link, &target),
            SharedPathOp::Ensure,
        );
        let out = run_pipeline(&rig, "up", true).expect("pipeline runs");
        assert!(!out.is_success(), "wrong symlink should fail");
        assert_eq!(fs::read_link(&link).expect("read link"), other);
    });
}

#[test]
fn test_shared_path_ensure_rejects_broken_matching_symlink() {
    with_isolated_home(|_home| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let target = tmp.path().join("missing-primary-node_modules");
        let link = tmp.path().join("worktree-node_modules");
        std::os::unix::fs::symlink(&target, &link).expect("preexisting symlink");

        let rig = rig_with_shared_path(
            "shared-broken-symlink",
            shared(&link, &target),
            SharedPathOp::Ensure,
        );
        let out = run_pipeline(&rig, "up", true).expect("pipeline runs");
        assert!(!out.is_success(), "broken dependency symlink should fail");
        assert!(link.is_symlink(), "ensure must not remove broken symlink");
    });
}

#[test]
fn test_shared_path_verify_accepts_local_directory_and_rejects_missing() {
    with_isolated_home(|_home| {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let target = tmp.path().join("primary-node_modules");
        let local = tmp.path().join("local-node_modules");
        let missing = tmp.path().join("missing-node_modules");
        fs::create_dir(&target).expect("target dir");
        fs::create_dir(&local).expect("local dir");

        let local_rig = rig_with_shared_path(
            "shared-verify-local",
            shared(&local, &target),
            SharedPathOp::Verify,
        );
        let local_out = run_pipeline(&local_rig, "up", true).expect("local verify");
        assert!(local_out.is_success(), "local deps satisfy verify");

        let missing_rig = rig_with_shared_path(
            "shared-verify-missing",
            shared(&missing, &target),
            SharedPathOp::Verify,
        );
        let missing_out = run_pipeline(&missing_rig, "up", true).expect("missing verify");
        assert!(!missing_out.is_success(), "missing deps should fail verify");
    });
}
