//! Generic-runner trace workflow, provenance, and baseline-root tests.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use crate::core::component::Component;
use crate::core::engine::baseline::BaselineFlags;
use crate::core::engine::run_dir::RunDir;
use crate::core::error::{Error, ErrorCode};
use crate::core::extension::trace::attach::TraceAttachment;
use crate::core::extension::trace::canonicality::TraceCanonicalPolicy;
use crate::core::extension::trace::parsing::{TraceGitProvenance, TraceResults, TraceStatus};
use crate::core::extension::trace::probes::TraceProbeConfig;
use crate::core::extension::{ExtensionCapability, ExtensionExecutionContext, RunnerOutput};

use super::super::list::run_trace_list_workflow;
use super::super::provenance::{
    git_dirty_state, git_provenance, push_git_provenance_reasons, trace_provenance,
};
use super::super::runner::{
    build_trace_runner, failure_from_output, resolve_trace_baseline_root, trace_is_unclaimed,
    trace_probes_with_fswatch_attachments,
};
use super::super::types::{TraceListWorkflowArgs, TraceRunWorkflowArgs, TraceRunnerInputs};
use super::super::workflow::run_trace_workflow;
#[test]
fn test_build_trace_runner() {
    let temp = tempfile::tempdir().unwrap();
    let component = test_component(temp.path());
    let run_dir = RunDir::create().unwrap();
    let output = build_trace_runner(
        None,
        &component,
        &test_run_args(temp.path()),
        &run_dir,
        false,
    )
    .unwrap();
    assert!(!output.success);
    assert_eq!(output.exit_code, 3);
    run_dir.cleanup();
}

#[test]
fn test_run_trace_list_workflow() {
    crate::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().unwrap();
        let component = test_component(temp.path());
        let run_dir = RunDir::create().unwrap();
        let list = run_trace_list_workflow(
            &component,
            TraceListWorkflowArgs {
                component_label: "example".to_string(),
                component_id: "example".to_string(),
                path_override: Some(temp.path().to_string_lossy().to_string()),
                settings: Vec::new(),
                runner_inputs: TraceRunnerInputs::default(),
                rig_id: None,
            },
            &run_dir,
        )
        .unwrap();
        assert!(list.scenarios.is_empty());
        run_dir.cleanup();
    });
}

#[test]
fn test_run_trace_workflow() {
    crate::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().unwrap();
        let component = test_component(temp.path());
        let run_dir = RunDir::create().unwrap();
        let result =
            run_trace_workflow(&component, test_run_args(temp.path()), &run_dir, None).unwrap();
        assert_eq!(result.status, "error");
        assert_eq!(result.exit_code, 3);
        run_dir.cleanup();
    });
}

