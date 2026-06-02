use std::fmt::Write;

use crate::core::error::{Error, Result};
use crate::core::extension::{self, ExtensionManifest};
use crate::core::release::types::{ReleaseState, ReleaseStepResult};

use super::{build_release_payload, step_failed, step_skipped, step_success};

/// Invoke the `release.publish` action on the named extension.
pub(crate) fn run_publish(
    extensions: &[ExtensionManifest],
    state: &ReleaseState,
    component_id: &str,
    component_local_path: &str,
    target: &str,
) -> Result<ReleaseStepResult> {
    let extension = extensions.iter().find(|m| m.id == target).ok_or_else(|| {
        Error::validation_invalid_argument(
            "release.publish",
            format!("No extension '{}' found for publish target", target),
            None,
            Some(vec![format!(
                "Add extension to component config: \"extensions\": {{ \"{}\": {{}} }}",
                target
            )]),
        )
    })?;

    let action_id = "release.publish";
    let has_action = extension.actions.iter().any(|a| a.id == action_id);
    if !has_action {
        return Err(Error::validation_invalid_argument(
            "release.publish",
            format!(
                "Extension '{}' does not provide action '{}'",
                target, action_id
            ),
            None,
            None,
        ));
    }

    let payload = build_release_payload(state, component_id, component_local_path, None);
    let response = extension::execute_action(&extension.id, action_id, None, None, Some(&payload))?;
    let extension_data = serde_json::to_value(&response).map_err(|e| {
        Error::internal_json(e.to_string(), Some("extension action output".to_string()))
    })?;

    let step_id = format!("publish.{}", target);
    let data = serde_json::json!({
        "target": target,
        "extension": extension.id,
        "action": action_id,
        "response": extension_data,
    });

    Ok(publish_step_result(
        &step_id,
        target,
        &extension.id,
        Some(data),
        &response,
        state.version.as_deref(),
    ))
}

fn publish_step_result(
    step_id: &str,
    target: &str,
    extension_id: &str,
    data: Option<serde_json::Value>,
    response: &serde_json::Value,
    expected_version: Option<&str>,
) -> ReleaseStepResult {
    if let Some(reason) = extension_auth_required_reason(response) {
        return auth_required_skip_result(step_id, target, extension_id, data, reason);
    }

    if let Some(reason) = extension_blocking_publish_reason(response) {
        return step_failed(
            step_id,
            step_id,
            data,
            Some(format!(
                "Publish to {} via {} was not completed: {}",
                target, extension_id, reason
            )),
            Vec::new(),
        );
    }

    if !response
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
    {
        if let Some(reason) = publish_output_auth_required_reason(response) {
            return auth_required_skip_result(step_id, target, extension_id, data, reason);
        }
    }

    if let Some(reason) = extension_skip_reason(response) {
        return step_skipped(
            step_id,
            step_id,
            data,
            format!(
                "Skipped publish to {} via {}: {}",
                target, extension_id, reason
            ),
        );
    }

    if response
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
    {
        if let Some(failure) = verify_publish_response(target, response, expected_version) {
            return step_failed(step_id, step_id, data, Some(failure), Vec::new());
        }

        return step_success(step_id, step_id, data, Vec::new());
    }

    step_failed(
        step_id,
        step_id,
        data,
        Some(publish_failure_message(target, response)),
        Vec::new(),
    )
}

fn auth_required_skip_result(
    step_id: &str,
    target: &str,
    extension_id: &str,
    data: Option<serde_json::Value>,
    reason: String,
) -> ReleaseStepResult {
    step_skipped(
        step_id,
        step_id,
        data,
        format!(
            "Publish to {} via {} requires authentication: {}",
            target, extension_id, reason
        ),
    )
}

fn extension_auth_required_reason(response: &serde_json::Value) -> Option<String> {
    let status = response.get("status").and_then(|v| v.as_str())?;
    if status != "auth_required" {
        return None;
    }

    publish_response_reason(response).or_else(|| Some("authentication required".to_string()))
}

fn extension_blocking_publish_reason(response: &serde_json::Value) -> Option<String> {
    let status = response.get("status").and_then(|v| v.as_str())?;
    if status != "missing_secret" {
        return None;
    }

    publish_response_reason(response).or_else(|| Some(status.replace('_', " ")))
}

