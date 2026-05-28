use crate::core::component::Component;
use crate::core::extension;
use crate::core::refactor::auto::{self, FixApplied};
use crate::core::{Error, ErrorCode};
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
    #[serde(default)]
    fatal: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

pub(super) fn try_extension_refactor_source_stage<T: Serialize>(
    source: &str,
    component: &Component,
    root: &Path,
    source_result: &T,
    write: bool,
    settings: &[(String, String)],
) -> crate::core::Result<Option<PlannedStage>> {
    let setting_key = format!("refactor.{}.extension", source);
    let Some(extension_id) = setting_value(settings, &setting_key) else {
        return Ok(None);
    };
    let manifest = extension::load_extension(extension_id)?;
    let command = serde_json::json!({
        "command": "refactor_source",
        "source": source,
        "component_id": component.id,
        "root": root.to_string_lossy(),
        "source_result": source_result,
        "write": write,
        "settings": settings.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
    });
    let value = match extension::run_refactor_script_result(&manifest, &command) {
        Ok(Some(value)) => value,
        Ok(None) => {
            return Err(Error::validation_invalid_argument(
                setting_key.clone(),
                format!(
                    "Extension '{}' did not handle refactor source '{}'",
                    extension_id, source
                ),
                None,
                Some(vec![
                    "Remove the setting to use Homeboy's built-in refactor source".to_string(),
                    "Or update the extension refactor script to support command=refactor_source"
                        .to_string(),
                ]),
            ));
        }
        Err(err) => {
            return Err(extension_refactor_source_execution_error(
                &setting_key,
                extension_id,
                source,
                &err,
            ));
        }
    };
    let response_value = unwrap_refactor_source_payload(value);
    let response: ExtensionRefactorSourceResponse = serde_json::from_value(response_value.clone())
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

    if let Some(reason) = response_fatal_reason(&response) {
        return Err(extension_refactor_source_fatal_response_error(
            &setting_key,
            extension_id,
            source,
            &reason,
            response_value,
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

fn response_fatal_reason(response: &ExtensionRefactorSourceResponse) -> Option<String> {
    response
        .fatal
        .as_deref()
        .or(response.error.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            response
                .status
                .as_deref()
                .map(str::trim)
                .filter(|status| status.eq_ignore_ascii_case("failed"))
                .map(|status| format!("extension reported status {status}"))
        })
}

fn extension_refactor_source_fatal_response_error(
    setting_key: &str,
    extension_id: &str,
    source: &str,
    reason: &str,
    response: serde_json::Value,
) -> Error {
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "Invalid argument '{}': Extension '{}' failed while handling refactor source '{}': {}",
            setting_key, extension_id, source, reason
        ),
        serde_json::json!({
            "field": setting_key,
            "problem": "extension_refactor_source_failed",
            "extension_id": extension_id,
            "source": source,
            "response": response,
        }),
    )
}

fn extension_refactor_source_execution_error(
    setting_key: &str,
    extension_id: &str,
    source: &str,
    err: &extension::RefactorScriptRunError,
) -> Error {
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "Invalid argument '{}': Extension '{}' failed while handling refactor source '{}': {}",
            setting_key,
            extension_id,
            source,
            err.summary()
        ),
        serde_json::json!({
            "field": setting_key,
            "problem": "extension_refactor_source_failed",
            "extension_id": extension_id,
            "source": source,
            "script_path": err.script_path,
            "exit_code": err.exit_code,
            "stdout": err.stdout,
            "stderr": err.stderr,
            "io_error": err.io_error,
        }),
    )
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

pub(super) fn read_optional_json(path: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
}