#[test]
fn trace_failure_records_child_cleanup_context() {
    let temp = tempfile::tempdir().unwrap();
    let artifact_dir = temp.path().join("artifacts");
    let mut args = test_run_args(temp.path());
    args.runner_inputs.json_settings.push((
        "recipe_path".to_string(),
        serde_json::Value::String("recipes/stripe-ece.yml".to_string()),
    ));
    let output = RunnerOutput {
        exit_code: 143,
        success: false,
        stdout: String::new(),
        stderr: "Homeboy interrupted by signal 15".to_string(),
        timed_out: false,
        child_resource: Some(
            crate::core::engine::resource::ExtensionChildResourceSummary {
                child: crate::core::engine::resource::ChildProcessIdentity {
                    root_pid: 4242,
                    command_label: "custom-provider run recipes/browser.yml".to_string(),
                },
                phase: None,
                started_at: "2026-06-06T00:00:00Z".to_string(),
                finished_at: "2026-06-06T00:00:01Z".to_string(),
                duration_ms: 1000,
                sampled_peak_rss_bytes: None,
                sampled_peak_cpu_percent: None,
                sampled_peak_at_ms: None,
                sampled_peak_child_count: None,
                samples: Vec::new(),
                warnings: Vec::new(),
            },
        ),
        extension_phase_timings: Vec::new(),
    };
    let results = TraceResults {
        component_id: "example".to_string(),
        scenario_id: "fixture".to_string(),
        status: TraceStatus::Error,
        summary: None,
        failure: None,
        rig: None,
        evidence: None,
        preview: None,
        timeline: vec![crate::core::observation::timeline::ObservationEvent {
            t_ms: 250,
            source: "homeboy".to_string(),
            event: "recipe.waiting".to_string(),
            data: BTreeMap::new(),
        }],
        span_definitions: Vec::new(),
        span_results: Vec::new(),
        assertions: Vec::new(),
        temporal_assertions: Vec::new(),
        artifacts: Vec::new(),
        metrics: Default::default(),
        dependencies: Vec::new(),
        toolchain: None,
        components: None,
    };

    let failure = failure_from_output(&args, &output, Some(&artifact_dir), Some(&results));

    assert_eq!(failure.child_pid, Some(4242));
    assert_eq!(
        failure.child_command.as_deref(),
        Some("custom-provider run recipes/browser.yml")
    );
    assert_eq!(
        failure.recipe_path.as_deref(),
        Some("recipes/stripe-ece.yml")
    );
    assert_eq!(
        failure.artifact_root.as_deref(),
        Some(artifact_dir.to_string_lossy().as_ref())
    );
    assert_eq!(
        failure.last_observed_homeboy_event.as_deref(),
        Some("homeboy.recipe.waiting")
    );
    assert_eq!(
        failure.current_phase.as_deref(),
        Some("homeboy.recipe.waiting")
    );
    assert_eq!(failure.cleanup_succeeded, Some(true));
}

#[test]
fn test_trace_is_unclaimed() {
    let unsupported = Error::new(
        ErrorCode::ExtensionUnsupported,
        "No extension provider configured for component 'example'",
        serde_json::json!({}),
    );
    assert!(trace_is_unclaimed(&unsupported));
}

#[test]
fn resolve_trace_baseline_root_without_rig_returns_component_path() {
    let temp = tempfile::tempdir().unwrap();
    let component_path = temp.path().to_string_lossy().to_string();
    let root = resolve_trace_baseline_root(&component_path, None).unwrap();
    assert_eq!(root, PathBuf::from(&component_path));
    // Crucially, no homeboy.json gets created in the component checkout
    // just by resolving — that only happens when a baseline is saved.
    assert!(!temp.path().join("homeboy.json").exists());
}

#[test]
fn resolve_trace_baseline_root_with_rig_uses_rig_state_dir_and_skips_component_path() {
    let temp = tempfile::tempdir().unwrap();
    let component_path = temp.path().to_string_lossy().to_string();
    let rig_id = format!("__hb-trace-baseline-test-{}", std::process::id());

    let root = resolve_trace_baseline_root(&component_path, Some(&rig_id)).unwrap();

    assert!(
        root.ends_with(format!("{}.state/baselines", rig_id)),
        "rig baseline root should live under <id>.state/baselines, got {}",
        root.display()
    );
    assert!(
        root.exists(),
        "rig baseline root should be created on resolve"
    );
    assert!(
        !root.starts_with(temp.path()),
        "rig baseline root must not live inside the component checkout"
    );
    assert!(
        !temp.path().join("homeboy.json").exists(),
        "resolving a rig baseline root must not touch component homeboy.json"
    );

    // Cleanup: best-effort remove the rig state dir we created.
    if let Some(state_dir) = root.parent() {
        let _ = std::fs::remove_dir_all(state_dir);
    }
}

