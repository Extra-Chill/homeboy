mod general {
    use super::super::*;
    use homeboy_core::test_support::{home_env_guard, with_isolated_home};

    #[test]
    fn ensure_all_helpers_writes_all_files() {
        with_isolated_home(|_| {
            let pairs = ensure_all_helpers().expect("all helpers should be written");
            assert_eq!(pairs.len(), HELPERS.len());

            for (i, (env_var, path)) in pairs.iter().enumerate() {
                assert_eq!(env_var, HELPERS[i].env_var);
                let contents = std::fs::read_to_string(path).expect("helper should be readable");
                assert_eq!(contents, HELPERS[i].content);
            }

            assert!(
                pairs.iter().any(|(k, _)| k == FAILURE_TRAP_ENV),
                "failure trap helper should be in pairs"
            );
            assert!(
                pairs.iter().any(|(k, _)| k == RUNNER_PRELUDE_ENV),
                "runner prelude helper should be in pairs"
            );
            assert!(
                pairs.iter().any(|(k, _)| k == COMMAND_CAPTURE_ENV),
                "command capture helper should be in pairs"
            );
            assert!(
                pairs.iter().any(|(k, _)| k == BASH_PREFLIGHT_ENV),
                "bash preflight helper should be in pairs"
            );
            assert!(
                pairs.iter().any(|(k, _)| k == WRITE_TEST_RESULTS_ENV),
                "write test results helper should be in pairs"
            );
            assert!(
                pairs.iter().any(|(k, _)| k == EMIT_TEST_FAILURE_ENV),
                "emit test failure helper should be in pairs"
            );
            assert!(
                pairs.iter().any(|(k, _)| k == SIDECAR_WRITER_ENV),
                "sidecar writer helper should be in pairs"
            );
            assert!(
                pairs.iter().any(|(k, _)| k == RESOLVE_CONTEXT_ENV),
                "resolve context helper should be in pairs"
            );
            assert!(
                pairs.iter().any(|(k, _)| k == DISPOSABLE_LOCAL_DB_ENV),
                "disposable local DB helper should be in pairs"
            );
            assert!(
                pairs.iter().any(|(k, _)| k == BENCH_HELPER_JS_ENV),
                "bench JS helper should be in pairs"
            );
            assert!(
                pairs.iter().any(|(k, _)| k == RUNTIME_SETTINGS_HELPER_ENV),
                "runtime settings helper should be in pairs"
            );
        });
    }

    #[test]
    fn helper_path_resolves_by_filename_and_env_var() {
        with_isolated_home(|_| {
            let by_filename = helper_path("command-capture.sh").expect("helper path by filename");
            let by_env = helper_path(COMMAND_CAPTURE_ENV).expect("helper path by env var");

            assert_eq!(by_filename, by_env);
            assert!(by_filename.is_file());
        });
    }

    #[test]
    fn command_capture_survives_invocation_tmpdir_disappearance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp_dir = dir.path().join("tmp");
        let artifact_dir = dir.path().join("artifacts");
        let result_file = dir.path().join("result");
        let helper_file = dir.path().join("command-capture.sh");
        std::fs::create_dir_all(&tmp_dir).expect("tmp dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");
        std::fs::write(&helper_file, assets::COMMAND_CAPTURE_SH).expect("command capture helper");

        let status = std::process::Command::new("bash")
            .arg("-c")
            .arg(
                r#"
set -euo pipefail
source "$COMMAND_CAPTURE_HELPER"
homeboy_run_step_capture captured_path captured_exit "capture" -- \
    bash -c 'rm -f "$TMPDIR"/homeboy-command.*; printf captured'
printf '%s\n' "$captured_exit" > "$RESULT_FILE"
cat "$captured_path" >> "$RESULT_FILE"
homeboy_cleanup_step_capture "$captured_path"
"#,
            )
            .env("COMMAND_CAPTURE_HELPER", &helper_file)
            .env("HOMEBOY_INVOCATION_ARTIFACT_DIR", &artifact_dir)
            .env("TMPDIR", &tmp_dir)
            .env("RESULT_FILE", &result_file)
            .status()
            .expect("run command capture helper");

        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(&result_file).expect("capture result"),
            "0\ncaptured"
        );
        assert_eq!(
            std::fs::read_dir(&artifact_dir)
                .expect("artifact dir")
                .count(),
            0,
            "explicit capture cleanup should remove the artifact"
        );
    }

    #[test]
    fn declared_helper_is_content_addressed_and_reports_provenance() {
        with_isolated_home(|_| {
            let requirement = RuntimeHelperRequirement {
                id: RUNTIME_SETTINGS_HELPER_ID.to_string(),
                revision: None,
            };
            let provision = provision_declared_helpers(&[requirement])
                .expect("core materializes declared helper");

            assert_eq!(provision.len(), 1);
            assert_eq!(provision[0].env_var, RUNTIME_SETTINGS_HELPER_ENV);
            assert_eq!(provision[0].source, "homeboy-core-embedded");
            assert!(provision[0].revision.starts_with("sha256:"));
            assert!(provision[0].path.contains(&provision[0].revision));
            assert_eq!(
                std::fs::read_to_string(&provision[0].path).expect("helper contents"),
                assets::SETTINGS_SH
            );

            std::fs::write(&provision[0].path, "tampered helper").expect("tamper helper");
            let repaired = provision_declared_helpers(&[RuntimeHelperRequirement {
                id: RUNTIME_SETTINGS_HELPER_ID.to_string(),
                revision: Some(provision[0].revision.clone()),
            }])
            .expect("core replaces tampered content-addressed helper");
            assert_eq!(repaired[0].path, provision[0].path);
            assert_eq!(
                std::fs::read_to_string(&repaired[0].path).expect("repaired helper contents"),
                assets::SETTINGS_SH
            );
        });
    }

    #[test]
    fn declared_helper_rejects_missing_or_incompatible_identity() {
        with_isolated_home(|_| {
            let missing = provision_declared_helpers(&[RuntimeHelperRequirement {
                id: "missing-helper".to_string(),
                revision: None,
            }])
            .expect_err("unknown identity must fail before execution");
            assert!(missing.message.contains("not supplied by Homeboy core"));

            let incompatible = provision_declared_helpers(&[RuntimeHelperRequirement {
                id: RUNTIME_SETTINGS_HELPER_ID.to_string(),
                revision: Some("sha256:incompatible".to_string()),
            }])
            .expect_err("revision mismatch must fail before execution");
            assert!(incompatible.message.contains("requires revision"));
            assert!(incompatible.message.contains("Homeboy core provides"));
        });
    }

    #[test]
    fn runner_prelude_initializes_context_steps_trap_and_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime_dir = dir.path().join("runtime");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::write(
            runtime_dir.join("runner-prelude.sh"),
            assets::RUNNER_PRELUDE_SH,
        )
        .expect("write prelude");
        std::fs::write(
            runtime_dir.join("resolve-context.sh"),
            assets::RESOLVE_CONTEXT_SH,
        )
        .expect("write resolve context");
        std::fs::write(runtime_dir.join("runner-steps.sh"), assets::RUNNER_STEPS_SH)
            .expect("write runner steps");
        std::fs::write(runtime_dir.join("failure-trap.sh"), assets::FAILURE_TRAP_SH)
            .expect("write failure trap");
        std::fs::write(
            runtime_dir.join("sidecar-writer.sh"),
            assets::SIDECAR_WRITER_SH,
        )
        .expect("write sidecar writer");

        let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_runner_init --bash 4 --component-alias PLUGIN_PATH --steps --failure-trap --sidecar-writer; should_run_step test; type homeboy_append_lint_finding >/dev/null; printf '%s|%s|%s|%s' \"$EXTENSION_PATH\" \"$COMPONENT_PATH\" \"$PLUGIN_PATH\" \"$FAILED_STEP\"",
            runtime_dir.join("runner-prelude.sh").display()
        ))
        .env("HOMEBOY_EXTENSION_PATH", "/tmp/ext")
        .env("HOMEBOY_COMPONENT_PATH", "/tmp/project")
        .env("HOMEBOY_COMPONENT_ID", "demo")
        .output()
        .expect("run bash");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "/tmp/ext|/tmp/project|/tmp/project|"
        );
    }

    #[test]
    fn ensure_all_helpers_falls_back_when_home_is_unavailable() {
        let _guard = home_env_guard();
        let prior = std::env::var("HOME").ok();
        std::env::remove_var("HOME");

        let pairs = ensure_all_helpers().expect("helpers should fall back to temp runtime dir");
        let sidecar = pairs
            .iter()
            .find_map(|(key, value)| (key == SIDECAR_WRITER_ENV).then_some(value))
            .expect("sidecar writer helper");
        let sidecar_exists = std::path::Path::new(sidecar).is_file();

        match prior {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }

        assert!(
            sidecar_exists,
            "sidecar helper should be written under fallback runtime dir: {sidecar}"
        );
    }

    #[test]
    fn sidecar_writer_rejects_undeclared_targets() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("sidecar-writer.sh");
        std::fs::write(&helper, assets::SIDECAR_WRITER_SH).unwrap();
        for (kind, key) in [
            ("lint.finding", "lint.findings"),
            ("test.failure", "test.failures"),
            ("test.result", "test.results"),
            ("fix.result", "fix.results"),
            ("annotation.demo", "annotations"),
        ] {
            for operation in ["write", "emit", "merge"] {
                let output = std::process::Command::new("bash")
                    .env_remove("HOMEBOY_LINT_FINDINGS_FILE")
                    .env_remove("HOMEBOY_TEST_FAILURES_FILE")
                    .env_remove("HOMEBOY_TEST_RESULTS_FILE")
                    .env_remove("HOMEBOY_FIX_RESULTS_FILE")
                    .env_remove("HOMEBOY_ANNOTATIONS_DIR")
                    .arg("-c")
                    .arg(format!(
                        "source {}; homeboy_sidecar_{operation} {kind} '{{}}'",
                        helper.display()
                    ))
                    .output()
                    .unwrap();
                assert!(!output.status.success(), "{operation} {kind}");
                let error = String::from_utf8_lossy(&output.stderr);
                assert!(error.contains(key), "{error}");
                assert!(
                    error.contains("declare it in structured_sidecars"),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn disposable_local_db_helper_exports_requested_env_names_without_framework_terms() {
        let dir = tempfile::tempdir().expect("tempdir");
        let helper_path = dir.path().join("disposable-local-db.sh");
        std::fs::write(&helper_path, assets::DISPOSABLE_LOCAL_DB_SH).expect("write helper");

        let output = std::process::Command::new("bash")
            .arg("-n")
            .arg(&helper_path)
            .output()
            .expect("syntax check helper");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let content = std::fs::read_to_string(&helper_path).expect("helper content");
        assert!(content.contains("--env-database"));
        assert!(content.contains("homeboy_disposable_local_db_start"));
        assert!(!content.to_lowercase().contains("wpcom"));
        assert!(!content.to_lowercase().contains("wordpress"));
    }

    #[test]
    fn write_test_results_creates_parent_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let helper_path = dir.path().join("write-test-results.sh");
        let results_path = dir.path().join("nested").join("test-results.json");
        std::fs::write(&helper_path, assets::WRITE_TEST_RESULTS_SH).expect("write helper");

        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
            "source {}; HOMEBOY_TEST_RESULTS_FILE={}; homeboy_write_test_results 5 3 1 1; cat {}",
            helper_path.display(),
            results_path.display(),
            results_path.display()
        ))
            .output()
            .expect("run bash");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(r#""total": 5"#));
    }

    #[test]
    fn bash_preflight_helper_accepts_current_bash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let helper_path = dir.path().join("bash-preflight.sh");
        std::fs::write(&helper_path, assets::BASH_PREFLIGHT_SH).expect("write helper");

        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "source {}; homeboy_require_bash_version 3; printf ok",
                helper_path.display()
            ))
            .output()
            .expect("run bash");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "ok");
    }

    #[test]
    fn resolve_context_helper_exports_homeboy_env_and_aliases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let helper_path = dir.path().join("resolve-context.sh");
        std::fs::write(&helper_path, assets::RESOLVE_CONTEXT_SH).expect("write helper");

        let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; homeboy_resolve_context --component-alias PLUGIN_PATH; printf '%s|%s|%s|%s' \"$EXTENSION_PATH\" \"$COMPONENT_PATH\" \"$COMPONENT_ID\" \"$PLUGIN_PATH\"",
            helper_path.display()
        ))
        .env("HOMEBOY_EXTENSION_PATH", "/tmp/ext")
        .env("HOMEBOY_COMPONENT_PATH", "/tmp/project")
        .env("HOMEBOY_COMPONENT_ID", "demo")
        .output()
        .expect("run bash");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "/tmp/ext|/tmp/project|demo|/tmp/project"
        );
    }

    #[test]
    fn resolve_context_helper_supports_direct_invocation_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let extension_dir = dir.path().join("extension");
        let script_dir = extension_dir.join("scripts/test");
        let component_dir = dir.path().join("component");
        std::fs::create_dir_all(&script_dir).expect("script dir");
        std::fs::create_dir_all(&component_dir).expect("component dir");
        std::fs::write(extension_dir.join("extension.json"), "{}").expect("manifest marker");
        let helper_path = dir.path().join("resolve-context.sh");
        std::fs::write(&helper_path, assets::RESOLVE_CONTEXT_SH).expect("write helper");

        let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "cd {}; source {}; SCRIPT_DIR={}; homeboy_resolve_context; printf '%s|%s|%s' \"$EXTENSION_PATH\" \"$COMPONENT_PATH\" \"$COMPONENT_ID\"",
            component_dir.display(),
            helper_path.display(),
            script_dir.display()
        ))
        .env_remove("HOMEBOY_EXTENSION_PATH")
        .env_remove("HOMEBOY_COMPONENT_PATH")
        .env_remove("HOMEBOY_COMPONENT_ID")
        .env_remove("HOMEBOY_PROJECT_PATH")
        .output()
        .expect("run bash");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!(
                "{}|{}|component",
                extension_dir.display(),
                component_dir.display()
            )
        );
    }
}