fn setting_value<'a>(settings: &'a [(String, String)], key: &str) -> Option<&'a str> {
    settings
        .iter()
        .rev()
        .find(|(setting_key, _)| setting_key == key)
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::Component;
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
                }
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

    fn write_failing_refactor_source_extension(home: &Path, id: &str, artifact_file: &Path) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        fs::create_dir_all(&extension_dir).unwrap();
        fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": "Failing Refactor Source Fixture",
                "version": "0.0.0",
                "scripts": {
                    "refactor": "refactor-source.sh"
                }
            })
            .to_string(),
        )
        .unwrap();

        fs::write(
            extension_dir.join("refactor-source.sh"),
            format!(
                "#!/bin/sh
cat > /dev/null
printf '%s\n' '{{\"variant\":\"refactor_source\",\"payload\":{{\"handled\":true,\"fatal\":\"fanout failed\",\"artifact\":\"{}\"}}}}'
printf '%s\n' 'fatal: WP Codebox fanout failed; artifact={}' >&2
exit 42
",
                artifact_file.display(),
                artifact_file.display()
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

    fn write_fatal_response_refactor_source_extension(home: &Path, id: &str) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        fs::create_dir_all(&extension_dir).unwrap();
        fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": "Fatal Response Refactor Source Fixture",
                "version": "0.0.0",
                "scripts": {
                    "refactor": "refactor-source.sh"
                }
            })
            .to_string(),
        )
        .unwrap();

        fs::write(
            extension_dir.join("refactor-source.sh"),
            "#!/bin/sh\ncat > /dev/null\nprintf '%s' '{\"variant\":\"refactor_source\",\"payload\":{\"handled\":true,\"fatal\":\"fanout failed after handling\",\"warnings\":[\"artifact=/tmp/fanout-run.json\"]}}'\n",
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
        crate::test_support::with_isolated_home(|home| {
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
                    "generic".to_string(),
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
                    "lint_findings": [{
                        "id": "lint-1",
                        "message": "demo",
                        "category": "style"
                    }]
                }),
                true,
                &[
                    ("refactor.lint.extension".to_string(), "generic".to_string()),
                    ("extension.setting".to_string(), "value".to_string()),
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
            assert_eq!(command["source_result"]["lint_findings"][0]["id"], "lint-1");
            assert!(command.get("audit_result").is_none());

            let _ = fs::remove_dir_all(root);
        });
    }

    #[test]
    fn extension_refactor_source_preserves_handled_failure_output() {
        crate::test_support::with_isolated_home(|home| {
            let root = tmp_dir("generic-extension-fatal");
            fs::create_dir_all(&root).unwrap();
            let artifact_file = root.join("fanout-run.json");
            write_failing_refactor_source_extension(home.path(), "fatal-source", &artifact_file);

            let result = try_extension_refactor_source_stage(
                "audit",
                &test_component(&root),
                &root,
                &serde_json::json!({
                    "component_id": "component",
                    "findings": []
                }),
                true,
                &[(
                    "refactor.audit.extension".to_string(),
                    "fatal-source".to_string(),
                )],
            );
            let err = match result {
                Ok(_) => panic!("fatal extension result should fail the source stage"),
                Err(err) => err,
            };

            let message = err.to_string();
            assert!(message.contains(
                "Extension 'fatal-source' failed while handling refactor source 'audit'"
            ));
            assert!(message.contains("exit code 42"));
            assert!(message.contains("WP Codebox fanout failed"));
            assert!(!message.contains("did not handle refactor source"));

            assert_eq!(err.details["extension_id"], "fatal-source");
            assert_eq!(err.details["source"], "audit");
            assert_eq!(err.details["exit_code"], 42);
            assert!(err.details["stderr"]
                .as_str()
                .unwrap()
                .contains(artifact_file.to_string_lossy().as_ref()));
            assert!(err.details["stdout"]
                .as_str()
                .unwrap()
                .contains("\"handled\":true"));

            let _ = fs::remove_dir_all(root);
        });
    }

    #[test]
    fn extension_refactor_source_reports_handled_fatal_response() {
        crate::test_support::with_isolated_home(|home| {
            let root = tmp_dir("generic-extension-fatal-response");
            fs::create_dir_all(&root).unwrap();
            write_fatal_response_refactor_source_extension(home.path(), "fatal-response");

            let result = try_extension_refactor_source_stage(
                "audit",
                &test_component(&root),
                &root,
                &serde_json::json!({
                    "component_id": "component",
                    "findings": []
                }),
                true,
                &[(
                    "refactor.audit.extension".to_string(),
                    "fatal-response".to_string(),
                )],
            );
            let err = match result {
                Ok(_) => panic!("handled fatal extension response should fail the source stage"),
                Err(err) => err,
            };

            let message = err.to_string();
            assert!(message.contains(
                "Extension 'fatal-response' failed while handling refactor source 'audit'"
            ));
            assert!(message.contains("fanout failed after handling"));
            assert!(!message.contains("did not handle refactor source"));
            assert_eq!(err.details["response"]["handled"], true);
            assert_eq!(
                err.details["response"]["warnings"][0],
                "artifact=/tmp/fanout-run.json"
            );

            let _ = fs::remove_dir_all(root);
        });
    }
}
