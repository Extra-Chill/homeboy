//! Patch pipeline-step tests.

use std::collections::HashMap;
use std::fs;

use crate::pipeline::run_pipeline;
use crate::spec::{ComponentSpec, PatchOp, PipelineStep, RigSpec};

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
            lab_stack: None,
            branch: None,
            r#ref: None,
            default_ref: None,
            extensions: None,
        },
    );
    let mut pipeline = HashMap::new();
    pipeline.insert("up".to_string(), vec![step]);
    RigSpec {
        effective_component_paths: Default::default(),
        workloads: Default::default(),
        id: "patch-test".to_string(),
        description: String::new(),
        components,
        services: Default::default(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        resources: Default::default(),
        lifecycle: Default::default(),
        requirements: Default::default(),
        pipeline,
        bench: None,
        fuzz: None,
        trace: Default::default(),
        bench_workloads: Default::default(),
        trace_workloads: Default::default(),
        fuzz_workloads: Default::default(),
        trace_workload_defaults: Default::default(),
        trace_variants: Default::default(),
        trace_profiles: Default::default(),
        trace_experiments: Default::default(),
        trace_guardrails: Default::default(),
        bench_profiles: Default::default(),
        fuzz_profiles: Default::default(),
        app_launcher: None,
    }
}

fn run_patch(
    initial: &str,
    marker: &str,
    after: Option<&str>,
    content: &str,
    op: PatchOp,
) -> (bool, String, String) {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let file = tmp.path().join("x.c");
    fs::write(&file, initial).expect("write");

    let step = PipelineStep::Patch {
        step_id: None,
        depends_on: Vec::new(),
        component: "c".to_string(),
        file: "x.c".to_string(),
        marker: marker.to_string(),
        after: after.map(str::to_string),
        content: content.to_string(),
        op,
        label: None,
    };
    let rig = rig_with_patch(&tmp.path().to_string_lossy(), step);
    let out =
        run_pipeline(&crate::state::test_state_store(), &rig, "up", true).expect("pipeline runs");
    let error = out.steps[0].error.clone().unwrap_or_default();
    (
        out.is_success(),
        fs::read_to_string(&file).expect("read"),
        error,
    )
}

#[test]
fn test_patch_apply_and_verify_succeed_with_expected_file_body() {
    for (name, initial, marker, after, content, op, expected) in [
        (
            "append without anchor",
            "original\n",
            "MARKER-XYZ",
            None,
            "/* MARKER-XYZ */\n",
            PatchOp::Apply,
            "original\n/* MARKER-XYZ */\n",
        ),
        (
            "idempotent when marker present",
            "original\n/* MARKER-XYZ */\n",
            "MARKER-XYZ",
            None,
            "/* MARKER-XYZ */\n",
            PatchOp::Apply,
            "original\n/* MARKER-XYZ */\n",
        ),
        (
            "insert on the line after the anchor",
            "line1\n/* ANCHOR */\nline3\n",
            "MARKER-INSERTED",
            Some("/* ANCHOR */"),
            "/* MARKER-INSERTED */\n",
            PatchOp::Apply,
            "line1\n/* ANCHOR */\n/* MARKER-INSERTED */\nline3\n",
        ),
        (
            "verify passes when marker present",
            "/* ALREADY-PATCHED */\n",
            "ALREADY-PATCHED",
            None,
            "/* ALREADY-PATCHED */\n",
            PatchOp::Verify,
            "/* ALREADY-PATCHED */\n",
        ),
    ] {
        let (success, body, error) = run_patch(initial, marker, after, content, op);
        assert!(success, "{name}: {error}");
        assert_eq!(body, expected, "{name}");
    }
}

#[test]
fn test_patch_failures_leave_file_untouched() {
    for (name, initial, marker, after, content, op, needle) in [
        (
            "missing anchor",
            "no anchor here\n",
            "MARKER",
            Some("/* ANCHOR */"),
            "/* MARKER */\n",
            PatchOp::Apply,
            "anchor",
        ),
        // Marker absent from content would re-apply forever.
        (
            "content missing marker",
            "x\n",
            "M",
            None,
            "no-marker-here",
            PatchOp::Apply,
            "",
        ),
        (
            "verify is read-only when marker absent",
            "no marker\n",
            "M-MISSING",
            None,
            "/* M-MISSING */\n",
            PatchOp::Verify,
            "",
        ),
    ] {
        let (success, body, error) = run_patch(initial, marker, after, content, op);
        assert!(!success, "{name} must fail");
        assert_eq!(body, initial, "{name} must not mutate the file");
        assert!(error.contains(needle), "{name}: {error}");
    }
}
