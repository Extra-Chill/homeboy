use homeboy_core::error::Result;
use homeboy_extension_contract::api::v1::{
    ExtensionApiCapabilityDescriptor, ExtensionApiCompatibility, ExtensionApiCompatibilityFailure,
    ExtensionApiCompatibilityFailureCode, ExtensionApiCompatibilityStatus, ExtensionApiDescriptor,
    ExtensionApiExecutionRequirements, ExtensionApiHandshakeRequest, ExtensionApiHandshakeResponse,
    ExtensionApiIdentity, ExtensionApiReadinessDescriptor, ExtensionApiRuntimeRequirement,
    ExtensionApiVersion, EXTENSION_API_DESCRIPTOR_SCHEMA, EXTENSION_API_HANDSHAKE_REQUEST_SCHEMA,
    EXTENSION_API_HANDSHAKE_RESPONSE_SCHEMA, EXTENSION_API_V1,
};
use homeboy_extension_contract::{evaluate_core_compatibility, ExtensionCapability};

use super::load_extension;

const SUPPORTED_API_VERSIONS: &[ExtensionApiVersion] = &[EXTENSION_API_V1];

/// Project an installed manifest into the stable Extension API v1 descriptor.
pub fn api_descriptor(extension_id: &str) -> Result<ExtensionApiDescriptor> {
    let extension = load_extension(extension_id)?;
    let mut capabilities = [
        ExtensionCapability::Lint,
        ExtensionCapability::Test,
        ExtensionCapability::Build,
        ExtensionCapability::Bench,
        ExtensionCapability::Fuzz,
        ExtensionCapability::Trace,
        ExtensionCapability::Deps,
        ExtensionCapability::Audit,
    ]
    .into_iter()
    .filter(|capability| capability.has_manifest_support(&extension))
    .map(|capability| capability_descriptor(capability.label()))
    .collect::<Vec<_>>();

    if extension
        .runtime()
        .and_then(|runtime| runtime.run_command.as_ref())
        .is_some()
    {
        capabilities.push(capability_descriptor("execute"));
    }
    capabilities.extend(
        extension
            .actions
            .iter()
            .map(|action| capability_descriptor(&format!("action.{}", action.id))),
    );
    capabilities.extend(
        extension
            .deployment_providers
            .iter()
            .map(|provider| capability_descriptor(&format!("deployment-provider.{}", provider.id))),
    );
    capabilities.extend(
        extension
            .recipe_run_providers
            .iter()
            .filter_map(|provider| {
                provider
                    .declared_str("id")
                    .map(|id| capability_descriptor(&format!("recipe-run-provider.{id}")))
            }),
    );
    if extension.env_provider.is_some() {
        capabilities.push(capability_descriptor("environment"));
    }
    capabilities.extend(
        extension
            .agent_runtimes
            .iter()
            .map(|runtime| capability_descriptor(&format!("agent-runtime.{}", runtime.id))),
    );
    capabilities.sort_by(|left, right| left.id.cmp(&right.id));
    capabilities.dedup_by(|left, right| left.id == right.id);

    let mut toolchain_probe_ids = extension
        .toolchain_readiness
        .iter()
        .map(|probe| probe.id.clone())
        .collect::<Vec<_>>();
    toolchain_probe_ids.sort();
    toolchain_probe_ids.dedup();

    let mut runtimes = extension
        .runtime
        .as_ref()
        .into_iter()
        .flat_map(|requirements| requirements.runtimes.iter())
        .map(|(id, requirement)| ExtensionApiRuntimeRequirement {
            id: id.clone(),
            version: requirement.version.clone(),
        })
        .collect::<Vec<_>>();
    runtimes.sort_by(|left, right| left.id.cmp(&right.id));

    Ok(ExtensionApiDescriptor {
        schema: EXTENSION_API_DESCRIPTOR_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        identity: ExtensionApiIdentity {
            id: extension.id.clone(),
            name: extension.name.clone(),
            version: extension.version.clone(),
            source_revision: crate::extension::lifecycle::read_source_revision(&extension.id),
        },
        capabilities,
        readiness: ExtensionApiReadinessDescriptor {
            runtime_probe: extension
                .runtime()
                .and_then(|runtime| runtime.ready_check.as_ref())
                .is_some(),
            toolchain_probe_ids,
        },
        execution_requirements: ExtensionApiExecutionRequirements { runtimes },
        requires_homeboy: extension
            .requires
            .as_ref()
            .and_then(|requirements| requirements.homeboy.clone()),
    })
}

