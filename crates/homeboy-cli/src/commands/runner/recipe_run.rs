use std::path::{Component, Path};

use homeboy::core::Error;
use homeboy::runner::runners::RunnerExecOutput;
use homeboy_extension_contract::api::v1::{
    ExtensionApiRecipeRunPlanRequest, ExtensionApiRecipeRunProviderInventoryEntry,
    ExtensionApiRecipeRunProviderInventoryRequest, EXTENSION_API_RECIPE_RUN_PLAN_REQUEST_SCHEMA,
    EXTENSION_API_RECIPE_RUN_PROVIDER_INVENTORY_REQUEST_SCHEMA, EXTENSION_API_V1,
};

use super::super::CmdResult;
use super::exec::exec_with_hydration;

pub(super) fn recipe_run(
    runner_id: &str,
    provider_id: &str,
    sync_workspace: String,
    recipe: String,
    artifacts: String,
    run_id: String,
) -> CmdResult<RunnerExecOutput> {
    validate_workspace_relative_path("recipe", &recipe)?;
    validate_workspace_relative_path("artifacts", &artifacts)?;
    if run_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "runner recipe-run requires a non-empty durable --run-id",
            Some(run_id),
            None,
        ));
    }
    let response = homeboy_core::extension::recipe_run_api::recipe_run_plan_api(
        &ExtensionApiRecipeRunPlanRequest {
            schema: EXTENSION_API_RECIPE_RUN_PLAN_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            provider_id: provider_id.to_string(),
            recipe_path: recipe,
            artifact_path: artifacts.clone(),
        },
    );
    let plan = match response.plan {
        Some(plan) => plan,
        None => {
            let message = response
                .selection_failure
                .map(|failure| failure.message)
                .or_else(|| response.failure.map(|failure| failure.message))
                .unwrap_or_else(|| "Recipe-run planning returned no plan".to_string());
            return Err(recipe_run_operation_error(
                provider_id,
                message,
                &response.available_provider_ids,
            ));
        }
    };
    let command = plan.command;
    let (output, exit_code) = exec_with_hydration(
        runner_id,
        None,
        Some(sync_workspace),
        None,
        false,
        None,
        false,
        false,
        Vec::new(),
        None,
        Vec::new(),
        Vec::new(),
        None,
        None,
        false,
        Some(run_id.clone()),
        Vec::new(),
        vec![artifacts],
        Vec::new(),
        false,
        false,
        command.clone(),
        Vec::new(),
    )?;
    let terminal_json = serde_json::from_str(&output.stdout).unwrap_or_else(|_| {
        serde_json::json!({ "exit_code": output.exit_code, "stdout": output.stdout, "stderr": output.stderr })
    });
    let source_snapshot =
        serde_json::to_value(&output.source_snapshot).expect("runner source snapshot serializes");
    homeboy_agents::agent_task_lifecycle::record_runner_exec_provider_result(
        &run_id,
        &plan.provider_id,
        &plan.provider_version,
        &command,
        &source_snapshot,
        &terminal_json,
    )?;
    Ok((output, exit_code))
}

pub(super) fn recipe_run_provider_inventory(
) -> homeboy::core::Result<Vec<ExtensionApiRecipeRunProviderInventoryEntry>> {
    let response = homeboy_core::extension::recipe_run_api::recipe_run_provider_inventory_api(
        &ExtensionApiRecipeRunProviderInventoryRequest {
            schema: EXTENSION_API_RECIPE_RUN_PROVIDER_INVENTORY_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
        },
    );
    match response.failure {
        Some(failure) => Err(recipe_run_operation_error(
            "inventory",
            failure.message,
            &[],
        )),
        None => Ok(response.providers),
    }
}

fn recipe_run_operation_error(
    provider_id: &str,
    message: String,
    available_provider_ids: &[String],
) -> Error {
    let mut error = Error::validation_invalid_argument(
        "provider",
        message,
        Some(provider_id.to_string()),
        None,
    )
    .with_hint("Run 'homeboy runner recipe-providers' to inspect installed providers.");
    if !available_provider_ids.is_empty() {
        error = error.with_hint(format!(
            "Available provider IDs: {}",
            available_provider_ids.join(", ")
        ));
    }
    error
}

