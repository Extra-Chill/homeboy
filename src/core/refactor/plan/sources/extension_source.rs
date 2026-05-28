use crate::core::component::Component;
use crate::core::extension;
use crate::core::refactor::auto::{self, FixApplied};
use crate::core::Error;
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
    fatal_error: Option<String>,
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
    let value = match extension::run_refactor_script_with_outcome(&manifest, &command) {
        extension::RefactorScriptOutcome::Succeeded(value) => value,
        extension::RefactorScriptOutcome::Missing => {
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
        extension::RefactorScriptOutcome::Failed(failure) => {
            return Err(refactor_source_failure_error(
                extension_id,
                source,
                &failure,
                Vec::new(),
            ));
        }
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

    if let Some(fatal_error) = response
        .fatal_error
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        return Err(refactor_source_failure_error(
            extension_id,
            source,
            &extension::RefactorScriptFailure {
                script_path: manifest
                    .extension_path
                    .as_deref()
                    .and_then(|extension_path| {
                        manifest.refactor_script().map(|script| {
                            Path::new(extension_path)
                                .join(script)
                                .to_string_lossy()
                                .to_string()
                        })
                    })
                    .unwrap_or_default(),
                exit_code: Some(0),
                stdout: String::new(),
                stderr: fatal_error.to_string(),
                io_error: None,
                json_error: None,
            },
            response.warnings.clone(),
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

fn refactor_source_failure_error(
    extension_id: &str,
    source: &str,
    failure: &extension::RefactorScriptFailure,
    warnings: Vec<String>,
) -> Error {
    let message = refactor_source_failure_message(failure);
    Error::extension_execution_failed(
        extension_id,
        Some(source.to_string()),
        "refactor_source",
        message,
        crate::core::error::ExtensionExecutionFailedDetails {
            extension_id: extension_id.to_string(),
            source: Some(source.to_string()),
            command: "refactor_source".to_string(),
            script_path: if failure.script_path.is_empty() {
                None
            } else {
                Some(failure.script_path.clone())
            },
            exit_code: failure.exit_code,
            stdout: failure.stdout.clone(),
            stderr: failure.stderr.clone(),
            io_error: failure.io_error.clone(),
            json_error: failure.json_error.clone(),
            warnings,
        },
    )
}

fn refactor_source_failure_message(failure: &extension::RefactorScriptFailure) -> String {
    if let Some(error) = failure
        .io_error
        .as_deref()
        .filter(|error| !error.is_empty())
    {
        return error.to_string();
    }

    if let Some(error) = failure
        .json_error
        .as_deref()
        .filter(|error| !error.is_empty())
    {
        return format!("invalid JSON response: {error}");
    }

    let detail = failure
        .stderr
        .trim()
        .lines()
        .next()
        .filter(|line| !line.is_empty())
        .or_else(|| {
            failure
                .stdout
                .trim()
                .lines()
                .next()
                .filter(|line| !line.is_empty())
        });

    match (failure.exit_code, detail) {
        (Some(code), Some(detail)) => format!("exit {code}: {detail}"),
        (Some(code), None) => format!("exit {code}"),
        (None, Some(detail)) => detail.to_string(),
        (None, None) => "extension refactor script failed".to_string(),
    }
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
                }
            })
            .to_string(),
        )
        .unwrap();

        fs::write(
            extension_dir.join("refactor-source.sh"),
            "#!/bin/sh\nprintf '%s\n' 'WP Codebox audit fan-out failed: 7 of 11 task(s) failed' >&2\nprintf '%s\n' 'artifact: /tmp/homeboy-wp-codebox-audit-o7yqgh91/fanout-run.json' >&2\nexit 17\n",
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
    fn extension_refactor_source_failure_reports_execution_error() {
        crate::test_support::with_isolated_home(|home| {
            let root = tmp_dir("failing-extension-dispatch");
            fs::create_dir_all(&root).unwrap();
            write_failing_refactor_source_extension(home.path(), "generic");

            let err = match try_extension_refactor_source_stage(
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
                    "generic".to_string(),
                )],
            ) {
                Ok(_) => {
                    panic!("non-zero handled extension failure should surface as execution failure")
                }
                Err(err) => err,
            };

            assert_eq!(err.code, crate::core::ErrorCode::ExtensionExecutionFailed);
            assert!(err
                .message
                .contains("Extension 'generic' failed refactor_source source 'audit'"));
            assert!(err.message.contains("WP Codebox audit fan-out failed"));
            assert!(!err.message.contains("did not handle refactor source"));
            assert_eq!(err.details["extension_id"], "generic");
            assert_eq!(err.details["source"], "audit");
            assert_eq!(err.details["exit_code"], 17);
            assert!(err.details["stderr"]
                .as_str()
                .unwrap()
                .contains("/tmp/homeboy-wp-codebox-audit-o7yqgh91/fanout-run.json"));

            let _ = fs::remove_dir_all(root);
        });
    }
}
