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
