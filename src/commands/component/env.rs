//! `component env` — detect runtime environment requirements from a component's
//! source files and configured extension.
//!
//! Split out of `component.rs` to keep the top-level command dispatch focused on
//! CRUD operations.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

use homeboy::core::component;

use super::{CmdResult, ComponentOutput};

/// Runtime environment requirements detected from the component's source files.
#[derive(Debug, Serialize)]
struct ComponentEnvOutput {
    command: String,
    id: String,
    extension: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    runtimes: BTreeMap<String, ComponentRuntimeRequirement>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
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

    // Read component-scoped runtime requirements from raw homeboy.json; the typed
    // extension settings intentionally preserve extension-owned unknown fields.
    if let Some(ref ext_id) = extension_id {
        let config_path = local_path.join("homeboy.json");
        if let Ok(raw) = std::fs::read_to_string(&config_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(ext_obj) = json.get("extensions").and_then(|e| e.get(ext_id.as_str())) {
                    if let Ok(requirements) = serde_json::from_value::<
                        homeboy::core::extension::RuntimeRequirementsConfig,
                    >(ext_obj.clone())
                    {
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
    }

    let extension = if let Some(ref ext_id) = extension_id {
        homeboy::core::extension::load_extension(ext_id).ok()
    } else {
        None
    };

    if let Some(ref extension) = extension {
        if let Some(detected) = run_component_env_detector(extension, local_path)? {
            apply_component_env_detector_output(detected, &mut runtimes);
        }
    }

    if let (Some(ext_id), Some(extension)) = (extension_id.as_ref(), extension.as_ref()) {
        if let Some(runtime) = extension.runtime.as_ref() {
            apply_extension_runtime_requirements(ext_id, runtime, &mut runtimes);
        }
    }

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

fn run_component_env_detector(
    extension: &homeboy::core::extension::ExtensionManifest,
    component_path: &Path,
) -> homeboy::core::Result<Option<homeboy::core::extension::RuntimeRequirementsConfig>> {
    let Some(component_env) = extension.component_env.as_ref() else {
        return Ok(None);
    };

    let extension_path = extension.extension_path.as_ref().ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "extension",
            "Extension manifest is missing extension_path",
            Some(extension.id.clone()),
            None,
        )
    })?;
    let script_path = Path::new(extension_path).join(&component_env.detect_script);
    if !script_path.exists() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "extension",
            format!(
                "Extension '{}' component env detector is missing {}",
                extension.id,
                script_path.display()
            ),
            None,
            None,
        ));
    }

    let command = homeboy::core::engine::shell::quote_path(&script_path.to_string_lossy());
    let output = homeboy::core::server::execute_local_command_in_dir(
        &command,
        Some(&component_path.to_string_lossy()),
        None,
    );

    if !output.success {
        return Err(homeboy::core::Error::internal_io(
            format!(
                "Component env detector for extension '{}' failed with exit code {}",
                extension.id, output.exit_code
            ),
            Some(output.stderr),
        ));
    }

    let trimmed = output.stdout.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let detected =
        serde_json::from_str::<homeboy::core::extension::RuntimeRequirementsConfig>(trimmed)
            .map_err(|error| {
                homeboy::core::Error::validation_invalid_json(
                    error,
                    Some(format!(
                        "parse component env detector output for extension '{}'",
                        extension.id
                    )),
                    Some(trimmed.chars().take(200).collect()),
                )
            })?;

    Ok(Some(detected))
}

fn apply_component_env_detector_output(
    detected: homeboy::core::extension::RuntimeRequirementsConfig,
    runtimes: &mut BTreeMap<String, ComponentRuntimeRequirement>,
) {
    apply_component_runtime_requirements(detected, runtimes, "component", true);
}

fn apply_extension_runtime_requirements(
    extension_id: &str,
    runtime: &homeboy::core::extension::RuntimeRequirementsConfig,
    runtimes: &mut BTreeMap<String, ComponentRuntimeRequirement>,
) {
    let source = format!("extension:{}", extension_id);
    apply_component_runtime_requirements(runtime.clone(), runtimes, &source, false);
}