fn extension_skip_reason(response: &serde_json::Value) -> Option<String> {
    let status = response.get("status").and_then(|v| v.as_str())?;
    if status != "skipped" {
        return None;
    }

    publish_response_reason(response).or_else(|| Some(status.replace('_', " ")))
}

fn publish_response_reason(response: &serde_json::Value) -> Option<String> {
    response
        .get("reason")
        .or_else(|| response.get("message"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let output = publish_response_output(response);
            let detail = output.trim();
            (!detail.is_empty()).then(|| detail.to_string())
        })
}

fn publish_output_auth_required_reason(response: &serde_json::Value) -> Option<String> {
    let output = publish_response_output(response);
    if !output.contains("ENEEDAUTH") {
        return None;
    }

    Some(
        "npm authentication required (ENEEDAUTH). Run `npm login` for the target registry, then retry the registry publish."
            .to_string(),
    )
}

fn verify_publish_response(
    target: &str,
    response: &serde_json::Value,
    expected_version: Option<&str>,
) -> Option<String> {
    let verification = publish_registry_verification(response, expected_version)?;
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            return Some(format!(
                "Registry verification setup failed for {}: {}",
                target, err
            ));
        }
    };

    let http_response = match client.get(&verification.url).send() {
        Ok(response) => response,
        Err(err) => {
            return Some(format!(
                "Registry verification failed for {} {}: {}",
                verification.package_name, verification.version, err
            ));
        }
    };

    let status = http_response.status();
    if !status.is_success() {
        return Some(format!(
            "Registry verification failed for {} {}: registry returned {}",
            verification.package_name, verification.version, status
        ));
    }

    let body = match http_response.text() {
        Ok(body) => body,
        Err(err) => {
            return Some(format!(
                "Registry verification failed for {} {}: could not read response: {}",
                verification.package_name, verification.version, err
            ));
        }
    };

    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(actual_version) = json.get("version").and_then(|value| value.as_str()) {
            if actual_version != verification.version {
                return Some(format!(
                    "Registry verification failed for {}: expected version {}, registry returned {}",
                    verification.package_name, verification.version, actual_version
                ));
            }
        }
    }

    None
}

#[derive(Debug)]
struct PublishRegistryVerification {
    package_name: String,
    version: String,
    url: String,
}

fn publish_registry_verification(
    response: &serde_json::Value,
    expected_version: Option<&str>,
) -> Option<PublishRegistryVerification> {
    let source = response
        .get("registry_verification")
        .or_else(|| response.get("registryVerification"))
        .unwrap_or(response);

    let package_name = string_field(source, &["package_name", "packageName", "package", "name"])?;
    let version = string_field(
        source,
        &["version", "published_version", "publishedVersion"],
    )
    .or(expected_version)
    .map(str::to_string)?;
    let url = string_field(source, &["version_url", "versionUrl", "url"])
        .map(str::to_string)
        .or_else(|| {
            let registry_url = string_field(source, &["registry_url", "registryUrl"])?;
            Some(package_registry_version_url(
                registry_url,
                package_name,
                &version,
            ))
        })?;

    Some(PublishRegistryVerification {
        package_name: package_name.to_string(),
        version,
        url,
    })
}

fn string_field<'a>(value: &'a serde_json::Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn package_registry_version_url(registry_url: &str, package_name: &str, version: &str) -> String {
    format!(
        "{}/{}/{}",
        registry_url.trim_end_matches('/'),
        percent_encode_path_segment(package_name),
        percent_encode_path_segment(version)
    )
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => {
                let _ = write!(&mut encoded, "%{:02X}", byte);
            }
        }
    }
    encoded
}

pub(crate) fn publish_response_output(response: &serde_json::Value) -> String {
    let stdout = response
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let stderr = response
        .get("stderr")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    format!("{}\n{}", stdout, stderr)
}

fn publish_failure_message(target: &str, response: &serde_json::Value) -> String {
    let exit_code = response
        .get("exit_code")
        .or_else(|| response.get("exitCode"))
        .and_then(|v| v.as_i64());
    let output = publish_response_output(response);
    let detail = output.trim();

    match (exit_code, detail.is_empty()) {
        (Some(code), false) => format!("Publish to {} failed (exit {}): {}", target, code, detail),
        (Some(code), true) => format!("Publish to {} failed (exit {})", target, code),
        (None, false) => format!("Publish to {} failed: {}", target, detail),
        (None, true) => format!("Publish to {} failed", target),
    }
}

