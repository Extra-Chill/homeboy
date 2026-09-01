//! Typed Extension API discovery and invocation for deployment providers.

use std::collections::BTreeMap;
use std::path::Path;

use homeboy_extension_contract::api::v1::{
    ExtensionApiCatalogEntryStatus, ExtensionApiCatalogRequest,
    ExtensionApiDeploymentProviderDescriptor, ExtensionApiDeploymentProviderDiagnostic,
    ExtensionApiDeploymentProviderDiagnosticKind, ExtensionApiDeploymentProviderInventoryRequest,
    ExtensionApiDeploymentProviderInventoryResponse, ExtensionApiDeploymentProviderInvokeRequest,
    ExtensionApiDeploymentProviderInvokeResponse, ExtensionApiDeploymentProviderResolveRequest,
    ExtensionApiDeploymentProviderResolveResponse, ExtensionApiDeploymentProviderResult,
    ExtensionApiDeploymentProviderValidation, ExtensionApiOperationFailure,
    DEPLOYMENT_PROVIDER_CAPABILITY_PREFIX, EXTENSION_API_CATALOG_REQUEST_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_INVENTORY_REQUEST_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_INVENTORY_RESPONSE_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_REQUEST_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_RESPONSE_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_REQUEST_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_RESPONSE_SCHEMA, EXTENSION_API_V1,
};
use homeboy_extension_contract::{DeploymentProviderManifest, ExtensionManifest};

use crate::extension::catalog::{snapshot_api, validate_operation_request};
use crate::extension::invoke::{build_exec_env, execute_extension_command, ExtensionExecutionMode};
use crate::extension::readiness::extension_ready_status;

#[derive(Debug, Clone)]
struct DeploymentProviderCandidate {
    descriptor: ExtensionApiDeploymentProviderDescriptor,
    provider: Option<DeploymentProviderManifest>,
    extension: ExtensionManifest,
}

/// One immutable provider catalog used for deployment planning and execution.
pub struct DeploymentProviderApi {
    candidates: Vec<DeploymentProviderCandidate>,
    invalid_extensions: BTreeMap<String, String>,
    failure: Option<ExtensionApiOperationFailure>,
}

pub struct DeploymentProviderInvocationContext<'a> {
    pub component_path: &'a Path,
    pub input_path: &'a Path,
}

impl DeploymentProviderApi {
    pub fn discover(request: &ExtensionApiDeploymentProviderInventoryRequest) -> Self {
        if let Some(failure) = validate_operation_request(
            &request.schema,
            EXTENSION_API_DEPLOYMENT_PROVIDER_INVENTORY_REQUEST_SCHEMA,
            request.api_version,
        ) {
            return Self {
                candidates: Vec::new(),
                invalid_extensions: BTreeMap::new(),
                failure: Some(failure),
            };
        }

        let snapshot = snapshot_api(&ExtensionApiCatalogRequest {
            schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
            api_version: request.api_version,
        });
        if let Some(failure) = snapshot.response.failure {
            return Self {
                candidates: Vec::new(),
                invalid_extensions: BTreeMap::new(),
                failure: Some(failure),
            };
        }

        let mut candidates = Vec::new();
        let mut invalid_extensions = BTreeMap::new();
        for catalog_entry in snapshot.response.entries {
            let Some(manifest) = snapshot.manifests.get(&catalog_entry.id) else {
                if let Some(diagnostic) = catalog_entry.diagnostic {
                    invalid_extensions.insert(catalog_entry.id, diagnostic.message);
                }
                continue;
            };
            let advertised_capabilities = catalog_entry
                .descriptor
                .as_ref()
                .into_iter()
                .flat_map(|descriptor| &descriptor.capabilities)
                .map(|capability| capability.id.as_str())
                .filter(|id| id.starts_with(DEPLOYMENT_PROVIDER_CAPABILITY_PREFIX))
                .collect::<Vec<_>>();
            for provider in &manifest.deployment_providers {
                let capability_advertised = advertised_capabilities.iter().any(|capability| {
                    *capability == format!("{DEPLOYMENT_PROVIDER_CAPABILITY_PREFIX}{}", provider.id)
                });
                let valid = catalog_entry.status == ExtensionApiCatalogEntryStatus::Available
                    && capability_advertised
                    && !provider.id.trim().is_empty()
                    && !provider.command.trim().is_empty();
                candidates.push(DeploymentProviderCandidate {
                    descriptor: ExtensionApiDeploymentProviderDescriptor {
                        id: provider.id.clone(),
                        owning_extension: manifest.id.clone(),
                        supports_dry_run: provider.dry_run_command.is_some(),
                        input_schema: provider
                            .layered_input
                            .as_ref()
                            .map(|layered| layered.schema.clone()),
                        target_required: provider
                            .layered_input
                            .as_ref()
                            .is_some_and(|layered| layered.target_required),
                        result_schema: provider
                            .layered_input
                            .as_ref()
                            .and_then(|layered| layered.result_schema.clone()),
                        resolvable: valid,
                        validation: if valid {
                            ExtensionApiDeploymentProviderValidation::Valid
                        } else {
                            ExtensionApiDeploymentProviderValidation::Invalid
                        },
                        diagnostic: (!valid).then(|| {
                            "Provider requires a non-empty id and command on a compatible extension."
                                .to_string()
                        }),
                    },
                    provider: valid.then_some(provider.clone()),
                    extension: manifest.as_ref().clone(),
                });
            }
        }
        mark_duplicates(&mut candidates);
        candidates.sort_by(|left, right| {
            (&left.descriptor.owning_extension, &left.descriptor.id)
                .cmp(&(&right.descriptor.owning_extension, &right.descriptor.id))
        });

        Self {
            candidates,
            invalid_extensions,
            failure: None,
        }
    }

