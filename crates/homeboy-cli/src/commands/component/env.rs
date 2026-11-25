//! `component env` — detect runtime environment requirements from a component's
//! source files and configured extension.
//!
//! Split out of `component.rs` to keep the top-level command dispatch focused on
//! CRUD operations.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use homeboy::core::component;
use homeboy_extension_contract::api::v1::{
    ExtensionApiComponentEnvDetectRequest, ExtensionApiOperationFailureCode,
    ExtensionApiRuntimeRequirement, EXTENSION_API_COMPONENT_ENV_DETECT_REQUEST_SCHEMA,
    EXTENSION_API_V1,
};
use homeboy_extension_contract::manifest_capability_config::RuntimeRequirementsConfig;

use super::{CmdResult, ComponentOutput};

/// Runtime environment requirements detected from the component's source files.
#[derive(Debug, Serialize, Deserialize)]
struct ComponentEnvOutput {
    command: String,
    id: String,
    extension: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    runtimes: BTreeMap<String, ComponentRuntimeRequirement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ComponentRuntimeRequirement {
    version: String,
    source: String,
}

pub(super) fn env(id: Option<&str>, path: Option<&str>) -> CmdResult<ComponentOutput> {
    let component = component::resolve_target(component::TargetSpec {
        component_id: id,
        path_override: path,
        allow_synthetic: id.is_some() || path.is_none(),
        ..component::TargetSpec::new(id, path)
    })
    .map_err(|e| {
        if id.is_none() && path.is_none() {
            homeboy::core::Error::validation_missing_argument(vec!["id or --path".to_string()])
        } else {
            e.with_contextual_hint()
        }
    })?
    .component;

    let comp_id = component.id.clone();
    let local_path = Path::new(&component.local_path);

    // Determine the primary extension
    let extension_id = component
        .extensions
        .as_ref()
        .and_then(|exts| exts.keys().next().cloned());

    let mut runtimes: BTreeMap<String, ComponentRuntimeRequirement> = BTreeMap::new();

    // Read component-scoped runtime requirements from the resolved typed component.
    if let Some(ref ext_id) = extension_id {
        if let Some(settings) = component
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.get(ext_id))
            .map(|config| &config.settings)
        {
            if let Some(runtime_values) = settings.get("runtimes") {
                if let Ok(requirements) = serde_json::from_value::<RuntimeRequirementsConfig>(
                    serde_json::json!({ "runtimes": runtime_values }),
                ) {
                    apply_component_runtime_requirements(
                        requirements,
                        &mut runtimes,
                        "component",
                        true,
                    );
                }
            }
        }
    }

    if let Some(ref ext_id) = extension_id {
        let response = homeboy_core::extension::invoke::detect_component_env_api(
            &ExtensionApiComponentEnvDetectRequest {
                schema: EXTENSION_API_COMPONENT_ENV_DETECT_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: ext_id.clone(),
            },
            homeboy_core::extension::invoke::ComponentEnvDetectionContext::new(local_path),
        );
        if let Some(failure) = response.failure {
            match failure.code {
                ExtensionApiOperationFailureCode::CapabilityNotProvided
                | ExtensionApiOperationFailureCode::ExtensionNotFound => {}
                ExtensionApiOperationFailureCode::CapabilityExecutionFailed => {
                    return Err(homeboy::core::Error::internal_io(
                        failure.message,
                        response.process.map(|process| process.stderr),
                    ));
                }
                ExtensionApiOperationFailureCode::CapabilityOutputInvalid => {
                    return Err(homeboy::core::Error::validation_invalid_argument(
                        "component",
                        failure.message,
                        response
                            .process
                            .map(|process| process.stdout.chars().take(200).collect()),
                        None,
                    ));
                }
                _ => {
                    return Err(homeboy::core::Error::validation_invalid_argument(
                        "extension",
                        failure.message,
                        Some(ext_id.clone()),
                        None,
                    ));
                }
            }
        } else {
            apply_detected_runtimes(&response.detected_runtimes, &mut runtimes);
        }
        apply_extension_runtime_requirements(ext_id, &response.extension_runtimes, &mut runtimes);
    }

    // Top-level declarations are authoritative, even over detected versions.
    apply_component_runtime_requirements(
        RuntimeRequirementsConfig {
            runtimes: component.runtimes,
        },
        &mut runtimes,
        "component",
        true,
    );

    let env_output = ComponentEnvOutput {
        command: "component.env".to_string(),
        id: comp_id.clone(),
        extension: extension_id,
        runtimes,
    };

