use std::path::Path;

use homeboy_core::error::Result;
use homeboy_extension_contract::api::v1::{
    ExtensionApiCapabilityDescriptor, ExtensionApiCatalogDiagnostic,
    ExtensionApiCatalogDiagnosticCode, ExtensionApiCatalogEntry, ExtensionApiCatalogEntryStatus,
    ExtensionApiCatalogRequest, ExtensionApiCatalogResponse, ExtensionApiCompatibility,
    ExtensionApiCompatibilityFailure, ExtensionApiCompatibilityFailureCode,
    ExtensionApiCompatibilityStatus, ExtensionApiDescriptor, ExtensionApiExecutionRequirements,
    ExtensionApiHandshakeRequest, ExtensionApiHandshakeResponse, ExtensionApiIdentity,
    ExtensionApiOperationFailure, ExtensionApiOperationFailureCode,
    ExtensionApiReadinessDescriptor, ExtensionApiReadinessMode, ExtensionApiReadinessRequest,
    ExtensionApiReadinessResponse, ExtensionApiReadinessState, ExtensionApiReadinessStatus,
    ExtensionApiResolveRequest, ExtensionApiResolveResponse, ExtensionApiRuntimeRequirement,
    ExtensionApiVersion, COMPILER_WARNINGS_CAPABILITY_ID, COMPILER_WARNINGS_INPUT_SCHEMA,
    COMPILER_WARNINGS_OUTPUT_SCHEMA, COMPILER_WARNING_FIXES_CAPABILITY_ID,
    COMPILER_WARNING_FIXES_INPUT_SCHEMA, COMPILER_WARNING_FIXES_OUTPUT_SCHEMA,
    EXTENSION_API_CATALOG_REQUEST_SCHEMA, EXTENSION_API_CATALOG_RESPONSE_SCHEMA,
    EXTENSION_API_DESCRIPTOR_SCHEMA, EXTENSION_API_HANDSHAKE_REQUEST_SCHEMA,
    EXTENSION_API_HANDSHAKE_RESPONSE_SCHEMA, EXTENSION_API_READINESS_REQUEST_SCHEMA,
    EXTENSION_API_READINESS_RESPONSE_SCHEMA, EXTENSION_API_RESOLVE_REQUEST_SCHEMA,
    EXTENSION_API_RESOLVE_RESPONSE_SCHEMA, EXTENSION_API_V1, FINGERPRINT_FILE_CAPABILITY_PREFIX,
    FORMAT_FILE_CAPABILITY_PREFIX, REFACTOR_ANALYSIS_INPUT_SCHEMA, REFACTOR_ANALYSIS_OUTPUT_SCHEMA,
    REFACTOR_FILE_CAPABILITY_PREFIX,
};
use homeboy_extension_contract::{evaluate_core_compatibility, ExtensionCapability};

use super::{discover_extensions, load_extension, DiscoveredExtension};

const SUPPORTED_API_VERSIONS: &[ExtensionApiVersion] = &[EXTENSION_API_V1];