#[test]
fn rig_save_baseline_does_not_write_component_homeboy_json() {
    use crate::core::extension::trace::baseline;
    use crate::core::extension::trace::parsing::{
        TraceResults, TraceSpanResult, TraceSpanStatus, TraceStatus,
    };

    let temp = tempfile::tempdir().unwrap();
    let component_path = temp.path().to_string_lossy().to_string();
    let rig_id = format!("__hb-trace-save-test-{}", std::process::id());

    let baseline_root = resolve_trace_baseline_root(&component_path, Some(&rig_id)).unwrap();

    let results = TraceResults {
        component_id: "studio".to_string(),
        scenario_id: "create-site".to_string(),
        status: TraceStatus::Pass,
        summary: None,
        failure: None,
        rig: None,
        evidence: None,
        timeline: Vec::new(),
        span_definitions: Vec::new(),
        span_results: vec![TraceSpanResult {
            id: "submit_to_cli".to_string(),
            from: "ui.submit".to_string(),
            to: "cli.start".to_string(),
            status: TraceSpanStatus::Ok,
            duration_ms: Some(120),
            from_t_ms: Some(0),
            to_t_ms: Some(120),
            missing: Vec::new(),
            message: None,
        }],
        assertions: Vec::new(),
        temporal_assertions: Vec::new(),
        artifacts: Vec::new(),
        metrics: Default::default(),
        toolchain: None,
        components: None,
        dependencies: Vec::new(),
        preview: None,
    };

    let written = baseline::save_baseline(&baseline_root, "studio", &results, Some(&rig_id))
        .expect("rig baseline saves into rig state dir");

    assert!(
        written.starts_with(&baseline_root),
        "rig baseline must be written under the rig baseline root, got {}",
        written.display()
    );
    assert!(
        !temp.path().join("homeboy.json").exists(),
        "rig baseline save must not write homeboy.json into the component checkout"
    );

    let loaded = baseline::load_baseline(&baseline_root, Some(&rig_id))
        .expect("rig baseline loads from rig state dir");
    assert_eq!(loaded.metadata.spans[0].id, "submit_to_cli");

    if let Some(state_dir) = baseline_root.parent() {
        let _ = std::fs::remove_dir_all(state_dir);
    }
}

#[test]
fn fswatch_attachments_add_file_watch_probes_without_duplicates() {
    let attachment = TraceAttachment::parse("fswatch:/tmp/auth.json").unwrap();
    let existing_probe = TraceProbeConfig::FileWatch {
        path: "/tmp/auth.json".to_string(),
        interval_ms: Some(50),
    };

    let merged =
        trace_probes_with_fswatch_attachments(&[existing_probe.clone()], &[attachment.clone()]);
    assert_eq!(merged, vec![existing_probe]);

    let merged = trace_probes_with_fswatch_attachments(&[], &[attachment]);
    assert_eq!(
        merged,
        vec![TraceProbeConfig::FileWatch {
            path: "/tmp/auth.json".to_string(),
            interval_ms: None,
        }]
    );
}

#[test]
fn git_dirty_state_reports_clean_checkout_as_known_clean() {
    let temp = tempfile::tempdir().unwrap();
    let init = Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .expect("git init runs");
    assert!(init.status.success());

    assert_eq!(git_dirty_state(temp.path()), Some(false));

    std::fs::write(temp.path().join("untracked.txt"), "changed").unwrap();

    assert_eq!(git_dirty_state(temp.path()), Some(true));
}

#[test]
fn git_provenance_resolves_file_paths_to_checkout_root() {
    let temp = tempfile::tempdir().unwrap();
    let init = Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .expect("git init runs");
    assert!(init.status.success());
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp.path())
        .output()
        .expect("git config user.email runs");
    Command::new("git")
        .args(["config", "user.name", "Homeboy Test"])
        .current_dir(temp.path())
        .output()
        .expect("git config user.name runs");
    let bin = temp.path().join("packages/cli/dist/index.js");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "#!/usr/bin/env node\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(temp.path())
        .output()
        .expect("git add runs");
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(temp.path())
        .output()
        .expect("git commit runs");

    let provenance = git_provenance(&bin, Some("fixture-toolchain"));

    assert_eq!(
        provenance.path,
        temp.path().canonicalize().unwrap().to_string_lossy()
    );
    assert!(provenance.sha.is_some());
    assert_eq!(provenance.dirty, Some(false));
}

