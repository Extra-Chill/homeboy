//! Pipeline executor tests for `src/core/rig/pipeline.rs`.
//!
//! End-to-end pipeline runs exercise real services + filesystem mutations
//! and are covered by the manual smoke documented in #1468. Scope here is
//! the public outcome types — shape, serialization, `is_success` contract.

use crate::core::rig::pipeline::{
    run_pipeline, run_pipeline_check_groups, run_pipeline_with_settings, PipelineOutcome,
    PipelineStepOutcome,
};
use crate::core::rig::spec::RigSpec;

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

#[test]
fn test_run_pipeline_check_groups() {
    let rig: RigSpec = serde_json::from_str(
        r#"{
            "id": "scoped-pipeline",
            "pipeline": {
                "check": [
                    {
                        "kind": "check",
                        "label": "selected",
                        "groups": ["desktop-app"],
                        "command": "true"
                    },
                    {
                        "kind": "check",
                        "label": "unselected",
                        "groups": ["cli"],
                        "command": "false"
                    }
                ]
            }
        }"#,
    )
    .expect("parse rig");

    let outcome = run_pipeline_check_groups(&rig, &["desktop-app".to_string()], false)
        .expect("scoped pipeline runs");

    assert!(outcome.is_success());
    assert_eq!(outcome.steps.len(), 1);
    assert_eq!(outcome.steps[0].label, "selected");
}

#[test]
fn test_command_if_missing_runs_when_path_is_missing() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("installed.txt");
    let marker_arg = marker.to_string_lossy();
    let cwd_arg = tmp.path().to_string_lossy();
    let rig: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "command-if-missing-run",
            "pipeline": {{
                "bench_prepare": [{{
                    "kind": "command-if-missing",
                    "cwd": "{}",
                    "missing": "node_modules/.bin/project-env",
                    "command": "printf installed > {}"
                }}]
            }}
        }}"#,
        cwd_arg, marker_arg
    ))
    .expect("parse rig");

    let out = run_pipeline(&rig, "bench_prepare", true).expect("pipeline runs");
    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert_eq!(
        std::fs::read_to_string(marker).expect("marker"),
        "installed"
    );
}

#[test]
fn test_command_if_missing_skips_when_path_exists() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let tool_bin = tmp.path().join("node_modules/.bin/project-env");
    std::fs::create_dir_all(tool_bin.parent().expect("parent")).expect("mkdir");
    std::fs::write(&tool_bin, "#!/bin/sh\n").expect("write");
    let marker = tmp.path().join("installed.txt");
    let marker_arg = marker.to_string_lossy();
    let cwd_arg = tmp.path().to_string_lossy();
    let rig: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "command-if-missing-skip",
            "pipeline": {{
                "bench_prepare": [{{
                    "kind": "command-if-missing",
                    "cwd": "{}",
                    "missing": "node_modules/.bin/project-env",
                    "command": "printf installed > {}"
                }}]
            }}
        }}"#,
        cwd_arg, marker_arg
    ))
    .expect("parse rig");

    let out = run_pipeline(&rig, "bench_prepare", true).expect("pipeline runs");
    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert!(!marker.exists(), "guarded command should not run");
}

#[test]
fn test_command_step_exposes_pipeline_settings_env() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("setting.txt");
    let marker_arg = marker.to_string_lossy();
    let rig: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "command-settings-env",
            "pipeline": {{
                "bench_prepare": [{{
                    "kind": "command",
                    "command": "printf '%s' \"$HOMEBOY_SETTINGS_SAMPLE_RUNTIME_SOURCE_ROOT\" > {}"
                }}]
            }}
        }}"#,
        marker_arg
    ))
    .expect("parse rig");

    let settings = vec![(
        "sample_runtime_source_root".to_string(),
        "/tmp/sample-runtime".to_string(),
    )];
    let out =
        run_pipeline_with_settings(&rig, "bench_prepare", true, &settings).expect("pipeline runs");

    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert_eq!(
        std::fs::read_to_string(marker).expect("marker"),
        "/tmp/sample-runtime"
    );
}

