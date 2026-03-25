mod build;
mod build_exec_env;
mod capability;
mod execute;
mod extension;
mod helpers;
mod resolve;
mod types;

pub use build::*;
pub use build_exec_env::*;
pub use capability::*;
pub use execute::*;
pub use extension::*;
pub use helpers::*;
pub use resolve::*;
pub use types::*;

use crate::component::{self, Component};
use crate::engine::command::CapturedOutput;
use crate::engine::local_files;
use crate::engine::shell;
use crate::engine::{template, validation};
use crate::error::{Error, Result};
use crate::project::{self, Project};
use crate::server::http::ApiClient;
use crate::server::{
    execute_local_command_in_dir, execute_local_command_interactive,
    execute_local_command_passthrough, CommandOutput,
};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

use super::exec_context;
use super::load_extension;
use super::manifest::{ActionConfig, ActionType, ExtensionManifest, HttpMethod, RuntimeConfig};
use super::runner_contract::RunnerStepFilter;
use super::runtime_helper;
use super::scope::ExtensionScope;

fn build_template_vars<'a>(
    extension_path: &'a str,
    args_str: &'a str,
    runtime: &'a RuntimeConfig,
    project: Option<&'a Project>,
    project_id: &'a Option<String>,
) -> Vec<(&'a str, &'a str)> {
    let entrypoint = runtime.entrypoint.as_deref().unwrap_or("");

    if let Some(proj) = project {
        let domain = proj.domain.as_deref().unwrap_or("");
        let site_path = proj.base_path.as_deref().unwrap_or("");
        vec![
            ("extension_path", extension_path),
            ("entrypoint", entrypoint),
            ("args", args_str),
            ("projectId", project_id.as_deref().unwrap_or("")),
            ("domain", domain),
            ("sitePath", site_path),
        ]
    } else {
        vec![
            ("extension_path", extension_path),
            ("entrypoint", entrypoint),
            ("args", args_str),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_exec_env_includes_runtime_runner_helper_path() {
        let env = build_exec_env("rust", None, None, "{}", Some("/tmp/ext"), None, None, None);

        let helper = env
            .iter()
            .find(|(k, _)| k == runtime_helper::RUNNER_STEPS_ENV)
            .map(|(_, v)| v.clone());

        assert!(helper.is_some());
        assert!(helper.unwrap().ends_with("runner-steps.sh"));
    }

    #[test]
    fn build_settings_json_preserves_array_values() {
        // Regression test for #844: array values in extension settings
        // were serialized as empty strings.
        let manifest = serde_json::json!({
            "settings": [
                { "id": "string_setting", "default": "hello" },
                { "id": "array_default", "default": ["a", "b"] }
            ]
        });

        let extension_settings: Vec<(String, serde_json::Value)> = vec![
            (
                "validation_dependencies".to_string(),
                serde_json::json!(["data-machine"]),
            ),
            (
                "plain_string".to_string(),
                serde_json::Value::String("value".to_string()),
            ),
        ];

        let overrides: Vec<(String, String)> = vec![];

        let json = build_settings_json_from_manifest(&manifest, &extension_settings, &overrides)
            .expect("should serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("should parse");

        // Array from extension settings is preserved
        assert_eq!(
            parsed["validation_dependencies"],
            serde_json::json!(["data-machine"]),
            "Array setting should be preserved, not flattened to empty string"
        );

        // String from extension settings is preserved
        assert_eq!(parsed["plain_string"], serde_json::json!("value"));

        // String default from manifest is preserved
        assert_eq!(parsed["string_setting"], serde_json::json!("hello"));

        // Array default from manifest is preserved
        assert_eq!(parsed["array_default"], serde_json::json!(["a", "b"]));
    }

    #[test]
    fn build_settings_json_cli_overrides_replace_values() {
        let manifest = serde_json::json!({});
        let extension_settings: Vec<(String, serde_json::Value)> =
            vec![("key".to_string(), serde_json::json!(["original"]))];
        let overrides = vec![("key".to_string(), "override_value".to_string())];

        let json = build_settings_json_from_manifest(&manifest, &extension_settings, &overrides)
            .expect("should serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("should parse");

        // CLI override replaces the array value with a string
        assert_eq!(parsed["key"], serde_json::json!("override_value"));
    }

    #[test]
    fn build_exec_env_preserves_step_filter_contract() {
        let filter = RunnerStepFilter {
            step: Some("lint,test".to_string()),
            skip: Some("lint".to_string()),
        };

        let mut env = build_exec_env("rust", None, None, "{}", Some("/tmp/ext"), None, None, None);
        env.extend(filter.to_env_pairs());

        assert!(env
            .iter()
            .any(|(k, v)| k == "HOMEBOY_STEP" && v == "lint,test"));
        assert!(env.iter().any(|(k, v)| k == "HOMEBOY_SKIP" && v == "lint"));
    }
}
