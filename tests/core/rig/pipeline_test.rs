//! Pipeline executor tests for `src/core/rig/pipeline.rs`.
//!
//! End-to-end pipeline runs exercise real services + filesystem mutations
//! and are covered by the manual smoke documented in #1468. Scope here is
//! the public outcome types — shape, serialization, `is_success` contract.

use crate::rig::pipeline::{PipelineOutcome, PipelineStepOutcome};

fn step(status: &str) -> PipelineStepOutcome {
    PipelineStepOutcome {
        kind: "command".to_string(),
        label: "noop".to_string(),
        status: status.to_string(),
        error: None,
    }
}

#[test]
fn test_pipeline_outcome_success_when_zero_failures() {
    let outcome = PipelineOutcome {
        name: "up".to_string(),
        steps: vec![step("pass"), step("pass")],
        passed: 2,
        failed: 0,
    };
    assert!(outcome.is_success());
}

#[test]
fn test_pipeline_outcome_failure_when_any_step_failed() {
    let outcome = PipelineOutcome {
        name: "up".to_string(),
        steps: vec![step("pass"), step("fail")],
        passed: 1,
        failed: 1,
    };
    assert!(!outcome.is_success());
}

#[test]
fn test_pipeline_step_outcome_serializes_error_when_present() {
    let outcome = PipelineStepOutcome {
        kind: "service".to_string(),
        label: "svc start".to_string(),
        status: "fail".to_string(),
        error: Some("boom".to_string()),
    };
    let json = serde_json::to_string(&outcome).expect("serialize");
    assert!(json.contains("\"error\":\"boom\""));
}

#[test]
fn test_pipeline_step_outcome_omits_error_when_absent() {
    let outcome = step("pass");
    let json = serde_json::to_string(&outcome).expect("serialize");
    assert!(!json.contains("\"error\""));
}

// ---- Patch step end-to-end -------------------------------------------------
//
// The patch step is the smallest of the three new pipeline kinds and the
// only one that mutates files, so it's worth proper coverage. We run a real
// rig pipeline (single-step) so the dispatch + serialization wiring is
// exercised, not just the inner helper.

mod patch {
    use std::collections::HashMap;
    use std::fs;

    use crate::rig::pipeline::run_pipeline;
    use crate::rig::spec::{ComponentSpec, PatchOp, PipelineStep, RigSpec};

    fn rig_with_patch(component_path: &str, step: PipelineStep) -> RigSpec {
        let mut components = HashMap::new();
        components.insert(
            "c".to_string(),
            ComponentSpec {
                path: component_path.to_string(),
                stack: None,
                branch: None,
            },
        );
        let mut pipeline = HashMap::new();
        pipeline.insert("up".to_string(), vec![step]);
        RigSpec {
            id: "patch-test".to_string(),
            description: String::new(),
            components,
            services: Default::default(),
            symlinks: Vec::new(),
            shared_paths: Vec::new(),
            pipeline,
            bench: None,
        }
    }

    #[test]
    fn test_patch_appends_when_no_anchor() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let file = tmp.path().join("x.c");
        fs::write(&file, "original\n").expect("write");

        let step = PipelineStep::Patch {
            component: "c".to_string(),
            file: "x.c".to_string(),
            marker: "MARKER-XYZ".to_string(),
            after: None,
            content: "/* MARKER-XYZ */\n".to_string(),
            op: PatchOp::Apply,
            label: None,
        };
        let rig = rig_with_patch(&tmp.path().to_string_lossy(), step);
        let out = run_pipeline(&rig, "up", true).expect("pipeline");
        assert!(out.is_success(), "outcomes: {:?}", out.steps);