    let entity = serde_json::to_value(&env_output).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "component",
            "Failed to serialize env output",
            Some(error.to_string()),
            None,
        )
    })?;

    Ok((
        ComponentOutput {
            command: "component.env".to_string(),
            id: Some(comp_id),
            entity: Some(entity),
            ..Default::default()
        },
        0,
    ))
}

fn apply_detected_runtimes(
    detected: &[ExtensionApiRuntimeRequirement],
    runtimes: &mut BTreeMap<String, ComponentRuntimeRequirement>,
) {
    apply_runtime_requirements(detected, runtimes, "component", true);
}

fn apply_extension_runtime_requirements(
    extension_id: &str,
    requirements: &[ExtensionApiRuntimeRequirement],
    runtimes: &mut BTreeMap<String, ComponentRuntimeRequirement>,
) {
    let source = format!("extension:{extension_id}");
    apply_runtime_requirements(requirements, runtimes, &source, false);
}

fn apply_runtime_requirements(
    requirements: &[ExtensionApiRuntimeRequirement],
    runtimes: &mut BTreeMap<String, ComponentRuntimeRequirement>,
    source: &str,
    overwrite: bool,
) {
    for requirement in requirements {
        if overwrite || !runtimes.contains_key(&requirement.id) {
            runtimes.insert(
                requirement.id.clone(),
                ComponentRuntimeRequirement {
                    version: requirement.version.clone(),
                    source: source.to_string(),
                },
            );
        }
    }
}

