use crate::core::component::Component;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{CommandEvidence, Error, Result};
use crate::core::extension::{self, ExtensionCapability};
use std::path::Path;

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
/// Distinguishes a genuine lint failure (real findings exist) from a harness /
/// infrastructure failure (the lint wrapper exited non-zero while reporting
/// zero findings). The latter must not hard-block a release — the underlying
/// linter is clean and a broken runner harness should surface as a warning.
#[derive(Debug)]
pub(super) enum LintQualityOutcome {
    /// Lint ran and passed, or no lint runner is configured (`ran == false`).
    Passed { ran: bool },
    /// Lint produced real findings — this is a genuine, hard-blocking failure.
    Failed(Error),
    /// The lint harness exited non-zero while reporting zero findings.
    ///
    /// This is a harness/infra error, not a code-quality failure. The release
    /// continues with a warning instead of aborting.
    HarnessError { message: String },
}

/// Run release lint via the component's extension.
///
/// Returns a [`LintQualityOutcome`] distinguishing real lint findings from a
/// harness/infrastructure failure. Missing lint support is not a release
/// blocker because not every extension provides it.
pub(super) fn validate_lint_quality(component: &Component) -> LintQualityOutcome {
    // Shared mapping for a lint-runner construction/execution error into the
    // standard hard-blocking outcome, used by both the scripts.lint and the
    // extension-resolved lint paths below.
    let runner_error = |e: Error| {
        LintQualityOutcome::Failed(quality_error("lint", format!("Lint runner error: {}", e)))
    };

    if component.has_script(ExtensionCapability::Lint) {
        log_status!("release", "Running lint (scripts.lint)...");

        let workflow = match extension::lint::run_self_check_lint_workflow(
            component,
            Path::new(&component.local_path),
            component.id.clone(),
            false,
        ) {
            Ok(workflow) => workflow,
            Err(e) => return runner_error(e),
        };

        if workflow.status == "passed" {
            log_status!("release", "Lint passed");
            return LintQualityOutcome::Passed { ran: true };
        }

        // The lint harness exited non-zero. When the underlying linter is clean
        // and the wrapper/harness itself failed (e.g. the long-standing missing
        // `runner-steps.sh` environmental issue), surface a non-blocking warning
        // rather than aborting the release.
        if workflow.harness_error {
            return LintQualityOutcome::HarnessError {
                message: harness_failure_message("Lint", workflow.exit_code),
            };
        }

        return LintQualityOutcome::Failed(quality_error(
            "lint",
            format!("Lint failed (exit code {})", workflow.exit_code),
        ));
    }

    let lint_context = extension::lint::resolve_lint_command(component);

    let Ok(lint_context) = lint_context else {
        return LintQualityOutcome::Passed { ran: false };
    };

    log_status!("release", "Running lint ({})...", lint_context.extension_id);

    let release_run_dir = match RunDir::create() {
        Ok(dir) => dir,
        Err(e) => return LintQualityOutcome::Failed(e),
    };
    let lint_findings_file = release_run_dir.step_file(run_dir::files::LINT_FINDINGS);

    let output = match extension::lint::build_lint_runner(
        component,
        None,
        &[],
        false,
        None,
        None,
        false,
        None,
        None,
        None,
        None,
        None,
        &release_run_dir,
    )
    .and_then(|runner| runner.run())
    {
        Ok(output) => output,
        Err(e) => return runner_error(e),
    };

    if output.success {
        log_status!("release", "Lint passed");
        return LintQualityOutcome::Passed { ran: true };
    }

    let source_path = std::path::Path::new(&component.local_path);
    let findings = crate::core::extension::lint::baseline::parse_findings_file(&lint_findings_file)
        .unwrap_or_default();

    // A non-zero exit with zero parsed findings is a harness/infra failure, not
    // a real lint failure — the linter found nothing to report. The underlying
    // linter is clean, so this must not hard-block the release.
    if findings.is_empty() {
        return LintQualityOutcome::HarnessError {
            message: harness_failure_message("Lint", output.exit_code),
        };
    }

    if let Some(baseline) = crate::core::extension::lint::baseline::load_baseline(source_path) {
        let comparison = crate::core::extension::lint::baseline::compare(&findings, &baseline);
        if comparison.drift_increased {
            log_status!(
                "release",
                "Lint baseline drift increased: {} new finding(s)",
                comparison.new_items.len()
            );
        } else {
            log_status!(
                "release",
                "Lint has known findings but no new drift (baseline honored)"
            );
            log_status!("release", "Lint passed");
            return LintQualityOutcome::Passed { ran: true };
        }
    }

    LintQualityOutcome::Failed(quality_error(
        "lint",
        code_quality_failure_message("Lint", &output),
    ))
}

