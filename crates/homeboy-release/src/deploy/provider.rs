use std::io::Write;
use std::path::{Path, PathBuf};

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
            .map(|component| run_component(project_id, project, component, config))
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
    project: &Project,
    component: &Component,
    config: &DeployConfig,
) -> Result<ComponentDeployResult> {
    let project_attachment = project
        .components
        .iter()
        .find(|attachment| attachment.id == component.id)
        .expect("component resolution requires an attachment");
    if project_attachment.deployment_provider_input.is_some()
        && project_attachment.deployment_provider.is_some()
    {
        return Err(Error::validation_invalid_argument(
            "components.deployment_provider_input",
            "Project deployment provider policy override cannot be combined with project provider input",
            Some(component.id.clone()),
            None,
        ));
    }

    // With target input, the resolved provider attachment is necessarily the
    // repository-owned policy because a project override was rejected above.
    // Without input, retain the legacy resolved project override behavior.
    let attachment = component
        .deployment_provider
        .as_ref()
        .expect("checked by caller");
    let layered = homeboy_extension::deployment_provider_layered_input(
        &attachment.extension,
        &attachment.provider,
    )?;
    let target_input = project_attachment.deployment_provider_input.as_ref();
    let layered = match layered {
        Some(layered)
            if layered.schema == homeboy_extension::DEPLOYMENT_PROVIDER_PAYLOAD_SCHEMA =>
        {
            Some(layered)
        }
        Some(_) => {
            return Err(Error::validation_invalid_argument(
                "deployment_provider.layered_input",
                "Deployment provider declares an unsupported layered input schema",
                Some(component.id.clone()),
                None,
            ));
        }
        None => None,
    };
    if target_input.is_some() && layered.is_none() {
        return Err(Error::validation_invalid_argument(
            "components.deployment_provider_input",
            "Deployment provider does not support project provider input",
            Some(component.id.clone()),
            None,
        ));
    }
    if layered
        .as_ref()
        .is_some_and(|layered| layered.target_required)
        && target_input.is_none()
    {
        return Err(Error::validation_invalid_argument(
            "components.deployment_provider_input",
            "Deployment provider requires project provider input",
            Some(component.id.clone()),
            None,
        ));
    }

    validate_repository_policy(component, layered.is_some(), attachment)?;

    let is_layered = layered.is_some();
    let layered_result_schema = layered
        .as_ref()
        .and_then(|layered| layered.result_schema.as_deref());
    let dry_run = config.dry_run || config.check;
    let run = if layered.is_some() {
        let payload = layered_payload(
            component,
            attachment.policy.as_ref().expect("validated inline policy"),
            target_input,
        )?;
        homeboy_extension::run_deployment_provider(
            &attachment.extension,
            &attachment.provider,
            project_id,
            &component.id,
            payload.path(),
            dry_run,
        )
    } else {
        let contract = repository_contract(
            component,
            attachment
                .contract
                .as_deref()
                .expect("validated legacy contract"),
        )?;
        homeboy_extension::run_deployment_provider(
            &attachment.extension,
            &attachment.provider,
            project_id,
            &component.id,
            &contract,
            dry_run,
        )
    }?;
    let evidence = run.output.unwrap_or_default();
    let output = format!("{}{}", evidence.stdout, evidence.stderr);
    // Layered input can contain target secrets. Provider output is therefore not
    // promoted into deploy evidence or errors on that path.
    let provider_result = if is_layered && layered_result_schema.is_some() {
        layered_provider_evidence(&evidence.stdout, layered_result_schema.expect("checked"))
    } else if is_layered {
        serde_json::json!({ "status": "opaque" })
    } else {
        serde_json::from_str::<serde_json::Value>(&evidence.stdout)
            .unwrap_or_else(|_| serde_json::json!({ "status": "unstructured", "output": output }))
    };
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
    if is_layered {
        result.local_path = None;
    }
    result.deploy_exit_code = Some(run.exit_code);
    result.error = (run.exit_code != 0).then(|| {
        if is_layered {
            "Deployment provider failed".to_string()
        } else {
            output
        }
    });
    result.deployment_provider = Some(provider_result);
    Ok(result)
}

fn layered_provider_evidence(stdout: &str, expected_schema: &str) -> serde_json::Value {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return serde_json::json!({ "status": "opaque" });
    };
    if value
        .as_object()
        .and_then(|object| object.get("schema"))
        .and_then(serde_json::Value::as_str)
        == Some(expected_schema)
    {
        value
    } else {
        serde_json::json!({ "status": "opaque" })
    }
}