#[test]
fn test_command_step_fails_with_structured_runner_capability_missing() {
    let _env_lock = capability_env_lock();
    let _guard = EnvRestore::set("HOMEBOY_RUNNER_ID", Some("fixture-runner"));
    let _capabilities = EnvRestore::set("HOMEBOY_RUNNER_CAPABILITIES", None);
    let _providers = EnvRestore::set("HOMEBOY_RUNNER_PROVIDERS", None);
    let rig: RigSpec = serde_json::from_str(
        r#"{
            "id": "capability-missing",
            "pipeline": {
                "bench_prepare": [{
                    "kind": "command",
                    "command": "true",
                    "requires_capabilities": ["fixture.workspace.prepared"],
                    "requires_providers": ["fixture.runtime"]
                }]
            }
        }"#,
    )
    .expect("parse rig");

    let out = run_pipeline(&rig, "bench_prepare", true).expect("pipeline reports failure");

    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("fixture-runner"), "error: {error}");
    assert!(
        error.contains("fixture.workspace.prepared"),
        "error: {error}"
    );
    assert!(error.contains("fixture.runtime"), "error: {error}");
}

#[test]
fn test_bootstrap_step_provides_required_runner_capability_before_consumer() {
    let _env_lock = capability_env_lock();
    let _runner = EnvRestore::set("HOMEBOY_RUNNER_ID", None);
    let _capabilities = EnvRestore::set("HOMEBOY_RUNNER_CAPABILITIES", None);
    let _providers = EnvRestore::set("HOMEBOY_RUNNER_PROVIDERS", None);
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("order.txt");
    let marker_arg = marker.to_string_lossy();
    let rig: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "capability-bootstrap",
            "pipeline": {{
                "bench_prepare": [
                    {{
                        "kind": "command",
                        "id": "consumer",
                        "label": "consumer",
                        "command": "printf consumer >> {0}",
                        "requires_capabilities": ["fixture.workspace.prepared"],
                        "requires_providers": ["fixture.runtime"]
                    }},
                    {{
                        "kind": "command",
                        "id": "bootstrap",
                        "label": "bootstrap",
                        "command": "printf bootstrap: > {0}",
                        "provides_capabilities": ["fixture.workspace.prepared"],
                        "provides_providers": ["fixture.runtime"]
                    }}
                ]
            }}
        }}"#,
        marker_arg
    ))
    .expect("parse rig");

    let out = run_pipeline(&rig, "bench_prepare", true).expect("pipeline runs");

    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert_eq!(out.steps[0].label, "bootstrap");
    assert_eq!(
        std::fs::read_to_string(marker).expect("marker"),
        "bootstrap:consumer"
    );
}

struct EnvRestore {
    name: &'static str,
    previous: Option<String>,
}

fn capability_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .expect("capability env lock")
}

impl EnvRestore {
    fn set(name: &'static str, value: Option<&str>) -> Self {
        let previous = std::env::var(name).ok();
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
        Self { name, previous }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[test]
fn test_requirement_passes_when_declared_file_exists() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("marker.txt");
    std::fs::write(&marker, "ok").expect("write marker");
    let marker_arg = marker.to_string_lossy();
    let rig: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "requirement-file",
            "pipeline": {{
                "check": [{{
                    "kind": "requirement",
                    "file": "{}"
                }}]
            }}
        }}"#,
        marker_arg
    ))
    .expect("parse rig");

    let out = run_pipeline(&rig, "check", true).expect("pipeline runs");
    assert!(out.is_success(), "outcomes: {:?}", out.steps);
}

#[test]
fn test_requirement_prepare_command_runs_only_in_declared_phase() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("prepared.txt");
    let marker_arg = marker.to_string_lossy();
    let rig: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "requirement-prepare",
            "pipeline": {{
                "bench_prepare": [{{
                    "kind": "requirement",
                    "file": "{}",
                    "prepare_command": "printf prepared > {}",
                    "prepare_phases": ["bench_prepare"]
                }}]
            }}
        }}"#,
        marker_arg, marker_arg
    ))
    .expect("parse rig");

    let out = run_pipeline(&rig, "bench_prepare", true).expect("pipeline runs");
    assert!(out.is_success(), "outcomes: {:?}", out.steps);
    assert_eq!(std::fs::read_to_string(marker).expect("marker"), "prepared");
}

