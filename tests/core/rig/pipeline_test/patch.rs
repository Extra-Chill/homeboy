//! Patch pipeline-step tests. Split out of `pipeline_test.rs` to keep the
//! parent test module under the structural god-file threshold.

use std::collections::HashMap;
use std::fs;

use crate::core::rig::pipeline::run_pipeline;
use crate::core::rig::spec::{ComponentSpec, PatchOp, PipelineStep, RigSpec};

fn rig_with_patch(component_path: &str, step: PipelineStep) -> RigSpec {
    let mut components = HashMap::new();
    components.insert(
        "c".to_string(),
        ComponentSpec {
            path: component_path.to_string(),
            component_id: None,
            path_setting: None,
            checkout_root: None,
            remote_url: None,
            triage_remote_url: None,
            stack: None,
            branch: None,
            r#ref: None,
            default_ref: None,
            extensions: None,
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

#[test]
fn test_patch_appends_when_no_anchor() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let file = tmp.path().join("x.c");
    fs::write(&file, "original\n").expect("write");

    let step = PipelineStep::Patch {
        step_id: None,
        depends_on: Vec::new(),
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
        step_id: None,
        depends_on: Vec::new(),
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
        step_id: None,
        depends_on: Vec::new(),
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
        step_id: None,
        depends_on: Vec::new(),
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
        step_id: None,
        depends_on: Vec::new(),
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
        step_id: None,
        depends_on: Vec::new(),
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
        step_id: None,
        depends_on: Vec::new(),
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
