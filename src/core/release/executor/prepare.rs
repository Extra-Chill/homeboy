use crate::core::error::{Error, Result};
use crate::core::extension::{self, ExtensionManifest};

use super::{build_release_payload, publish_response_output, step_failed, step_success};
use crate::core::release::types::{ReleaseState, ReleaseStepResult};

/// Invoke every `release.prepare` action provided by the component's extensions.
pub(crate) fn run_prepare(
    extensions: &[ExtensionManifest],
    state: &ReleaseState,
    component_id: &str,
    component_local_path: &str,
) -> Result<ReleaseStepResult> {
    let providers: Vec<&ExtensionManifest> = extensions
        .iter()
        .filter(|m| m.actions.iter().any(|a| a.id == "release.prepare"))
        .collect();

    if providers.is_empty() {
        return Err(Error::validation_invalid_argument(
            "release.prepare",
            "No extension provides release.prepare action",
            None,
            Some(vec![
                "Add an extension with a release.prepare action to the component".to_string(),
            ]),
        ));
    }

    let payload = build_release_payload(state, component_id, component_local_path, None);
    let mut responses = Vec::new();

    for extension in providers {
        let response = extension::execute_action(
            &extension.id,
            "release.prepare",
            None,
            None,
            Some(&payload),
        )?;
        responses.push(serde_json::json!({
            "extension": extension.id,
            "action": "release.prepare",
            "response": response,
        }));
    }

    let data = serde_json::json!({ "responses": responses });
    Ok(prepare_step_result(Some(data)))
}

fn prepare_step_result(data: Option<serde_json::Value>) -> ReleaseStepResult {
    let failing_response = data
        .as_ref()
        .and_then(|data| data.get("responses"))
        .and_then(serde_json::Value::as_array)
        .and_then(|responses| {
            responses.iter().find(|entry| {
                entry
                    .get("response")
                    .and_then(|response| response.get("success"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
            })
        });

    if let Some(entry) = failing_response {
        let extension = entry
            .get("extension")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("extension");
        let response = entry.get("response").unwrap_or(&serde_json::Value::Null);
        let error = extension_action_failure_message("release.prepare", extension, response);
        return step_failed(
            "release.prepare",
            "release.prepare",
            data,
            Some(error),
            Vec::new(),
        );
    }

    step_success("release.prepare", "release.prepare", data, Vec::new())
}

fn extension_action_failure_message(
    action_id: &str,
    extension_id: &str,
    response: &serde_json::Value,
) -> String {
    let exit_code = response
        .get("exit_code")
        .or_else(|| response.get("exitCode"))
        .and_then(|v| v.as_i64());
    let output = publish_response_output(response);
    let detail = output.trim();

    match (exit_code, detail.is_empty()) {
        (Some(code), false) => format!(
            "Action {} from {} failed (exit {}): {}",
            action_id, extension_id, code, detail
        ),
        (Some(code), true) => format!(
            "Action {} from {} failed (exit {})",
            action_id, extension_id, code
        ),
        (None, false) => format!(
            "Action {} from {} failed: {}",
            action_id, extension_id, detail
        ),
        (None, true) => format!("Action {} from {} failed", action_id, extension_id),
    }
}

#[cfg(test)]
mod tests {
    use super::prepare_step_result;
    use crate::core::release::ReleaseStepStatus;

    #[test]
    fn prepare_step_fails_when_extension_command_fails() {
        let data = serde_json::json!({
            "responses": [
                {
                    "extension": "fixture",
                    "action": "release.prepare",
                    "response": {
                        "success": false,
                        "exitCode": 1,
                        "stderr": "generated file is out of sync"
                    }
                }
            ]
        });

        let result = prepare_step_result(Some(data));

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        let error = result.error.expect("prepare failure message");
        assert!(error.contains("release.prepare"));
        assert!(error.contains("fixture"));
        assert!(error.contains("generated file is out of sync"));
    }
}