#[test]
fn test_requirement_fails_with_remediation_without_prepare_phase() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let marker = tmp.path().join("missing.txt");
    let marker_arg = marker.to_string_lossy();
    let rig: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "requirement-remediation",
            "pipeline": {{
                "check": [{{
                    "kind": "requirement",
                    "file": "{}",
                    "prepare_command": "printf prepared > {}",
                    "prepare_phases": ["bench_prepare"],
                    "remediation": "run bench_prepare"
                }}]
            }}
        }}"#,
        marker_arg, marker_arg
    ))
    .expect("parse rig");

    let out = run_pipeline(&rig, "check", true).expect("pipeline runs");
    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(error.contains("run bench_prepare"), "error: {error}");
    assert!(!marker.exists(), "check phase should not prepare");
}

#[test]
fn test_requirement_validates_component_path_contains() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let component_path = tmp.path().join("plugins/example-plugin");
    std::fs::create_dir_all(&component_path).expect("mkdir component");
    let component_arg = component_path.to_string_lossy();
    let rig: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "requirement-component-path",
            "components": {{
                "example": {{ "path": "{}" }}
            }},
            "pipeline": {{
                "check": [{{
                    "kind": "requirement",
                    "component": "example",
                    "component_path_contains": "plugins/example-plugin"
                }}]
            }}
        }}"#,
        component_arg
    ))
    .expect("parse rig");

    let out = run_pipeline(&rig, "check", true).expect("pipeline runs");
    assert!(out.is_success(), "outcomes: {:?}", out.steps);
}

#[test]
fn test_requirement_executable_falls_back_to_path_when_env_override_missing() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let path_bin_dir = tmp.path().join("path-bin");
    std::fs::create_dir_all(&path_bin_dir).expect("mkdir path bin");
    write_executable(&path_bin_dir.join("demo-tool"));
    let missing_override = tmp.path().join("missing-override-tool");

    let rig: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "requirement-executable-env",
            "pipeline": {{
                "check": [{{
                    "kind": "requirement",
                    "executable": "demo-tool",
                    "executable_env": "DEMO_TOOL_BIN",
                    "env": {{
                        "DEMO_TOOL_BIN": "{}",
                        "PATH": "{}"
                    }}
                }}]
            }}
        }}"#,
        missing_override.to_string_lossy(),
        path_bin_dir.to_string_lossy()
    ))
    .expect("parse rig");

    let out = run_pipeline(&rig, "check", true).expect("pipeline runs");
    assert!(out.is_success(), "outcomes: {:?}", out.steps);
}

#[test]
fn test_requirement_executable_uses_ordered_env_aliases_before_path() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let alias_bin = tmp.path().join("alias-tool");
    write_executable(&alias_bin);
    let missing_override = tmp.path().join("missing-override-tool");

    let rig: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "requirement-executable-env-aliases",
            "pipeline": {{
                "check": [{{
                    "kind": "requirement",
                    "executable": "demo-tool",
                    "executable_env": "DEMO_TOOL_BIN",
                    "executable_env_aliases": ["DEMO_TOOL_ALIAS_BIN"],
                    "env": {{
                        "DEMO_TOOL_BIN": "{}",
                        "DEMO_TOOL_ALIAS_BIN": "{}",
                        "PATH": "/nonexistent"
                    }}
                }}]
            }}
        }}"#,
        missing_override.to_string_lossy(),
        alias_bin.to_string_lossy()
    ))
    .expect("parse rig");

    let out = run_pipeline(&rig, "check", true).expect("pipeline runs");
    assert!(out.is_success(), "outcomes: {:?}", out.steps);
}

#[test]
fn test_requirement_executable_falls_back_to_path_when_env_override_is_unusable() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let path_bin_dir = tmp.path().join("path-bin");
    std::fs::create_dir_all(&path_bin_dir).expect("mkdir path bin");
    write_executable(&path_bin_dir.join("demo-tool"));

    let rig: RigSpec = serde_json::from_str(&format!(
        r#"{{
            "id": "requirement-executable-path",
            "pipeline": {{
                "check": [{{
                    "kind": "requirement",
                    "executable": "demo-tool",
                    "executable_env": "DEMO_TOOL_BIN",
                    "env": {{
                        "DEMO_TOOL_BIN": "/nonexistent/demo-tool",
                        "PATH": "{}"
                    }}
                }}]
            }}
        }}"#,
        path_bin_dir.to_string_lossy()
    ))
    .expect("parse rig");

    let out = run_pipeline(&rig, "check", true).expect("pipeline runs");
    assert!(out.is_success(), "outcomes: {:?}", out.steps);
}

