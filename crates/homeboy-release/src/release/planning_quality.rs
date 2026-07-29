use homeboy_core::component::Component;
use homeboy_core::engine::run_dir::RunDir;
use homeboy_core::error::{ActionSafety, CommandEvidence, Error, ExecutableAction, Result};
use homeboy_extension as extension;
use homeboy_extension::{self, ExtensionCapability};
use std::path::Path;

use super::scope::ReleaseScope;

/// Maximum captured stdout/stderr bytes retained per stream in command
/// evidence. Bounds the structured error payload while keeping the tail (where
/// the failing assertion / stack trace almost always lives) visible.
const COMMAND_EVIDENCE_MAX_BYTES: usize = 16 * 1024;

/// Bound a captured stream to its last [`COMMAND_EVIDENCE_MAX_BYTES`], keeping
/// the most recent (tail) output. Returns the bounded string and whether it was
/// truncated. Splits on a UTF-8 boundary so the result is always valid.
fn bound_evidence_stream(stream: &str) -> (String, bool) {
    if stream.len() <= COMMAND_EVIDENCE_MAX_BYTES {
        return (stream.to_string(), false);
    }

    let mut start = stream.len() - COMMAND_EVIDENCE_MAX_BYTES;
    while start < stream.len() && !stream.is_char_boundary(start) {
        start += 1;
    }
    (stream[start..].to_string(), true)
}

