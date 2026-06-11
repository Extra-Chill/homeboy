    use std::collections::HashMap;
    use std::fs;
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};

    use crate::core::component::{Component, ScopedExtensionConfig};
    use crate::core::extension::{ExtensionCapability, ExtensionExecutionContext};
    use crate::test_support::with_isolated_home;

    use super::*;

    static TRACE_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn with_extra_workloads<T>(value: Option<String>, f: impl FnOnce() -> T) -> T {
        let _guard = TRACE_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("trace env lock");
        let prior = std::env::var_os("HOMEBOY_TRACE_EXTRA_WORKLOADS");
        match value {
            Some(value) => std::env::set_var("HOMEBOY_TRACE_EXTRA_WORKLOADS", value),
            None => std::env::remove_var("HOMEBOY_TRACE_EXTRA_WORKLOADS"),
        }
        let result = f();
        match prior {
            Some(value) => std::env::set_var("HOMEBOY_TRACE_EXTRA_WORKLOADS", value),
            None => std::env::remove_var("HOMEBOY_TRACE_EXTRA_WORKLOADS"),
        }
        result
    }

    #[test]
    fn test_build_trace_runner() {
        let _home_env_guard = crate::test_support::home_env_guard();
        let temp = tempfile::tempdir().unwrap();
        let extension_dir = temp.path().join("extension");
        let component_dir = temp.path().join("component");
        fs::create_dir_all(&extension_dir).unwrap();
        fs::create_dir_all(&component_dir).unwrap();
        write_extension_manifest(&extension_dir);
        let script = extension_dir.join("trace-runner.sh");
        fs::write(
            &script,
            r#"#!/usr/bin/env bash
set -euo pipefail
{
  printf 'results=%s\n' "$HOMEBOY_TRACE_RESULTS_FILE"
  printf 'scenario=%s\n' "$HOMEBOY_TRACE_SCENARIO"
  printf 'list=%s\n' "$HOMEBOY_TRACE_LIST_ONLY"
  printf 'artifact=%s\n' "$HOMEBOY_TRACE_ARTIFACT_DIR"
  printf 'run=%s\n' "$HOMEBOY_RUN_DIR"
  printf 'rig=%s\n' "${HOMEBOY_TRACE_RIG_ID:-}"
  printf 'component_path=%s\n' "${HOMEBOY_TRACE_COMPONENT_PATH:-}"
  printf 'extra_workloads=%s\n' "${HOMEBOY_TRACE_EXTRA_WORKLOADS:-}"
  printf 'attachments=%s\n' "${HOMEBOY_TRACE_ATTACHMENTS:-}"
} > "$HOMEBOY_TRACE_ARTIFACT_DIR/env.txt"
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"example","scenario_id":"close-window","status":"pass","timeline":[],"assertions":[],"artifacts":[{"label":"env","path":"artifacts/env.txt"}]}
JSON
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }

        let component = component_with_extension("example", &component_dir);
        let context = trace_context(&component, &extension_dir);
        let run_dir = RunDir::create().unwrap();
        let args = TraceRunWorkflowArgs {
            component_label: "example".to_string(),
            component_id: "example".to_string(),
            path_override: Some(component_dir.to_string_lossy().to_string()),
            settings: Vec::new(),
            runner_inputs: TraceRunnerInputs {
                json_settings: Vec::new(),
                env: Vec::new(),
                workload_paths: vec![component_dir.join("trace-fixture.trace.mjs")],
                probes: Vec::new(),
                attachments: vec![TraceAttachment::parse("logfile:/tmp/homeboy-trace.log").unwrap()],
                dependencies: Vec::new(),
                runner_capabilities: Vec::new(),
                invocation_requirements: InvocationRequirements::default(),
                public_preview: None,
            },
            scenario_id: "close-window".to_string(),
            json_summary: false,
            rig_id: Some("studio".to_string()),
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
        };

        let output =
            build_trace_runner(Some(&context), &component, &args, &run_dir, false).unwrap();
        assert!(output.success);

        let env_dump = fs::read_to_string(run_dir.path().join("artifacts/env.txt")).unwrap();
        assert!(env_dump.contains("scenario=close-window"));
        assert!(env_dump.contains("list=0"));
        assert!(env_dump.contains("rig=studio"));
        assert!(env_dump.contains(&format!("component_path={}", component_dir.display())));
        assert!(env_dump.contains("trace-fixture.trace.mjs"));
        assert!(env_dump.contains("logfile"));
        assert!(env_dump.contains("/tmp/homeboy-trace.log"));
        assert!(env_dump.contains("results="));
        assert!(env_dump.contains("artifact="));
        assert!(env_dump.contains("run="));
        run_dir.cleanup();
    }

    #[test]
    fn test_run_trace_list_workflow() {
        let _home_env_guard = crate::test_support::home_env_guard();
        let temp = tempfile::tempdir().unwrap();
        let extension_dir = temp.path().join("extension");
        let component_dir = temp.path().join("component");
        fs::create_dir_all(&extension_dir).unwrap();
        fs::create_dir_all(&component_dir).unwrap();
        write_extension_manifest(&extension_dir);
        let script = extension_dir.join("trace-runner.sh");
        fs::write(
            &script,
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s' "$HOMEBOY_TRACE_LIST_ONLY" > "$HOMEBOY_TRACE_ARTIFACT_DIR/list.txt"
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"example","scenarios":[{"id":"close-window"}]}
JSON
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }

        let component = component_with_extension("example", &component_dir);
        let context = trace_context(&component, &extension_dir);
        let run_dir = RunDir::create().unwrap();
        let args = TraceRunWorkflowArgs {
            component_label: "example".to_string(),
            component_id: "example".to_string(),
            path_override: Some(component_dir.to_string_lossy().to_string()),
            settings: Vec::new(),
            runner_inputs: TraceRunnerInputs::default(),
            scenario_id: String::new(),
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
        };

        let output = build_trace_runner(Some(&context), &component, &args, &run_dir, true).unwrap();
        assert!(output.success);
        assert_eq!(
            fs::read_to_string(run_dir.path().join("artifacts/list.txt")).unwrap(),
            "1"
        );
        run_dir.cleanup();
    }

    #[test]
    fn generic_trace_discovery_includes_conventions_and_extra_workloads() {
        let temp = tempfile::tempdir().unwrap();
        let component_dir = temp.path().join("component");
        let traces_dir = component_dir.join("traces");
        let scripts_trace_dir = component_dir.join("scripts/trace");
        fs::create_dir_all(&traces_dir).unwrap();
        fs::create_dir_all(&scripts_trace_dir).unwrap();
        fs::write(traces_dir.join("startup.trace.mjs"), "").unwrap();
        fs::write(scripts_trace_dir.join("smoke.py"), "").unwrap();
        let extra = temp.path().join("external.trace.sh");
        fs::write(&extra, "").unwrap();
        let extra_env = std::env::join_paths([extra.as_path()])
            .unwrap()
            .to_string_lossy()
            .to_string();

        with_extra_workloads(Some(extra_env), || {
            let workloads = discover_generic_trace_workloads(
                &component_dir,
                &TraceRunnerInputs {
                    workload_paths: vec![temp.path().join("rig.trace.mjs")],
                    ..TraceRunnerInputs::default()
                },
            )
            .unwrap();
            let scenario_ids = workloads
                .iter()
                .map(|path| trace_workload_scenario_id(path))
                .collect::<Vec<_>>();

            assert!(scenario_ids.contains(&"startup".to_string()));
            assert!(scenario_ids.contains(&"smoke".to_string()));
            assert!(scenario_ids.contains(&"external".to_string()));
            assert!(scenario_ids.contains(&"rig".to_string()));
        });
    }

    #[test]
    fn test_trace_is_unclaimed() {
        let unsupported = Error::new(
            ErrorCode::ExtensionUnsupported,
            "No extension provider configured for component 'example'",
            serde_json::json!({}),
        );
        assert!(trace_is_unclaimed(&unsupported));

        let missing_trace = Error::validation_invalid_argument(
            "extension",
            "Component 'example' has no linked extensions that provide trace support",
            None,
            None,
        );
        assert!(trace_is_unclaimed(&missing_trace));

        let other =
            Error::validation_invalid_argument("extension", "different problem", None, None);
        assert!(!trace_is_unclaimed(&other));
    }

    #[test]
    fn test_run_trace_workflow() {
        let args = TraceRunWorkflowArgs {
            component_label: "example".to_string(),
            component_id: "example".to_string(),
            path_override: Some("/tmp/example".to_string()),
            settings: Vec::new(),
            runner_inputs: TraceRunnerInputs::default(),
            scenario_id: "close-window".to_string(),
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
        };
        let output = RunnerOutput {
            success: false,
            exit_code: 2,
            stdout: String::new(),
            stderr: (0..25)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
            child_resource: None,
        };

        let failure = failure_from_output(&args, &output, None, None);

        assert_eq!(failure.component_id, "example");
        assert_eq!(failure.scenario_id, "close-window");
        assert_eq!(failure.exit_code, 2);
        assert!(failure.stderr_excerpt.contains("line 24"));
        assert!(!failure.stderr_excerpt.contains("line 0"));
    }

    #[test]
    fn run_trace_workflow_merges_probe_events_into_results() {
        let _home_env_guard = crate::test_support::home_env_guard();
        let temp = tempfile::tempdir().unwrap();
        let extension_dir = temp.path().join("extension");
        let component_dir = temp.path().join("component");
        let log_path = temp.path().join("probe.log");
        fs::create_dir_all(&extension_dir).unwrap();
        fs::create_dir_all(&component_dir).unwrap();
        fs::write(&log_path, "before\n").unwrap();
        write_extension_manifest(&extension_dir);
        let script = extension_dir.join("trace-runner.sh");
        fs::write(
            &script,
            format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
printf 'probe needle\n' >> '{}'
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{{"component_id":"example","scenario_id":"probes","status":"pass","timeline":[],"assertions":[],"artifacts":[]}}
JSON
"#,
                log_path.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }

        let component = component_with_extension("example", &component_dir);
        let context = trace_context(&component, &extension_dir);
        let run_dir = RunDir::create().unwrap();
        let args = TraceRunWorkflowArgs {
            component_label: "example".to_string(),
            component_id: "example".to_string(),
            path_override: Some(component_dir.to_string_lossy().to_string()),
            settings: Vec::new(),
            runner_inputs: TraceRunnerInputs {
                probes: vec![TraceProbeConfig::LogTail {
                    path: log_path.to_string_lossy().to_string(),
                    grep: Some("needle".to_string()),
                    match_pattern: None,
                }],
                ..TraceRunnerInputs::default()
            },
            scenario_id: "probes".to_string(),
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
        };

        let output =
            run_trace_workflow_with_context(Some(&context), &component, args, &run_dir, None)
                .expect("trace workflow");
        let results = output.results.expect("results");
        assert!(results
            .timeline
            .iter()
            .any(|event| event.event == "log.match"));
        run_dir.cleanup();
    }

    #[test]
    fn parsed_pass_result_overrides_noisy_runner_exit() {
        let _home_env_guard = crate::test_support::home_env_guard();
        let temp = tempfile::tempdir().unwrap();
        let extension_dir = temp.path().join("extension");
        let component_dir = temp.path().join("component");
        fs::create_dir_all(&extension_dir).unwrap();
        fs::create_dir_all(&component_dir).unwrap();
        write_extension_manifest(&extension_dir);
        let script = extension_dir.join("trace-runner.sh");
        fs::write(
            &script,
            r#"#!/usr/bin/env bash
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"example","scenario_id":"close-window","status":"pass","summary":"captured metrics","timeline":[],"assertions":[{"id":"trace-ok","status":"pass","message":"trace completed"}],"artifacts":[]}
JSON
printf 'runner envelope could not parse JSON\n' >&2
exit 1
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }

        let component = component_with_extension("example", &component_dir);
        let context = trace_context(&component, &extension_dir);
        let run_dir = RunDir::create().unwrap();
        let args = TraceRunWorkflowArgs {
            component_label: "example".to_string(),
            component_id: "example".to_string(),
            path_override: Some(component_dir.to_string_lossy().to_string()),
            settings: Vec::new(),
            runner_inputs: TraceRunnerInputs::default(),
            scenario_id: "close-window".to_string(),
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
        };

        let output =
            run_trace_workflow_with_context(Some(&context), &component, args, &run_dir, None)
                .expect("trace workflow");

        assert_eq!(output.status, "pass");
        assert_eq!(output.exit_code, 0);
        assert!(output.failure.is_none());
        assert_eq!(
            output.results.as_ref().and_then(|results| results.summary.as_deref()),
            Some("captured metrics")
        );
        run_dir.cleanup();
    }

    #[test]
    fn trace_attach_logfile_observes_without_owning_lifecycle() {
        let _home_env_guard = crate::test_support::home_env_guard();
        let temp = tempfile::tempdir().unwrap();
        let extension_dir = temp.path().join("extension");
        let component_dir = temp.path().join("component");
        fs::create_dir_all(&extension_dir).unwrap();
        fs::create_dir_all(&component_dir).unwrap();
        write_extension_manifest(&extension_dir);
        let log_path = temp.path().join("already-running.log");
        fs::write(&log_path, "before\n").unwrap();
        let script = extension_dir.join("trace-runner.sh");
        fs::write(
            &script,
            format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
printf 'during\n' >> '{}'
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{{"component_id":"example","scenario_id":"attach","status":"pass","timeline":[{{"t_ms":5,"source":"runner","event":"scenario"}}],"assertions":[],"artifacts":[]}}
JSON
"#,
                log_path.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }

        let component = component_with_extension("example", &component_dir);
        let context = trace_context(&component, &extension_dir);
        let run_dir = RunDir::create().unwrap();
        let result = run_trace_workflow_with_context(
            Some(&context),
            &component,
            TraceRunWorkflowArgs {
                component_label: "example".to_string(),
                component_id: "example".to_string(),
                path_override: Some(component_dir.to_string_lossy().to_string()),
                settings: Vec::new(),
                runner_inputs: TraceRunnerInputs {
                    json_settings: Vec::new(),
                    env: Vec::new(),
                    workload_paths: Vec::new(),
                    probes: Vec::new(),
                    attachments: vec![TraceAttachment::parse(&format!(
                        "logfile:{}",
                        log_path.display()
                    ))
                    .unwrap()],
                    dependencies: Vec::new(),
                    runner_capabilities: Vec::new(),
                    invocation_requirements: InvocationRequirements::default(),
                    public_preview: None,
                },
                scenario_id: "attach".to_string(),
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
            },
            &run_dir,
            None,
        )
        .unwrap();

        let results = result.results.expect("trace results");
        assert_eq!(results.timeline.len(), 3);
        assert!(results
            .timeline
            .iter()
            .any(|event| { event.source == "attach.logfile" && event.event == "before.present" }));
        assert!(results
            .timeline
            .iter()
            .any(|event| { event.source == "attach.logfile" && event.event == "after.present" }));
        assert!(results
            .artifacts
            .iter()
            .any(|artifact| artifact.path == "artifacts/trace-attachments.json"));
        assert!(run_dir
            .path()
            .join("artifacts/trace-attachments.json")
            .exists());
        assert_eq!(fs::read_to_string(&log_path).unwrap(), "before\nduring\n");
        run_dir.cleanup();
    }

    #[test]
    fn trace_overlay_applies_for_run_and_reverts_afterward() {
        with_isolated_home(|_| {
            let fixture = overlay_fixture(false);
            let run_dir = RunDir::create().unwrap();
            let context = trace_context(&fixture.component, &fixture.extension_dir);

            let result = run_trace_workflow_with_context(
                Some(&context),
                &fixture.component,
                fixture.args,
                &run_dir,
                None,
            )
            .unwrap();

            assert_eq!(result.exit_code, 0);
            assert_eq!(result.overlays.len(), 1);
            assert_eq!(result.overlays[0].touched_files, vec!["scenario.txt"]);
            assert!(!result.overlays[0].kept);
            assert_eq!(
                fs::read_to_string(fixture.component_dir.join("scenario.txt")).unwrap(),
                "base\n"
            );
            run_dir.cleanup();
        });
    }

    #[test]
    fn trace_overlay_dirty_target_file_fails_before_patching() {
        let fixture = overlay_fixture(false);
        fs::write(fixture.component_dir.join("scenario.txt"), "dirty\n").unwrap();

        let err = apply_trace_overlays(
            &[TraceOverlayRequest {
                variant: None,
                component_id: Some("example".to_string()),
                component_path: fixture.component_dir.to_string_lossy().to_string(),
                overlay_path: fixture.patch_path.to_string_lossy().to_string(),
            }],
            false,
        )
        .unwrap_err();

        assert!(err.message.contains("pre-existing changes"));
        assert_eq!(
            fs::read_to_string(fixture.component_dir.join("scenario.txt")).unwrap(),
            "dirty\n"
        );
    }

    #[test]
    fn trace_overlay_keep_overlay_leaves_changes_in_place() {
        with_isolated_home(|_| {
            let fixture = overlay_fixture(true);
            let run_dir = RunDir::create().unwrap();
            let context = trace_context(&fixture.component, &fixture.extension_dir);

            let result = run_trace_workflow_with_context(
                Some(&context),
                &fixture.component,
                fixture.args,
                &run_dir,
                None,
            )
            .unwrap();

            assert_eq!(result.exit_code, 0);
            assert_eq!(result.overlays.len(), 1);
            assert!(result.overlays[0].kept);
            assert_eq!(
                fs::read_to_string(fixture.component_dir.join("scenario.txt")).unwrap(),
                "overlay\n"
            );
            run_dir.cleanup();
        });
    }

    #[test]
    fn trace_overlay_run_failure_reverts_patch_and_releases_lock() {
        with_isolated_home(|_| {
            let fixture = overlay_fixture(false);
            write_failing_overlay_runner(&fixture.extension_dir.join("trace-runner.sh"));
            let run_dir = RunDir::create().unwrap();
            let context = trace_context(&fixture.component, &fixture.extension_dir);
            let result = run_trace_workflow_with_context(
                Some(&context),
                &fixture.component,
                fixture.args,
                &run_dir,
                None,
            )
            .unwrap();

            assert_eq!(result.status, "error");
            assert_eq!(result.exit_code, 7);
            assert_eq!(
                fs::read_to_string(fixture.component_dir.join("scenario.txt")).unwrap(),
                "base\n"
            );
            assert!(crate::core::extension::trace::list_trace_overlay_locks()
                .unwrap()
                .is_empty());
            run_dir.cleanup();
        });
    }

    struct OverlayFixture {
        _temp: tempfile::TempDir,
        component: Component,
        component_dir: std::path::PathBuf,
        extension_dir: std::path::PathBuf,
        patch_path: std::path::PathBuf,
        args: TraceRunWorkflowArgs,
    }

    fn overlay_fixture(keep_overlay: bool) -> OverlayFixture {
        let temp = tempfile::tempdir().unwrap();
        let extension_dir = temp.path().join("extension");
        let component_dir = temp.path().join("component");
        fs::create_dir_all(&extension_dir).unwrap();
        fs::create_dir_all(&component_dir).unwrap();
        write_extension_manifest(&extension_dir);
        write_overlay_runner(&extension_dir.join("trace-runner.sh"));
        fs::write(component_dir.join("scenario.txt"), "base\n").unwrap();
        init_git_repo(&component_dir);
        let patch_path = temp.path().join("overlay.patch");
        fs::write(
            &patch_path,
            r#"--- a/scenario.txt
+++ b/scenario.txt
@@ -1 +1 @@
-base
+overlay
"#,
        )
        .unwrap();
        let component = component_with_extension("example", &component_dir);
        let args = TraceRunWorkflowArgs {
            component_label: "example".to_string(),
            component_id: "example".to_string(),
            path_override: Some(component_dir.to_string_lossy().to_string()),
            settings: Vec::new(),
            runner_inputs: TraceRunnerInputs::default(),
            scenario_id: "overlay".to_string(),
            json_summary: false,
            rig_id: None,
            overlays: vec![TraceOverlayRequest {
                variant: None,
                component_id: Some("example".to_string()),
                component_path: component_dir.to_string_lossy().to_string(),
                overlay_path: patch_path.to_string_lossy().to_string(),
            }],
            keep_overlay,
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
        };
        OverlayFixture {
            _temp: temp,
            component,
            component_dir,
            extension_dir,
            patch_path,
            args,
        }
    }

    fn write_overlay_runner(script: &std::path::Path) {
        fs::write(
            script,
            r#"#!/usr/bin/env bash
set -euo pipefail
grep -q '^overlay$' "$HOMEBOY_TRACE_COMPONENT_PATH/scenario.txt"
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"example","scenario_id":"overlay","status":"pass","timeline":[],"assertions":[],"artifacts":[]}
JSON
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(script, perms).unwrap();
        }
    }

    fn write_failing_overlay_runner(script: &std::path::Path) {
        fs::write(
            script,
            r#"#!/usr/bin/env bash
set -euo pipefail
grep -q '^overlay$' "$HOMEBOY_TRACE_COMPONENT_PATH/scenario.txt"
printf 'intentional trace failure\n' >&2
exit 7
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(script, perms).unwrap();
        }
    }

    fn init_git_repo(path: &std::path::Path) {
        git(path, &["init"]);
        git(path, &["add", "scenario.txt"]);
        git(
            path,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "init",
            ],
        );
    }

    fn git(path: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn component_with_extension(id: &str, path: &std::path::Path) -> Component {
        let mut extensions = HashMap::new();
        extensions.insert(
            "trace-extension".to_string(),
            ScopedExtensionConfig::default(),
        );
        Component {
            id: id.to_string(),
            local_path: path.to_string_lossy().to_string(),
            extensions: Some(extensions),
            ..Default::default()
        }
    }

    fn trace_context(
        component: &Component,
        extension_dir: &std::path::Path,
    ) -> ExtensionExecutionContext {
        ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Trace,
            extension_id: "trace-extension".to_string(),
            extension_path: extension_dir.to_path_buf(),
            script_path: "trace-runner.sh".to_string(),
            settings: Vec::new(),
        }
    }

    fn write_extension_manifest(extension_dir: &std::path::Path) {
        fs::write(
            extension_dir.join("extension.json"),
            r#"{
                "name":"Trace Extension",
                "version":"0.0.0",
                "trace":{"extension_script":"trace-runner.sh"}
            }"#,
        )
        .unwrap();
    }