    pub fn inventory_api(&self) -> ExtensionApiDeploymentProviderInventoryResponse {
        ExtensionApiDeploymentProviderInventoryResponse {
            schema: EXTENSION_API_DEPLOYMENT_PROVIDER_INVENTORY_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            providers: self
                .candidates
                .iter()
                .map(|candidate| candidate.descriptor.clone())
                .collect(),
            failure: self.failure.clone(),
        }
    }

    pub fn resolve_api(
        &self,
        request: &ExtensionApiDeploymentProviderResolveRequest,
    ) -> ExtensionApiDeploymentProviderResolveResponse {
        if let Some(failure) = validate_operation_request(
            &request.schema,
            EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_REQUEST_SCHEMA,
            request.api_version,
        ) {
            return resolve_failure(failure);
        }
        if let Some(failure) = self.failure.clone() {
            return resolve_failure(failure);
        }
        match self.select(&request.extension_id, &request.provider_id) {
            Ok(candidate) => ExtensionApiDeploymentProviderResolveResponse {
                schema: EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_RESPONSE_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                provider: Some(candidate.descriptor.clone()),
                diagnostic: None,
                failure: None,
            },
            Err(diagnostic) => resolve_diagnostic(diagnostic),
        }
    }

    pub fn invoke_api(
        &self,
        request: &ExtensionApiDeploymentProviderInvokeRequest,
        context: DeploymentProviderInvocationContext<'_>,
    ) -> ExtensionApiDeploymentProviderInvokeResponse {
        if let Some(failure) = validate_operation_request(
            &request.schema,
            EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_REQUEST_SCHEMA,
            request.api_version,
        ) {
            return invoke_failure(failure);
        }
        if let Some(failure) = self.failure.clone() {
            return invoke_failure(failure);
        }
        let candidate = match self.select(&request.extension_id, &request.provider_id) {
            Ok(candidate) => candidate,
            Err(diagnostic) => return invoke_diagnostic(diagnostic),
        };
        let Some(provider) = candidate.provider.as_ref() else {
            return invoke_diagnostic(diagnostic(
                request,
                ExtensionApiDeploymentProviderDiagnosticKind::Invalid,
                "The installed deployment provider declaration is invalid.",
            ));
        };
        if request.dry_run && provider.dry_run_command.is_none() {
            return invoke_diagnostic(diagnostic(
                request,
                ExtensionApiDeploymentProviderDiagnosticKind::DryRunUnsupported,
                &format!(
                    "Provider '{}' does not declare a non-mutating dry-run command",
                    request.provider_id
                ),
            ));
        }
        let readiness = extension_ready_status(&candidate.extension);
        if readiness.ready != Some(true) {
            return invoke_diagnostic(diagnostic(
                request,
                ExtensionApiDeploymentProviderDiagnosticKind::NotReady,
                readiness
                    .detail
                    .or(readiness.reason)
                    .as_deref()
                    .unwrap_or("The deployment extension is not ready."),
            ));
        }
        let Some(extension_path) = candidate.extension.extension_path.as_deref() else {
            return invoke_diagnostic(diagnostic(
                request,
                ExtensionApiDeploymentProviderDiagnosticKind::Invalid,
                "The deployment extension has no installation path.",
            ));
        };
        let Some(input_path) = context.input_path.to_str() else {
            return invoke_diagnostic(diagnostic(
                request,
                ExtensionApiDeploymentProviderDiagnosticKind::InvalidInput,
                "Deployment provider input path is not valid UTF-8.",
            ));
        };
        let Some(component_path) = context.component_path.to_str() else {
            return invoke_diagnostic(diagnostic(
                request,
                ExtensionApiDeploymentProviderDiagnosticKind::InvalidInput,
                "Deployment component path is not valid UTF-8.",
            ));
        };

        let command = if request.dry_run {
            provider
                .dry_run_command
                .as_deref()
                .expect("dry-run command checked above")
        } else {
            &provider.command
        };
        let quoted_input = homeboy_engine_primitives::shell::quote_path(input_path);
        let execution = match execute_extension_command(
            command,
            &[
                ("extension_path", extension_path),
                ("payload.contract", &quoted_input),
            ],
            Some(extension_path),
            &build_exec_env(
                &request.extension_id,
                Some(&request.project_id),
                Some(&request.component_id),
                "{}",
                Some(extension_path),
                None,
                None,
                Some(component_path),
            ),
            ExtensionExecutionMode::Captured,
        ) {
            Ok(execution) => execution,
            Err(error) => {
                return invoke_diagnostic(diagnostic(
                    request,
                    ExtensionApiDeploymentProviderDiagnosticKind::ExecutionFailed,
                    &error.to_string(),
                ));
            }
        };
        let evidence =
            provider_evidence(&execution.output.stdout, &execution.output.stderr, provider);

        ExtensionApiDeploymentProviderInvokeResponse {
            schema: EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            result: Some(ExtensionApiDeploymentProviderResult {
                exit_code: execution.exit_code,
                evidence,
                error: (execution.exit_code != 0).then(|| {
                    if provider.layered_input.is_some() {
                        "Deployment provider failed".to_string()
                    } else {
                        format!("{}{}", execution.output.stdout, execution.output.stderr)
                    }
                }),
            }),
            diagnostic: None,
            failure: None,
        }
    }