/// Build [`CommandEvidence`] from a resolved command description and captured
/// runner output, bounding each stream for the structured error payload.
fn command_evidence(
    command: String,
    cwd: Option<String>,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> CommandEvidence {
    let (stdout, stdout_truncated) = bound_evidence_stream(stdout);
    let (stderr, stderr_truncated) = bound_evidence_stream(stderr);
    CommandEvidence {
        command,
        cwd,
        // The release quality gates run on the local controller. Offloaded
        // runner execution carries its own evidence path.
        location: Some("local".to_string()),
        exit_code,
        stdout,
        stderr,
        truncated: stdout_truncated || stderr_truncated,
    }
}

/// Outcome of a release lint preflight.
///
#[derive(Debug)]
pub(super) enum LintQualityOutcome {
    /// Lint ran and passed, or no lint runner is configured (`ran == false`).
    Passed { ran: bool },
    /// Lint findings or a lint harness/tool/evidence failure blocked release.
    Failed(Error),
}

/// Run release lint via the component's extension.
///
/// Missing lint support is not a release blocker because not every extension
/// provides it.
pub(super) fn validate_lint_quality(
    component: &Component,
    component_id: &str,
) -> LintQualityOutcome {
    // Preserve infrastructure errors verbatim. Reclassifying them as invalid
    // lint arguments discards their typed diagnostics and evidence.
    let runner_error = |error: Error, extension_id: Option<&str>, changed_since: Option<&str>| {
        LintQualityOutcome::Failed(lint_runner_error(
            error,
            component,
            component_id,
            extension_id,
            changed_since,
        ))
    };

    if component.has_script(ExtensionCapability::Lint) {
        homeboy_core::log_status!("release", "Running lint (scripts.lint)...");

        let workflow = match extension::lint::run_self_check_lint_workflow(
            component,
            Path::new(&component.local_path),
            component.id.clone(),
            false,
        ) {
            Ok(workflow) => workflow,
            Err(error) => return runner_error(error, None, None),
        };

        if workflow.status == "passed" {
            homeboy_core::log_status!("release", "Lint passed");
            return LintQualityOutcome::Passed { ran: true };
        }

        return LintQualityOutcome::Failed(quality_error(
            "lint",
            format!("Lint failed (exit code {})", workflow.exit_code),
        ));
    }

    let lint_context =
        match homeboy_core::extension_execution::resolve_execution_context_if_available(
            component,
            ExtensionCapability::Lint,
        ) {
            Ok(Some(context)) => context,
            Ok(None) => return LintQualityOutcome::Passed { ran: false },
            Err(error) => return runner_error(error, None, None),
        };

    homeboy_core::log_status!("release", "Running lint ({})...", lint_context.extension_id);

    let release_run_dir = match RunDir::create() {
        Ok(dir) => dir,
        Err(e) => return LintQualityOutcome::Failed(e),
    };
    let changed_since =
        match ReleaseScope::resolve(component, component_id).and_then(|scope| scope.latest_tag()) {
            Ok(tag) => tag,
            Err(e) => {
                release_run_dir.finish(false);
                return runner_error(e, Some(&lint_context.extension_id), None);
            }
        };
    let workflow = extension::lint::run_main_lint_workflow(
        component,
        Path::new(&component.local_path),
        extension::lint::LintRunWorkflowArgs {
            component_label: component_id.to_string(),
            component_id: component_id.to_string(),
            path_override: None,
            settings: Vec::new(),
            summary: false,
            file: None,
            glob: None,
            changed_only: false,
            changed_since: changed_since.clone(),
            precomputed_changed_files: None,
            sniff_filters: extension::lint::LintSniffFilters::default(),
            category: None,
            ci_env: Vec::new(),
            baseline_flags: Default::default(),
            json_summary: true,
        },
        &release_run_dir,
    );
    let workflow = match workflow {
        Ok(workflow) => workflow,
        Err(error) => {
            release_run_dir.finish(false);
            return runner_error(
                error,
                Some(&lint_context.extension_id),
                changed_since.as_deref(),
            );
        }
    };

    if workflow.status == "passed" && workflow.exit_code == 0 {
        homeboy_core::log_status!("release", "Lint passed");
        release_run_dir.finish(true);
        LintQualityOutcome::Passed { ran: true }
    } else {
        release_run_dir.finish(false);
        LintQualityOutcome::Failed(lint_workflow_failure(&workflow, &release_run_dir))
    }
}

/// Attach a diagnostic action without changing the original infrastructure
/// error's code, message, details, or evidence. The command intentionally says
/// "fresh" because a new CLI invocation cannot promise the release process's
/// complete runtime identity.
fn lint_runner_error(
    error: Error,
    component: &Component,
    component_id: &str,
    extension_id: Option<&str>,
    changed_since: Option<&str>,
) -> Error {
    let mut args = vec![
        "--placement".to_string(),
        "local".to_string(),
        "lint".to_string(),
        component_id.to_string(),
        "--path".to_string(),
        component.local_path.clone(),
    ];
    if let Some(extension_id) = extension_id {
        args.extend(["--extension".to_string(), extension_id.to_string()]);
    }
    if let Some(changed_since) = changed_since {
        args.extend(["--changed-since".to_string(), changed_since.to_string()]);
    }

    error.with_action(
        ExecutableAction::new(
            "release.lint.fresh_diagnostic",
            "run a fresh local lint diagnostic with the release lint scope",
            "homeboy",
            args,
            ActionSafety::ReadOnly,
        )
        .with_evidence(serde_json::json!({
            "kind": "fresh_diagnostic",
            "release_execution_location": "local",
            "source_path": component.local_path,
            "component_id": component_id,
            "extension_id": extension_id,
            "changed_since": changed_since,
            "settings": {},
        })),
    )
}

fn lint_workflow_failure(
    workflow: &extension::lint::LintRunWorkflowResult,
    run_dir: &RunDir,
) -> Error {
    let findings = workflow.findings.as_deref().unwrap_or_default();
    let producer_errors = workflow
        .producer_summaries
        .iter()
        .filter(|producer| producer.status == "error")
        .count();
    let baseline_new = workflow
        .baseline_comparison
        .as_ref()
        .map(|comparison| comparison.new_items.len());
    let baseline_known = baseline_new.map(|new| findings.len().saturating_sub(new));
    let message = format!(
        "Lint failed (exit code {}, {} finding(s), {} producer error(s){}{})",
        workflow.exit_code,
        findings.len(),
        producer_errors,
        baseline_new
            .map(|count| format!(", {} baseline-new", count))
            .unwrap_or_default(),
        baseline_known
            .map(|count| format!(", {} baseline-known", count))
            .unwrap_or_default()
    );
    let mut error = quality_error("lint", message);
    if let Some(details) = error.details.as_object_mut() {
        details.insert(
            "lint_workflow".to_string(),
            serde_json::json!({
                "exit_code": workflow.exit_code,
                "finding_count": findings.len(),
                "findings": findings.iter().take(20).collect::<Vec<_>>(),
                "producer_error_count": producer_errors,
                "producer_summaries": workflow.producer_summaries,
                "baseline_new_count": baseline_new,
                "baseline_known_count": baseline_known,
                "harness_error": workflow.harness_error,
                "hints": workflow.hints,
                "run_dir": run_dir.path(),
            }),
        );
    }
    error
}

/// Run release tests via the component's extension.
///
/// Returns whether a test command was available and executed. Missing test
/// support is not a release blocker because not every extension provides it.
pub(super) fn validate_test_quality(component: &Component) -> Result<bool> {
    if component.has_script(ExtensionCapability::Test) {
        homeboy_core::log_status!("release", "Running tests (scripts.test)...");

        let workflow = extension::test::run_self_check_test_workflow(
            component,
            Path::new(&component.local_path),
            component.id.clone(),
            false,
        )
        .map_err(|e| quality_error("test", format!("Test runner error: {}", e)))?;

        if workflow.status == "passed" {
            homeboy_core::log_status!("release", "Tests passed");
            return Ok(true);
        }

        // Surface the self-check command and its captured output (the workflow
        // already retains a bounded tail on failure) so the gate failure is
        // actionable instead of an opaque exit code.
        let (stdout, stderr) = workflow
            .raw_output
            .as_ref()
            .map(|raw| (raw.stdout_tail.clone(), raw.stderr_tail.clone()))
            .unwrap_or_default();
        let evidence = command_evidence(
            format!("{} self-check scripts.test", component.id),
            Some(component.local_path.clone()),
            workflow.exit_code,
            &stdout,
            &stderr,
        );

        return Err(quality_error_with_evidence(
            "test",
            format!("Tests failed (exit code {})", workflow.exit_code),
            evidence,
        ));
    }

    let test_context = extension::test::resolve_test_command(component);

    let Ok(test_context) = test_context else {
        return Ok(false);
    };

    homeboy_core::log_status!(
        "release",
        "Running tests ({})...",
        test_context.extension_id
    );
    let resolved_command = format!(
        "{} ({})",
        test_context.extension_id, test_context.script_path
    );
    let test_run_dir = RunDir::create()?;
    let output = extension::test::build_test_runner(
        component,
        None,
        &[],
        &[],
        false,
        false,
        None,
        None,
        &test_run_dir,
    )
    .and_then(|runner| runner.run())?;

    if output.success {
        homeboy_core::log_status!("release", "Tests passed");
        test_run_dir.finish(true);
        Ok(true)
    } else {
        let evidence = command_evidence(
            resolved_command,
            Some(component.local_path.clone()),
            output.exit_code,
            &output.stdout,
            &output.stderr,
        );
        test_run_dir.finish(false);
        Err(quality_error_with_evidence(
            "test",
            code_quality_failure_message("Tests", &output),
            evidence,
        ))
    }
}

fn quality_error(field: &str, message: String) -> Error {
    homeboy_core::log_status!("release", "Code quality check failed: {}", message);

    let mut tried = vec!["Fix the issue above before releasing".to_string()];
    tried.extend(scoped_skip_guidance(field));

    Error::validation_invalid_argument(field, message, None, Some(tried))
}

/// Like [`quality_error`] but attaches captured [`CommandEvidence`] so the
/// failing command and its stdout/stderr surface in the structured error's
/// `error_details.command_evidence`. The `tried` hints point operators at that
/// evidence instead of a phantom "issue above".
fn quality_error_with_evidence(field: &str, message: String, evidence: CommandEvidence) -> Error {
    homeboy_core::log_status!("release", "Code quality check failed: {}", message);
    homeboy_core::log_status!(
        "release",
        "Failing command: {} (exit code {})",
        evidence.command,
        evidence.exit_code
    );

    let mut tried = vec![
        "Inspect error_details.command_evidence for the failing command, cwd, exit code, and captured stdout/stderr".to_string(),
        "Reproduce in isolation: homeboy test <component>".to_string(),
    ];
    tried.extend(scoped_skip_guidance(field));

    Error::validation_invalid_argument_with_evidence(
        field,
        message,
        None,
        Some(tried),
        Some(evidence),
    )
}

/// Recovery guidance recommending the narrowest supported release-gate bypass
/// for the gate that failed (#9641).
///
/// A failing gate should point operators at `--skip-checks=<gate>` (which keeps
/// every other safety gate enabled) rather than the bare `--skip-checks` that
/// disables audit, lint, AND test. This matters most for autonomous agents,
/// which should preserve every green gate while making an explicit decision
/// about the single known baseline failure.
///
/// The `field` is the failing gate identity ("lint"/"test"). Unknown gates fall
/// back to the scoped-then-broad ordering without a specific gate name.
fn scoped_skip_guidance(field: &str) -> Vec<String> {
    // Only audit/lint/test are valid `--skip-checks=<check>` values.
    let gate = match field.to_ascii_lowercase().as_str() {
        "lint" => Some("lint"),
        "test" | "tests" => Some("test"),
        "audit" => Some("audit"),
        _ => None,
    };

    match gate {
        Some(gate) => {
            let remaining: Vec<&str> = ["audit", "lint", "test"]
                .into_iter()
                .filter(|check| *check != gate)
                .collect();
            vec![
                format!(
                    "To skip only this gate: homeboy release <component> --skip-checks={gate} (keeps {} enabled)",
                    remaining.join(", ")
                ),
                "Last resort — bare `--skip-checks` disables ALL audit/lint/test gates: homeboy release <component> --skip-checks".to_string(),
            ]
        }
        None => vec![
            "To skip only the failing gate: homeboy release <component> --skip-checks=<audit|lint|test>".to_string(),
            "Last resort — bare `--skip-checks` disables ALL audit/lint/test gates: homeboy release <component> --skip-checks".to_string(),
        ],
    }
}

fn code_quality_failure_message(check: &str, output: &extension::RunnerOutput) -> String {
    if is_runner_infrastructure_failure(output) {
        format!(
            "{} runner infrastructure failure (exit code {})",
            check, output.exit_code
        )
    } else {
        format!("{} failed (exit code {})", check, output.exit_code)
    }
}

fn is_runner_infrastructure_failure(output: &extension::RunnerOutput) -> bool {
    if output.exit_code >= 2 || output.exit_code < 0 {
        return true;
    }

    let combined = format!("{}\n{}", output.stdout, output.stderr).to_lowercase();
    // Core only matches ecosystem-agnostic infra markers. Ecosystem-specific
    // failure signatures must be detected by the extension that owns that
    // ecosystem, not hardcoded here.
    extension::GENERIC_INFRASTRUCTURE_FAILURE_MARKERS
        .iter()
        .any(|needle| combined.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::{
        code_quality_failure_message, is_runner_infrastructure_failure, validate_lint_quality,
        validate_test_quality, LintQualityOutcome,
    };
    use homeboy_core::component::{Component, ComponentScriptsConfig, ScopedExtensionConfig};
    use homeboy_core::error::Error;
    use homeboy_extension::RunnerOutput;
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    impl LintQualityOutcome {
        fn expect_passed_with_value(self, expected_ran: bool) -> bool {
            match self {
                LintQualityOutcome::Passed { ran } => {
                    assert_eq!(ran, expected_ran, "Passed.ran mismatch");
                    ran
                }
                other => panic!("expected Passed, got {:?}", other),
            }
        }

        fn expect_failed(self) -> homeboy_core::error::Error {
            match self {
                LintQualityOutcome::Failed(err) => err,
                other => panic!("expected Failed, got {:?}", other),
            }
        }
    }

    fn component_without_quality_runners() -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: "/tmp/fixture".to_string(),
            ..Default::default()
        }
    }

    fn write_script(root: &Path, name: &str, body: &str) {
        let script_dir = root.join("scripts");
        fs::create_dir_all(&script_dir).expect("script dir should be created");
        fs::write(script_dir.join(name), body).expect("script should be written");
    }

    fn script_component(root: &Path, scripts: ComponentScriptsConfig) -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: root.to_string_lossy().to_string(),
            scripts: Some(scripts),
            ..Default::default()
        }
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn extension_lint_component(
        home: &Path,
        source: &Path,
        script: &str,
        prior_tag: bool,
    ) -> Component {
        run_git(source, &["init", "-q"]);
        run_git(source, &["config", "user.email", "homeboy@example.com"]);
        run_git(source, &["config", "user.name", "Homeboy Test"]);
        fs::write(source.join("legacy.php"), "<?php\n").expect("legacy source");
        run_git(source, &["add", "legacy.php"]);
        run_git(source, &["commit", "-q", "-m", "chore: initial"]);
        if prior_tag {
            run_git(source, &["tag", "v1.0.0"]);
        }
        fs::write(source.join("changed.php"), "<?php echo 'changed';\n").expect("changed source");
        run_git(source, &["add", "changed.php"]);
        run_git(source, &["commit", "-q", "-m", "fix: changed file"]);

        let extension_dir = home.join(".config/homeboy/extensions/release-lint-fixture");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("release-lint-fixture.json"),
            r#"{"name":"Release lint fixture","version":"1.0.0","lint":{"extension_script":"lint.sh"}}"#,
        )
        .expect("extension manifest");
        fs::write(extension_dir.join("lint.sh"), script).expect("extension script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(extension_dir.join("lint.sh"))
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(extension_dir.join("lint.sh"), permissions)
                .expect("executable script");
        }

        Component {
            id: "fixture".to_string(),
            local_path: source.to_string_lossy().to_string(),
            extensions: Some(HashMap::from([(
                "release-lint-fixture".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Default::default()
        }
    }

    fn extension_test_component(home: &Path, source: &Path, script: &str) -> Component {
        let extension_dir = home.join(".config/homeboy/extensions/release-test-fixture");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("release-test-fixture.json"),
            r#"{
                "name":"Release test fixture",
                "version":"1.0.0",
                "test":{
                    "extension_script":"test.sh",
                    "secret_env":{"DECLARED_RELEASE_SECRET":"DECLARED_RELEASE_SECRET"}
                }
            }"#,
        )
        .expect("extension manifest");
        fs::write(extension_dir.join("test.sh"), script).expect("extension script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script_path = extension_dir.join("test.sh");
            let mut permissions = fs::metadata(&script_path)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(script_path, permissions).expect("executable script");
        }

        Component {
            id: "fixture".to_string(),
            local_path: source.to_string_lossy().to_string(),
            extensions: Some(HashMap::from([(
                "release-test-fixture".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Default::default()
        }
    }

    fn enable_split_lint_routes(home: &Path) {
        fs::write(
            home.join(".config/homeboy/extensions/release-lint-fixture/release-lint-fixture.json"),
            r#"{
                "name":"Release lint fixture",
                "version":"1.0.0",
                "lint":{
                    "extension_script":"lint.sh",
                    "changed_file_routes":[
                        {"extensions":["php"],"step":"php"},
                        {"extensions":["js"],"step":"js"}
                    ]
                }
            }"#,
        )
        .expect("split route manifest");
    }

    fn runner_output(exit_code: i32, stdout: &str, stderr: &str) -> RunnerOutput {
        RunnerOutput {
            exit_code,
            success: exit_code == 0,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            timed_out: false,
            child_resource: None,
            extension_phase_timings: Vec::new(),
        }
    }

    #[test]
    fn code_quality_failure_message_separates_test_findings_from_runner_infra() {
        let findings = runner_output(1, "FAILURES!\nTests: 3, Assertions: 4, Failures: 1", "");
        let infra = runner_output(
            2,
            "Error: Playground bootstrap helper not found at /tmp/missing",
            "",
        );

        assert!(!is_runner_infrastructure_failure(&findings));
        assert!(is_runner_infrastructure_failure(&infra));
        assert_eq!(
            code_quality_failure_message("Tests", &findings),
            "Tests failed (exit code 1)"
        );
        assert_eq!(
            code_quality_failure_message("Tests", &infra),
            "Tests runner infrastructure failure (exit code 2)"
        );
    }

    #[test]
    fn test_validate_lint_quality() {
        assert!(
            !validate_lint_quality(&component_without_quality_runners(), "fixture")
                .expect_passed_with_value(false)
        );
    }

    #[test]
    fn lint_runner_error_preserves_nested_io_diagnostics_and_emits_fresh_action() {
        let component = Component {
            id: "fixture".to_string(),
            local_path: "/workspace/fixture".to_string(),
            ..Default::default()
        };
        let error = super::lint_runner_error(
            Error::internal_io(
                "No such file or directory (os error 2)",
                Some("read lint evidence /tmp/run/findings.json".to_string()),
            ),
            &component,
            "fixture",
            Some("rust"),
            Some("v1.2.3"),
        );

        assert_eq!(error.code.as_str(), "internal.io_error");
        assert_eq!(
            error.details["error"].as_str(),
            Some("No such file or directory (os error 2)")
        );
        assert_eq!(
            error.details["context"].as_str(),
            Some("read lint evidence /tmp/run/findings.json")
        );

        let action = &error.details["_homeboy_actions"][0];
        assert_eq!(action["id"], "release.lint.fresh_diagnostic");
        assert!(action["label"]
            .as_str()
            .is_some_and(|label| label.contains("fresh local lint diagnostic")));
        assert_eq!(
            action["args"],
            serde_json::json!([
                "--placement",
                "local",
                "lint",
                "fixture",
                "--path",
                "/workspace/fixture",
                "--extension",
                "rust",
                "--changed-since",
                "v1.2.3",
            ])
        );
        assert_eq!(action["evidence"]["source_path"], "/workspace/fixture");
        assert_eq!(action["evidence"]["extension_id"], "rust");
        assert_eq!(action["evidence"]["changed_since"], "v1.2.3");
        assert_eq!(action["evidence"]["settings"], serde_json::json!({}));
    }

    #[test]
    fn test_validate_test_quality() {
        assert!(!validate_test_quality(&component_without_quality_runners())
            .expect("missing test runner should not block release"));
    }

    #[test]
    fn validate_lint_quality_runs_component_scripts() {
        let dir = tempfile::tempdir().expect("temp dir");
        write_script(
            dir.path(),
            "lint.sh",
            "printf 'release lint script ran\\n'\n",
        );

        let component = script_component(
            dir.path(),
            ComponentScriptsConfig {
                lint: vec!["sh scripts/lint.sh".to_string()],
                ..Default::default()
            },
        );

        assert!(validate_lint_quality(&component, "fixture").expect_passed_with_value(true));
    }

    #[test]
    fn validate_test_quality_runs_component_scripts() {
        let dir = tempfile::tempdir().expect("temp dir");
        write_script(
            dir.path(),
            "test.sh",
            "printf 'release test script ran\\n'\n",
        );

        let component = script_component(
            dir.path(),
            ComponentScriptsConfig {
                test: vec!["sh scripts/test.sh".to_string()],
                ..Default::default()
            },
        );

        assert!(validate_test_quality(&component).expect("test script should pass"));
    }

    #[test]
    fn validate_test_quality_fails_before_child_when_declared_secret_is_missing() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = tempfile::tempdir().expect("source dir");
            let marker = source.path().join("child-ran");
            let component = extension_test_component(
                home.path(),
                source.path(),
                &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
            );
            std::env::remove_var("DECLARED_RELEASE_SECRET");

            let error = validate_test_quality(&component)
                .expect_err("release preflight must reject missing secret before spawn");

            assert!(error.message.contains("DECLARED_RELEASE_SECRET"));
            assert!(error.details.to_string().contains("agent-task auth map-env"));
            assert!(!marker.exists(), "release test child must not start");
        });
    }

    #[test]
    fn validate_test_quality_redacts_injected_secret_from_release_evidence() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = tempfile::tempdir().expect("source dir");
            let marker = source.path().join("child-ran");
            let component = extension_test_component(
                home.path(),
                source.path(),
                &format!(
                    "#!/bin/sh\ntouch '{}'\nprintf 'received=%s\\n' \"$DECLARED_RELEASE_SECRET\" >&2\nexit 1\n",
                    marker.display()
                ),
            );
            std::env::set_var("DECLARED_RELEASE_SECRET", "release-fixture-secret");

            let error = validate_test_quality(&component)
                .expect_err("fixture child intentionally fails after injection");
            std::env::remove_var("DECLARED_RELEASE_SECRET");

            assert!(marker.exists(), "release test child received declared env");
            let rendered = format!("{}\n{}", error, error.details);
            assert!(rendered.contains("[REDACTED]"));
            assert!(!rendered.contains("release-fixture-secret"));
            assert!(!error.details["command_evidence"]["command"]
                .as_str()
                .unwrap_or_default()
                .contains("release-fixture-secret"));
        });
    }

    #[test]
    fn validate_test_quality_failure_carries_command_and_captured_output() {
        // Reproduces issue #6937: a failing release test gate must surface the
        // resolved command, exit code, and captured stdout/stderr in the
        // structured error's `command_evidence`, so the failure is actionable
        // instead of an opaque "Tests failed (exit code 1)".
        let dir = tempfile::tempdir().expect("temp dir");
        write_script(
            dir.path(),
            "test.sh",
            "printf 'running release tests\\n'\nprintf 'assertion failed: expected 1 got 2\\n' >&2\nexit 1\n",
        );

        let component = script_component(
            dir.path(),
            ComponentScriptsConfig {
                test: vec!["sh scripts/test.sh".to_string()],
                ..Default::default()
            },
        );

        let err = validate_test_quality(&component)
            .expect_err("failing test script must block the release");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.to_string().contains("Tests failed (exit code 1)"));

        // The captured command evidence is what `failed_result` serializes into
        // the release step's `error_details.command_evidence`.
        let evidence = err
            .details
            .get("command_evidence")
            .expect("failing test gate must attach command_evidence");

        assert_eq!(
            evidence.get("exit_code").and_then(|v| v.as_i64()),
            Some(1),
            "evidence must carry the command exit code"
        );
        assert!(
            evidence
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .contains(&component.id),
            "evidence command should describe what ran: {:?}",
            evidence.get("command")
        );
        assert_eq!(
            evidence.get("cwd").and_then(|v| v.as_str()),
            Some(component.local_path.as_str()),
            "evidence must record the working directory"
        );
        assert_eq!(
            evidence.get("location").and_then(|v| v.as_str()),
            Some("local"),
            "release quality gates run on the local controller"
        );
        let stderr = evidence
            .get("stderr")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            stderr.contains("assertion failed: expected 1 got 2"),
            "evidence must surface the failing command's stderr: {stderr:?}"
        );
    }

    #[test]
    fn bound_evidence_stream_keeps_tail_and_marks_truncation() {
        let short = "only a little output";
        let (bounded, truncated) = super::bound_evidence_stream(short);
        assert_eq!(bounded, short);
        assert!(!truncated);

        let long = "x".repeat(super::COMMAND_EVIDENCE_MAX_BYTES + 64) + "TAIL_MARKER";
        let (bounded, truncated) = super::bound_evidence_stream(&long);
        assert!(truncated, "oversized streams must be marked truncated");
        assert!(
            bounded.len() <= super::COMMAND_EVIDENCE_MAX_BYTES,
            "bounded stream must respect the byte cap"
        );
        assert!(
            bounded.ends_with("TAIL_MARKER"),
            "bounding must retain the tail of the stream"
        );
    }

    #[test]
    fn validate_lint_quality_fails_failing_component_script() {
        // A self-check lint that exits 1 with a plain failure message (no infra
        // markers) is a genuine lint failure — the release hard-blocks.
        let dir = tempfile::tempdir().expect("temp dir");
        write_script(
            dir.path(),
            "lint.sh",
            "printf 'lint failed\\n' >&2\nexit 1\n",
        );

        let component = script_component(
            dir.path(),
            ComponentScriptsConfig {
                lint: vec!["sh scripts/lint.sh".to_string()],
                ..Default::default()
            },
        );

        let err = validate_lint_quality(&component, "fixture").expect_failed();
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.to_string().contains("Lint failed (exit code 1)"));
    }

    #[test]
    fn validate_lint_quality_blocks_missing_runner_steps_harness() {
        let dir = tempfile::tempdir().expect("temp dir");
        write_script(
            dir.path(),
            "lint.sh",
            "printf 'runner-steps.sh: No such file or directory\\n' >&2\nexit 1\n",
        );

        let component = script_component(
            dir.path(),
            ComponentScriptsConfig {
                lint: vec!["sh scripts/lint.sh".to_string()],
                ..Default::default()
            },
        );

        let error = validate_lint_quality(&component, "fixture").expect_failed();
        assert!(error.to_string().contains("Lint failed (exit code 1)"));
    }

    #[test]
    fn validate_lint_quality_blocks_high_exit_code() {
        let dir = tempfile::tempdir().expect("temp dir");
        write_script(dir.path(), "lint.sh", "exit 7\n");

        let component = script_component(
            dir.path(),
            ComponentScriptsConfig {
                lint: vec!["sh scripts/lint.sh".to_string()],
                ..Default::default()
            },
        );

        let error = validate_lint_quality(&component, "fixture").expect_failed();
        assert!(error.to_string().contains("Lint failed (exit code 7)"));
    }

    #[test]
    fn extension_release_lint_blocks_extension_bootstrap_failure() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let component = Component {
                id: "fixture".to_string(),
                local_path: "/tmp/fixture".to_string(),
                extensions: Some(HashMap::from([(
                    "missing-lint-extension".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };

            let error = validate_lint_quality(&component, "fixture").expect_failed();
            assert!(error.to_string().contains("Lint runner error"));
        });
    }

    #[test]
    fn extension_release_lint_rejects_invalid_explicit_lint_ownership() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/unsupported");
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join("unsupported.json"),
                r#"{"name":"Unsupported","version":"1.0.0"}"#,
            )
            .expect("extension manifest");
            let mut component = Component {
                id: "fixture".to_string(),
                local_path: "/tmp/fixture".to_string(),
                extensions: Some(HashMap::from([(
                    "unsupported".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };
            assert!(!validate_lint_quality(&component, "fixture").expect_passed_with_value(false));
            component
                .capability_extensions
                .insert("lint".to_string(), "unsupported".to_string());

            let error = validate_lint_quality(&component, "fixture").expect_failed();
            assert!(error.to_string().contains("Lint runner error"));
        });
    }

    #[test]
    fn extension_release_lint_blocks_finding_in_file_changed_since_prior_tag() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = tempfile::tempdir().expect("source dir");
            let component = extension_lint_component(
                home.path(),
                source.path(),
                r#"#!/bin/sh
printf '[{"tool":"phpstan","message":"changed finding","fingerprint":"changed","file":"changed.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
exit 1
"#,
                true,
            );

            let error = validate_lint_quality(&component, "fixture").expect_failed();
            assert!(error.to_string().contains("Lint failed (exit code 1,"));
        });
    }

    #[test]
    fn extension_release_lint_includes_later_route_finding_in_failure_details() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = tempfile::tempdir().expect("source dir");
            let component = extension_lint_component(
                home.path(),
                source.path(),
                r#"#!/bin/sh
if [ "$HOMEBOY_STEP" = "php" ]; then
  printf '[]' > "$HOMEBOY_LINT_FINDINGS_FILE"
  exit 0
fi
printf '[{"tool":"eslint","message":"second route release finding","fingerprint":"second","file":"assets/app.js"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
exit 1
"#,
                true,
            );
            enable_split_lint_routes(home.path());
            fs::create_dir_all(source.path().join("assets")).expect("assets dir");
            fs::write(source.path().join("assets/app.js"), "broken();\n").expect("js source");
            run_git(source.path(), &["add", "assets/app.js"]);
            run_git(source.path(), &["commit", "-q", "-m", "fix: js route"]);

            let error = validate_lint_quality(&component, "fixture").expect_failed();
            assert_eq!(
                error.details["lint_workflow"]["finding_count"].as_u64(),
                Some(1)
            );
            assert_eq!(
                error.details["lint_workflow"]["findings"][0]["message"].as_str(),
                Some("second route release finding")
            );
            assert!(error.details["lint_workflow"]["run_dir"].is_string());
            assert!(error.details["lint_workflow"]["hints"].is_array());
        });
    }

    #[test]
    fn extension_release_lint_ignores_legacy_finding_outside_prior_tag_scope() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = tempfile::tempdir().expect("source dir");
            let component = extension_lint_component(
                home.path(),
                source.path(),
                r#"#!/bin/sh
printf '[{"tool":"phpstan","message":"legacy finding","fingerprint":"legacy","file":"legacy.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
exit 1
"#,
                true,
            );

            assert!(validate_lint_quality(&component, "fixture").expect_passed_with_value(true));
        });
    }

    #[test]
    fn first_extension_release_retains_full_lint() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = tempfile::tempdir().expect("source dir");
            let component = extension_lint_component(
                home.path(),
                source.path(),
                r#"#!/bin/sh
printf '[{"tool":"phpstan","message":"legacy finding","fingerprint":"legacy","file":"legacy.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
exit 1
"#,
                false,
            );

            let error = validate_lint_quality(&component, "fixture").expect_failed();
            assert!(error.to_string().contains("Lint failed (exit code 1,"));
        });
    }

    #[test]
    fn extension_release_lint_uses_component_prefixed_release_tag() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = tempfile::tempdir().expect("source dir");
            run_git(source.path(), &["init", "-q"]);
            run_git(
                source.path(),
                &["config", "user.email", "homeboy@example.com"],
            );
            run_git(source.path(), &["config", "user.name", "Homeboy Test"]);
            let component_path = source.path().join("packages/fixture");
            fs::create_dir_all(&component_path).expect("component dir");
            fs::write(component_path.join("legacy.php"), "<?php\n").expect("legacy source");
            run_git(source.path(), &["add", "packages/fixture/legacy.php"]);
            run_git(source.path(), &["commit", "-q", "-m", "chore: initial"]);
            run_git(source.path(), &["tag", "fixture-v1.0.0"]);
            fs::write(component_path.join("changed.php"), "<?php echo 1;\n")
                .expect("changed source");
            run_git(source.path(), &["add", "packages/fixture/changed.php"]);
            run_git(source.path(), &["commit", "-q", "-m", "fix: package"]);

            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/release-lint-fixture");
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join("release-lint-fixture.json"),
                r#"{"name":"Release lint fixture","version":"1.0.0","lint":{"extension_script":"lint.sh"}}"#,
            )
            .expect("extension manifest");
            fs::write(
                extension_dir.join("lint.sh"),
                r#"#!/bin/sh
printf '[{"tool":"phpstan","message":"legacy","fingerprint":"legacy","file":"packages/fixture/legacy.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
exit 1
"#,
            )
            .expect("lint script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let script = extension_dir.join("lint.sh");
                let mut permissions = fs::metadata(&script).expect("metadata").permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(script, permissions).expect("executable script");
            }
            let component = Component {
                id: "fixture".to_string(),
                local_path: component_path.to_string_lossy().to_string(),
                extensions: Some(HashMap::from([(
                    "release-lint-fixture".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };

            assert!(validate_lint_quality(&component, "fixture").expect_passed_with_value(true));
        });
    }

    #[test]
    fn extension_release_lint_blocks_malformed_missing_and_producer_error_evidence() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let malformed_source = tempfile::tempdir().expect("malformed source dir");
            let malformed = extension_lint_component(
                home.path(),
                malformed_source.path(),
                "#!/bin/sh\nprintf '{' > \"$HOMEBOY_LINT_FINDINGS_FILE\"\nexit 1\n",
                true,
            );
            let malformed_error = validate_lint_quality(&malformed, "fixture").expect_failed();
            assert!(malformed_error.to_string().contains("Lint runner error"));

            let missing_source = tempfile::tempdir().expect("missing source dir");
            let missing = extension_lint_component(
                home.path(),
                missing_source.path(),
                "#!/bin/sh\nexit 0\n",
                true,
            );
            let missing_error = validate_lint_quality(&missing, "fixture").expect_failed();
            assert!(missing_error.to_string().contains("Lint runner error"));

            let producer_source = tempfile::tempdir().expect("producer error source dir");
            let producer_error = extension_lint_component(
                home.path(),
                producer_source.path(),
                r#"#!/bin/sh
printf '[{"tool":"phpstan","message":"known","fingerprint":"known","file":"changed.php"}]' > "$HOMEBOY_LINT_FINDINGS_FILE"
printf '[{"tool":"phpstan","status":"error","finding_count":1}]' > "$HOMEBOY_LINT_PRODUCERS_FILE"
exit 0
"#,
                true,
            );
            let mut known = homeboy_core::finding::HomeboyFinding::builder("phpstan", "known")
                .fingerprint("known")
                .build();
            known.location.file = Some("changed.php".to_string());
            homeboy_extension::lint::baseline::save_baseline(
                producer_source.path(),
                "fixture",
                &[known],
            )
            .expect("save accepted baseline");
            let producer_error = validate_lint_quality(&producer_error, "fixture").expect_failed();
            assert!(producer_error
                .to_string()
                .contains("Lint failed (exit code 1,"));
            assert_eq!(
                producer_error.details["lint_workflow"]["producer_error_count"].as_u64(),
                Some(1)
            );
            assert_eq!(
                producer_error.details["lint_workflow"]["baseline_new_count"].as_u64(),
                Some(0)
            );
            assert_eq!(
                producer_error.details["lint_workflow"]["baseline_known_count"].as_u64(),
                Some(1)
            );
        });
    }

    #[test]
    fn lint_failure_recommends_scoped_skip_checks_lint() {
        // #9641: a failing lint gate must recommend the narrowest bypass
        // (--skip-checks=lint), state which gates stay enabled, and present the
        // bare --skip-checks only as an explicit last resort.
        let hints = super::scoped_skip_guidance("lint");

        let scoped = hints
            .iter()
            .find(|h| h.contains("--skip-checks=lint"))
            .expect("lint failure must recommend --skip-checks=lint");
        assert!(
            scoped.contains("audit") && scoped.contains("test"),
            "scoped hint must name the gates that stay enabled: {scoped}"
        );
        let last_resort = hints
            .iter()
            .find(|h| h.contains("Last resort"))
            .expect("bare --skip-checks must be flagged as a last resort");
        assert!(
            last_resort.contains("disables ALL"),
            "bare --skip-checks must warn it disables all gates: {last_resort}"
        );
        // The last-resort broad bypass must appear after the scoped one.
        let scoped_idx = hints.iter().position(|h| h == scoped).unwrap();
        let broad_idx = hints.iter().position(|h| h == last_resort).unwrap();
        assert!(
            scoped_idx < broad_idx,
            "scoped bypass must be recommended before the broad last resort"
        );
    }

    #[test]
    fn test_failure_recommends_scoped_skip_checks_test() {
        let hints = super::scoped_skip_guidance("test");
        let scoped = hints
            .iter()
            .find(|h| h.contains("--skip-checks=test"))
            .expect("test failure must recommend --skip-checks=test");
        assert!(
            scoped.contains("audit") && scoped.contains("lint"),
            "scoped hint must name the gates that stay enabled: {scoped}"
        );
    }

    #[test]
    fn audit_failure_recommends_scoped_skip_checks_audit() {
        let hints = super::scoped_skip_guidance("audit");
        let scoped = hints
            .iter()
            .find(|h| h.contains("--skip-checks=audit"))
            .expect("audit failure must recommend --skip-checks=audit");
        assert!(
            scoped.contains("lint") && scoped.contains("test"),
            "scoped hint must name the gates that stay enabled: {scoped}"
        );
    }

    #[test]
    fn unknown_gate_falls_back_to_generic_scoped_guidance() {
        let hints = super::scoped_skip_guidance("mystery");
        assert!(
            hints
                .iter()
                .any(|h| h.contains("--skip-checks=<audit|lint|test>")),
            "unknown gate should still steer toward a scoped bypass: {hints:?}"
        );
        assert!(
            hints.iter().any(|h| h.contains("Last resort")),
            "unknown gate should still flag bare --skip-checks as a last resort: {hints:?}"
        );
    }

    #[test]
    fn quality_error_tried_hints_prefer_scoped_bypass() {
        // The full error hint list (not just the helper) must carry the scoped
        // command and flag the broad bypass as a last resort.
        let err = super::quality_error("lint", "Lint failed (exit code 1)".to_string());
        let rendered = format!("{:?}", err);
        assert!(
            rendered.contains("--skip-checks=lint"),
            "error tried hints must include the scoped bypass: {rendered}"
        );
        assert!(
            rendered.contains("Last resort"),
            "error tried hints must flag bare --skip-checks as a last resort: {rendered}"
        );
    }

    #[test]
    fn code_quality_failure_message_detects_generic_infra_marker_at_exit_one() {
        // Core only recognizes ecosystem-agnostic infra markers. Ecosystem-specific
        // failure signatures are detected by the owning extension, not here.
        let output = runner_output(1, "test harness infrastructure failure", "");

        assert!(is_runner_infrastructure_failure(&output));
        assert_eq!(
            code_quality_failure_message("Tests", &output),
            "Tests runner infrastructure failure (exit code 1)"
        );
    }
}
