use std::io::Write;
use std::path::{Path, PathBuf};

use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};
use homeboy_core::project::Project;

use super::lifecycle::DeployObservation;
use super::route::DeployTarget;
use super::types::{ComponentDeployResult, DeployConfig, DeployOrchestrationResult, DeploySummary};

pub(super) struct PreparedProviderDeployment {
    components: Vec<PreparedProviderComponent>,
    unresolvable: Vec<UnresolvableComponent>,
}

struct PreparedProviderComponent {
    project_id: String,
    component: Component,
    extension: String,
    provider: String,
    input: PreparedProviderInput,
    result_schema: Option<String>,
    explicit_route: bool,
    dry_run: bool,
}

enum PreparedProviderInput {
    Layered(tempfile::NamedTempFile),
    Repository(PathBuf),
}

#[cfg(test)]
pub(super) fn run_if_configured(
    project_id: &str,
    project: &Project,
    config: &DeployConfig,
    observation: Option<&mut DeployObservation>,
) -> Result<Option<DeployOrchestrationResult>> {
    prepare_if_configured(project_id, project, config)?
        .map(|prepared| apply_prepared(prepared, observation))
        .transpose()
}

pub(super) fn prepare_if_configured(
    project_id: &str,
    project: &Project,
    config: &DeployConfig,
) -> Result<Option<PreparedProviderDeployment>> {
    let component_ids = if config.component_ids.is_empty() && config.check {
        project
            .components
            .iter()
            .map(|component| component.id.as_str())
            .collect::<Vec<_>>()
    } else {
        config
            .component_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    };
    if component_ids.is_empty() {
        return Ok(None);
    }
    // Deciding whether this project is provider-owned requires reading each
    // component's declared configuration from its checkout. In check mode a
    // component whose checkout is absent must not abort that decision for the
    // whole project (#12214): set it aside as a scoped finding, decide ownership
    // from the components this host can actually resolve, and report the rest.
    let (component_ids, unresolvable): (Vec<&str>, Vec<UnresolvableComponent>) = if config.check {
        partition_unresolvable_components(project, &component_ids, config)
    } else {
        (component_ids, Vec::new())
    };
    if component_ids.is_empty() {
        return Ok(None);
    }
    let components = component_ids
        .iter()
        .map(|id| {
            super::planning::resolve_project_component(
                project,
                id,
                None,
                config.prepared_projection.as_ref(),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let provider_count = component_ids
        .iter()
        .filter(|id| super::route::resolve(project, id, config) == DeployTarget::Provider)
        .count();
    if config.check && provider_count > 0 && provider_count < components.len() {
        return Err(Error::validation_invalid_argument(
            "component_ids",
            "Project-wide check spans provider-owned and server-deployed components",
            Some(project_id.to_string()),
            Some(vec!["Run component-scoped checks so each component uses its declared deployment lifecycle".to_string()]),
        ));
    }
    if provider_count == components.len() {
        let components = components
            .iter()
            .map(|component| prepare_component(project_id, project, component, config))
            .collect::<Result<Vec<_>>>()?;
        return Ok(Some(PreparedProviderDeployment {
            components,
            unresolvable,
        }));
    }
    Ok(None)
}

/// A component this host cannot resolve, with the operator-facing reason.
struct UnresolvableComponent {
    id: String,
    reason: String,
}

/// Split requested components into those this host can resolve and those whose
/// local checkout is absent.
///
/// A projected source stands in for the on-disk checkout, so a component the
/// caller already materialized is always resolvable.
fn partition_unresolvable_components<'a>(
    project: &Project,
    component_ids: &[&'a str],
    config: &DeployConfig,
) -> (Vec<&'a str>, Vec<UnresolvableComponent>) {
    let mut resolvable = Vec::with_capacity(component_ids.len());
    let mut unresolvable = Vec::new();

    for id in component_ids {
        if super::planning::projected_component(project, id, config.prepared_projection.as_ref())
            .is_some()
        {
            resolvable.push(*id);
            continue;
        }

        let findings = homeboy_core::project::component_local_path_findings(project, id);
        if findings.is_empty() {
            resolvable.push(*id);
        } else {
            unresolvable.push(UnresolvableComponent {
                id: (*id).to_string(),
                reason: findings.join("; "),
            });
        }
    }

    (resolvable, unresolvable)
}

/// Build `status: "skipped"` rows so a project-wide check reports the components
/// it could not resolve alongside the ones it did.
fn unresolvable_results(unresolvable: &[UnresolvableComponent]) -> Vec<ComponentDeployResult> {
    unresolvable
        .iter()
        .map(|skip| {
            let component = Component {
                id: skip.id.clone(),
                ..Default::default()
            };
            let mut result = ComponentDeployResult::new(&component, "").with_status("skipped");
            result.local_path = None;
            result.warnings.push(format!("skipped: {}", skip.reason));
            result
        })
        .collect()
}

fn prepare_component(
    project_id: &str,
    project: &Project,
    component: &Component,
    config: &DeployConfig,
) -> Result<PreparedProviderComponent> {
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

    let attachment = component.deployment_provider.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "components.deployment_provider_input",
            "Selected deployment provider has no component provider policy",
            Some(component.id.clone()),
            (config.target == Some(DeployTarget::Provider)).then(|| {
                vec![
                    "--target provider requires the component to declare deployment_provider in its homeboy.json".to_string(),
                    "Deploy the server deliverable instead: --target server".to_string(),
                ]
            }),
        )
    })?;
    let layered = homeboy_core::extension::deployment_provider_layered_input(
        &attachment.extension,
        &attachment.provider,
    )?;
    let target_input = project_attachment.deployment_provider_input.as_ref();
    let layered = match layered {
        Some(layered)
            if layered.schema == homeboy_core::extension::DEPLOYMENT_PROVIDER_PAYLOAD_SCHEMA =>
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
        // A dual-deliverable component still has a deployable server artifact
        // here, so the remedy is two-sided: configure the provider target, or
        // say which deliverable this deploy meant (#12853).
        let mut tried = vec![format!(
            "Select this provider target by setting components[].deployment_provider_input for '{}' on project '{}'",
            component.id, project_id
        )];
        if !component
            .build_artifact
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        {
            tried.push(
                "Deploy this component's server artifact instead: homeboy deploy --component <id> --target server"
                    .to_string(),
            );
        }
        return Err(Error::validation_invalid_argument(
            "components.deployment_provider_input",
            "Deployment provider requires project provider input",
            Some(component.id.clone()),
            Some(tried),
        ));
    }

    validate_repository_policy(component, layered.is_some(), attachment)?;

    let result_schema = layered
        .as_ref()
        .and_then(|layered| layered.result_schema.clone());
    let dry_run = config.dry_run || config.check;
    let input = if layered.is_some() {
        PreparedProviderInput::Layered(layered_payload(
            component,
            attachment.policy.as_ref().expect("validated inline policy"),
            target_input,
        )?)
    } else {
        PreparedProviderInput::Repository(repository_contract(
            component,
            attachment
                .contract
                .as_deref()
                .expect("validated legacy contract"),
        )?)
    };
    Ok(PreparedProviderComponent {
        project_id: project_id.to_string(),
        component: component.clone(),
        extension: attachment.extension.clone(),
        provider: attachment.provider.clone(),
        input,
        result_schema,
        explicit_route: config.target == Some(DeployTarget::Provider),
        dry_run,
    })
}