/// Negotiate a client's supported API versions against one installed extension.
pub fn negotiate_api(
    extension_id: &str,
    request: &ExtensionApiHandshakeRequest,
) -> Result<ExtensionApiHandshakeResponse> {
    let descriptor = api_descriptor(extension_id)?;
    let supported_versions = SUPPORTED_API_VERSIONS.to_vec();
    let valid_schema = request.schema == EXTENSION_API_HANDSHAKE_REQUEST_SCHEMA;
    let selected_version = valid_schema
        .then(|| {
            request
                .supported_versions
                .iter()
                .filter(|version| SUPPORTED_API_VERSIONS.contains(version))
                .max()
                .copied()
        })
        .flatten();
    let mut failures = Vec::new();

    if !valid_schema {
        failures.push(ExtensionApiCompatibilityFailure {
            code: ExtensionApiCompatibilityFailureCode::InvalidHandshakeSchema,
            message: format!(
                "Unsupported Extension API handshake schema '{}'; expected '{}'",
                request.schema, EXTENSION_API_HANDSHAKE_REQUEST_SCHEMA
            ),
        });
    } else if selected_version.is_none() {
        failures.push(ExtensionApiCompatibilityFailure {
            code: ExtensionApiCompatibilityFailureCode::NoSharedApiVersion,
            message: "Client and Homeboy do not share an Extension API major version".to_string(),
        });
    }

    match evaluate_core_compatibility(descriptor.requires_homeboy.as_deref(), None) {
        Ok(report) if report.status == "incompatible" => {
            failures.push(ExtensionApiCompatibilityFailure {
                code: ExtensionApiCompatibilityFailureCode::HomeboyVersionIncompatible,
                message: format!(
                    "Extension requires Homeboy {}, but {} is installed",
                    report.requires_homeboy.as_deref().unwrap_or("<undeclared>"),
                    report.installed_homeboy
                ),
            });
        }
        Err(error) => failures.push(ExtensionApiCompatibilityFailure {
            code: ExtensionApiCompatibilityFailureCode::InvalidHomeboyVersionConstraint,
            message: error.message,
        }),
        _ => {}
    }

    let status = if failures.is_empty() {
        ExtensionApiCompatibilityStatus::Compatible
    } else {
        ExtensionApiCompatibilityStatus::Incompatible
    };
    Ok(ExtensionApiHandshakeResponse {
        schema: EXTENSION_API_HANDSHAKE_RESPONSE_SCHEMA.to_string(),
        supported_versions,
        selected_version,
        descriptor: selected_version.map(|_| descriptor),
        compatibility: ExtensionApiCompatibility { status, failures },
    })
}