#[cfg(test)]
mod tests {
    use super::publish_step_result;
    use crate::core::release::ReleaseStepStatus;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    struct RegistryServer {
        registry_url: String,
        handle: std::thread::JoinHandle<String>,
    }

    fn registry_server(status: u16, body: &'static str) -> RegistryServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind registry server");
        let port = listener.local_addr().expect("registry server addr").port();
        let registry_url = format!("http://127.0.0.1:{}", port);
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept registry request");
            let mut buffer = [0; 1024];
            let bytes_read = stream.read(&mut buffer).expect("read registry request");
            let request = String::from_utf8_lossy(&buffer[..bytes_read]).to_string();
            let request_line = request.lines().next().unwrap_or_default().to_string();
            let reason = if status == 200 { "OK" } else { "Not Found" };
            let response = format!(
                "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                status,
                reason,
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write registry response");
            request_line
        });

        RegistryServer {
            registry_url,
            handle,
        }
    }

    #[test]
    fn publish_step_fails_when_extension_reports_missing_secret() {
        let response = serde_json::json!({
            "success": false,
            "status": "missing_secret",
            "reason": "registry token is not configured",
            "stdout": "",
            "stderr": "",
        });
        let data = serde_json::json!({ "response": response.clone() });

        let result = publish_step_result(
            "publish.registry",
            "registry",
            "registry",
            Some(data),
            &response,
            None,
        );

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(result
            .error
            .unwrap()
            .contains("registry token is not configured"));
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn publish_step_skips_when_extension_reports_auth_required() {
        let response = serde_json::json!({
            "success": false,
            "status": "auth_required",
            "message": "run the extension login command",
        });

        let result = publish_step_result(
            "publish.registry",
            "registry",
            "registry",
            None,
            &response,
            None,
        );

        assert_eq!(result.status, ReleaseStepStatus::Skipped);
        assert!(result
            .warnings
            .join("\n")
            .contains("run the extension login command"));
        assert!(result.error.is_none());
    }

    #[test]
    fn publish_step_skips_when_npm_output_reports_eneedauth() {
        let response = serde_json::json!({
            "success": false,
            "exitCode": 1,
            "stdout": "",
            "stderr": "npm ERR! code ENEEDAUTH\nnpm ERR! need auth This command requires you to be logged in to https://registry.npmjs.org/",
        });

        let result =
            publish_step_result("publish.nodejs", "nodejs", "nodejs", None, &response, None);

        assert_eq!(result.status, ReleaseStepStatus::Skipped);
        assert!(result.warnings.join("\n").contains("ENEEDAUTH"));
        assert!(result.error.is_none());
    }

    #[test]
    fn publish_step_fails_when_extension_error_has_no_skip_status() {
        let response = serde_json::json!({
            "success": false,
            "exitCode": 1,
            "stderr": "error: failed to upload package: 500 server error",
        });

        let result = publish_step_result(
            "publish.registry",
            "registry",
            "registry",
            None,
            &response,
            None,
        );

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(result.error.unwrap().contains("500 server error"));
    }

    #[test]
    fn publish_step_fails_when_registry_version_is_missing() {
        let server = registry_server(404, r#"{"error":"not found"}"#);
        let response = serde_json::json!({
            "success": true,
            "registry_verification": {
                "registry_url": server.registry_url,
                "package_name": "@extrachill/components",
                "version": "0.5.2"
            }
        });

        let result =
            publish_step_result("publish.npm", "npm", "npm", None, &response, Some("0.5.2"));

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(result
            .error
            .unwrap()
            .contains("registry returned 404 Not Found"));
        assert_eq!(
            server.handle.join().expect("server request"),
            "GET /%40extrachill%2Fcomponents/0.5.2 HTTP/1.1"
        );
    }

    #[test]
    fn publish_step_succeeds_when_registry_version_is_present() {
        let server = registry_server(200, r#"{"version":"0.5.2"}"#);
        let response = serde_json::json!({
            "success": true,
            "registry_verification": {
                "registry_url": server.registry_url,
                "package_name": "@extrachill/components",
                "version": "0.5.2"
            }
        });

        let result =
            publish_step_result("publish.npm", "npm", "npm", None, &response, Some("0.5.2"));

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert_eq!(
            server.handle.join().expect("server request"),
            "GET /%40extrachill%2Fcomponents/0.5.2 HTTP/1.1"
        );
    }
}