    fn select(
        &self,
        extension_id: &str,
        provider_id: &str,
    ) -> Result<&DeploymentProviderCandidate, ExtensionApiDeploymentProviderDiagnostic> {
        let matches = self
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.descriptor.owning_extension == extension_id
                    && candidate.descriptor.id == provider_id
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [candidate]
                if candidate.descriptor.validation
                    == ExtensionApiDeploymentProviderValidation::Valid =>
            {
                Ok(*candidate)
            }
            [] if self.invalid_extensions.contains_key(extension_id) => Err(provider_diagnostic(
                extension_id,
                provider_id,
                ExtensionApiDeploymentProviderDiagnosticKind::Invalid,
                self.invalid_extensions
                    .get(extension_id)
                    .expect("invalid extension checked"),
            )),
            [] => Err(provider_diagnostic(
                extension_id,
                provider_id,
                ExtensionApiDeploymentProviderDiagnosticKind::Unknown,
                &format!(
                    "Extension '{extension_id}' does not declare deployment provider '{provider_id}'"
                ),
            )),
            [candidate]
                if candidate.descriptor.validation
                    == ExtensionApiDeploymentProviderValidation::Duplicate =>
            {
                Err(provider_diagnostic(
                    extension_id,
                    provider_id,
                    ExtensionApiDeploymentProviderDiagnosticKind::Ambiguous,
                    "The extension declares this deployment provider more than once.",
                ))
            }
            [candidate] => Err(provider_diagnostic(
                extension_id,
                provider_id,
                ExtensionApiDeploymentProviderDiagnosticKind::Invalid,
                candidate
                    .descriptor
                    .diagnostic
                    .as_deref()
                    .unwrap_or("The deployment provider declaration is invalid."),
            )),
            _ => Err(provider_diagnostic(
                extension_id,
                provider_id,
                ExtensionApiDeploymentProviderDiagnosticKind::Ambiguous,
                "The extension declares this deployment provider more than once.",
            )),
        }
    }
}