mod bench {
    use super::super::*;

    #[test]
    fn bench_shell_helper_writes_empty_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let helper_path = dir.path().join("bench-helper.sh");
        let results_path = dir.path().join("bench-results.json");
        std::fs::write(&helper_path, assets::BENCH_HELPER_SH).expect("write helper");

        let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}; HOMEBOY_BENCH_RESULTS_FILE={}; homeboy_write_empty_bench_results demo 7; cat {}",
            helper_path.display(),
            results_path.display(),
            results_path.display()
        ))
        .output()
        .expect("run bash");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "{\"component_id\":\"demo\",\"iterations\":7,\"scenarios\":[]}\n"
        );
    }

    #[test]
    fn bench_js_helper_writes_scenario_inventory_and_filters_selection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let helper_path = dir.path().join("bench-helper.mjs");
        let runner_path = dir.path().join("runner.mjs");
        let results_path = dir.path().join("bench-results.json");
        std::fs::write(&helper_path, assets::BENCH_HELPER_JS).expect("write helper");
        std::fs::write(
        &runner_path,
        format!(
            r#"import {{
    homeboyBenchArtifactRef,
    homeboyBenchScenarioInventoryEntry,
    homeboyBenchScenarioSelected,
    homeboyWriteBenchScenarioInventory,
}} from './bench-helper.mjs';