        let body = fs::read_to_string(&file).expect("read");
        assert!(body.contains("MARKER-XYZ"));
        assert!(body.starts_with("original"));
    }

    #[test]
    fn test_patch_idempotent_when_marker_present() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let file = tmp.path().join("x.c");
        fs::write(&file, "original\n/* MARKER-XYZ */\n").expect("write");
        let before = fs::read_to_string(&file).expect("read before");

        let step = PipelineStep::Patch {
            component: "c".to_string(),
            file: "x.c".to_string(),
            marker: "MARKER-XYZ".to_string(),
            after: None,
            content: "/* MARKER-XYZ */\n".to_string(),
            op: PatchOp::Apply,
            label: None,
        };
        let rig = rig_with_patch(&tmp.path().to_string_lossy(), step);
        run_pipeline(&rig, "up", true).expect("pipeline");

        let after = fs::read_to_string(&file).expect("read after");
        assert_eq!(before, after, "second apply should be a no-op");
    }

    #[test]
    fn test_patch_inserts_after_anchor() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let file = tmp.path().join("x.c");
        fs::write(&file, "line1\n/* ANCHOR */\nline3\n").expect("write");

        let step = PipelineStep::Patch {
            component: "c".to_string(),
            file: "x.c".to_string(),
            marker: "MARKER-INSERTED".to_string(),
            after: Some("/* ANCHOR */".to_string()),
            content: "/* MARKER-INSERTED */\n".to_string(),
            op: PatchOp::Apply,
            label: None,
        };
        let rig = rig_with_patch(&tmp.path().to_string_lossy(), step);
        run_pipeline(&rig, "up", true).expect("pipeline");

        let body = fs::read_to_string(&file).expect("read");
        // Patch goes on the line after the anchor, so the anchor's line
        // is preserved and the next line is the patch.
        assert_eq!(body, "line1\n/* ANCHOR */\n/* MARKER-INSERTED */\nline3\n");
    }

    #[test]
    fn test_patch_fails_when_anchor_missing() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let file = tmp.path().join("x.c");
        fs::write(&file, "no anchor here\n").expect("write");

        let step = PipelineStep::Patch {
            component: "c".to_string(),
            file: "x.c".to_string(),
            marker: "MARKER".to_string(),
            after: Some("/* ANCHOR */".to_string()),
            content: "/* MARKER */\n".to_string(),
            op: PatchOp::Apply,
            label: None,
        };
        let rig = rig_with_patch(&tmp.path().to_string_lossy(), step);
        let out = run_pipeline(&rig, "up", true).expect("pipeline runs");
        assert!(!out.is_success(), "missing anchor must fail");
        let err = out.steps[0].error.as_deref().unwrap_or("");
        assert!(err.contains("anchor"), "error must mention anchor: {}", err);
    }

    #[test]
    fn test_patch_rejects_content_missing_marker() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let file = tmp.path().join("x.c");
        fs::write(&file, "x\n").expect("write");

        // Marker not in content ⇒ would re-apply forever.
        let step = PipelineStep::Patch {
            component: "c".to_string(),
            file: "x.c".to_string(),
            marker: "M".to_string(),
            after: None,
            content: "no-marker-here".to_string(),
            op: PatchOp::Apply,
            label: None,
        };
        let rig = rig_with_patch(&tmp.path().to_string_lossy(), step);
        let out = run_pipeline(&rig, "up", true).expect("pipeline runs");
        assert!(!out.is_success(), "must reject re-apply-forever shape");
    }

    #[test]
    fn test_patch_verify_passes_when_marker_present() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let file = tmp.path().join("x.c");
        fs::write(&file, "/* ALREADY-PATCHED */\n").expect("write");

        let step = PipelineStep::Patch {
            component: "c".to_string(),
            file: "x.c".to_string(),
            marker: "ALREADY-PATCHED".to_string(),
            after: None,
            content: "/* ALREADY-PATCHED */\n".to_string(),
            op: PatchOp::Verify,
            label: None,
        };
        let rig = rig_with_patch(&tmp.path().to_string_lossy(), step);
        let out = run_pipeline(&rig, "up", true).expect("pipeline");
        assert!(out.is_success());
    }

    #[test]
    fn test_patch_verify_fails_when_marker_absent_and_does_not_mutate() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let file = tmp.path().join("x.c");
        fs::write(&file, "no marker\n").expect("write");
        let before = fs::read_to_string(&file).expect("read before");

        let step = PipelineStep::Patch {
            component: "c".to_string(),
            file: "x.c".to_string(),
            marker: "M-MISSING".to_string(),
            after: None,
            content: "/* M-MISSING */\n".to_string(),
            op: PatchOp::Verify,
            label: None,
        };
        let rig = rig_with_patch(&tmp.path().to_string_lossy(), step);
        let out = run_pipeline(&rig, "up", true).expect("pipeline runs");
        assert!(!out.is_success());

        let after = fs::read_to_string(&file).expect("read after");
        assert_eq!(before, after, "verify must be read-only");
    }
}

// ---- Shared path step end-to-end -------------------------------------------

#[cfg(unix)]
mod shared_path {
    use std::collections::HashMap;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    use tempfile::TempDir;

    use crate::rig::pipeline::{cleanup_shared_paths, run_pipeline};
    use crate::rig::spec::{PipelineStep, RigSpec, SharedPathOp, SharedPathSpec};
    use crate::rig::state::RigState;

    fn rig_with_shared_path(id: &str, shared: SharedPathSpec, op: SharedPathOp) -> RigSpec {
        let mut pipeline = HashMap::new();
        pipeline.insert("up".to_string(), vec![PipelineStep::SharedPath { op }]);
        RigSpec {
            id: id.to_string(),
            description: String::new(),
            components: Default::default(),
            services: Default::default(),
            symlinks: Vec::new(),
            shared_paths: vec![shared],
            pipeline,
            bench: None,
        }
    }

    fn home_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_isolated_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
        let guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("HOME").ok();
        let dir = TempDir::new().expect("home tempdir");
        std::env::set_var("HOME", dir.path());
        let result = body(&dir);
        match prior {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        drop(guard);
        result
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
}