/// Project an installed manifest into the stable Extension API v1 descriptor.
fn api_descriptor(extension_id: &str) -> Result<ExtensionApiDescriptor> {
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
                provider.declared_str("id").map(|id| {
                    versioned_capability_descriptor(
                        &format!("recipe-run-provider.{id}"),
                        provider.declared_str("version"),
                    )
                })
            }),
    );
    if extension.env_provider.is_some() {
        capabilities.push(capability_descriptor("environment"));
    }
    if extension.compiler_warnings_script().is_some() {
        capabilities.push(schema_capability_descriptor(
            COMPILER_WARNINGS_CAPABILITY_ID,
            COMPILER_WARNINGS_INPUT_SCHEMA,
            COMPILER_WARNINGS_OUTPUT_SCHEMA,
        ));
    }
    if extension.compiler_warning_fixes_script().is_some() {
        capabilities.push(schema_capability_descriptor(
            COMPILER_WARNING_FIXES_CAPABILITY_ID,
            COMPILER_WARNING_FIXES_INPUT_SCHEMA,
            COMPILER_WARNING_FIXES_OUTPUT_SCHEMA,
        ));
    }
    if extension.fingerprint_script().is_some() {
        capabilities.extend(
            extension
                .provided_file_extensions()
                .iter()
                .map(|file_extension| {
                    capability_descriptor(&format!(
                        "{FINGERPRINT_FILE_CAPABILITY_PREFIX}{file_extension}"
                    ))
                }),
        );
    }
    if extension.format_script().is_some() {
        capabilities.extend(
            extension
                .provided_file_extensions()
                .iter()
                .map(|file_extension| {
                    capability_descriptor(&format!(
                        "{FORMAT_FILE_CAPABILITY_PREFIX}{file_extension}"
                    ))
                }),
        );
    }
    if extension.refactor_script().is_some() {
        capabilities.extend(
            extension
                .provided_file_extensions()
                .iter()
                .map(|file_extension| {
                    schema_capability_descriptor(
                        &format!("{REFACTOR_FILE_CAPABILITY_PREFIX}{file_extension}"),
                        REFACTOR_ANALYSIS_INPUT_SCHEMA,
                        REFACTOR_ANALYSIS_OUTPUT_SCHEMA,
                    )
                }),
        );
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
fn negotiate_api(
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

/// List every installed extension through the stable v1 catalog contract.
pub fn list_api(request: &ExtensionApiCatalogRequest) -> ExtensionApiCatalogResponse {
    if let Some(failure) = validate_operation_request(
        &request.schema,
        EXTENSION_API_CATALOG_REQUEST_SCHEMA,
        request.api_version,
    ) {
        return ExtensionApiCatalogResponse {
            schema: EXTENSION_API_CATALOG_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            entries: Vec::new(),
            failure: Some(failure),
        };
    }

    let entries = discover_extensions()
        .into_iter()
        .map(|extension| match extension {
            DiscoveredExtension::Valid(extension) => {
                let id = extension.id.clone();
                match negotiate_api(
                    &id,
                    &ExtensionApiHandshakeRequest {
                        schema: EXTENSION_API_HANDSHAKE_REQUEST_SCHEMA.to_string(),
                        supported_versions: vec![request.api_version],
                    },
                ) {
                    Ok(handshake) => {
                        let status = if handshake.compatibility.status
                            == ExtensionApiCompatibilityStatus::Compatible
                        {
                            ExtensionApiCatalogEntryStatus::Available
                        } else {
                            ExtensionApiCatalogEntryStatus::Incompatible
                        };
                        ExtensionApiCatalogEntry {
                            id,
                            status,
                            descriptor: handshake.descriptor,
                            compatibility: Some(handshake.compatibility),
                            diagnostic: None,
                        }
                    }
                    Err(error) => {
                        invalid_catalog_entry(id, "catalog_projection_failed", error.message)
                    }
                }
            }
            DiscoveredExtension::Invalid(failure) => {
                invalid_catalog_entry(failure.id, failure.category, failure.diagnostic.to_string())
            }
        })
        .collect();

    ExtensionApiCatalogResponse {
        schema: EXTENSION_API_CATALOG_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        entries,
        failure: None,
    }
}

/// Select installed providers for a capability, preferring component-linked extensions.
pub fn capability_provider_ids(root: &Path, capability_id: &str) -> Vec<String> {
    let catalog = list_api(&ExtensionApiCatalogRequest {
        schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
    });
    let mut providers = catalog
        .entries
        .into_iter()
        .filter(|entry| entry.status == ExtensionApiCatalogEntryStatus::Available)
        .filter(|entry| {
            entry.descriptor.as_ref().is_some_and(|descriptor| {
                descriptor
                    .capabilities
                    .iter()
                    .any(|capability| capability.id == capability_id)
            })
        })
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    providers.sort();

    if let Some(component_extensions) = homeboy_core::component::discover_from_portable(root)
        .and_then(|component| component.extensions)
    {
        let linked = providers
            .iter()
            .filter(|id| component_extensions.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        if !linked.is_empty() {
            return linked;
        }
    }
    providers
}

/// Resolve one explicitly named installed extension capability through v1.
pub(crate) fn resolve_api(request: &ExtensionApiResolveRequest) -> ExtensionApiResolveResponse {
    if let Some(failure) = validate_operation_request(
        &request.schema,
        EXTENSION_API_RESOLVE_REQUEST_SCHEMA,
        request.api_version,
    ) {
        return resolve_failure(None, None, failure);
    }

    let catalog = list_api(&ExtensionApiCatalogRequest {
        schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
        api_version: request.api_version,
    });
    let Some(entry) = catalog
        .entries
        .into_iter()
        .find(|entry| entry.id == request.extension_id)
    else {
        return resolve_failure(
            None,
            None,
            operation_failure(
                ExtensionApiOperationFailureCode::ExtensionNotFound,
                format!("Extension '{}' is not installed", request.extension_id),
            ),
        );
    };

    if entry.status == ExtensionApiCatalogEntryStatus::Invalid {
        return resolve_failure(
            None,
            None,
            operation_failure(
                ExtensionApiOperationFailureCode::ExtensionInvalid,
                format!(
                    "Extension '{}' has an invalid installation",
                    request.extension_id
                ),
            ),
        );
    }

    let (Some(descriptor), Some(compatibility)) = (entry.descriptor, entry.compatibility) else {
        return resolve_failure(
            None,
            None,
            operation_failure(
                ExtensionApiOperationFailureCode::ExtensionInvalid,
                format!(
                    "Extension '{}' has an incomplete catalog projection",
                    request.extension_id
                ),
            ),
        );
    };
    if entry.status == ExtensionApiCatalogEntryStatus::Incompatible {
        return resolve_failure(
            Some(descriptor),
            Some(compatibility),
            operation_failure(
                ExtensionApiOperationFailureCode::ExtensionIncompatible,
                format!(
                    "Extension '{}' is incompatible with this Homeboy runtime",
                    request.extension_id
                ),
            ),
        );
    }

    let Some(capability) = descriptor
        .capabilities
        .iter()
        .find(|capability| capability.id == request.capability_id)
        .cloned()
    else {
        return resolve_failure(
            Some(descriptor),
            Some(compatibility),
            operation_failure(
                ExtensionApiOperationFailureCode::CapabilityNotProvided,
                format!(
                    "Extension '{}' does not provide capability '{}'",
                    request.extension_id, request.capability_id
                ),
            ),
        );
    };

    ExtensionApiResolveResponse {
        schema: EXTENSION_API_RESOLVE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        descriptor: Some(descriptor),
        capability: Some(capability),
        compatibility: Some(compatibility),
        failure: None,
    }
}

/// Serve several v1 readiness requests from one catalog discovery pass.
pub fn readiness_api_batch(
    requests: &[ExtensionApiReadinessRequest],
) -> Vec<ExtensionApiReadinessResponse> {
    let extensions = discover_extensions();
    if requests
        .iter()
        .any(|request| request.mode == ExtensionApiReadinessMode::Probe)
    {
        std::thread::scope(|scope| {
            requests
                .iter()
                .map(|request| scope.spawn(|| readiness_api_for_discovered(request, &extensions)))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|probe| probe.join().expect("extension readiness probe panicked"))
                .collect()
        })
    } else {
        requests
            .iter()
            .map(|request| readiness_api_for_discovered(request, &extensions))
            .collect()
    }
}

fn readiness_api_for_discovered(
    request: &ExtensionApiReadinessRequest,
    extensions: &[DiscoveredExtension],
) -> ExtensionApiReadinessResponse {
    if let Some(failure) = validate_operation_request(
        &request.schema,
        EXTENSION_API_READINESS_REQUEST_SCHEMA,
        request.api_version,
    ) {
        return readiness_failure(request, failure);
    }

    let Some(extension) = extensions.iter().find(|extension| match extension {
        DiscoveredExtension::Valid(extension) => extension.id == request.extension_id,
        DiscoveredExtension::Invalid(failure) => failure.id == request.extension_id,
    }) else {
        return readiness_failure(
            request,
            operation_failure(
                ExtensionApiOperationFailureCode::ExtensionNotFound,
                format!("Extension '{}' is not installed", request.extension_id),
            ),
        );
    };

    let DiscoveredExtension::Valid(extension) = extension else {
        return readiness_failure(
            request,
            operation_failure(
                ExtensionApiOperationFailureCode::ExtensionInvalid,
                format!(
                    "Extension '{}' has an invalid installation",
                    request.extension_id
                ),
            ),
        );
    };
    let mode = match request.mode {
        ExtensionApiReadinessMode::Cached => {
            crate::extension::readiness::ExtensionReadinessMode::Cached
        }
        ExtensionApiReadinessMode::Probe => {
            crate::extension::readiness::ExtensionReadinessMode::Probe
        }
    };
    let status = crate::extension::readiness::extension_ready_status_with(extension, mode);

    ExtensionApiReadinessResponse {
        schema: EXTENSION_API_READINESS_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        extension_id: request.extension_id.clone(),
        status: Some(ExtensionApiReadinessStatus {
            state: match status.state {
                crate::extension::readiness::ExtensionReadinessState::Ready => {
                    ExtensionApiReadinessState::Ready
                }
                crate::extension::readiness::ExtensionReadinessState::NotReady => {
                    ExtensionApiReadinessState::NotReady
                }
                crate::extension::readiness::ExtensionReadinessState::Unknown => {
                    ExtensionApiReadinessState::Unknown
                }
                crate::extension::readiness::ExtensionReadinessState::TimedOut => {
                    ExtensionApiReadinessState::TimedOut
                }
            },
            ready: status.ready,
            reason: status.reason,
            detail: status.detail,
            cache_age_seconds: status.cache_age_seconds,
            probe_duration_ms: status.probe_duration_ms,
            timeout_ms: status.timeout_ms,
            follow_up_command: status.follow_up_command,
        }),
        failure: None,
    }
}

pub(crate) fn validate_operation_request(
    actual_schema: &str,
    expected_schema: &str,
    api_version: ExtensionApiVersion,
) -> Option<ExtensionApiOperationFailure> {
    if actual_schema != expected_schema {
        return Some(operation_failure(
            ExtensionApiOperationFailureCode::InvalidRequestSchema,
            format!("Unsupported request schema '{actual_schema}'; expected '{expected_schema}'"),
        ));
    }
    (api_version != EXTENSION_API_V1).then(|| {
        operation_failure(
            ExtensionApiOperationFailureCode::UnsupportedApiVersion,
            format!(
                "Extension API major {} is not supported; Homeboy supports major {}",
                api_version.major, EXTENSION_API_V1.major
            ),
        )
    })
}

fn invalid_catalog_entry(id: String, category: &str, message: String) -> ExtensionApiCatalogEntry {
    ExtensionApiCatalogEntry {
        id,
        status: ExtensionApiCatalogEntryStatus::Invalid,
        descriptor: None,
        compatibility: None,
        diagnostic: Some(ExtensionApiCatalogDiagnostic {
            code: if category == "target_missing" {
                ExtensionApiCatalogDiagnosticCode::BrokenInstallation
            } else {
                ExtensionApiCatalogDiagnosticCode::InvalidManifest
            },
            category: category.to_string(),
            message,
        }),
    }
}

fn resolve_failure(
    descriptor: Option<ExtensionApiDescriptor>,
    compatibility: Option<ExtensionApiCompatibility>,
    failure: ExtensionApiOperationFailure,
) -> ExtensionApiResolveResponse {
    ExtensionApiResolveResponse {
        schema: EXTENSION_API_RESOLVE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        descriptor,
        capability: None,
        compatibility,
        failure: Some(failure),
    }
}

fn readiness_failure(
    request: &ExtensionApiReadinessRequest,
    failure: ExtensionApiOperationFailure,
) -> ExtensionApiReadinessResponse {
    ExtensionApiReadinessResponse {
        schema: EXTENSION_API_READINESS_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        extension_id: request.extension_id.clone(),
        status: None,
        failure: Some(failure),
    }
}

fn operation_failure(
    code: ExtensionApiOperationFailureCode,
    message: String,
) -> ExtensionApiOperationFailure {
    ExtensionApiOperationFailure { code, message }
}

fn capability_descriptor(id: &str) -> ExtensionApiCapabilityDescriptor {
    versioned_capability_descriptor(id, None)
}

fn versioned_capability_descriptor(
    id: &str,
    contract_version: Option<String>,
) -> ExtensionApiCapabilityDescriptor {
    ExtensionApiCapabilityDescriptor {
        id: id.to_string(),
        contract_version,
        configuration_schema: None,
        input_schema: None,
        output_schema: None,
        artifact_schemas: Vec::new(),
    }
}

fn schema_capability_descriptor(
    id: &str,
    input_schema: &str,
    output_schema: &str,
) -> ExtensionApiCapabilityDescriptor {
    ExtensionApiCapabilityDescriptor {
        id: id.to_string(),
        contract_version: None,
        configuration_schema: None,
        input_schema: Some(
            homeboy_extension_contract::api::v1::ExtensionApiSchemaReference {
                schema: input_schema.to_string(),
            },
        ),
        output_schema: Some(
            homeboy_extension_contract::api::v1::ExtensionApiSchemaReference {
                schema: output_schema.to_string(),
            },
        ),
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

    fn catalog_request() -> ExtensionApiCatalogRequest {
        ExtensionApiCatalogRequest {
            schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
        }
    }

    fn resolve_request(extension_id: &str, capability_id: &str) -> ExtensionApiResolveRequest {
        ExtensionApiResolveRequest {
            schema: EXTENSION_API_RESOLVE_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: extension_id.to_string(),
            capability_id: capability_id.to_string(),
        }
    }

    fn readiness_request(
        extension_id: &str,
        mode: ExtensionApiReadinessMode,
    ) -> ExtensionApiReadinessRequest {
        ExtensionApiReadinessRequest {
            schema: EXTENSION_API_READINESS_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: extension_id.to_string(),
            mode,
        }
    }

    fn readiness_response(request: &ExtensionApiReadinessRequest) -> ExtensionApiReadinessResponse {
        readiness_api_for_discovered(request, &discover_extensions())
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
                    "scripts": {
                        "compiler_warnings": "warnings.sh",
                        "compiler_warning_fixes": "warning-fixes.sh",
                        "fingerprint": "fingerprint.sh",
                        "format": "format.sh",
                        "refactor": "refactor.sh"
                    },
                    "provides": { "file_extensions": ["rs", "php"] },
                    "executable": { "runtime": { "run_command": "fixture", "ready_check": "fixture --ready" } },
                    "recipe_run_providers": [{
                        "id": "fixture.recipe",
                        "version": "2",
                        "executable": "fixture-run",
                        "command": ["fixture-run"]
                    }],
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
                vec![
                    COMPILER_WARNING_FIXES_CAPABILITY_ID,
                    COMPILER_WARNINGS_CAPABILITY_ID,
                    "execute",
                    "fingerprint.php",
                    "fingerprint.rs",
                    "format.php",
                    "format.rs",
                    "recipe-run-provider.fixture.recipe",
                    "refactor.php",
                    "refactor.rs",
                    "test"
                ]
            );
            assert_eq!(
                descriptor.capabilities[7].contract_version.as_deref(),
                Some("2")
            );
            let warnings = descriptor
                .capabilities
                .iter()
                .find(|capability| capability.id == COMPILER_WARNINGS_CAPABILITY_ID)
                .expect("compiler warnings capability");
            assert_eq!(
                warnings
                    .input_schema
                    .as_ref()
                    .map(|schema| schema.schema.as_str()),
                Some(COMPILER_WARNINGS_INPUT_SCHEMA)
            );
            assert_eq!(
                warnings
                    .output_schema
                    .as_ref()
                    .map(|schema| schema.schema.as_str()),
                Some(COMPILER_WARNINGS_OUTPUT_SCHEMA)
            );
            let refactor = descriptor
                .capabilities
                .iter()
                .find(|capability| capability.id == "refactor.rs")
                .expect("refactor capability");
            assert_eq!(
                refactor
                    .input_schema
                    .as_ref()
                    .map(|schema| schema.schema.as_str()),
                Some(REFACTOR_ANALYSIS_INPUT_SCHEMA)
            );
            assert_eq!(
                refactor
                    .output_schema
                    .as_ref()
                    .map(|schema| schema.schema.as_str()),
                Some(REFACTOR_ANALYSIS_OUTPUT_SCHEMA)
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

    #[test]
    fn catalog_is_sorted_and_retains_invalid_installs() {
        crate::test_support::with_isolated_home(|_| {
            write_extension(
                "zeta",
                serde_json::json!({ "name": "Zeta", "version": "1.0.0" }),
            );
            let broken_dir = crate::paths::extensions()
                .expect("extensions path")
                .join("alpha");
            std::fs::create_dir_all(&broken_dir).expect("broken extension directory");
            std::fs::write(broken_dir.join("alpha.json"), "{").expect("broken manifest");

            let response = list_api(&catalog_request());

            assert!(response.failure.is_none());
            assert_eq!(
                response
                    .entries
                    .iter()
                    .map(|entry| entry.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["alpha", "zeta"]
            );
            assert_eq!(
                response.entries[0].status,
                ExtensionApiCatalogEntryStatus::Invalid
            );
            assert_eq!(
                response.entries[0]
                    .diagnostic
                    .as_ref()
                    .expect("diagnostic")
                    .category,
                "manifest_json_malformed"
            );
            assert_eq!(
                response.entries[1].status,
                ExtensionApiCatalogEntryStatus::Available
            );
        });
    }

    #[test]
    fn resolve_returns_the_named_capability() {
        crate::test_support::with_isolated_home(|_| {
            write_extension(
                "fixture",
                serde_json::json!({
                    "name": "Fixture",
                    "version": "1.0.0",
                    "test": { "extension_script": "test.sh" }
                }),
            );

            let response = resolve_api(&resolve_request("fixture", "test"));

            assert!(response.failure.is_none());
            assert_eq!(response.capability.expect("capability").id, "test");
            assert_eq!(
                response.descriptor.expect("descriptor").identity.id,
                "fixture"
            );
        });
    }

    #[test]
    fn resolve_reports_missing_capability_without_dropping_descriptor() {
        crate::test_support::with_isolated_home(|_| {
            write_extension(
                "fixture",
                serde_json::json!({ "name": "Fixture", "version": "1.0.0" }),
            );

            let response = resolve_api(&resolve_request("fixture", "test"));

            assert_eq!(
                response.failure.expect("failure").code,
                ExtensionApiOperationFailureCode::CapabilityNotProvided
            );
            assert!(response.descriptor.is_some());
        });
    }

    #[test]
    fn resolve_reports_incompatible_extension_before_capability_selection() {
        crate::test_support::with_isolated_home(|_| {
            write_extension(
                "fixture",
                serde_json::json!({
                    "name": "Fixture",
                    "version": "1.0.0",
                    "test": { "extension_script": "test.sh" },
                    "requires": { "homeboy": ">=999.0.0" }
                }),
            );

            let response = resolve_api(&resolve_request("fixture", "test"));

            assert_eq!(
                response.failure.expect("failure").code,
                ExtensionApiOperationFailureCode::ExtensionIncompatible
            );
            assert_eq!(
                response.compatibility.expect("compatibility").status,
                ExtensionApiCompatibilityStatus::Incompatible
            );
            assert!(response.capability.is_none());
        });
    }

    #[cfg(unix)]
    #[test]
    fn readiness_api_preserves_cached_and_live_probe_semantics() {
        crate::test_support::with_isolated_home(|home| {
            let marker = home.path().join("ready");
            std::fs::write(&marker, "ready").expect("ready marker");
            write_extension(
                "fixture",
                serde_json::json!({
                    "name": "Fixture",
                    "version": "1.0.0",
                    "executable": {
                        "runtime": { "ready_check": format!("test -f '{}'", marker.display()) }
                    }
                }),
            );

            let cached = readiness_response(&readiness_request(
                "fixture",
                ExtensionApiReadinessMode::Cached,
            ));
            assert_eq!(
                cached.status.expect("cached status").state,
                ExtensionApiReadinessState::Unknown
            );

            let probed = readiness_response(&readiness_request(
                "fixture",
                ExtensionApiReadinessMode::Probe,
            ));
            assert_eq!(
                probed.status.expect("probed status").state,
                ExtensionApiReadinessState::Ready
            );

            let cached = readiness_response(&readiness_request(
                "fixture",
                ExtensionApiReadinessMode::Cached,
            ));
            assert_eq!(
                cached.status.expect("cached status").state,
                ExtensionApiReadinessState::Ready
            );
        });
    }

    #[test]
    fn readiness_api_returns_typed_invalid_extension_failure() {
        crate::test_support::with_isolated_home(|_| {
            let broken_dir = crate::paths::extensions()
                .expect("extensions path")
                .join("broken");
            std::fs::create_dir_all(&broken_dir).expect("broken extension directory");
            std::fs::write(broken_dir.join("broken.json"), "{").expect("broken manifest");

            let response = readiness_response(&readiness_request(
                "broken",
                ExtensionApiReadinessMode::Cached,
            ));

            assert_eq!(
                response.failure.expect("failure").code,
                ExtensionApiOperationFailureCode::ExtensionInvalid
            );
            assert!(response.status.is_none());
        });
    }

    #[cfg(unix)]
    #[test]
    fn readiness_batch_probes_independent_extensions_concurrently() {
        crate::test_support::with_isolated_home(|home| {
            let first = home.path().join("first-started");
            let second = home.path().join("second-started");
            let wait_for_peer = |own: &std::path::Path, peer: &std::path::Path| {
                format!(
                    "touch '{}' && i=0; while [ ! -f '{}' ] && [ $i -lt 200 ]; do i=$((i + 1)); sleep 0.01; done; test -f '{}'",
                    own.display(),
                    peer.display(),
                    peer.display()
                )
            };
            write_extension(
                "first",
                serde_json::json!({
                    "name": "First",
                    "version": "1.0.0",
                    "executable": { "runtime": { "ready_check": wait_for_peer(&first, &second) } }
                }),
            );
            write_extension(
                "second",
                serde_json::json!({
                    "name": "Second",
                    "version": "1.0.0",
                    "executable": { "runtime": { "ready_check": wait_for_peer(&second, &first) } }
                }),
            );

            let responses = readiness_api_batch(&[
                readiness_request("first", ExtensionApiReadinessMode::Probe),
                readiness_request("second", ExtensionApiReadinessMode::Probe),
            ]);

            assert!(responses.iter().all(|response| {
                response
                    .status
                    .as_ref()
                    .is_some_and(|status| status.state == ExtensionApiReadinessState::Ready)
            }));
        });
    }
}