if (!homeboyBenchScenarioSelected('cold')) throw new Error('cold should be selected');
if (homeboyBenchScenarioSelected('hot')) throw new Error('hot should not be selected');

await homeboyWriteBenchScenarioInventory('{results}', 'demo', 9, [
    homeboyBenchScenarioInventoryEntry({{
        id: 'cold',
        file: 'bench/cold.bench.js',
        source: 'in_tree',
        artifacts: {{ log: homeboyBenchArtifactRef('bench.log', {{ kind: 'text', label: 'Bench log' }}) }},
    }}),
]);
"#,
            results = results_path.display()
        ),
    )
    .expect("write runner");

        let output = std::process::Command::new("node")
            .arg(&runner_path)
            .env("HOMEBOY_BENCH_SCENARIOS", "cold,warm")
            .output()
            .expect("run node");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&results_path).expect("read results"))
                .expect("json");
        assert_eq!(value["component_id"], "demo");
        assert_eq!(value["iterations"], 0);
        assert_eq!(value["scenarios"][0]["id"], "cold");
        assert_eq!(value["scenarios"][0]["iterations"], 0);
        assert_eq!(value["scenarios"][0]["default_iterations"], 9);
        assert_eq!(
            value["scenarios"][0]["artifacts"]["log"]["path"],
            "bench.log"
        );
    }

    #[test]
    fn bench_js_helper_records_phase_resource_summary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let helper_path = dir.path().join("bench-helper.mjs");
        let runner_path = dir.path().join("runner.mjs");
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(&helper_path, assets::BENCH_HELPER_JS).expect("write helper");
        std::fs::write(
        &runner_path,
        r#"import { homeboyRunBenchPhase } from './bench-helper.mjs';

const result = await homeboyRunBenchPhase('install', process.execPath, ['-e', 'setTimeout(() => {}, 25)'], {
    sampleIntervalMs: 10,
});
if (result.code !== 0) throw new Error(`phase command failed: ${result.code}`);
"#,
    )
    .expect("write runner");

        let output = std::process::Command::new("node")
            .arg(&runner_path)
            .env("HOMEBOY_RUN_DIR", &run_dir)
            .output()
            .expect("run node");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let children_dir = run_dir.join("extension-children");
        let child_file = std::fs::read_dir(&children_dir)
            .expect("children dir")
            .flatten()
            .find(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
            .expect("phase resource file")
            .path();
        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(child_file).expect("read child resource"),
        )
        .expect("json");
        assert_eq!(value["phase"], "install");
        assert!(value["command_label"].as_str().is_some());
        assert!(value["samples"]
            .as_array()
            .is_some_and(|samples| !samples.is_empty()));
        assert_eq!(value["samples"][0]["phase"], "install");
    }

    #[test]
    fn bench_js_helper_emits_compact_progress_to_stderr() {
        let dir = tempfile::tempdir().expect("tempdir");
        let helper_path = dir.path().join("bench-helper.mjs");
        let runner_path = dir.path().join("runner.mjs");
        std::fs::write(&helper_path, assets::BENCH_HELPER_JS).expect("write helper");
        std::fs::write(
        &runner_path,
        r#"import { homeboyBenchProgress } from './bench-helper.mjs';
homeboyBenchProgress({ scenario: 'studio-agent-site-build', run: 'bfb', elapsed_ms: 252000, turn: 18, tools: 23, last: 'cli page_update' });
"#,
    )
    .expect("write runner");

        let output = std::process::Command::new("node")
            .arg(&runner_path)
            .env("HOMEBOY_BENCH_PROGRESS", "1")
            .env("HOMEBOY_BENCH_PROGRESS_STREAM", "stderr")
            .output()
            .expect("run node");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "studio-agent-site-build [bfb] 04:12 turn=18 tools=23 last=cli page_update\n"
        );
    }

    #[test]
    fn bench_js_helper_keeps_progress_quiet_when_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let helper_path = dir.path().join("bench-helper.mjs");
        let runner_path = dir.path().join("runner.mjs");
        std::fs::write(&helper_path, assets::BENCH_HELPER_JS).expect("write helper");
        std::fs::write(
            &runner_path,
            r#"import { homeboyBenchProgress } from './bench-helper.mjs';
homeboyBenchProgress({ scenario: 'studio-agent-site-build', phase: 'setup' });
"#,
        )
        .expect("write runner");

        let output = std::process::Command::new("node")
            .arg(&runner_path)
            .env("HOMEBOY_BENCH_PROGRESS", "0")
            .output()
            .expect("run node");

        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    }

    #[test]
    fn bench_runtime_helpers_document_shared_contract() {
        for content in [assets::BENCH_HELPER_JS, assets::BENCH_HELPER_SH] {
            assert!(
                content.contains("R-7 percentile"),
                "helper should document percentile method"
            );
            assert!(
                content.contains("scenario") && content.contains("slug"),
                "helper should own scenario slugging"
            );
            assert!(
                content.contains("component_id") && content.contains("scenarios"),
                "helper should own BenchResults envelope shape"
            );
        }
        // The shell helper delegates its percentile math to the native writer.
        for content in [
            assets::BENCH_HELPER_JS,
            include_str!("../../runtime_json/bench.rs"),
        ] {
            assert!(
                content.contains("p * (n - 1)"),
                "helper should use R-7 rank formula"
            );
        }
    }
}