fn mark_duplicates(candidates: &mut [DeploymentProviderCandidate]) {
    let counts = candidates
        .iter()
        .fold(BTreeMap::new(), |mut counts, candidate| {
            *counts
                .entry((
                    candidate.descriptor.owning_extension.clone(),
                    candidate.descriptor.id.clone(),
                ))
                .or_insert(0usize) += 1;
            counts
        });
    for candidate in candidates {
        let key = (
            candidate.descriptor.owning_extension.clone(),
            candidate.descriptor.id.clone(),
        );
        if counts.get(&key).copied().unwrap_or_default() > 1 {
            candidate.descriptor.resolvable = false;
            candidate.descriptor.validation = ExtensionApiDeploymentProviderValidation::Duplicate;
            candidate.descriptor.diagnostic =
                Some("The extension declares this provider ID more than once.".to_string());
            candidate.provider = None;
        }
    }
}

fn provider_evidence(
    stdout: &str,
    stderr: &str,
    provider: &DeploymentProviderManifest,
) -> serde_json::Value {
    let Some(layered) = provider.layered_input.as_ref() else {
        return serde_json::from_str(stdout).unwrap_or_else(|_| {
            serde_json::json!({ "status": "unstructured", "output": format!("{stdout}{stderr}") })
        });
    };
    let Some(expected_schema) = layered.result_schema.as_deref() else {
        return serde_json::json!({ "status": "opaque" });
    };
    serde_json::from_str::<serde_json::Value>(stdout)
        .ok()
        .filter(|value| {
            value.get("schema").and_then(serde_json::Value::as_str) == Some(expected_schema)
        })
        .unwrap_or_else(|| serde_json::json!({ "status": "opaque" }))
}

fn diagnostic(
    request: &ExtensionApiDeploymentProviderInvokeRequest,
    kind: ExtensionApiDeploymentProviderDiagnosticKind,
    message: &str,
) -> ExtensionApiDeploymentProviderDiagnostic {
    provider_diagnostic(&request.extension_id, &request.provider_id, kind, message)
}

fn provider_diagnostic(
    extension_id: &str,
    provider_id: &str,
    kind: ExtensionApiDeploymentProviderDiagnosticKind,
    message: &str,
) -> ExtensionApiDeploymentProviderDiagnostic {
    ExtensionApiDeploymentProviderDiagnostic {
        extension_id: extension_id.to_string(),
        provider_id: provider_id.to_string(),
        kind,
        message: message.to_string(),
    }
}

fn resolve_failure(
    failure: ExtensionApiOperationFailure,
) -> ExtensionApiDeploymentProviderResolveResponse {
    ExtensionApiDeploymentProviderResolveResponse {
        schema: EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        provider: None,
        diagnostic: None,
        failure: Some(failure),
    }
}

fn resolve_diagnostic(
    diagnostic: ExtensionApiDeploymentProviderDiagnostic,
) -> ExtensionApiDeploymentProviderResolveResponse {
    ExtensionApiDeploymentProviderResolveResponse {
        schema: EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        provider: None,
        diagnostic: Some(diagnostic),
        failure: None,
    }
}

