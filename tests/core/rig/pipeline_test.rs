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

// ---- Dependency-aware ordering ---------------------------------------------

mod dag {
    use std::collections::HashMap;
    use std::fs;

    use crate::rig::pipeline::run_pipeline;
    use crate::rig::spec::{ComponentSpec, PipelineStep, RigSpec, StackOp};

    fn command(id: &str, depends_on: &[&str], cmd: String, cwd: Option<String>) -> PipelineStep {
        PipelineStep::Command {
            step_id: Some(id.to_string()),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            cmd,
            cwd,
            env: HashMap::new(),
            label: Some(id.to_string()),
        }
    }

    fn stack(id: &str, component: &str, stack_id: &str) -> (String, PipelineStep) {
        (
            stack_id.to_string(),
            PipelineStep::Stack {
                step_id: Some(id.to_string()),
                depends_on: Vec::new(),
                component: component.to_string(),
                op: StackOp::Sync,
                dry_run: true,
                label: Some(id.to_string()),
            },
        )
    }

    fn rig_with_steps(
        steps: Vec<PipelineStep>,
        components: HashMap<String, ComponentSpec>,
    ) -> RigSpec {
        let mut pipeline = HashMap::new();
        pipeline.insert("up".to_string(), steps);
        RigSpec {
            id: "dag-test".to_string(),
            description: String::new(),
            components,
            services: Default::default(),
            symlinks: Vec::new(),
            shared_paths: Vec::new(),
            resources: Default::default(),
            pipeline,
            bench: None,
            app_launcher: None,
            bench_workloads: Default::default(),
        }
    }

    #[test]
    fn test_pipeline_orders_steps_by_dependencies() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let log = tmp.path().join("order.txt");
        let log_arg = log.to_string_lossy();
        let rig = rig_with_steps(
            vec![
                command(
                    "studio-build",
                    &["studio-install"],
                    format!("printf 'build\\n' >> {}", log_arg),
                    None,
                ),
                command(
                    "playground-build",
                    &[],
                    format!("printf 'playground\\n' >> {}", log_arg),
                    None,
                ),
                command(
                    "studio-install",
                    &["playground-build"],
                    format!("printf 'install\\n' >> {}", log_arg),
                    None,
                ),
            ],
            HashMap::new(),
        );

        let out = run_pipeline(&rig, "up", true).expect("pipeline");
        assert!(out.is_success(), "outcomes: {:?}", out.steps);
        assert_eq!(
            fs::read_to_string(&log).expect("read log"),
            "playground\ninstall\nbuild\n"
        );
        assert_eq!(
            out.steps
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            vec!["playground-build", "studio-install", "studio-build"]
        );
    }

    #[test]
    fn test_pipeline_errors_on_missing_dependency() {
        let rig = rig_with_steps(
            vec![command(
                "studio-build",
                &["missing-step"],
                "true".to_string(),
                None,
            )],
            HashMap::new(),
        );

        let err = run_pipeline(&rig, "up", true).expect_err("missing dependency errors");
        let msg = err.to_string();
        assert!(msg.contains("missing step id 'missing-step'"), "{msg}");
    }

    #[test]
    fn test_pipeline_errors_on_dependency_cycle() {
        let rig = rig_with_steps(
            vec![
                command("a", &["b"], "true".to_string(), None),
                command("b", &["a"], "true".to_string(), None),
            ],
            HashMap::new(),
        );

        let err = run_pipeline(&rig, "up", true).expect_err("cycle errors");
        let msg = err.to_string();
        assert!(msg.contains("dependency cycle"), "{msg}");
        assert!(msg.contains("'a'"), "{msg}");
        assert!(msg.contains("'b'"), "{msg}");
    }

    #[test]
    fn test_pipeline_preserves_linear_order_without_dependencies() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let log = tmp.path().join("linear.txt");
        let log_arg = log.to_string_lossy();
        let rig = rig_with_steps(
            vec![
                command("one", &[], format!("printf 'one\\n' >> {}", log_arg), None),
                command("two", &[], format!("printf 'two\\n' >> {}", log_arg), None),
                command(
                    "three",
                    &[],
                    format!("printf 'three\\n' >> {}", log_arg),
                    None,
                ),
            ],
            HashMap::new(),
        );

        let out = run_pipeline(&rig, "up", true).expect("pipeline");
        assert!(out.is_success());
        assert_eq!(
            fs::read_to_string(&log).expect("read log"),
            "one\ntwo\nthree\n"
        );
    }

    #[test]
    fn test_stack_failure_skips_later_steps_when_fail_fast() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let marker = tmp.path().join("marker.txt");
        let marker_arg = marker.to_string_lossy();
        let (stack_id, stack_step) = stack("sync-stack", "studio", "missing-stack-for-test");
        let mut components = HashMap::new();
        components.insert(
            "studio".to_string(),
            ComponentSpec {
                path: tmp.path().to_string_lossy().into_owned(),
                remote_url: None,
                triage_remote_url: None,
                stack: Some(stack_id),
                branch: None,
                extensions: None,
            },
        );
        let rig = rig_with_steps(
            vec![
                stack_step,
                command(
                    "build-after-stack",
                    &[],
                    format!("printf 'should-not-run' > {}", marker_arg),
                    None,
                ),
            ],
            components,
        );

        let out = run_pipeline(&rig, "up", true).expect("pipeline report");
        assert!(!out.is_success());
        assert_eq!(out.failed, 1);
        assert_eq!(out.steps[0].kind, "stack");
        assert_eq!(out.steps[0].status, "fail");
        assert_eq!(out.steps[1].status, "skip");
        assert!(!marker.exists());
    }

    #[test]
    fn test_pipeline_keeps_cross_component_path_expansion_after_reordering() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let component_a = tmp.path().join("component-a");
        let component_b = tmp.path().join("component-b");
        fs::create_dir_all(&component_a).expect("component a");
        fs::create_dir_all(&component_b).expect("component b");

        let mut components = HashMap::new();
        components.insert(
            "a".to_string(),
            ComponentSpec {
                path: component_a.to_string_lossy().into_owned(),
                remote_url: None,
                triage_remote_url: None,
                stack: None,
                branch: None,
                extensions: None,
            },
        );
        components.insert(
            "b".to_string(),
            ComponentSpec {
                path: component_b.to_string_lossy().into_owned(),
                remote_url: None,
                triage_remote_url: None,
                stack: None,
                branch: None,
                extensions: None,
            },
        );

        let rig = rig_with_steps(
            vec![
                command(
                    "write-from-a",
                    &["prepare-b"],
                    "printf 'from-a' > ${components.b.path}/from-a.txt".to_string(),
                    Some("${components.a.path}".to_string()),
                ),
                command(
                    "prepare-b",
                    &[],
                    "printf 'ready' > ready.txt".to_string(),
                    Some("${components.b.path}".to_string()),
                ),
            ],
            components,
        );

        let out = run_pipeline(&rig, "up", true).expect("pipeline");
        assert!(out.is_success(), "outcomes: {:?}", out.steps);
        assert_eq!(
            fs::read_to_string(component_b.join("ready.txt")).expect("ready"),
            "ready"
        );
        assert_eq!(
            fs::read_to_string(component_b.join("from-a.txt")).expect("from-a"),
            "from-a"
        );
    }
}