/// Run release tests via the component's extension.
///
/// Returns whether a test command was available and executed. Missing test
/// support is not a release blocker because not every extension provides it.
pub(super) fn validate_test_quality(component: &Component) -> Result<bool> {
    if component.has_script(ExtensionCapability::Test) {
        log_status!("release", "Running tests (scripts.test)...");

        let workflow = extension::test::run_self_check_test_workflow(
            component,
            Path::new(&component.local_path),
            component.id.clone(),
            false,
        )
        .map_err(|e| quality_error("test", format!("Test runner error: {}", e)))?;

        if workflow.status == "passed" {
            log_status!("release", "Tests passed");
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

    log_status!(
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
        false,
        false,
        None,
        None,
        &test_run_dir,
    )
    .and_then(|runner| runner.run())
    .map_err(|e| quality_error("test", format!("Test runner error: {}", e)))?;

    if output.success {
        log_status!("release", "Tests passed");
        Ok(true)
    } else {
        let evidence = command_evidence(
            resolved_command,
            Some(component.local_path.clone()),
            output.exit_code,
            &output.stdout,
            &output.stderr,
        );
        Err(quality_error_with_evidence(
            "test",
            code_quality_failure_message("Tests", &output),
            evidence,
        ))
    }
}

/// Message for a lint/test harness failure (non-zero exit, zero findings).
///
/// This is distinct from a real code-quality failure: the underlying tool is
/// clean and the wrapper/harness itself failed. The release continues with a
/// warning carrying this message.
fn harness_failure_message(check: &str, exit_code: i32) -> String {
    format!(
        "{} harness exited {} with zero findings — treating as a harness/infra error, not a code-quality failure. \
Release continues; the underlying linter is clean. To re-run lint in isolation: homeboy lint <component>. \
To skip only this gate: homeboy release <component> --skip-checks=lint",
        check, exit_code
    )
}

fn quality_error(field: &str, message: String) -> Error {
    log_status!("release", "Code quality check failed: {}", message);

    Error::validation_invalid_argument(
        field,
        message,
        None,
        Some(vec![
            "Fix the issue above before releasing".to_string(),
            "To bypass: homeboy release <component> --skip-checks".to_string(),
        ]),
    )
}

/// Like [`quality_error`] but attaches captured [`CommandEvidence`] so the
/// failing command and its stdout/stderr surface in the structured error's
/// `error_details.command_evidence`. The `tried` hints point operators at that
/// evidence instead of a phantom "issue above".
fn quality_error_with_evidence(field: &str, message: String, evidence: CommandEvidence) -> Error {
    log_status!("release", "Code quality check failed: {}", message);
    log_status!(
        "release",
        "Failing command: {} (exit code {})",
        evidence.command,
        evidence.exit_code
    );

    Error::validation_invalid_argument_with_evidence(
        field,
        message,
        None,
        Some(vec![
            "Inspect error_details.command_evidence for the failing command, cwd, exit code, and captured stdout/stderr".to_string(),
            "Reproduce in isolation: homeboy test <component>".to_string(),
            "To bypass: homeboy release <component> --skip-checks".to_string(),
        ]),
        Some(evidence),
    )
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
        code_quality_failure_message, harness_failure_message, is_runner_infrastructure_failure,
        validate_lint_quality, validate_test_quality, LintQualityOutcome,
    };
    use crate::core::component::{Component, ComponentScriptsConfig};
    use crate::core::extension::RunnerOutput;
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

        fn expect_harness_error(self) -> String {
            match self {
                LintQualityOutcome::HarnessError { message } => message,
                other => panic!("expected HarnessError, got {:?}", other),
            }
        }

        fn expect_failed(self) -> crate::core::error::Error {
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
        assert!(!validate_lint_quality(&component_without_quality_runners())
            .expect_passed_with_value(false));
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

        assert!(validate_lint_quality(&component).expect_passed_with_value(true));
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

        let err = validate_lint_quality(&component).expect_failed();
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.to_string().contains("Lint failed (exit code 1)"));
    }

    #[test]
    fn validate_lint_quality_warns_on_missing_runner_steps_harness() {
        // Reproduces issue #4586: the lint harness exits non-zero (1) while the
        // underlying linter is clean. The missing runner-steps.sh marker in the
        // output identifies this as a harness/infra failure that must NOT
        // hard-block the release.
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

        let message = validate_lint_quality(&component).expect_harness_error();
        assert!(
            message.contains("harness exited 1 with zero findings"),
            "harness error message should mention zero findings: {}",
            message
        );
        assert!(
            message.contains("--skip-checks=lint"),
            "harness error message should mention granular skip: {}",
            message
        );
    }

    #[test]
    fn validate_lint_quality_warns_on_high_exit_code() {
        // Exit codes >= 2 conventionally indicate tooling/internal errors, not
        // real lint findings — treated as a harness failure (warning).
        let dir = tempfile::tempdir().expect("temp dir");
        write_script(dir.path(), "lint.sh", "exit 7\n");

        let component = script_component(
            dir.path(),
            ComponentScriptsConfig {
                lint: vec!["sh scripts/lint.sh".to_string()],
                ..Default::default()
            },
        );

        let message = validate_lint_quality(&component).expect_harness_error();
        assert!(message.contains("harness exited 7"));
    }

    #[test]
    fn harness_failure_message_mentions_granular_skip() {
        let message = harness_failure_message("Lint", 1);
        assert!(message.contains("harness exited 1 with zero findings"));
        assert!(message.contains("homeboy lint <component>"));
        assert!(message.contains("--skip-checks=lint"));
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