fn invoke_failure(
    failure: ExtensionApiOperationFailure,
) -> ExtensionApiDeploymentProviderInvokeResponse {
    ExtensionApiDeploymentProviderInvokeResponse {
        schema: EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        result: None,
        diagnostic: None,
        failure: Some(failure),
    }
}

fn invoke_diagnostic(
    diagnostic: ExtensionApiDeploymentProviderDiagnostic,
) -> ExtensionApiDeploymentProviderInvokeResponse {
    ExtensionApiDeploymentProviderInvokeResponse {
        schema: EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        result: None,
        diagnostic: Some(diagnostic),
        failure: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_extension_contract::api::v1::{
        ExtensionApiDeploymentProviderResolveRequest, EXTENSION_API_CATALOG_REQUEST_SCHEMA,
    };

    fn write_extension(id: &str, providers: serde_json::Value, script: &str) {
        let extension = crate::paths::extensions()
            .expect("extensions root")
            .join(id);
        std::fs::create_dir_all(&extension).expect("extension directory");
        std::fs::write(
            extension.join(format!("{id}.json")),
            serde_json::json!({
                "name": id,
                "version": "1.0.0",
                "deployment_providers": providers,
            })
            .to_string(),
        )
        .expect("manifest");
        std::fs::write(extension.join("run.sh"), script).expect("provider script");
    }

    fn discover() -> DeploymentProviderApi {
        DeploymentProviderApi::discover(&ExtensionApiDeploymentProviderInventoryRequest {
            schema: EXTENSION_API_DEPLOYMENT_PROVIDER_INVENTORY_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
        })
    }

    fn resolve(
        api: &DeploymentProviderApi,
        extension_id: &str,
        provider_id: &str,
    ) -> ExtensionApiDeploymentProviderResolveResponse {
        api.resolve_api(&ExtensionApiDeploymentProviderResolveRequest {
            schema: EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id: extension_id.to_string(),
            provider_id: provider_id.to_string(),
        })
    }

    fn invoke(
        api: &DeploymentProviderApi,
        extension_id: &str,
        provider_id: &str,
        input_path: &std::path::Path,
        component_path: &std::path::Path,
        dry_run: bool,
    ) -> ExtensionApiDeploymentProviderInvokeResponse {
        api.invoke_api(
            &ExtensionApiDeploymentProviderInvokeRequest {
                schema: EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: extension_id.to_string(),
                provider_id: provider_id.to_string(),
                project_id: "site".to_string(),
                component_id: "fixture".to_string(),
                dry_run,
            },
            DeploymentProviderInvocationContext {
                component_path,
                input_path,
            },
        )
    }

    #[test]
    fn inventory_is_safe_and_capability_references_invocation_schemas() {
        homeboy_core::test_support::with_isolated_home(|_| {
            write_extension(
                "fixture-provider",
                serde_json::json!([{
                    "id": "fixture.deploy",
                    "command": "sh {{extension_path}}/run.sh apply {{payload.contract}}",
                    "dry_run_command": "sh {{extension_path}}/run.sh validate {{payload.contract}}",
                    "layered_input": {
                        "schema": "homeboy/deployment-provider-payload/v1",
                        "target_required": true,
                        "result_schema": "fixture/result/v1"
                    }
                }]),
                "#!/bin/sh\nexit 0\n",
            );

            let api = discover();
            let inventory = api.inventory_api();
            assert!(inventory.failure.is_none());
            assert_eq!(inventory.providers.len(), 1);
            assert!(inventory.providers[0].supports_dry_run);
            assert!(inventory.providers[0].target_required);
            assert_eq!(
                inventory.providers[0].result_schema.as_deref(),
                Some("fixture/result/v1")
            );
            let wire = serde_json::to_value(inventory).expect("inventory JSON");
            let provider = &wire["providers"][0];
            for private in [
                "command",
                "dry_run_command",
                "extension_path",
                "environment",
            ] {
                assert!(provider.get(private).is_none(), "leaked {private}");
            }

            let catalog = snapshot_api(&ExtensionApiCatalogRequest {
                schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
            })
            .response;
            let capability = catalog.entries[0]
                .descriptor
                .as_ref()
                .expect("descriptor")
                .capabilities
                .iter()
                .find(|capability| capability.id == "deployment-provider.fixture.deploy")
                .expect("deployment capability");
            assert_eq!(
                capability.input_schema.as_ref().expect("input").schema,
                EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_REQUEST_SCHEMA
            );
            assert_eq!(
                capability.output_schema.as_ref().expect("output").schema,
                EXTENSION_API_DEPLOYMENT_PROVIDER_INVOKE_RESPONSE_SCHEMA
            );
        });
    }

    #[test]
    fn immutable_session_uses_the_discovered_dry_run_command() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let providers = |dry_run: &str| {
                serde_json::json!([{
                    "id": "fixture.deploy",
                    "command": "sh {{extension_path}}/run.sh apply {{payload.contract}}",
                    "dry_run_command": format!("sh {{{{extension_path}}}}/run.sh {dry_run} {{{{payload.contract}}}}")
                }])
            };
            write_extension(
                "fixture-provider",
                providers("first"),
                "#!/bin/sh\nprintf '%s|%s' \"$1\" \"$HOMEBOY_COMPONENT_PATH\"\n",
            );
            let api = discover();
            write_extension(
                "fixture-provider",
                providers("second"),
                "#!/bin/sh\nprintf '%s|%s' \"$1\" \"$HOMEBOY_COMPONENT_PATH\"\n",
            );
            let input = tempfile::NamedTempFile::new().expect("input");
            let component = tempfile::tempdir().expect("component");

            let response = invoke(
                &api,
                "fixture-provider",
                "fixture.deploy",
                input.path(),
                component.path(),
                true,
            );
            let result = response.result.expect("provider result");
            assert_eq!(result.exit_code, 0);
            assert_eq!(result.evidence["status"], "unstructured");
            assert_eq!(
                result.evidence["output"],
                format!("first|{}", component.path().display())
            );
        });
    }

    #[test]
    fn duplicate_provider_ids_are_deterministically_unresolvable() {
        homeboy_core::test_support::with_isolated_home(|_| {
            write_extension(
                "fixture-provider",
                serde_json::json!([
                    { "id": "fixture.deploy", "command": "true" },
                    { "id": "fixture.deploy", "command": "true" }
                ]),
                "",
            );

            let api = discover();
            let inventory = api.inventory_api();
            assert_eq!(inventory.providers.len(), 2);
            assert!(inventory.providers.iter().all(|provider| {
                provider.validation == ExtensionApiDeploymentProviderValidation::Duplicate
                    && !provider.resolvable
            }));
            assert_eq!(
                resolve(&api, "fixture-provider", "fixture.deploy")
                    .diagnostic
                    .expect("duplicate diagnostic")
                    .kind,
                ExtensionApiDeploymentProviderDiagnosticKind::Ambiguous
            );
        });
    }

    #[test]
    fn layered_output_is_projected_only_when_its_schema_matches() {
        homeboy_core::test_support::with_isolated_home(|_| {
            write_extension(
                "fixture-provider",
                serde_json::json!([{
                    "id": "fixture.deploy",
                    "command": "sh {{extension_path}}/run.sh leak {{payload.contract}}",
                    "layered_input": {
                        "schema": "homeboy/deployment-provider-payload/v1",
                        "result_schema": "fixture/result/v1"
                    }
                }]),
                "#!/bin/sh\ncat \"$2\"\n",
            );
            let api = discover();
            let mut input = tempfile::NamedTempFile::new().expect("input");
            use std::io::Write;
            write!(
                input,
                "{{\"schema\":\"wrong\",\"secret\":\"private-target\"}}"
            )
            .expect("payload");
            let component = tempfile::tempdir().expect("component");

            let response = invoke(
                &api,
                "fixture-provider",
                "fixture.deploy",
                input.path(),
                component.path(),
                false,
            );
            let result = response.result.expect("provider result");
            assert_eq!(result.evidence, serde_json::json!({ "status": "opaque" }));
            assert!(!serde_json::to_string(&result)
                .unwrap()
                .contains("private-target"));
        });
    }
}
