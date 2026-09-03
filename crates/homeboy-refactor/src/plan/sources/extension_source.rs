use crate::auto::{self, FixApplied};
use homeboy_core::component::Component;
use homeboy_core::Error;
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::planning::{PlannedStage, SourceStageSummary};

#[derive(Debug, Clone, Deserialize)]
struct ExtensionRefactorSourceResponse {
    #[serde(default)]
    handled: bool,
    #[serde(default)]
    detected_findings: Option<usize>,
    #[serde(default)]
    changed_files: Vec<String>,
    #[serde(default)]
    fix_results: Vec<FixApplied>,
    #[serde(default)]
    warnings: Vec<String>,
}

pub(super) fn try_extension_refactor_source_stage<T: Serialize>(
    source: &str,
    component: &Component,
    root: &Path,
    source_result: &T,
    write: bool,
    settings: &[(String, serde_json::Value)],
) -> homeboy_core::Result<Option<PlannedStage>> {
    let setting_key = format!("refactor.{}.extension", source);
    let Some(extension_id) = setting_value(settings, &setting_key) else {
        return Ok(None);
    };
    let provider = crate::refactor_provider::refactor_providers()
        .into_iter()
        .find(|provider| provider.extension_id == extension_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                setting_key.clone(),
                format!(
                    "Extension '{}' does not provide a refactor capability",
                    extension_id
                ),
                None,
                Some(vec![
                    "Install an extension that declares refactor.<file-extension> capabilities."
                        .to_string(),
                ]),
            )
        })?;
    // Source-level commands are not tied to one file. Any advertised refactor
    // capability resolves to the provider's shared analysis entrypoint.
    let file_extension = provider
        .file_extensions
        .first()
        .expect("refactor providers advertise at least one file extension")
        .clone();
    let command = serde_json::json!({
        "command": "refactor_source",
        "source": source,
        "component_id": component.id,
        "root": root.to_string_lossy(),
        "source_result": source_result,
        "write": write,
        "settings": settings.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
    });
    let invocation =
        crate::refactor_provider::invoke_refactor(&provider, root, &file_extension, command);
    let Some(value) = invocation.output.clone() else {
        return Err(extension_refactor_source_failure_error(
            &setting_key,
            extension_id,
            source,
            invocation,
        ));
    };
    let response_value = unwrap_refactor_source_payload(value);
    let response: ExtensionRefactorSourceResponse = serde_json::from_value(response_value)
        .map_err(|err| {
            Error::validation_invalid_argument(
                setting_key.clone(),
                format!(
                    "Extension '{}' returned an invalid refactor source response: {}",
                    extension_id, err
                ),
                None,
                None,
            )
        })?;

    if !response.handled {
        return Err(Error::validation_invalid_argument(
            setting_key,
            format!(
                "Extension '{}' declined refactor source '{}'",
                extension_id, source
            ),
            None,
            Some(vec![
                "Remove the setting to use Homeboy's built-in refactor source".to_string(),
            ]),
        ));
    }

    let fix_summary = if response.fix_results.is_empty() {
        None
    } else {
        Some(auto::summarize_fix_results(&response.fix_results))
    };

    Ok(Some(PlannedStage {
        source: source.to_string(),
        summary: SourceStageSummary {
            stage: source.to_string(),
            collected: true,
            applied: write && !response.changed_files.is_empty(),
            edit_count: response.fix_results.len(),
            files_modified: response.changed_files.len(),
            detected_findings: response.detected_findings,
            changed_files: response.changed_files,
            fix_summary,
            warnings: response.warnings,
        },
        fix_results: response.fix_results,
    }))
}