fn layered_payload(
    component: &Component,
    policy: &serde_json::Value,
    target: Option<&serde_json::Value>,
) -> Result<tempfile::NamedTempFile> {
    let policy_bytes = serde_json::to_vec(policy)
        .map_err(|_| Error::internal_io("Could not serialize deployment provider policy", None))?;
    let revision = clean_head_revision(component)?;
    let payload = serde_json::json!({
        "schema": homeboy_extension::DEPLOYMENT_PROVIDER_PAYLOAD_SCHEMA,
        "policy": {
            "value": policy,
            "reference": {
                "component": component.id,
                "path": "homeboy.json#/deployment_provider/policy",
                "digest": homeboy_engine_primitives::content_hash::sha256_hex(&policy_bytes),
            }
        },
        "target": target,
        "source": { "component": component.id, "revision": revision },
    });
    let mut file = tempfile::NamedTempFile::new()
        .map_err(|_| Error::internal_io("Could not prepare deployment provider input", None))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| Error::internal_io("Could not secure deployment provider input", None))?;
    }
    serde_json::to_writer(&mut file, &payload)
        .map_err(|_| Error::internal_io("Could not prepare deployment provider input", None))?;
    file.flush()
        .map_err(|_| Error::internal_io("Could not prepare deployment provider input", None))?;
    Ok(file)
}

fn provider_policy_error(component: &Component, message: &str) -> Error {
    Error::validation_invalid_argument(
        "deployment_provider",
        message,
        Some(component.id.clone()),
        None,
    )
}

fn validate_repository_policy(
    component: &Component,
    layered: bool,
    attachment: &homeboy_core::component::DeploymentProviderAttachment,
) -> Result<()> {
    match (layered, &attachment.contract, &attachment.policy) {
        (true, None, Some(_)) | (false, Some(_), None) => Ok(()),
        (true, Some(_), _) => Err(provider_policy_error(
            component,
            "Layered deployment provider must use inline repository policy without a legacy contract",
        )),
        (true, None, None) => Err(provider_policy_error(
            component,
            "Layered deployment provider requires inline repository policy",
        )),
        (false, None, Some(_)) => Err(provider_policy_error(
            component,
            "Unlayered deployment provider does not support inline repository policy",
        )),
        (false, Some(_), Some(_)) => Err(provider_policy_error(
            component,
            "Deployment provider must declare exactly one repository policy source",
        )),
        (false, None, None) => Err(provider_policy_error(
            component,
            "Unlayered deployment provider requires a legacy contract",
        )),
    }
}

fn clean_head_revision(component: &Component) -> Result<String> {
    let root = Path::new(&component.local_path);
    let revision = homeboy_core::git::head_sha(root).ok_or_else(|| {
        Error::validation_invalid_argument(
            "deployment_provider.source",
            "Deployment provider source must be a checked-out Git revision",
            Some(component.id.clone()),
            None,
        )
    })?;
    if homeboy_core::git::status_porcelain(root).as_deref() != Some("") {
        return Err(Error::validation_invalid_argument(
            "deployment_provider.source",
            "Deployment provider source checkout must be clean",
            Some(component.id.clone()),
            None,
        ));
    }
    Ok(revision)
}