#[test]
fn test_requirement_executable_reports_resolution_sources() {
    let rig: RigSpec = serde_json::from_str(
        r#"{
            "id": "requirement-executable-missing-aliases",
            "pipeline": {
                "check": [{
                    "kind": "requirement",
                    "executable": "missing-homeboy-test-tool",
                    "executable_env": "PRIMARY_TOOL_BIN",
                    "executable_env_aliases": ["SECONDARY_TOOL_BIN"],
                    "env": {
                        "PATH": "/nonexistent",
                        "PRIMARY_TOOL_BIN": "/nonexistent/primary-tool",
                        "SECONDARY_TOOL_BIN": ""
                    }
                }]
            }
        }"#,
    )
    .expect("parse rig");

    let out = run_pipeline(&rig, "check", true).expect("pipeline runs");
    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(
        error.contains("PRIMARY_TOOL_BIN=/nonexistent/primary-tool"),
        "error: {error}"
    );
    assert!(
        error.contains("SECONDARY_TOOL_BIN is unset or empty"),
        "error: {error}"
    );
    assert!(
        error.contains("PATH lookup for `missing-homeboy-test-tool`"),
        "error: {error}"
    );
}

#[test]
fn test_requirement_executable_reports_missing_tool() {
    let rig: RigSpec = serde_json::from_str(
        r#"{
            "id": "requirement-executable-missing",
            "pipeline": {
                "check": [{
                    "kind": "requirement",
                    "executable": "missing-homeboy-test-tool",
                    "env": { "PATH": "/nonexistent" },
                    "remediation": "install the tool"
                }]
            }
        }"#,
    )
    .expect("parse rig");

    let out = run_pipeline(&rig, "check", true).expect("pipeline runs");
    assert!(!out.is_success());
    let error = out.steps[0].error.as_deref().unwrap_or_default();
    assert!(
        error.contains("missing-homeboy-test-tool"),
        "error: {error}"
    );
    assert!(error.contains("install the tool"), "error: {error}");
}

fn write_executable(path: &std::path::Path) {
    std::fs::write(path, "#!/bin/sh\nexit 0\n").expect("write executable");
    make_executable(path);
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(not(unix))]
fn make_executable(_path: &std::path::Path) {}

// ---- Dependency-aware ordering ---------------------------------------------

mod dag {
    use std::collections::HashMap;
    use std::fs;

    use crate::core::rig::pipeline::run_pipeline;
    use crate::core::rig::spec::{ComponentSpec, PipelineStep, RigSpec, StackOp};