#[test]
fn trace_provenance_records_declared_toolchain_env_path() {
    crate::test_support::with_isolated_home(|home| {
        let component_dir = tempfile::tempdir().unwrap();
        let toolchain_dir = tempfile::tempdir().unwrap();
        init_git_repo(component_dir.path());
        init_git_repo(toolchain_dir.path());
        let bin = toolchain_dir.path().join("packages/cli/dist/index.js");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/usr/bin/env node\n").unwrap();
        git(toolchain_dir.path(), &["add", "."]);
        git(toolchain_dir.path(), &["commit", "-m", "add cli bin"]);

        let component = test_component(component_dir.path());
        let context = write_trace_extension(home.path(), &component);
        let mut args = test_run_args(component_dir.path());
        args.runner_inputs.env.push((
            "FIXTURE_TOOLCHAIN_BIN".to_string(),
            bin.to_string_lossy().to_string(),
        ));

        let (toolchain, _) = trace_provenance(
            Some(&context),
            &component_dir.path().to_string_lossy(),
            &args,
        )
        .unwrap();

        let provenance = toolchain
            .toolchains
            .get("fixture-toolchain")
            .expect("declared toolchain provenance");
        assert_eq!(
            provenance.source.as_deref(),
            Some("env:FIXTURE_TOOLCHAIN_BIN")
        );
        assert_eq!(
            toolchain.toolchains.get("fixture-toolchain"),
            Some(provenance)
        );
    });
}

#[test]
fn dirty_git_provenance_makes_toolchain_non_canonical() {
    let mut reasons = Vec::new();
    let provenance = TraceGitProvenance {
        path: "/tmp/homeboy".to_string(),
        sha: Some("abc123".to_string()),
        branch: Some("main".to_string()),
        dirty: Some(true),
        source: Some("homeboy".to_string()),
    };

    push_git_provenance_reasons("homeboy", &provenance, &mut reasons);

    assert_eq!(
        reasons,
        vec!["homeboy checkout is dirty for trace toolchain provenance: /tmp/homeboy"]
    );
}

fn test_component(path: &std::path::Path) -> Component {
    Component {
        id: "example".to_string(),
        local_path: path.to_string_lossy().to_string(),
        ..Default::default()
    }
}

fn write_trace_extension(
    home: &std::path::Path,
    component: &Component,
) -> ExtensionExecutionContext {
    let extension_id = "fixture-extension";
    let extension_dir = home.join(".config/homeboy/extensions").join(extension_id);
    std::fs::create_dir_all(&extension_dir).expect("extension dir");
    std::fs::write(
        extension_dir.join(format!("{extension_id}.json")),
        serde_json::json!({
            "name": "Fixture Extension",
            "version": "0.0.0",
            "trace": {
                "extension_script": "trace.js",
                "toolchain_provenance": [
                    {
                        "id": "fixture-toolchain",
                        "label": "Fixture Toolchain",
                        "env_keys": ["FIXTURE_TOOLCHAIN_BIN"]
                    }
                ]
            }
        })
        .to_string(),
    )
    .expect("extension manifest");
    std::fs::write(extension_dir.join("trace.js"), "#!/usr/bin/env node\n").unwrap();

    ExtensionExecutionContext {
        component: component.clone(),
        capability: ExtensionCapability::Trace,
        extension_id: extension_id.to_string(),
        extension_path: extension_dir,
        script_path: "trace.js".to_string(),
        settings: Vec::new(),
        accepted_setting_keys: Vec::new(),
    }
}

fn init_git_repo(path: &std::path::Path) {
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Homeboy Test"]);
    std::fs::write(path.join("README.md"), "test\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "initial"]);
}

fn git(path: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap_or_else(|err| panic!("git {:?} failed to start: {}", args, err));
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn test_run_args(path: &std::path::Path) -> TraceRunWorkflowArgs {
    TraceRunWorkflowArgs {
        component_label: "example".to_string(),
        component_id: "example".to_string(),
        path_override: Some(path.to_string_lossy().to_string()),
        settings: Vec::new(),
        runner_inputs: TraceRunnerInputs::default(),
        scenario_id: "missing".to_string(),
        json_summary: false,
        rig_id: None,
        overlays: Vec::new(),
        keep_overlay: false,
        span_definitions: Vec::new(),
        baseline_flags: BaselineFlags {
            baseline: false,
            ignore_baseline: true,
            ratchet: false,
        },
        regression_threshold_percent:
            crate::core::extension::trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
        regression_min_delta_ms:
            crate::core::extension::trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
        canonical_policy: TraceCanonicalPolicy::Development,
        checkout_provenance: None,
    }
}