fn apply_component_runtime_requirements(
    requirements: RuntimeRequirementsConfig,
    runtimes: &mut BTreeMap<String, ComponentRuntimeRequirement>,
    source: &str,
    overwrite: bool,
) {
    let mapped = requirements
        .runtimes
        .into_iter()
        .map(|(id, requirement)| ExtensionApiRuntimeRequirement {
            id,
            version: requirement.version,
        })
        .collect::<Vec<_>>();
    apply_runtime_requirements(&mapped, runtimes, source, overwrite);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    fn install_detector(home: &Path, id: &str, manifest: &str, script: &str) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        fs::create_dir_all(extension_dir.join("scripts/env")).expect("extension dirs");
        fs::write(extension_dir.join(format!("{id}.json")), manifest).expect("manifest");
        let script_path = extension_dir.join("scripts/env/detect.sh");
        homeboy_test_support::write_executable(&script_path, script, 0o755)
            .expect("write detector");
    }

    fn write_component(homeboy_json: &str) -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("homeboy.json"), homeboy_json).expect("homeboy.json");
        let git_init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(temp.path())
            .output()
            .expect("git init");
        assert!(git_init.status.success());
        temp
    }

    #[cfg(unix)]
    fn runtimes_from(output: &ComponentOutput) -> BTreeMap<String, ComponentRuntimeRequirement> {
        serde_json::from_value::<ComponentEnvOutput>(output.entity.clone().expect("entity"))
            .expect("env output")
            .runtimes
    }

    #[test]
    fn component_env_path_uses_shared_git_root_portable_discovery() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let subdir = repo.join("packages").join("plugin");
        fs::create_dir_all(&subdir).expect("subdir");
        fs::write(repo.join("homeboy.json"), r#"{"id":"portable-env"}"#).expect("homeboy.json");
        let git_init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .output()
            .expect("git init");
        assert!(git_init.status.success());

        let (output, code) = env(None, Some(&subdir.to_string_lossy())).expect("component env");

        assert_eq!(code, 0);
        assert_eq!(output.id.as_deref(), Some("portable-env"));
        assert_eq!(output.command, "component.env");
    }

    #[test]
    fn component_env_without_extension_reports_only_top_level_runtimes() {
        for (homeboy_json, expected_runtimes) in [
            (
                r#"{"id":"portable-env","runtimes":{"node":{"version":"24"}}}"#,
                Some(serde_json::json!({"node": {"version": "24", "source": "component"}})),
            ),
            // No declarations: the `runtimes` key is omitted entirely.
            (r#"{"id":"portable-env"}"#, None),
        ] {
            let component = write_component(homeboy_json);
            let (output, code) =
                env(None, Some(&component.path().to_string_lossy())).expect("component env");

            assert_eq!(code, 0);
            let entity = output.entity.expect("entity");
            assert_eq!(entity["extension"], serde_json::Value::Null);
            assert_eq!(entity.get("runtimes").cloned(), expected_runtimes);
        }
    }

    #[cfg(unix)]
    #[test]
    fn component_env_top_level_runtimes_override_detected_and_extension_settings() {
        homeboy_core::test_support::with_isolated_home(|home| {
            install_detector(
                home.path(),
                "demo",
                r#"{"id":"demo","name":"Demo","version":"1.0.0","component_env":{"detect_script":"scripts/env/detect.sh"},"runtime":{"runtimes":{"node":{"version":"22"},"ruby":{"version":"3.3"}}}}"#,
                "#!/bin/sh\nprintf '{\"runtimes\":{\"node\":{\"version\":\"20\"},\"php\":{\"version\":\"8.2\"}}}'\n",
            );
            let component = write_component(
                r#"{"id":"portable-env","runtimes":{"node":{"version":"24"}},"extensions":{"demo":{"runtimes":{"node":{"version":"18"},"python":{"version":"3.12"}}}}}"#,
            );
            let (output, code) =
                env(None, Some(&component.path().to_string_lossy())).expect("component env");

            assert_eq!(code, 0);
            let runtimes = runtimes_from(&output);
            assert_eq!(
                runtimes["node"],
                ComponentRuntimeRequirement {
                    version: "24".to_string(),
                    source: "component".to_string(),
                }
            );
            assert_eq!(runtimes["php"].version, "8.2");
            assert_eq!(runtimes["python"].version, "3.12");
            assert_eq!(runtimes["ruby"].source, "extension:demo");
        });
    }

    #[cfg(unix)]
    #[test]
    fn component_env_cli_applies_detector_precedence_and_provenance() {
        homeboy_core::test_support::with_isolated_home(|home| {
            install_detector(
                home.path(),
                "demo",
                r#"{"id":"demo","name":"Demo","version":"1.0.0","component_env":{"detect_script":"scripts/env/detect.sh"},"runtime":{"runtimes":{"node":{"version":"24"},"php":{"version":"8.4"}}}}"#,
                "#!/bin/sh\nprintf '{\"runtimes\":{\"php\":{\"version\":\"8.2\"}}}'\n",
            );
            let component = write_component(
                r#"{"id":"portable-env","extensions":{"demo":{"runtimes":{"node":{"version":"20"},"php":{"version":"8.0"}}}}}"#,
            );
            let (output, code) =
                env(None, Some(&component.path().to_string_lossy())).expect("component env");

            assert_eq!(code, 0);
            let runtimes = runtimes_from(&output);
            assert_eq!(runtimes["php"].version, "8.2");
            assert_eq!(runtimes["php"].source, "component");
            assert_eq!(runtimes["node"].version, "20");
            assert_eq!(runtimes["node"].source, "component");
        });
    }

    #[cfg(unix)]
    #[test]
    fn component_env_cli_skips_missing_capability_and_keeps_extension_defaults() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions").join("demo");
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(
                extension_dir.join("demo.json"),
                r#"{"id":"demo","name":"Demo","version":"1.0.0","runtime":{"runtimes":{"node":{"version":"24"}}}}"#,
            )
            .expect("manifest");
            let component = write_component(
                r#"{"id":"portable-env","extensions":{"demo":{"runtimes":{"php":{"version":"8.2"}}}}}"#,
            );
            let (output, code) =
                env(None, Some(&component.path().to_string_lossy())).expect("component env");

            assert_eq!(code, 0);
            let runtimes = runtimes_from(&output);
            assert_eq!(runtimes["php"].version, "8.2");
            assert_eq!(runtimes["php"].source, "component");
            assert_eq!(runtimes["node"].version, "24");
            assert_eq!(runtimes["node"].source, "extension:demo");
        });
    }

    #[cfg(unix)]
    #[test]
    fn component_env_cli_fails_explicitly_when_the_detector_fails_or_emits_invalid_output() {
        for (script, expected) in [
            (
                "#!/bin/sh\nprintf 'detector boom' >&2\nexit 7\n",
                "IO error",
            ),
            (
                "#!/bin/sh\nprintf 'not json'\n",
                "parse component env detector output",
            ),
        ] {
            homeboy_core::test_support::with_isolated_home(|home| {
                install_detector(
                    home.path(),
                    "demo",
                    r#"{"id":"demo","name":"Demo","version":"1.0.0","component_env":{"detect_script":"scripts/env/detect.sh"}}"#,
                    script,
                );
                let component =
                    write_component(r#"{"id":"portable-env","extensions":{"demo":{}}}"#);
                let error = env(None, Some(&component.path().to_string_lossy()))
                    .expect_err("detector failure must be explicit");
                assert!(
                    error.message.contains(expected),
                    "{expected}: {}",
                    error.message
                );
            });
        }
    }
}
