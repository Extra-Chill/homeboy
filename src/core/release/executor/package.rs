//! The `package` release step: invoke each extension's `release.package` action
//! (with a bounded retry), parse emitted artifacts, and build the action
//! payload. Also hosts the extension release-preflight step.
//!
//! Split out of `executor.rs` to keep packaging/payload logic together.

use crate::core::error::{Error, Result};
use crate::core::extension::{self, ExtensionManifest};

use super::super::types::{ReleaseArtifact, ReleaseState, ReleaseStepResult};
use super::super::utils::parse_release_artifacts;
use super::{step_failed, step_success};

/// Maximum number of attempts for a transient package-command failure.
///
/// Dependency-install commands can fail intermittently due to registry
/// hiccups, lock contention, or output-pipe timing. A warm-cache retry usually
/// succeeds, so we retry once before surfacing the error. Issue #3238.
const PACKAGE_ACTION_MAX_ATTEMPTS: usize = 2;

/// Invoke the `release.package` action on every extension that provides it,
/// parse the emitted artifacts, and stash them in [`ReleaseState::artifacts`]
/// for downstream publish targets and for the GitHub Release step.
pub(crate) fn run_package(
    extensions: &[ExtensionManifest],
    state: &mut ReleaseState,
    component_id: &str,
    component_local_path: &str,
    skip_build_validation: bool,
) -> Result<ReleaseStepResult> {
    let package_extensions: Vec<&ExtensionManifest> = extensions
        .iter()
        .filter(|m| m.actions.iter().any(|a| a.id == "release.package"))
        .collect();

    if package_extensions.is_empty() {
        return Err(Error::validation_invalid_argument(
            "release.package",
            "No extension provides release.package action",
            None,
            Some(vec![
                "Add an extension with a release.package action to the component".to_string(),
            ]),
        ));
    }

    let extra_config = package_build_config(skip_build_validation);
    let mut responses = Vec::new();
    for extension in package_extensions {
        let payload = build_release_payload(
            state,
            component_id,
            component_local_path,
            extra_config.as_ref(),
        );
        let response = run_package_action_with_retry(&extension.id, &payload)
            .map_err(|err| package_provider_error(&extension.id, err))?;

        store_artifacts_from_output(state, &response)
            .map_err(|err| package_provider_error(&extension.id, err))?;
        responses.push(serde_json::json!({
            "extension": extension.id,
            "response": response,
        }));
    }

    let data = if responses.len() == 1 {
        let response = responses.pop().expect("single package response");
        serde_json::json!({
            "extension": response["extension"],
            "action": "release.package",
            "response": response["response"],
        })
    } else {
        serde_json::json!({
            "action": "release.package",
            "extensions": responses.iter().map(|response| response["extension"].clone()).collect::<Vec<_>>(),
            "responses": responses,
        })
    };

    Ok(step_success("package", "package", Some(data), Vec::new()))
}