pub(super) fn apply_prepared(
    prepared: PreparedProviderDeployment,
    mut observation: Option<&mut DeployObservation>,
) -> Result<DeployOrchestrationResult> {
    let skipped = prepared.unresolvable.len() as u32;
    let mut results = Vec::with_capacity(prepared.components.len() + prepared.unresolvable.len());
    for component in prepared.components {
        results.push(apply_component(component, observation.as_deref_mut())?);
    }
    results.extend(unresolvable_results(&prepared.unresolvable));
    let failed = results
        .iter()
        .filter(|result| result.status == "failed")
        .count() as u32;
    let total = results.len() as u32;
    Ok(DeployOrchestrationResult {
        results,
        summary: DeploySummary {
            total,
            succeeded: total - failed - skipped,
            failed,
            skipped,
        },
        deploy_run_id: None,
    })
}

fn apply_component(
    prepared: PreparedProviderComponent,
    mut observation: Option<&mut DeployObservation>,
) -> Result<ComponentDeployResult> {
    let PreparedProviderComponent {
        project_id,
        component,
        extension,
        provider,
        input,
        result_schema,
        explicit_route,
        dry_run,
    } = prepared;
    if !dry_run {
        if let Some(observation) = observation.as_deref_mut() {
            observation.phase("provider_execute", true)?;
        }
    }
    let (input, is_layered) = match &input {
        PreparedProviderInput::Layered(payload) => (payload.path(), true),
        PreparedProviderInput::Repository(contract) => (contract.as_path(), false),
    };
    let run = homeboy_core::extension::run_deployment_provider(
        &extension,
        &provider,
        &project_id,
        &component.id,
        &component.local_path,
        input,
        dry_run,
    )?;
    if !dry_run {
        if let Some(observation) = observation {
            observation.phase("verify", true)?;
        }
    }
    let evidence = run.output.unwrap_or_default();
    let output = format!("{}{}", evidence.stdout, evidence.stderr);
    // Layered input can contain target secrets. Provider output is therefore not
    // promoted into deploy evidence or errors on that path.
    let provider_result = match (is_layered, result_schema.as_deref()) {
        (true, Some(schema)) => layered_provider_evidence(&evidence.stdout, schema),
        (true, None) => serde_json::json!({ "status": "opaque" }),
        (false, _) => serde_json::from_str::<serde_json::Value>(&evidence.stdout)
            .unwrap_or_else(|_| serde_json::json!({ "status": "unstructured", "output": output })),
    };
    let status = if run.exit_code == 0 {
        if dry_run {
            "validated"
        } else {
            "deployed"
        }
    } else {
        "failed"
    };
    let mut result = ComponentDeployResult::new(&component, "").with_status(status);
    result.warnings.push(
        match explicit_route {
            true => "deployment route: provider (selected by --target provider)",
            _ => "deployment route: provider (selected by project deployment provider target)",
        }
        .to_string(),
    );
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

/// What `layered_payload` was doing when a step failed.
///
/// Named rather than inlined because three of these steps used to emit the
/// same literal — `"Could not prepare deployment provider input"` — which made
/// a failure unattributable to a line. `distinct_payload_step_contexts` pins
/// that they stay distinguishable (#11134).
const ENCODE_POLICY_CONTEXT: &str =
    "canonically encode the deployment provider policy for its digest";
const CREATE_INPUT_CONTEXT: &str = "create the deployment provider input tempfile";
#[cfg(unix)]
const SECURE_INPUT_CONTEXT: &str = "restrict the deployment provider input tempfile to mode 0600";
const WRITE_INPUT_CONTEXT: &str = "write the deployment provider payload to its input tempfile";
const FLUSH_INPUT_CONTEXT: &str = "flush the deployment provider input tempfile";

fn layered_payload(
    component: &Component,
    policy: &serde_json::Value,
    target: Option<&serde_json::Value>,
) -> Result<tempfile::NamedTempFile> {
    let policy_bytes = homeboy_engine_primitives::canonical_json::canonical_json_bytes(policy)
        .map_err(|error| Error::from_json_error(&error, Some(ENCODE_POLICY_CONTEXT.to_string())))?;
    let revision = clean_head_revision(component)?;
    let payload = serde_json::json!({
        "schema": homeboy_core::extension::DEPLOYMENT_PROVIDER_PAYLOAD_SCHEMA,
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
    // Each step below reports its own operation and hands over the real error.
    // Discarding it with `|_|` cost the errno, the path, and the kind — which
    // is how an out-of-space or read-only /tmp reached an operator as the
    // unactionable sentence "Could not prepare deployment provider input".
    // Routing through `from_io_error`/`from_json_error` also keeps #11188's
    // ENOSPC classification, so a full disk here is `storage.exhausted` rather
    // than a generic `internal.io_error` the caller cannot degrade on.
    let mut file = tempfile::NamedTempFile::new()
        .map_err(|error| Error::from_io_error(&error, Some(CREATE_INPUT_CONTEXT.to_string())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                Error::from_io_error(&error, Some(SECURE_INPUT_CONTEXT.to_string()))
            })?;
    }
    serde_json::to_writer(&mut file, &payload)
        .map_err(|error| Error::from_json_error(&error, Some(WRITE_INPUT_CONTEXT.to_string())))?;
    file.flush()
        .map_err(|error| Error::from_io_error(&error, Some(FLUSH_INPUT_CONTEXT.to_string())))?;
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
        layered_payload, layered_provider_evidence, repository_contract, run_if_configured,
        validate_repository_policy, CREATE_INPUT_CONTEXT, ENCODE_POLICY_CONTEXT,
        FLUSH_INPUT_CONTEXT, WRITE_INPUT_CONTEXT,
    };
    use crate::route::DeployTarget;
    use crate::DeployConfig;
    use homeboy_core::component::{Component, DeploymentProviderAttachment};
    use homeboy_core::error::Error;
    use homeboy_core::project::{Project, ProjectComponentAttachment};
    use std::process::Command;

    /// Three of these steps used to emit the identical literal
    /// `"Could not prepare deployment provider input"`, so a report named the
    /// function but not the line (#11134).
    #[test]
    fn every_payload_step_reports_a_distinct_operation() {
        let mut contexts = vec![
            ENCODE_POLICY_CONTEXT,
            CREATE_INPUT_CONTEXT,
            WRITE_INPUT_CONTEXT,
            FLUSH_INPUT_CONTEXT,
        ];
        #[cfg(unix)]
        contexts.push(super::SECURE_INPUT_CONTEXT);

        let total = contexts.len();
        contexts.sort_unstable();
        contexts.dedup();

        assert_eq!(
            contexts.len(),
            total,
            "payload steps must stay individually attributable: {contexts:?}"
        );
        assert!(contexts.iter().all(|context| !context.is_empty()));
    }

    /// `.map_err(|_| ...)` discarded the `io::Error` outright, so the report
    /// carried no errno, no path, and no kind. The classifying constructors
    /// must keep all of it alongside the operation.
    #[test]
    fn a_failed_payload_step_reports_the_underlying_io_error() {
        let io_error = std::fs::File::create(
            std::path::Path::new("/homeboy-nonexistent-root").join("provider-input"),
        )
        .expect_err("creating under a missing root fails");

        let error = Error::from_io_error(&io_error, Some(CREATE_INPUT_CONTEXT.to_string()));

        assert_eq!(error.details["context"], CREATE_INPUT_CONTEXT);
        let reported = error.details["error"].as_str().expect("io error text");
        assert!(
            reported.contains("os error"),
            "the errno must survive: {reported}"
        );
    }

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

    fn provider_repository(id: &str) -> tempfile::TempDir {
        let repository = tempfile::tempdir().expect("repository");
        std::fs::write(
            repository.path().join("homeboy.json"),
            serde_json::json!({
                "id": id,
                "deployment_provider": {
                    "extension": "fixture-provider",
                    "provider": "fixture.deploy",
                    "policy": { "repository": "shared" }
                }
            })
            .to_string(),
        )
        .expect("portable component");
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
        repository
    }

    fn write_provider_extension(home: &std::path::Path, dry_run_command: Option<&str>) {
        write_provider_extension_with_target_requirement(home, dry_run_command, false);
    }

    fn write_provider_extension_with_target_requirement(
        home: &std::path::Path,
        dry_run_command: Option<&str>,
        target_required: bool,
    ) {
        let extension = home.join(".config/homeboy/extensions/fixture-provider");
        std::fs::create_dir_all(&extension).expect("extension directory");
        let mut provider = serde_json::json!({
            "id": "fixture.deploy",
            "command": "sh {{extension_path}}/run.sh apply {{payload.contract}}",
            "layered_input": {
                "schema": "homeboy/deployment-provider-payload/v1",
                "target_required": target_required
            }
        });
        if let Some(command) = dry_run_command {
            provider["dry_run_command"] = serde_json::Value::String(command.to_string());
        }
        std::fs::write(
            extension.join("fixture-provider.json"),
            serde_json::json!({
                "name": "fixture-provider",
                "version": "1.0.0",
                "deployment_providers": [provider]
            })
            .to_string(),
        )
        .expect("extension manifest");
        std::fs::write(
            extension.join("run.sh"),
            "#!/bin/sh\nif [ \"$1\" = apply ]; then touch \"$HOMEBOY_COMPONENT_PATH/applied\"; fi\nprintf '%s' '{\"status\":\"checked\"}'\n",
        )
        .expect("provider script");
    }

    fn provider_project(repository: &std::path::Path) -> Project {
        Project {
            id: "site".to_string(),
            components: vec![ProjectComponentAttachment {
                id: "fixture".to_string(),
                local_path: repository.to_string_lossy().to_string(),
                deployment_provider_input: Some(serde_json::json!({ "target": "site" })),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn project_wide_check_dispatches_to_provider_without_ssh_configuration() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let repository = provider_repository("fixture");
            write_provider_extension_with_target_requirement(
                home.path(),
                Some("sh {{extension_path}}/run.sh check {{payload.contract}}"),
                true,
            );
            let project = provider_project(repository.path());

            let result = run_if_configured(
                "site",
                &project,
                &DeployConfig::check_all_no_pull_head(),
                None,
            )
            .expect("provider check")
            .expect("provider-owned result");

            assert_eq!(result.summary.total, 1);
            assert_eq!(result.results[0].status, "validated");
            assert_eq!(
                result.results[0].deployment_provider.as_ref().unwrap()["status"],
                "opaque"
            );
            assert!(!repository.path().join("applied").exists());
        });
    }

    #[test]
    fn local_attachment_defers_a_repository_provider_without_a_target() {
        let repository = provider_repository("fixture");
        let project = Project {
            id: "local-site".to_string(),
            components: vec![ProjectComponentAttachment {
                id: "fixture".to_string(),
                local_path: repository.path().to_string_lossy().to_string(),
                remote_path: Some("wp-content/plugins/fixture".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut config = DeployConfig::check_all_no_pull_head();
        config.check = false;
        config.all = false;
        config.component_ids = vec!["fixture".to_string()];
        config.dry_run = true;

        assert!(
            run_if_configured("local-site", &project, &config, None)
                .expect("local attachment must not invoke the provider")
                .is_none(),
            "the standard deployment route must remain available when no provider target is selected"
        );
    }

    /// The provider is an option, not an owner. `--target server` must reach the
    /// standard deployment route even on a project that configures a provider
    /// target, so a dual-deliverable component stays deployable both ways.
    #[test]
    fn an_explicit_server_target_declines_a_selected_provider() {
        let repository = provider_repository("fixture");
        let project = provider_project(repository.path());
        let mut config = DeployConfig::check_all_no_pull_head();
        config.target = Some(DeployTarget::Server);

        assert!(
            run_if_configured("site", &project, &config, None)
                .expect("an explicit server target must not invoke the provider")
                .is_none(),
            "--target server must fall through to the standard deployment route"
        );
    }

    /// The reported failure (#12853). Routing to the provider is now explicit,
    /// so when its required input is missing the error must name both ways out:
    /// configure the target, or deploy the server deliverable instead.
    #[test]
    fn a_missing_provider_target_reports_the_server_deliverable_as_a_remedy() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let repository = tempfile::tempdir().expect("repository");
            std::fs::write(
                repository.path().join("homeboy.json"),
                serde_json::json!({
                    "id": "fixture",
                    "build_artifact": "dist/fixture.zip",
                    "deployment_provider": {
                        "extension": "fixture-provider",
                        "provider": "fixture.deploy",
                        "policy": { "repository": "shared" }
                    }
                })
                .to_string(),
            )
            .expect("dual-deliverable component");
            write_provider_extension_with_target_requirement(
                home.path(),
                Some("sh {{extension_path}}/run.sh check {{payload.contract}}"),
                true,
            );
            let project = Project {
                id: "extrachill-site".to_string(),
                components: vec![ProjectComponentAttachment {
                    id: "fixture".to_string(),
                    local_path: repository.path().to_string_lossy().to_string(),
                    remote_path: Some("wp-content/plugins/fixture".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let mut config = DeployConfig::check_all_no_pull_head();
            config.target = Some(DeployTarget::Provider);

            let error = run_if_configured("extrachill-site", &project, &config, None)
                .expect_err("an explicitly requested provider must not fall back silently");

            assert_eq!(
                error.details["field"],
                "components.deployment_provider_input"
            );
            let tried = error.details["tried"].to_string();
            assert!(
                tried.contains("deployment_provider_input") && tried.contains("extrachill-site"),
                "the provider remedy must name the project attachment: {tried}"
            );
            assert!(
                tried.contains("--target server"),
                "the server deliverable must stay reachable: {tried}"
            );
        });
    }

    #[test]
    fn explicit_provider_target_without_required_input_fails_closed() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let repository = provider_repository("fixture");
            write_provider_extension_with_target_requirement(
                home.path(),
                Some("sh {{extension_path}}/run.sh check {{payload.contract}}"),
                true,
            );
            let project = Project {
                id: "provider-site".to_string(),
                components: vec![ProjectComponentAttachment {
                    id: "fixture".to_string(),
                    local_path: repository.path().to_string_lossy().to_string(),
                    deployment_provider: Some(DeploymentProviderAttachment {
                        extension: "fixture-provider".to_string(),
                        provider: "fixture.deploy".to_string(),
                        contract: None,
                        policy: Some(serde_json::json!({ "repository": "shared" })),
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            };

            let error = run_if_configured(
                "provider-site",
                &project,
                &DeployConfig::check_all_no_pull_head(),
                None,
            )
            .expect_err("the selected provider must require its target input");

            assert_eq!(
                error.details["field"],
                "components.deployment_provider_input"
            );
            assert!(error
                .message
                .contains("Deployment provider requires project provider input"));
        });
    }

    #[test]
    fn provider_without_check_capability_returns_provider_aware_error() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let repository = provider_repository("fixture");
            write_provider_extension(home.path(), None);
            let project = provider_project(repository.path());

            let error = run_if_configured(
                "site",
                &project,
                &DeployConfig::check_all_no_pull_head(),
                None,
            )
            .expect_err("missing check capability must be explicit");

            assert_eq!(
                error.details["field"],
                "deployment_provider.dry_run_command"
            );
            assert!(error.message.contains("Provider 'fixture.deploy'"));
            assert!(!error.message.contains("server_id"));
        });
    }

    #[test]
    fn project_wide_check_requires_mixed_components_to_be_checked_separately() {
        let provider = provider_repository("provider");
        let generic = tempfile::tempdir().expect("generic repository");
        std::fs::write(
            generic.path().join("homeboy.json"),
            r#"{"id":"generic","deploy_strategy":"git"}"#,
        )
        .expect("generic component");
        let project = Project {
            id: "site".to_string(),
            components: vec![
                ProjectComponentAttachment {
                    id: "provider".to_string(),
                    local_path: provider.path().to_string_lossy().to_string(),
                    deployment_provider_input: Some(serde_json::json!({ "target": "site" })),
                    ..Default::default()
                },
                ProjectComponentAttachment {
                    id: "generic".to_string(),
                    local_path: generic.path().to_string_lossy().to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let error = run_if_configured(
            "site",
            &project,
            &DeployConfig::check_all_no_pull_head(),
            None,
        )
        .expect_err("mixed project-wide check must not omit provider components");

        assert_eq!(error.details["field"], "component_ids");
        assert_eq!(error.details["id"], "site");
        assert_eq!(
            error.details["tried"],
            serde_json::json!([
                "Run component-scoped checks so each component uses its declared deployment lifecycle"
            ])
        );
        assert!(!error.message.contains("server_id"));
    }

    /// Deciding provider ownership resolves every attached component, so one stale
    /// attachment used to abort the whole project-wide check before any other
    /// component was inspected (#12214). Ownership must be decided from the
    /// components this host can resolve, with the rest reported as skipped.
    #[test]
    fn project_wide_check_reports_absent_checkout_and_still_checks_the_rest() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let repository = provider_repository("fixture");
            write_provider_extension(
                home.path(),
                Some("sh {{extension_path}}/run.sh check {{payload.contract}}"),
            );
            let mut project = provider_project(repository.path());
            project.components.push(ProjectComponentAttachment {
                id: "absent".to_string(),
                local_path: repository
                    .path()
                    .join("deleted-checkout")
                    .to_string_lossy()
                    .to_string(),
                ..Default::default()
            });

            let result = run_if_configured(
                "site",
                &project,
                &DeployConfig::check_all_no_pull_head(),
                None,
            )
            .expect("an absent checkout must not abort a project-wide check")
            .expect("provider-owned result");

            assert_eq!(result.summary.total, 2);
            assert_eq!(result.summary.skipped, 1);
            assert_eq!(result.summary.failed, 0);

            let checked = result
                .results
                .iter()
                .find(|row| row.id == "fixture")
                .expect("resolvable component is still checked");
            assert_eq!(checked.status, "validated");

            let skipped = result
                .results
                .iter()
                .find(|row| row.id == "absent")
                .expect("absent component is reported");
            assert_eq!(skipped.status, "skipped");
            assert!(
                skipped.warnings.iter().any(|warning| {
                    warning.contains("does not exist") && warning.contains("attach-path")
                }),
                "the skip must carry the remedy: {:?}",
                skipped.warnings
            );
        });
    }

    /// The reported shape: a server-deployed project with one stale attachment.
    /// Provider dispatch must decline it so orchestration can run the read-only
    /// status pass, instead of erroring out of the command entirely (#12214).
    #[test]
    fn project_wide_check_with_absent_checkout_defers_non_provider_project() {
        let generic = tempfile::tempdir().expect("generic repository");
        std::fs::write(
            generic.path().join("homeboy.json"),
            r#"{"id":"generic","deploy_strategy":"git"}"#,
        )
        .expect("generic component");
        let project = Project {
            id: "site".to_string(),
            components: vec![
                ProjectComponentAttachment {
                    id: "generic".to_string(),
                    local_path: generic.path().to_string_lossy().to_string(),
                    ..Default::default()
                },
                ProjectComponentAttachment {
                    id: "absent".to_string(),
                    local_path: generic
                        .path()
                        .join("deleted-checkout")
                        .to_string_lossy()
                        .to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let dispatched = run_if_configured(
            "site",
            &project,
            &DeployConfig::check_all_no_pull_head(),
            None,
        )
        .expect("an absent checkout must not abort provider dispatch");

        assert!(
            dispatched.is_none(),
            "a server-deployed project must fall through to orchestration"
        );
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
        let policy = serde_json::json!({
            "limits": {
                "timeout_ms": 120000,
                "attempts": 3
            },
            "steps": ["prepare", "apply"],
            "mode": "strict"
        });
        let payload = layered_payload(
            &component,
            &policy,
            Some(&serde_json::json!({ "target": "one" })),
        )
        .expect("payload");
        let path = payload.path().to_path_buf();
        assert!(!path.starts_with(repository.path()));
        let value: serde_json::Value =
            serde_json::from_reader(payload.reopen().expect("reopen")).expect("payload json");
        assert_eq!(value["schema"], "homeboy/deployment-provider-payload/v1");
        assert_eq!(value["policy"]["value"], policy);
        assert_eq!(value["policy"]["reference"]["component"], "fixture");
        assert_eq!(
            value["policy"]["reference"]["path"],
            "homeboy.json#/deployment_provider/policy"
        );
        assert_eq!(
            value["policy"]["reference"]["digest"],
            "949ad67abac10d72ad874d207bff61620811c313540a65be3a9d697d280f5412"
        );
        assert_eq!(value["target"], serde_json::json!({ "target": "one" }));
        assert_eq!(value["source"]["revision"].as_str().map(str::len), Some(40));
        let second = layered_payload(
            &component,
            &policy,
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
