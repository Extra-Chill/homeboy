use std::path::PathBuf;

use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};
use homeboy_core::project::Project;

use super::types::{ComponentDeployResult, DeployConfig, DeployOrchestrationResult, DeploySummary};

pub(super) fn run_if_configured(
    project_id: &str,
    project: &Project,
    config: &DeployConfig,
) -> Result<Option<DeployOrchestrationResult>> {
    if config.component_ids.is_empty() {
        return Ok(None);
    }
    let components = config
        .component_ids
        .iter()
        .map(|id| homeboy_core::project::resolve_project_component(project, id))
        .collect::<Result<Vec<_>>>()?;
    if components
        .iter()
        .all(|component| component.deployment_provider.is_some())
    {
        let results = components
            .iter()
            .map(|component| run_component(project_id, component, config))
            .collect::<Result<Vec<_>>>()?;
        let failed = results
            .iter()
            .filter(|result| result.status == "failed")
            .count() as u32;
        let total = results.len() as u32;
        return Ok(Some(DeployOrchestrationResult {
            results,
            summary: DeploySummary {
                total,
                succeeded: total - failed,
                failed,
                skipped: 0,
            },
            deploy_run_id: None,
        }));
    }
    Ok(None)
}

fn run_component(
    project_id: &str,
    component: &Component,
    config: &DeployConfig,
) -> Result<ComponentDeployResult> {
    let attachment = component
        .deployment_provider
        .as_ref()
        .expect("checked by caller");
    let contract = repository_contract(component, &attachment.contract)?;
    let run = homeboy_extension::run_deployment_provider(
        &attachment.extension,
        &attachment.provider,
        project_id,
        &component.id,
        &contract,
        config.dry_run || config.check,
    )?;
    let evidence = run.output.unwrap_or_default();
    let output = format!("{}{}", evidence.stdout, evidence.stderr);
    let provider_result = serde_json::from_str::<serde_json::Value>(&evidence.stdout)
        .unwrap_or_else(|_| serde_json::json!({ "status": "unstructured", "output": output }));
    let status = if run.exit_code == 0 {
        if config.dry_run || config.check {
            "validated"
        } else {
            "deployed"
        }
    } else {
        "failed"
    };
    let mut result = ComponentDeployResult::new(component, "").with_status(status);
    result.deploy_exit_code = Some(run.exit_code);
    result.error = (run.exit_code != 0).then(|| output);
    result.deployment_provider = Some(provider_result);
    Ok(result)
}

fn repository_contract(component: &Component, contract: &str) -> Result<PathBuf> {
    let root = std::fs::canonicalize(&component.local_path).map_err(|error| {
        Error::validation_invalid_argument(
            "deployment_provider.contract",
            format!("Component source is unavailable: {error}"),
            None,
            None,
        )
    })?;
    let path = root.join(contract);
    let path = std::fs::canonicalize(&path).map_err(|error| {
        Error::validation_invalid_argument(
            "deployment_provider.contract",
            format!("Contract '{contract}' is unavailable: {error}"),
            None,
            None,
        )
    })?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(Error::validation_invalid_argument(
            "deployment_provider.contract",
            "Contract must be a repository-contained file",
            None,
            None,
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::repository_contract;
    use homeboy_core::component::Component;

    #[test]
    fn requires_a_repository_contained_contract_file() {
        let repository = tempfile::tempdir().expect("repository");
        let contract = repository.path().join("deploy-contract.json");
        std::fs::write(&contract, "{}").expect("contract");
        let component = Component::new(
            "fixture".to_string(),
            repository.path().display().to_string(),
            String::new(),
            None,
        );

        assert_eq!(
            repository_contract(&component, "deploy-contract.json").expect("contained contract"),
            std::fs::canonicalize(&contract).expect("canonical contract")
        );
        assert!(repository_contract(&component, "../deploy-contract.json").is_err());
    }
}