    fn command(id: &str, depends_on: &[&str], cmd: String, cwd: Option<String>) -> PipelineStep {
        PipelineStep::Command {
            step_id: Some(id.to_string()),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            cmd,
            cwd,
            env: HashMap::new(),
            requires_capabilities: Vec::new(),
            requires_providers: Vec::new(),
            provides_capabilities: Vec::new(),
            provides_providers: Vec::new(),
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
            requirements: Default::default(),
            pipeline,
            bench: None,
            fuzz: None,
            app_launcher: None,
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
                component_id: None,
                path_setting: None,
                checkout_root: None,
                remote_url: None,
                triage_remote_url: None,
                stack: Some(stack_id),
                branch: None,
                r#ref: None,
                default_ref: None,
                extensions: None,
                dependency_cache: None,
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
                dependency_cache: None,
            },
        );
        components.insert(
            "b".to_string(),
            ComponentSpec {
                path: component_b.to_string_lossy().into_owned(),
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
                dependency_cache: None,
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

// ---- Git steps --------------------------------------------------------------

mod git_steps {
    use std::collections::HashMap;
    use std::fs;
    use std::process::Command;

    use crate::core::rig::pipeline::run_pipeline;
    use crate::core::rig::spec::{ComponentSpec, GitOp, PipelineStep, RigSpec};

    fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn rig_with_git_step(component_path: String, op: GitOp) -> RigSpec {
        let mut components = HashMap::new();
        components.insert(
            "repo".to_string(),
            ComponentSpec {
                path: component_path,
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
                dependency_cache: None,
            },
        );

        let mut pipeline = HashMap::new();
        pipeline.insert(
            "up".to_string(),
            vec![PipelineStep::Git {
                step_id: Some("git-status".to_string()),
                depends_on: Vec::new(),
                component: "repo".to_string(),
                op,
                args: Vec::new(),
                label: Some("git status".to_string()),
            }],
        );

        RigSpec {
            id: "git-step-test".to_string(),
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
            app_launcher: None,
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
        }
    }

    #[test]
    fn test_git_step_fails_when_command_leaves_unmerged_index() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let repo = tmp.path();
        run_git(repo, &["init"]);

        fs::write(repo.join("conflicted.txt"), "base\n").expect("base file");
        let base = run_git(repo, &["hash-object", "-w", "conflicted.txt"]);
        fs::write(repo.join("conflicted.txt"), "ours\n").expect("ours file");
        let ours = run_git(repo, &["hash-object", "-w", "conflicted.txt"]);
        fs::write(repo.join("conflicted.txt"), "theirs\n").expect("theirs file");
        let theirs = run_git(repo, &["hash-object", "-w", "conflicted.txt"]);

        let index_info = format!(
            "100644 {base} 1\tconflicted.txt\n100644 {ours} 2\tconflicted.txt\n100644 {theirs} 3\tconflicted.txt\n"
        );
        let mut update_index = Command::new("git")
            .args(["update-index", "--index-info"])
            .current_dir(repo)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("spawn update-index");
        use std::io::Write;
        update_index
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(index_info.as_bytes())
            .expect("write index info");
        let status = update_index.wait().expect("wait update-index");
        assert!(status.success(), "update-index failed");

        let rig = rig_with_git_step(repo.to_string_lossy().into_owned(), GitOp::Status);
        let outcome = run_pipeline(&rig, "up", true).expect("pipeline outcome");

        assert!(!outcome.is_success());
        assert_eq!(outcome.steps[0].status, "fail");
        let error = outcome.steps[0].error.as_deref().unwrap_or_default();
        assert!(error.contains("left unresolved conflicts"), "{error}");
        assert!(error.contains("conflicted.txt"), "{error}");
    }
}

// ---- Extension-backed lifecycle steps ---------------------------------------

mod extension_lifecycle {
    use std::collections::HashMap;
    use std::fs;

    use crate::core::component::ScopedExtensionConfig;
    use crate::core::rig::pipeline::run_pipeline;
    use crate::core::rig::spec::{ComponentSpec, PipelineStep, RigSpec};
    use crate::test_support;

    fn rig_with_step(component_path: String, step: PipelineStep) -> RigSpec {
        let mut component_settings = HashMap::new();
        component_settings.insert(
            "rig_local".to_string(),
            serde_json::Value::String("yes".to_string()),
        );

        let mut extensions = HashMap::new();
        extensions.insert(
            "fixture-build".to_string(),
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
                component_id: None,
                path_setting: None,
                checkout_root: None,
                remote_url: None,
                triage_remote_url: None,
                stack: None,
                branch: None,
                r#ref: None,
                default_ref: None,
                extensions: Some(extensions),
                dependency_cache: None,
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
            requirements: Default::default(),
            pipeline,
            bench: None,
            fuzz: None,
            app_launcher: None,
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
        }
    }

    fn write_fixture_build_extension(home: &std::path::Path) {
        let extension_dir = home.join(".config/homeboy/extensions/fixture-build");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("fixture-build.json"),
            r#"{
                "name": "Fixture Build",
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
            write_fixture_build_extension(home.path());
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

    use crate::core::rig::pipeline::run_pipeline;
    use crate::core::rig::spec::{PipelineStep, RigSpec};
    use crate::core::rig::toolchain;
    use crate::test_support::home_env_guard;

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
                requires_capabilities: Vec::new(),
                requires_providers: Vec::new(),
                provides_capabilities: Vec::new(),
                provides_providers: Vec::new(),
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
        let _guard = home_env_guard();
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
#[path = "pipeline_test/patch.rs"]
mod patch;

// ---- Shared path step end-to-end -------------------------------------------
#[path = "pipeline_test/shared_path.rs"]
mod shared_path;