/// Execute a `release.package` action with a bounded retry for transient
/// failures.
///
/// Returns the action response (which may carry `success: false` on the final
/// attempt) so the caller can surface the full captured stdout/stderr via
/// [`store_artifacts_from_output`].
fn run_package_action_with_retry(
    extension_id: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value> {
    for attempt in 1..=PACKAGE_ACTION_MAX_ATTEMPTS {
        match extension::execute_action(extension_id, "release.package", None, None, Some(payload))
        {
            Ok(response) => {
                let success = response
                    .get("success")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let exit_code = response
                    .get("exitCode")
                    .or_else(|| response.get("exit_code"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(-1);

                if success || exit_code == 0 {
                    return Ok(response);
                }

                // Transient failure — retry once before surfacing the error.
                if attempt < PACKAGE_ACTION_MAX_ATTEMPTS {
                    log_status!(
                        "package",
                        "Package command exited {} (attempt {}/{}); retrying…",
                        exit_code,
                        attempt,
                        PACKAGE_ACTION_MAX_ATTEMPTS
                    );
                    continue;
                }

                // Final attempt — return the response so the caller can
                // surface the full captured output in the error.
                return Ok(response);
            }
            Err(err) => {
                if attempt < PACKAGE_ACTION_MAX_ATTEMPTS {
                    log_status!(
                        "package",
                        "Package action error (attempt {}/{}); retrying…",
                        attempt,
                        PACKAGE_ACTION_MAX_ATTEMPTS
                    );
                    continue;
                }
                return Err(err);
            }
        }
    }

    // Unreachable when PACKAGE_ACTION_MAX_ATTEMPTS >= 1.
    Err(Error::internal_unexpected(
        "Package command did not produce a result",
    ))
}

/// Invoke an extension-declared release preflight action.
pub(crate) fn run_extension_release_preflight(
    step: &crate::core::plan::PlanStep,
    extensions: &[ExtensionManifest],
    state: &ReleaseState,
    component_id: &str,
    component_local_path: &str,
) -> ReleaseStepResult {
    let extension_id = step
        .inputs
        .get("extension")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let action_id = step
        .inputs
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    let Some(extension) = extensions
        .iter()
        .find(|extension| extension.id == extension_id)
    else {
        return step_failed(
            &step.id,
            &step.kind,
            Some(serde_json::json!({
                "extension": extension_id,
                "action": action_id,
            })),
            Some(format!(
                "Release preflight references missing extension '{}'",
                extension_id
            )),
            Vec::new(),
        );
    };

    if !extension
        .actions
        .iter()
        .any(|action| action.id == action_id)
    {
        return step_failed(
            &step.id,
            &step.kind,
            Some(serde_json::json!({
                "extension": extension_id,
                "action": action_id,
            })),
            Some(format!(
                "Release preflight references missing action '{}' on extension '{}'",
                action_id, extension_id
            )),
            Vec::new(),
        );
    }

    let payload = build_release_payload(state, component_id, component_local_path, None);
    let response =
        match extension::execute_action(extension_id, action_id, None, None, Some(&payload)) {
            Ok(response) => response,
            Err(err) => {
                return step_failed(&step.id, &step.kind, None, Some(err.message), err.hints)
            }
        };

    let data = Some(serde_json::json!({
        "extension": extension_id,
        "action": action_id,
        "response": response,
    }));

    if response.get("success").and_then(serde_json::Value::as_bool) == Some(false) {
        let reason = response
            .get("reason")
            .or_else(|| response.get("error"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("extension release preflight reported failure");
        return step_failed(
            &step.id,
            &step.kind,
            data,
            Some(reason.to_string()),
            Vec::new(),
        );
    }

    step_success(&step.id, &step.kind, data, Vec::new())
}

fn package_provider_error(extension_id: &str, err: Error) -> Error {
    let mut wrapped = Error::new(
        err.code,
        format!(
            "release.package failed for extension '{}': {}",
            extension_id, err.message
        ),
        serde_json::json!({
            "extension": extension_id,
            "action": "release.package",
            "source": err.details,
        }),
    );
    wrapped.hints = err.hints;
    wrapped.retryable = err.retryable;
    wrapped
}

fn package_build_config(
    skip_build_validation: bool,
) -> Option<std::collections::HashMap<String, serde_json::Value>> {
    if !skip_build_validation {
        return None;
    }

    let mut config = std::collections::HashMap::new();
    config.insert(
        "skip_build_validation".to_string(),
        serde_json::Value::Bool(true),
    );
    Some(config)
}

pub(crate) fn build_release_payload(
    state: &ReleaseState,
    component_id: &str,
    component_local_path: &str,
    extra_config: Option<&std::collections::HashMap<String, serde_json::Value>>,
) -> serde_json::Value {
    let version = state.version.clone().unwrap_or_default();
    let tag = state.tag.clone().unwrap_or_else(|| format!("v{}", version));
    let notes = state.notes.clone().unwrap_or_default();

    let mut payload = serde_json::json!({
        "release": {
            "version": version,
            "tag": tag,
            "notes": notes,
            "component_id": component_id,
            "local_path": component_local_path,
            "artifacts": state.artifacts,
        }
    });

    if let Some(config) = extra_config {
        if !config.is_empty() {
            payload["config"] = serde_json::to_value(config).unwrap_or(serde_json::Value::Null);
        }
    }

    payload
}

pub(super) fn store_artifacts_from_output(
    state: &mut ReleaseState,
    response: &serde_json::Value,
) -> Result<()> {
    let stdout = response
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let stderr = response
        .get("stderr")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let exit_code = response
        .get("exit_code")
        .or_else(|| response.get("exitCode"))
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);

    let success = response
        .get("success")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(exit_code == 0);

    // Surface the full captured output when the package command itself failed,
    // rather than trying to parse partial stdout as JSON (which swallowed
    // stderr behind a generic "Failed to parse" error).  Issue #3238: a
    // dependency install inside the build script can fail intermittently, and
    // the real error must be visible in the structured error payload.
    if !success {
        return Err(package_command_failure_error(exit_code, stdout, stderr));
    }

    if stdout.trim().is_empty() {
        return Err(Error::internal_unexpected(
            "Package command produced no artifact output. \
             The packaging tool may not be installed or configured correctly.",
        ));
    }

    let raw_artifacts: serde_json::Value = serde_json::from_str(stdout).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some(format!("Failed to parse package artifacts: {}", stdout)),
        )
    })?;
    let artifacts: Vec<ReleaseArtifact> = parse_release_artifacts(&raw_artifacts)?;
    state.artifacts.extend(artifacts);
    Ok(())
}

/// Build an [`Error`] that surfaces *all* captured output from a failed
/// package command — stdout, stderr, and exit code.
///
/// Package/build tools commonly write progress to stdout and errors to stderr.
/// Including both streams ensures the operator can diagnose the real failure
/// instead of seeing truncated output.  Issue #3238.
fn package_command_failure_error(exit_code: i64, stdout: &str, stderr: &str) -> Error {
    let stderr_trimmed = stderr.trim();
    let stdout_trimmed = stdout.trim();
    let has_stderr = !stderr_trimmed.is_empty();
    let has_stdout = !stdout_trimmed.is_empty();

    let mut detail = format!("Package command failed (exit {})", exit_code);

    if has_stderr {
        detail.push_str(": ");
        detail.push_str(stderr_trimmed);
    } else if has_stdout {
        detail.push_str(": ");
        detail.push_str(stdout_trimmed);
    } else {
        detail.push_str(". Check that the required packaging tool is installed and configured.");
    }

    // When both streams have content, append stdout as additional context.
    // Dependency-install failures often write progress lines to stdout and the
    // actual error to stderr; the operator needs both to see what happened
    // before the crash.
    if has_stderr && has_stdout {
        detail.push_str("\n\n--- stdout ---\n");
        detail.push_str(stdout_trimmed);
    }

    Error::internal_unexpected(detail)
}
