pub mod baseline;
pub mod report;
pub mod run;

use crate::core::component::Component;
use crate::core::extension::{ExtensionCapability, ExtensionExecutionContext, ExtensionRunner};

pub use baseline::{BaselineComparison, LintBaseline, LintBaselineMetadata};
pub use report::LintCommandOutput;
pub use run::{
    run_main_lint_workflow, run_self_check_lint_workflow, LintRunWorkflowArgs,
    LintRunWorkflowResult,
};

use crate::core::engine::run_dir::RunDir;

pub fn resolve_lint_command(
    component: &Component,
) -> crate::core::error::Result<ExtensionExecutionContext> {
    crate::core::extension::resolve_execution_context(component, ExtensionCapability::Lint)
}

#[allow(clippy::too_many_arguments)]
pub fn build_lint_runner(
    component: &Component,
    path_override: Option<String>,
    settings: &[(String, serde_json::Value)],
    summary: bool,
    file: Option<&str>,
    glob: Option<&str>,
    errors_only: bool,
    sniffs: Option<&str>,
    exclude_sniffs: Option<&str>,
    category: Option<&str>,
    step: Option<&str>,
    run_dir: &RunDir,
) -> crate::core::Result<ExtensionRunner> {
    let resolved = resolve_lint_command(component)?;
    let (string_settings, json_settings) = split_lint_settings(settings);

    Ok(ExtensionRunner::for_context(resolved)
        .component(component.clone())
        .path_override(path_override)
        .settings(&string_settings)
        .settings_json(&json_settings)
        .with_run_dir(run_dir)
        .env_if(summary, "HOMEBOY_SUMMARY_MODE", "1")
        .env_opt("HOMEBOY_LINT_FILE", &file.map(str::to_string))
        .env_opt("HOMEBOY_LINT_GLOB", &glob.map(str::to_string))
        .env_if(errors_only, "HOMEBOY_ERRORS_ONLY", "1")
        .env_opt("HOMEBOY_SNIFFS", &sniffs.map(str::to_string))
        .env_opt("HOMEBOY_STEP", &step.map(str::to_string))
        .env_opt(
            "HOMEBOY_EXCLUDE_SNIFFS",
            &exclude_sniffs.map(str::to_string),
        )
        .env_opt("HOMEBOY_CATEGORY", &category.map(str::to_string)))
}

fn split_lint_settings(
    settings: &[(String, serde_json::Value)],
) -> (Vec<(String, String)>, Vec<(String, serde_json::Value)>) {
    settings
        .iter()
        .cloned()
        .fold((Vec::new(), Vec::new()), |mut split, (key, value)| {
            match value {
                serde_json::Value::String(value) => split.0.push((key, value)),
                value => split.1.push((key, value)),
            }
            split
        })
}

pub(crate) fn settings_from_legacy_strings(
    settings: &[(String, String)],
) -> Vec<(String, serde_json::Value)> {
    settings
        .iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_lint_settings_preserves_typed_json_values() {
        let settings = vec![
            (
                "severity".to_string(),
                serde_json::Value::String("strict".to_string()),
            ),
            ("rules".to_string(), json!({ "security": true })),
        ];

        let (string_settings, json_settings) = split_lint_settings(&settings);

        assert_eq!(
            string_settings,
            vec![("severity".to_string(), "strict".to_string())]
        );
        assert_eq!(
            json_settings,
            vec![("rules".to_string(), json!({ "security": true }))]
        );
    }

    #[test]
    fn legacy_lint_settings_are_explicitly_wrapped_as_strings() {
        let settings = settings_from_legacy_strings(&[("mode".to_string(), "strict".to_string())]);

        assert_eq!(
            settings,
            vec![(
                "mode".to_string(),
                serde_json::Value::String("strict".to_string()),
            ),]
        );
    }
}