// ---- Extension-backed lifecycle steps ---------------------------------------

mod extension_lifecycle {
    use std::collections::HashMap;
    use std::fs;

    use crate::component::ScopedExtensionConfig;
    use crate::rig::pipeline::run_pipeline;
    use crate::rig::spec::{ComponentSpec, PipelineStep, RigSpec};
    use crate::test_support;

    fn rig_with_step(component_path: String, step: PipelineStep) -> RigSpec {
        let mut component_settings = HashMap::new();
        component_settings.insert(
            "rig_local".to_string(),
            serde_json::Value::String("yes".to_string()),
        );

        let mut extensions = HashMap::new();
        extensions.insert(
            "nodejs".to_string(),
            ScopedExtensionConfig {
                version: None,
                settings: component_settings,
            },
        );

        let mut components = HashMap::new();
        components.insert(
            "studio".to_string(),
            ComponentSpec {
                path: component_path,
                remote_url: None,
                triage_remote_url: None,
                stack: None,
                branch: None,
                extensions: Some(extensions),
            },
        );

        let mut pipeline = HashMap::new();
        pipeline.insert("up".to_string(), vec![step]);

        RigSpec {
            id: "extension-step-test".to_string(),
            description: String::new(),
            components,
            services: Default::default(),
            symlinks: Vec::new(),
            shared_paths: Vec::new(),
            resources: Default::default(),
            pipeline,
            bench: None,
            app_launcher: None,
            bench_workloads: Default::default(),
        }
    }