fn unwrap_refactor_source_payload(value: serde_json::Value) -> serde_json::Value {
    if value.get("variant").and_then(serde_json::Value::as_str) != Some("refactor_source") {
        return value;
    }

    value
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

fn extension_refactor_source_failure_error(
    setting_key: &str,
    extension_id: &str,
    source: &str,
    response: homeboy_extension_contract::api::v1::ExtensionApiInvokeResponse,
) -> Error {
    let failure = response.failure.expect("failed invocation has failure");
    let process = response.process;
    let summary = process.as_ref().and_then(process_failure_summary);
    let mut message = format!(
        "Extension '{}' failed refactor source '{}': {}",
        extension_id, source, failure.message
    );
    if let Some(exit_code) = process.as_ref().and_then(|process| process.exit_code) {
        message.push_str(&format!(" (exit code {exit_code})"));
    }
    if let Some(summary) = summary {
        message.push_str(": ");
        message.push_str(&summary);
    }

    Error::new(
        homeboy_core::error::ErrorCode::ValidationInvalidArgument,
        format!("Invalid argument '{}': {}", setting_key, message),
        serde_json::json!({
            "field": setting_key,
            "problem": message,
            "extension_id": extension_id,
            "source": source,
            "failure_kind": failure.code,
            "exit_code": process.as_ref().and_then(|process| process.exit_code),
            "stdout": process.as_ref().map(|process| process.stdout.clone()).unwrap_or_default(),
            "stderr": process.as_ref().map(|process| process.stderr.clone()).unwrap_or_default(),
            "refactor_source_response": process.as_ref().and_then(|process| process.parsed_output.clone()),
        }),
    )
}

fn process_failure_summary(
    process: &homeboy_extension_contract::api::v1::ExtensionApiInvocationProcessEvidence,
) -> Option<String> {
    process
        .parsed_output
        .as_ref()
        .and_then(refactor_source_failure_message)
        .or_else(|| trimmed_non_empty(&process.stderr))
        .or_else(|| trimmed_non_empty(&process.stdout))
}

fn refactor_source_failure_message(value: &serde_json::Value) -> Option<String> {
    let payload = unwrap_refactor_source_payload(value.clone());
    ["fatal", "error", "message", "failure", "reason"]
        .iter()
        .find_map(|key| payload.get(*key).and_then(serde_json::Value::as_str))
        .and_then(trimmed_non_empty)
}

fn trimmed_non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn read_optional_json(path: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
}

fn setting_value<'a>(settings: &'a [(String, serde_json::Value)], key: &str) -> Option<&'a str> {
    settings
        .iter()
        .rev()
        .find(|(setting_key, _)| setting_key == key)
        .and_then(|(_, value)| value.as_str())
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::component::Component;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-refactor-sources-{name}-{nanos}"))
    }

    fn test_component(root: &Path) -> Component {
        Component {
            id: "component".to_string(),
            local_path: root.to_string_lossy().to_string(),
            remote_path: String::new(),
            ..Default::default()
        }
    }

    fn write_refactor_source_extension(home: &Path, id: &str, capture_file: &Path) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        fs::create_dir_all(&extension_dir).unwrap();
        fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": "Generic Refactor Source Fixture",
                "version": "0.0.0",
                "scripts": {
                    "refactor": "refactor-source.sh"
                },
                "provides": { "file_extensions": ["rs"] }
            })
            .to_string(),
        )
        .unwrap();

        fs::write(
            extension_dir.join("refactor-source.sh"),
            format!(
                "#!/bin/sh\ncat > '{}'\nprintf '%s' '{{\"variant\":\"refactor_source\",\"payload\":{{\"handled\":true,\"detected_findings\":2,\"changed_files\":[\"src/lib.rs\"],\"fix_results\":[{{\"file\":\"src/lib.rs\",\"rule\":\"demo\"}}],\"warnings\":[\"extension handled\"]}}}}'\n",
                capture_file.display()
            ),
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = extension_dir.join("refactor-source.sh");
            let mut permissions = fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(script, permissions).unwrap();
        }
    }

    fn write_failing_refactor_source_extension(home: &Path, id: &str) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        fs::create_dir_all(&extension_dir).unwrap();
        fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": "Failing Refactor Source Fixture",
                "version": "0.0.0",
                "scripts": {
                    "refactor": "refactor-source.sh"
                },
                "provides": { "file_extensions": ["rs"] }
            })
            .to_string(),
        )
        .unwrap();

        fs::write(
            extension_dir.join("refactor-source.sh"),
            "#!/bin/sh\nprintf '%s' '{\"variant\":\"refactor_source\",\"payload\":{\"handled\":true,\"status\":\"failed\",\"fatal\":\"audit fanout failed\",\"artifact_path\":\"/tmp/fanout-run.json\",\"warnings\":[\"kept warning path\"]}}'\nprintf '%s\\n' 'extension stderr detail' >&2\nexit 7\n",
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = extension_dir.join("refactor-source.sh");
            let mut permissions = fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(script, permissions).unwrap();
        }
    }

    #[test]
    fn extension_refactor_source_dispatch_uses_generic_source_result() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let root = tmp_dir("generic-extension-dispatch");
            fs::create_dir_all(&root).unwrap();
            let capture_file = root.join("command.json");
            write_refactor_source_extension(home.path(), "generic", &capture_file);

            let audit_stage = try_extension_refactor_source_stage(
                "audit",
                &test_component(&root),
                &root,
                &serde_json::json!({
                    "component_id": "component",
                    "findings": []
                }),
                false,
                &[(
                    "refactor.audit.extension".to_string(),
                    serde_json::json!("generic"),
                )],
            )
            .unwrap()
            .expect("extension should handle audit refactor source");

            assert_eq!(audit_stage.source, "audit");
            let audit_command: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&capture_file).unwrap()).unwrap();
            assert_eq!(audit_command["source"], "audit");
            assert_eq!(audit_command["source_result"]["component_id"], "component");
            assert!(audit_command.get("audit_result").is_none());

            let stage = try_extension_refactor_source_stage(
                "lint",
                &test_component(&root),
                &root,
                &serde_json::json!({
                    "findings": [{
                        "tool": "lint",
                        "fingerprint": "lint-1",
                        "message": "demo",
                        "category": "style"
                    }]
                }),
                true,
                &[
                    (
                        "refactor.lint.extension".to_string(),
                        serde_json::json!("generic"),
                    ),
                    ("extension.setting".to_string(), serde_json::json!("value")),
                ],
            )
            .unwrap()
            .expect("extension should handle lint refactor source");

            assert_eq!(stage.source, "lint");
            assert_eq!(stage.summary.detected_findings, Some(2));
            assert_eq!(stage.summary.changed_files, vec!["src/lib.rs"]);
            assert_eq!(stage.fix_results.len(), 1);

            let command: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&capture_file).unwrap()).unwrap();
            assert_eq!(command["command"], "refactor_source");
            assert_eq!(command["source"], "lint");
            assert_eq!(command["write"], true);
            assert_eq!(command["settings"]["extension.setting"], "value");
            assert_eq!(
                command["source_result"]["findings"][0]["fingerprint"],
                "lint-1"
            );
            assert!(command.get("audit_result").is_none());

            let _ = fs::remove_dir_all(root);
        });
    }

    #[test]
    fn extension_refactor_source_nonzero_exit_reports_real_failure() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let root = tmp_dir("generic-extension-failure");
            fs::create_dir_all(&root).unwrap();
            write_failing_refactor_source_extension(home.path(), "fatal-source");

            let result = try_extension_refactor_source_stage(
                "audit",
                &test_component(&root),
                &root,
                &serde_json::json!({ "component_id": "component", "findings": [] }),
                false,
                &[(
                    "refactor.audit.extension".to_string(),
                    serde_json::json!("fatal-source"),
                )],
            );
            let err = match result {
                Ok(_) => panic!("non-zero extension result should fail with diagnostics"),
                Err(err) => err,
            };

            assert!(
                !err.message.contains("did not handle"),
                "fatal extension failures must not be reported as unhandled: {}",
                err.message
            );
            assert_eq!(err.details["extension_id"], "fatal-source");
            assert_eq!(err.details["source"], "audit");
            assert_eq!(err.details["failure_kind"], "capability_execution_failed");
            assert_eq!(err.details["exit_code"], 7);
            assert_eq!(err.details["stderr"], "extension stderr detail\n");
            assert_eq!(
                err.details["refactor_source_response"]["payload"]["artifact_path"],
                "/tmp/fanout-run.json"
            );
            assert_eq!(
                err.details["refactor_source_response"]["payload"]["warnings"][0],
                "kept warning path"
            );

            let _ = fs::remove_dir_all(root);
        });
    }
}