fn validate_workspace_relative_path(argument: &str, value: &str) -> homeboy::core::Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Error::validation_invalid_argument(argument, format!("runner recipe-run --{argument} must be a non-empty workspace-relative path without '..'"), Some(value.to_string()), None));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::extension::registry::ExtensionLifecycleValidation;
    fn install_fixture_extension(
        id: &str,
        provider_id: &str,
        command: Vec<String>,
    ) -> tempfile::TempDir {
        let source = tempfile::tempdir().expect("extension source");
        std::fs::write(
            source.path().join(format!("{id}.json")),
            serde_json::json!({
                "name": "Recipe fixture",
                "version": "1.0.0",
                "recipe_run_providers": [{
                    "id": provider_id,
                    "version": "1.0.0",
                    "executable": "sh",
                    "command": command,
                }],
            })
            .to_string(),
        )
        .expect("manifest");
        homeboy_core::extension::lifecycle::install(
            &source.path().display().to_string(),
            Some(id),
            ExtensionLifecycleValidation::declaration_only(),
        )
        .expect("install extension");
        // Installed extensions are linked to their source. Keep the fixture
        // alive until discovery and execution complete.
        source
    }

    #[test]
    fn rejects_absolute_and_escaping_paths() {
        assert!(validate_workspace_relative_path("recipe", "/recipe.json").is_err());
        assert!(validate_workspace_relative_path("artifacts", "../artifacts").is_err());
        assert!(validate_workspace_relative_path("recipe", "recipes/run.json").is_ok());
    }

    #[test]
    fn rejects_unknown_provider_before_workspace_sync() {
        let error = recipe_run(
            "missing-runner",
            "fixture.missing",
            "/missing".to_string(),
            "recipe.json".to_string(),
            "artifacts".to_string(),
            "missing-provider-run".to_string(),
        )
        .expect_err("provider must resolve before dispatch");
        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(
            error.message.contains("No installed extension declares"),
            "{}",
            error.message
        );
    }

    #[test]
    fn materializes_once_promotes_artifacts_and_records_provider_result() {
        homeboy::test_support::with_isolated_home(|_| {
            let runner_root = tempfile::tempdir().expect("runner root");
            let workspace = tempfile::tempdir().expect("workspace");
            std::fs::write(workspace.path().join("recipe.json"), "{}").expect("recipe");
            homeboy::runner::runners::create(
                &format!(
                    r#"{{"id":"recipe-local","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("runner");
            let _extension = install_fixture_extension(
                "recipe-fixture",
                "fixture.recipe-run",
                vec!["sh".to_string(), "-c".to_string(), "mkdir -p {artifacts}; printf '{\"recipe\":\"{recipe}\"}' | tee {artifacts}/result.json".to_string()],
            );

            let (output, code) = recipe_run(
                "recipe-local",
                "fixture.recipe-run",
                workspace.path().display().to_string(),
                "recipe.json".to_string(),
                "artifacts".to_string(),
                "recipe-provider-run".to_string(),
            )
            .expect("recipe run");

            assert_eq!(code, 0, "{}", output.stderr);
            assert_eq!(output.promoted_outputs.len(), 1);
            let store =
                homeboy::core::observation::ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run("recipe-provider-run")
                .expect("read run")
                .expect("run");
            assert_eq!(
                run.metadata_json["execution_provider"]["id"],
                "fixture.recipe-run"
            );
            assert_eq!(
                run.metadata_json["terminal_json_result"]["recipe"],
                "recipe.json"
            );
            assert!(run.metadata_json["source_snapshot"].is_object());
        });
    }

    #[test]
    fn provider_failure_retains_terminal_result_and_artifacts() {
        homeboy::test_support::with_isolated_home(|_| {
            let runner_root = tempfile::tempdir().expect("runner root");
            let workspace = tempfile::tempdir().expect("workspace");
            std::fs::write(workspace.path().join("recipe.json"), "{}").expect("recipe");
            homeboy::runner::runners::create(
                &format!(
                    r#"{{"id":"failure-local","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("runner");
            let _extension = install_fixture_extension(
                "recipe-failure-fixture",
                "fixture.recipe-run-failure",
                vec!["sh".to_string(), "-c".to_string(), "mkdir -p {artifacts}; printf '{\"status\":\"failed\"}' | tee {artifacts}/result.json; exit 7".to_string()],
            );

            let (output, code) = recipe_run(
                "failure-local",
                "fixture.recipe-run-failure",
                workspace.path().display().to_string(),
                "recipe.json".to_string(),
                "artifacts".to_string(),
                "recipe-provider-failure".to_string(),
            )
            .expect("provider failure is a terminal execution result");

            assert_eq!(code, 7);
            assert_eq!(output.promoted_outputs.len(), 1);
            let store =
                homeboy::core::observation::ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run("recipe-provider-failure")
                .expect("read run")
                .expect("run");
            assert_eq!(
                run.metadata_json["terminal_json_result"]["status"],
                "failed"
            );
        });
    }

    #[test]
    fn generic_substrate_contains_no_product_literals() {
        let source = include_str!("recipe_run.rs");
        let cms = concat!("Word", "Press");
        let sandbox = concat!("WP ", "Codebox");
        assert!(!source.contains(cms));
        assert!(!source.contains(sandbox));
    }
}