    fn write_nodejs_build_extension(home: &std::path::Path) {
        let extension_dir = home.join(".config/homeboy/extensions/nodejs");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("nodejs.json"),
            r#"{
                "name": "Node.js",
                "version": "1.0.0",
                "build": {
                    "extension_script": "build.sh",
                    "command_template": "sh {{script}}",
                    "script_names": []
                }
            }"#,
        )
        .expect("extension manifest");
        fs::write(
            extension_dir.join("build.sh"),
            "printf '%s\n' \"$HOMEBOY_COMPONENT_PATH\" > build-path.txt\nprintf '%s\n' \"$HOMEBOY_SETTINGS_JSON\" > build-settings.json\n",
        )
        .expect("extension script");
    }

    #[test]
    fn test_extension_build_uses_rig_component_path_and_extension_config() {
        test_support::with_isolated_home(|home| {
            write_nodejs_build_extension(home.path());
            let component_dir = tempfile::tempdir().expect("component dir");
            let component_path = component_dir.path().to_string_lossy().to_string();
            let rig = rig_with_step(
                component_path.clone(),
                PipelineStep::Extension {
                    step_id: None,
                    depends_on: Vec::new(),
                    component: "studio".to_string(),
                    op: "build".to_string(),
                    label: None,
                },
            );

            let out = run_pipeline(&rig, "up", true).expect("pipeline");
            assert!(out.is_success(), "outcomes: {:?}", out.steps);
            assert_eq!(out.steps[0].kind, "extension");

            let build_path = fs::read_to_string(component_dir.path().join("build-path.txt"))
                .expect("build path marker");
            assert_eq!(build_path.trim(), component_path);

            let settings = fs::read_to_string(component_dir.path().join("build-settings.json"))
                .expect("settings marker");
            assert!(settings.contains("rig_local"), "settings: {settings}");
            assert!(settings.contains("yes"), "settings: {settings}");
        });
    }

    #[test]
    fn test_extension_step_reports_unsupported_op() {
        let component_dir = tempfile::tempdir().expect("component dir");
        let rig = rig_with_step(
            component_dir.path().to_string_lossy().to_string(),
            PipelineStep::Extension {
                step_id: None,
                depends_on: Vec::new(),
                component: "studio".to_string(),
                op: "setup".to_string(),
                label: None,
            },
        );

        let out = run_pipeline(&rig, "up", true).expect("pipeline");
        assert!(!out.is_success());
        assert_eq!(out.steps[0].kind, "extension");
        let error = out.steps[0].error.as_deref().expect("error");
        assert!(
            error.contains("extension op 'setup' is not supported"),
            "{error}"
        );
        assert!(error.contains("supported ops: build"), "{error}");
    }
}

// ---- Command step environment ----------------------------------------------

mod command_env {
    use std::collections::HashMap;
    use std::fs;

    use crate::rig::pipeline::run_pipeline;
    use crate::rig::spec::{PipelineStep, RigSpec};
    use crate::rig::toolchain;

    fn rig_with_command(cmd: String, env: HashMap<String, String>) -> RigSpec {
        let mut pipeline = HashMap::new();
        pipeline.insert(
            "up".to_string(),
            vec![PipelineStep::Command {
                step_id: None,
                depends_on: Vec::new(),
                cmd,
                cwd: None,
                env,
                label: Some("command".to_string()),
            }],
        );

        RigSpec {
            id: "command-env-test".to_string(),
            description: String::new(),
            components: Default::default(),
            services: Default::default(),
            symlinks: Vec::new(),
            shared_paths: Vec::new(),
            resources: Default::default(),
            pipeline,
            bench: None,
            bench_workloads: Default::default(),
            app_launcher: None,
        }
    }

    #[test]
    fn test_command_step_explicit_path_env_wins() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path_file = tmp.path().join("path.txt");
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/tmp/explicit-toolchain".to_string());

        let rig = rig_with_command(
            format!("printf '%s' \"$PATH\" > {}", path_file.to_string_lossy()),
            env,
        );
        let outcome = run_pipeline(&rig, "up", true).expect("pipeline");

        assert!(outcome.is_success(), "outcomes: {:?}", outcome.steps);
        assert_eq!(
            fs::read_to_string(path_file).expect("path file"),
            "/tmp/explicit-toolchain"
        );
    }

    #[test]
    fn test_command_step_uses_bootstrapped_toolchain_path() {
        let expected = toolchain::command_step_path().expect("toolchain path");
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path_file = tmp.path().join("path.txt");

        let rig = rig_with_command(
            format!("printf '%s' \"$PATH\" > {}", path_file.to_string_lossy()),
            HashMap::new(),
        );
        let outcome = run_pipeline(&rig, "up", true).expect("pipeline");

        assert!(outcome.is_success(), "outcomes: {:?}", outcome.steps);
        assert_eq!(
            fs::read_to_string(path_file).expect("path file"),
            expected.to_string_lossy()
        );
    }

    #[test]
    fn test_command_step_exit_127_mentions_path_contract() {
        let rig = rig_with_command(
            "definitely-not-a-homeboy-test-command-1758 2>/dev/null".to_string(),
            HashMap::new(),
        );
        let outcome = run_pipeline(&rig, "up", true).expect("pipeline report");

        assert!(!outcome.is_success());
        let error = outcome.steps[0].error.as_deref().expect("error");
        assert!(error.contains("exited 127"), "{error}");
        assert!(error.contains("command not found"), "{error}");
        assert!(error.contains("env.PATH"), "{error}");
    }
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
                remote_url: None,
                triage_remote_url: None,
                stack: None,
                branch: None,
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
            pipeline,
            bench: None,
            bench_workloads: Default::default(),
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
}

// ---- Shared path step end-to-end -------------------------------------------

#[cfg(unix)]
mod shared_path {
    use std::collections::HashMap;
    use std::fs;

    use crate::rig::pipeline::{cleanup_shared_paths, run_pipeline};
    use crate::rig::spec::{PipelineStep, RigSpec, SharedPathOp, SharedPathSpec};
    use crate::rig::state::RigState;
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
            pipeline,
            bench: None,
            bench_workloads: Default::default(),
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
}