fn capability_descriptor(id: &str) -> ExtensionApiCapabilityDescriptor {
    ExtensionApiCapabilityDescriptor {
        id: id.to_string(),
        contract_version: 1,
        configuration_schema: None,
        input_schema: None,
        output_schema: None,
        artifact_schemas: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_extension(id: &str, manifest: serde_json::Value) {
        let extension_dir = crate::paths::extensions()
            .expect("extensions path")
            .join(id);
        std::fs::create_dir_all(&extension_dir).expect("extension directory");
        std::fs::write(
            extension_dir.join(format!("{id}.json")),
            manifest.to_string(),
        )
        .expect("extension manifest");
    }

    #[test]
    fn descriptor_normalizes_manifest_capabilities_and_requirements() {
        crate::test_support::with_isolated_home(|_| {
            write_extension(
                "fixture",
                serde_json::json!({
                    "name": "Fixture",
                    "version": "1.2.3",
                    "test": { "extension_script": "test.sh" },
                    "executable": { "runtime": { "run_command": "fixture", "ready_check": "fixture --ready" } },
                    "runtime": { "runtimes": { "php": { "version": ">=8.0" }, "node": { "version": ">=20" } } },
                    "toolchain_readiness": [
                        { "id": "zeta", "program": "zeta" },
                        { "id": "alpha", "program": "alpha" }
                    ],
                    "requires": { "homeboy": ">=0.1.0" }
                }),
            );

            let descriptor = api_descriptor("fixture").expect("descriptor");

            assert_eq!(descriptor.api_version, EXTENSION_API_V1);
            assert_eq!(descriptor.identity.id, "fixture");
            assert_eq!(
                descriptor
                    .capabilities
                    .iter()
                    .map(|capability| capability.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["execute", "test"]
            );
            assert!(descriptor.readiness.runtime_probe);
            assert_eq!(descriptor.readiness.toolchain_probe_ids, ["alpha", "zeta"]);
            assert_eq!(
                descriptor
                    .execution_requirements
                    .runtimes
                    .iter()
                    .map(|runtime| runtime.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["node", "php"]
            );
        });
    }

    #[test]
    fn handshake_selects_v1_and_returns_the_descriptor() {
        crate::test_support::with_isolated_home(|_| {
            write_extension(
                "fixture",
                serde_json::json!({ "name": "Fixture", "version": "1.0.0" }),
            );
            let response = negotiate_api(
                "fixture",
                &ExtensionApiHandshakeRequest {
                    schema: EXTENSION_API_HANDSHAKE_REQUEST_SCHEMA.to_string(),
                    supported_versions: vec![ExtensionApiVersion { major: 99 }, EXTENSION_API_V1],
                },
            )
            .expect("handshake");

            assert_eq!(response.selected_version, Some(EXTENSION_API_V1));
            assert!(response.descriptor.is_some());
            assert_eq!(
                response.compatibility.status,
                ExtensionApiCompatibilityStatus::Compatible
            );
        });
    }

    #[test]
    fn handshake_reports_no_shared_version_without_losing_supported_versions() {
        crate::test_support::with_isolated_home(|_| {
            write_extension(
                "fixture",
                serde_json::json!({ "name": "Fixture", "version": "1.0.0" }),
            );
            let response = negotiate_api(
                "fixture",
                &ExtensionApiHandshakeRequest {
                    schema: EXTENSION_API_HANDSHAKE_REQUEST_SCHEMA.to_string(),
                    supported_versions: vec![ExtensionApiVersion { major: 99 }],
                },
            )
            .expect("handshake");

            assert_eq!(response.selected_version, None);
            assert_eq!(response.supported_versions, [EXTENSION_API_V1]);
            assert_eq!(response.descriptor, None);
            assert_eq!(
                response.compatibility.failures[0].code,
                ExtensionApiCompatibilityFailureCode::NoSharedApiVersion
            );
        });
    }

    #[test]
    fn handshake_reports_controller_version_incompatibility() {
        crate::test_support::with_isolated_home(|_| {
            write_extension(
                "fixture",
                serde_json::json!({
                    "name": "Fixture",
                    "version": "1.0.0",
                    "requires": { "homeboy": ">=999.0.0" }
                }),
            );
            let response = negotiate_api(
                "fixture",
                &ExtensionApiHandshakeRequest {
                    schema: EXTENSION_API_HANDSHAKE_REQUEST_SCHEMA.to_string(),
                    supported_versions: vec![EXTENSION_API_V1],
                },
            )
            .expect("handshake");

            assert_eq!(response.selected_version, Some(EXTENSION_API_V1));
            assert_eq!(
                response.compatibility.failures[0].code,
                ExtensionApiCompatibilityFailureCode::HomeboyVersionIncompatible
            );
        });
    }

    #[test]
    fn handshake_rejects_the_wrong_request_schema() {
        crate::test_support::with_isolated_home(|_| {
            write_extension(
                "fixture",
                serde_json::json!({ "name": "Fixture", "version": "1.0.0" }),
            );
            let response = negotiate_api(
                "fixture",
                &ExtensionApiHandshakeRequest {
                    schema: "homeboy/extension-api-handshake-request/v2".to_string(),
                    supported_versions: vec![EXTENSION_API_V1],
                },
            )
            .expect("handshake");

            assert_eq!(response.selected_version, None);
            assert_eq!(
                response.compatibility.failures[0].code,
                ExtensionApiCompatibilityFailureCode::InvalidHandshakeSchema
            );
        });
    }
}
