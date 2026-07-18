use crate::component::Component;
use crate::engine::run_dir::RunDir;
use crate::error::Result;
use std::path::{Path, PathBuf};

use super::runner::ExtensionRunner;
use crate::extension_execution::{path_list_env_value, ExtensionExecutionContext};

pub struct ScenarioRunnerOptions<'a> {
    pub execution_context: &'a ExtensionExecutionContext,
    pub component: &'a Component,
    pub path_override: Option<String>,
    pub settings: &'a [(String, String)],
    pub settings_json: &'a [(String, serde_json::Value)],
    pub run_dir: &'a RunDir,
    pub results_env: Option<(&'a str, PathBuf)>,
    pub scenario_env: Option<(&'a str, &'a str)>,
    pub artifact_env: Option<(&'a str, &'a Path)>,
    pub list_only_env: Option<(&'a str, bool)>,
    pub extra_workloads_env: Option<(&'a str, &'a [PathBuf], &'a str)>,
    pub env_provider_extensions: &'a [String],
    pub invocation_requirements: crate::engine::invocation::InvocationRequirements,
}

pub fn build_scenario_runner(options: ScenarioRunnerOptions<'_>) -> Result<ExtensionRunner> {
    let mut runner = ExtensionRunner::for_context(options.execution_context.clone())
        .component(options.component.clone())
        .path_override(options.path_override)
        .settings(options.settings)
        .settings_json(options.settings_json)
        .with_run_dir(options.run_dir)
        .env_provider_extensions(options.env_provider_extensions)
        .invocation_requirements(options.invocation_requirements);

    if let Some((key, path)) = options.results_env {
        runner = runner.env(key, &path.to_string_lossy());
    }
    if let Some((key, value)) = options.scenario_env {
        runner = runner.env(key, value);
    }
    if let Some((key, path)) = options.artifact_env {
        runner = runner.env(key, &path.to_string_lossy());
    }
    if let Some((key, list_only)) = options.list_only_env {
        runner = runner.env(key, if list_only { "1" } else { "0" });
    }
    if let Some((key, paths, error_field)) = options.extra_workloads_env {
        if !paths.is_empty() {
            runner = runner.env(key, &path_list_env_value(error_field, paths)?);
        }
    }

    Ok(runner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Component, ScopedExtensionConfig};
    use crate::extension_execution::resolve_execution_context_for_project;
    use crate::project::Project;
    use homeboy_extension_contract::ExtensionCapability;
    use std::path::Path;

    fn write_extension_manifest(home: &Path, extension_id: &str, capability: &str) {
        let extension_dir = home.join(".config/homeboy/extensions").join(extension_id);
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join(format!("{extension_id}.json")),
            format!(
                r#"{{"name":"{extension_id}","version":"1.0.0","{capability}":{{"extension_script":"{capability}.sh"}}}}"#
            ),
        )
        .expect("extension manifest");
    }

    fn component_with_extensions(extension_ids: &[&str]) -> Component {
        let extensions = extension_ids
            .iter()
            .map(|extension_id| {
                (
                    (*extension_id).to_string(),
                    ScopedExtensionConfig::default(),
                )
            })
            .collect();

        Component {
            id: "consumer".to_string(),
            extensions: Some(extensions),
            ..Default::default()
        }
    }

    #[test]
    fn project_capability_context_applies_project_component_and_cli_settings() {
        crate::test_support::with_isolated_home(|home| {
            let component_dir = tempfile::tempdir().expect("component");
            std::fs::write(
                component_dir.path().join("homeboy.json"),
                r#"{"id":"consumer","extensions":{"fixture":{"settings":{"winner":"component"}}}}"#,
            )
            .expect("portable component");
            write_extension_manifest(home.path(), "fixture", "test");
            let extension_dir = home.path().join(".config/homeboy/extensions/fixture");
            let script = extension_dir.join("test.sh");
            std::fs::write(
                &script,
                "#!/bin/sh\nprintf '%s' \"$HOMEBOY_SETTINGS_JSON\"\n",
            )
            .expect("test script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&script)
                    .expect("script metadata")
                    .permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&script, permissions).expect("make script executable");
            }
            let project = crate::project::Project {
                id: "site".to_string(),
                extensions: Some(std::collections::HashMap::from([(
                    "fixture".to_string(),
                    crate::component::ScopedExtensionConfig {
                        settings: std::collections::HashMap::from([(
                            "winner".to_string(),
                            serde_json::json!("project"),
                        )]),
                        ..Default::default()
                    },
                )])),
                components: vec![crate::project::ProjectComponentAttachment {
                    id: "consumer".to_string(),
                    local_path: component_dir.path().to_string_lossy().to_string(),
                    remote_path: None,
                }],
                ..Default::default()
            };

            let context = resolve_execution_context_for_project(
                &project,
                "consumer",
                ExtensionCapability::Test,
            )
            .expect("project capability context");
            let output = ExtensionRunner::for_context(context)
                .settings(&[("winner".to_string(), "cli".to_string())])
                .passthrough(false)
                .run()
                .expect("capability run");
            let settings: serde_json::Value =
                serde_json::from_str(&output.stdout).expect("settings JSON");

            assert_eq!(settings["winner"], "cli");
        });
    }
}