fn repository_contract(component: &Component, contract: &str) -> Result<PathBuf> {
    let root = std::fs::canonicalize(&component.local_path).map_err(|_| {
        Error::validation_invalid_argument(
            "deployment_provider.contract",
            "Component source is unavailable",
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
    use super::{
        layered_payload, layered_provider_evidence, repository_contract, validate_repository_policy,
    };
    use homeboy_core::component::{Component, DeploymentProviderAttachment};
    use homeboy_core::project::{Project, ProjectComponentAttachment};
    use std::process::Command;

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

    #[test]
    fn repository_policy_source_is_unambiguous_by_provider_kind() {
        let component = Component::new("fixture".to_string(), String::new(), String::new(), None);
        let attachment = |contract: Option<&str>, policy: Option<serde_json::Value>| {
            DeploymentProviderAttachment {
                extension: "fixture-extension".to_string(),
                provider: "fixture.deploy".to_string(),
                contract: contract.map(str::to_string),
                policy,
            }
        };

        assert!(validate_repository_policy(
            &component,
            true,
            &attachment(None, Some(serde_json::json!({})))
        )
        .is_ok());
        assert!(validate_repository_policy(
            &component,
            false,
            &attachment(Some("legacy.json"), None)
        )
        .is_ok());
        assert!(validate_repository_policy(&component, true, &attachment(None, None)).is_err());
        assert!(validate_repository_policy(
            &component,
            true,
            &attachment(Some("legacy.json"), Some(serde_json::json!({})))
        )
        .is_err());
        assert!(validate_repository_policy(
            &component,
            false,
            &attachment(None, Some(serde_json::json!({})))
        )
        .is_err());
    }

    #[test]
    fn layered_evidence_requires_the_declared_object_schema() {
        let accepted = layered_provider_evidence(
            r#"{"schema":"fixture/result/v1","status":"ok"}"#,
            "fixture/result/v1",
        );
        assert_eq!(accepted["status"], "ok");

        for rejected in [
            r#"{"schema":"fixture/result/v2","target":"private-target","path":"/private/payload"}"#,
            r#"not json /private/payload private-target"#,
            r#"["fixture/result/v1"]"#,
        ] {
            let evidence = layered_provider_evidence(rejected, "fixture/result/v1");
            assert_eq!(evidence, serde_json::json!({ "status": "opaque" }));
            assert!(!evidence.to_string().contains("private-target"));
            assert!(!evidence.to_string().contains("/private/payload"));
        }
    }

    #[test]
    fn project_targets_are_distinct_from_shared_inline_policy() {
        let component: Component = serde_json::from_value(serde_json::json!({
            "id": "fixture",
            "deployment_provider": {
                "extension": "fixture-extension",
                "provider": "fixture.deploy",
                "policy": { "repository": "shared" }
            }
        }))
        .expect("portable component");
        let project = |id: &str, target: serde_json::Value| Project {
            id: id.to_string(),
            components: vec![ProjectComponentAttachment {
                id: "fixture".to_string(),
                local_path: "/source/fixture".to_string(),
                deployment_provider_input: Some(target),
                ..Default::default()
            }],
            ..Default::default()
        };
        let first = project("first", serde_json::json!({ "target": "one" }));
        let second = project("second", serde_json::json!({ "target": "two" }));

        assert_eq!(
            component
                .deployment_provider
                .as_ref()
                .expect("provider")
                .policy,
            Some(serde_json::json!({ "repository": "shared" }))
        );
        assert!(serde_json::to_value(&component)
            .expect("portable serialization")
            .get("deployment_provider_input")
            .is_none());
        assert_ne!(
            first.components[0].deployment_provider_input,
            second.components[0].deployment_provider_input
        );
    }

    fn git(path: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(output.status.success(), "git {:?} failed", args);
    }

    #[test]
    fn layered_payload_is_namespaced_private_and_removed() {
        let repository = tempfile::tempdir().expect("repository");
        std::fs::write(repository.path().join("README.md"), "fixture\n").expect("source");
        git(repository.path(), &["init", "--quiet"]);
        git(repository.path(), &["add", "."]);
        git(
            repository.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );
        let component = Component::new(
            "fixture".to_string(),
            repository.path().display().to_string(),
            String::new(),
            None,
        );
        let payload = layered_payload(
            &component,
            &serde_json::json!({ "policy": "repository" }),
            Some(&serde_json::json!({ "target": "one" })),
        )
        .expect("payload");
        let path = payload.path().to_path_buf();
        assert!(!path.starts_with(repository.path()));
        let value: serde_json::Value =
            serde_json::from_reader(payload.reopen().expect("reopen")).expect("payload json");
        assert_eq!(value["schema"], "homeboy/deployment-provider-payload/v1");
        assert_eq!(
            value["policy"]["value"],
            serde_json::json!({ "policy": "repository" })
        );
        assert_eq!(value["policy"]["reference"]["component"], "fixture");
        assert_eq!(
            value["policy"]["reference"]["path"],
            "homeboy.json#/deployment_provider/policy"
        );
        assert_eq!(
            value["policy"]["reference"]["digest"],
            homeboy_engine_primitives::content_hash::sha256_hex(
                &serde_json::to_vec(&serde_json::json!({ "policy": "repository" }))
                    .expect("policy bytes")
            )
        );
        assert_eq!(value["target"], serde_json::json!({ "target": "one" }));
        assert_eq!(value["source"]["revision"].as_str().map(str::len), Some(40));
        let second = layered_payload(
            &component,
            &serde_json::json!({ "policy": "repository" }),
            Some(&serde_json::json!({ "target": "two" })),
        )
        .expect("second payload");
        let second_value: serde_json::Value =
            serde_json::from_reader(second.reopen().expect("reopen")).expect("payload json");
        assert_eq!(second_value["policy"], value["policy"]);
        assert_ne!(second_value["target"], value["target"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(payload);
        assert!(
            !path.exists(),
            "payload must be removed after provider execution"
        );
    }
}