fn apply_component_runtime_requirements(
    requirements: homeboy::core::extension::RuntimeRequirementsConfig,
    runtimes: &mut BTreeMap<String, ComponentRuntimeRequirement>,
    source: &str,
    overwrite: bool,
) {
    for (id, requirement) in requirements.runtimes {
        if overwrite || !runtimes.contains_key(&id) {
            runtimes.insert(
                id,
                ComponentRuntimeRequirement {
                    version: requirement.version,
                    source: source.to_string(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

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
    fn extension_runtime_requirements_fill_missing_component_versions() {
        let runtime: homeboy::core::extension::RuntimeRequirementsConfig =
            serde_json::from_value(serde_json::json!({
                "runtimes": {
                    "node": { "version": "24" },
                    "php": { "version": "8.3" }
                }
            }))
            .expect("runtime requirements");
        let mut runtimes = BTreeMap::new();

        apply_extension_runtime_requirements("fixture-runtime", &runtime, &mut runtimes);

        assert_eq!(runtimes["node"].version, "24");
        assert_eq!(runtimes["node"].source, "extension:fixture-runtime");
        assert_eq!(runtimes["php"].version, "8.3");
        assert_eq!(runtimes["php"].source, "extension:fixture-runtime");
    }

    #[test]
    fn component_env_detector_executes_extension_script() {
        let temp = tempfile::tempdir().expect("tempdir");
        let extension_dir = temp.path().join("extensions/demo");
        let component_dir = temp.path().join("component");
        fs::create_dir_all(extension_dir.join("scripts/env")).expect("extension dirs");
        fs::create_dir_all(&component_dir).expect("component dir");

        let script = extension_dir.join("scripts/env/detect.sh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf '{\"runtimes\":{\"php\":{\"version\":\"8.2\"},\"node\":{\"version\":\"22\"}}}'\n",
        )
        .expect("write detector");
        let mut perms = fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).expect("chmod detector");

        let mut extension: homeboy::core::extension::ExtensionManifest =
            serde_json::from_value(serde_json::json!({
                "name": "Demo",
                "version": "1.0.0",
                "component_env": { "detect_script": "scripts/env/detect.sh" }
            }))
            .expect("extension manifest");
        extension.id = "demo".to_string();
        extension.extension_path = Some(extension_dir.to_string_lossy().to_string());

        let detected = run_component_env_detector(&extension, &component_dir)
            .expect("detector should run")
            .expect("detector output");

        assert_eq!(detected.runtimes["php"].version, "8.2");
        assert_eq!(detected.runtimes["node"].version, "22");
    }

    #[test]
    fn runtime_requirements_accept_canonical_shape_only() {
        let generic: homeboy::core::extension::RuntimeRequirementsConfig =
            serde_json::from_value(serde_json::json!({
                "runtimes": {
                    "python": { "version": "3.12" },
                    "ruby": { "version": "3.3" }
                }
            }))
            .expect("generic requirements");

        assert_eq!(generic.runtimes["python"].version, "3.12");
        assert_eq!(generic.runtimes["ruby"].version, "3.3");
    }

    #[test]
    fn component_env_detector_output_overrides_component_values_before_runtime_defaults() {
        let runtime: homeboy::core::extension::RuntimeRequirementsConfig =
            serde_json::from_value(serde_json::json!({
                "runtimes": {
                    "node": { "version": "24" },
                    "php": { "version": "8.4" }
                }
            }))
            .expect("runtime requirements");
        let mut runtimes = BTreeMap::from([
            (
                "node".to_string(),
                ComponentRuntimeRequirement {
                    version: "20".to_string(),
                    source: "component".to_string(),
                },
            ),
            (
                "php".to_string(),
                ComponentRuntimeRequirement {
                    version: "8.0".to_string(),
                    source: "component".to_string(),
                },
            ),
        ]);

        apply_component_env_detector_output(
            serde_json::from_value(serde_json::json!({
                "runtimes": { "php": { "version": "8.2" } }
            }))
            .expect("detected requirements"),
            &mut runtimes,
        );
        apply_extension_runtime_requirements("demo", &runtime, &mut runtimes);

        assert_eq!(runtimes["php"].version, "8.2");
        assert_eq!(runtimes["php"].source, "component");
        assert_eq!(runtimes["node"].version, "20");
        assert_eq!(runtimes["node"].source, "component");
    }

    #[test]
    fn component_versions_win_over_extension_runtime_requirements() {
        let runtime: homeboy::core::extension::RuntimeRequirementsConfig =
            serde_json::from_value(serde_json::json!({
                "runtimes": {
                    "node": { "version": "24" },
                    "php": { "version": "8.3" }
                }
            }))
            .expect("runtime requirements");
        let mut runtimes = BTreeMap::from([
            (
                "node".to_string(),
                ComponentRuntimeRequirement {
                    version: "22".to_string(),
                    source: "component".to_string(),
                },
            ),
            (
                "php".to_string(),
                ComponentRuntimeRequirement {
                    version: "8.2".to_string(),
                    source: "component".to_string(),
                },
            ),
        ]);

        apply_extension_runtime_requirements("fixture-runtime", &runtime, &mut runtimes);

        assert_eq!(runtimes["node"].version, "22");
        assert_eq!(runtimes["node"].source, "component");
        assert_eq!(runtimes["php"].version, "8.2");
        assert_eq!(runtimes["php"].source, "component");
    }
}
