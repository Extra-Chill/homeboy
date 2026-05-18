use crate::core::component::Component;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, Result};
use crate::core::extension;

/// Run release lint via the component's extension.
///
/// Returns whether a lint command was available and executed. Missing lint
/// support is not a release blocker because not every extension provides it.
pub(super) fn validate_lint_quality(component: &Component) -> Result<bool> {
    let lint_context = extension::lint::resolve_lint_command(component);

    let Ok(lint_context) = lint_context else {
        return Ok(false);
    };

    log_status!("release", "Running lint ({})...", lint_context.extension_id);

    let release_run_dir = RunDir::create()?;
    let lint_findings_file = release_run_dir.step_file(run_dir::files::LINT_FINDINGS);

    let output = extension::lint::build_lint_runner(
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
        &release_run_dir,
    )
    .and_then(|runner| runner.run())
    .map_err(|e| quality_error("lint", format!("Lint runner error: {}", e)))?;

    let lint_passed = if output.success {
        true
    } else {
        let source_path = std::path::Path::new(&component.local_path);
        let findings =
            crate::core::extension::lint::baseline::parse_findings_file(&lint_findings_file)
                .unwrap_or_default();

        if let Some(baseline) = crate::core::extension::lint::baseline::load_baseline(source_path) {
            let comparison = crate::core::extension::lint::baseline::compare(&findings, &baseline);
            if comparison.drift_increased {
                log_status!(
                    "release",
                    "Lint baseline drift increased: {} new finding(s)",
                    comparison.new_items.len()
                );
                false
            } else {
                log_status!(
                    "release",
                    "Lint has known findings but no new drift (baseline honored)"
                );
                true
            }
        } else {
            false
        }
    };

    if lint_passed {
        log_status!("release", "Lint passed");
        Ok(true)
    } else {
        Err(quality_error(
            "lint",
            code_quality_failure_message("Lint", &output),
        ))
    }
}

/// Run release tests via the component's extension.
///
/// Returns whether a test command was available and executed. Missing test
/// support is not a release blocker because not every extension provides it.
pub(super) fn validate_test_quality(component: &Component) -> Result<bool> {
    let test_context = extension::test::resolve_test_command(component);

    let Ok(test_context) = test_context else {
        return Ok(false);
    };

    log_status!(
        "release",
        "Running tests ({})...",
        test_context.extension_id
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
        Err(quality_error(
            "test",
            code_quality_failure_message("Tests", &output),
        ))
    }
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
    [
        "playground bootstrap helper not found",
        "playground php crash",
        "bootstrap failure:",
        "test harness infrastructure failure",
        "lint runner infrastructure failure",
        "failed opening required '/homeboy-extension/scripts/lib/playground-bootstrap.php'",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::{
        code_quality_failure_message, is_runner_infrastructure_failure, validate_lint_quality,
        validate_test_quality,
    };
    use crate::core::component::Component;
    use crate::core::extension::RunnerOutput;

    fn component_without_quality_runners() -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: "/tmp/fixture".to_string(),
            ..Default::default()
        }
    }

    fn runner_output(exit_code: i32, stdout: &str, stderr: &str) -> RunnerOutput {
        RunnerOutput {
            exit_code,
            success: exit_code == 0,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
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
            .expect("missing lint runner should not block release"));
    }

    #[test]
    fn test_validate_test_quality() {
        assert!(!validate_test_quality(&component_without_quality_runners())
            .expect("missing test runner should not block release"));
    }

    #[test]
    fn code_quality_failure_message_detects_pre_runner_playground_fatal_output() {
        let output = runner_output(
            1,
            "Fatal error: Uncaught Error: Failed opening required '/homeboy-extension/scripts/lib/playground-bootstrap.php'",
            "",
        );

        assert!(is_runner_infrastructure_failure(&output));
        assert_eq!(
            code_quality_failure_message("Tests", &output),
            "Tests runner infrastructure failure (exit code 1)"
        );
    }
}
