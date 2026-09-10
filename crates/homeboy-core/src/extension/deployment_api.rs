//! Typed Extension API discovery and invocation for deployment providers.

use std::collections::BTreeMap;
use std::path::Path;

use homeboy_control_plane_contract::{
    ControlPlaneAction, ControlPlaneActionFence, ControlPlaneActionIntent,
    ControlPlaneActionPayload, ControlPlaneActionRequest, ControlPlaneActionResource,
    ControlPlaneEffectAudit, ControlPlaneEffectTerminal, ControlPlaneRef, RunId,
    CONTROL_PLANE_ACTION_FENCE_SCHEMA, CONTROL_PLANE_ACTION_INTENT_SCHEMA,
    CONTROL_PLANE_ACTION_REQUEST_SCHEMA, CONTROL_PLANE_EFFECT_AUDIT_SCHEMA,
    CONTROL_PLANE_EFFECT_TERMINAL_SCHEMA,
};
use homeboy_extension_contract::api::v1::{
    ExtensionApiCatalogEntryStatus, ExtensionApiCatalogRequest,
    ExtensionApiDeploymentProviderDescriptor, ExtensionApiDeploymentProviderDiagnostic,
    ExtensionApiDeploymentProviderDiagnosticKind, ExtensionApiDeploymentProviderEffectState,
    ExtensionApiDeploymentProviderInventoryRequest,
    ExtensionApiDeploymentProviderInventoryResponse, ExtensionApiDeploymentProviderResolveRequest,
    ExtensionApiDeploymentProviderResolveResponse, ExtensionApiDeploymentProviderResult,
    ExtensionApiDeploymentProviderStatusRequest, ExtensionApiDeploymentProviderStatusResponse,
    ExtensionApiDeploymentProviderSubmitRequest, ExtensionApiDeploymentProviderSubmitResponse,
    ExtensionApiDeploymentProviderValidation, ExtensionApiOperationFailure,
    DEPLOYMENT_PROVIDER_CAPABILITY_PREFIX, EXTENSION_API_CATALOG_REQUEST_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_INVENTORY_REQUEST_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_INVENTORY_RESPONSE_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_REQUEST_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_RESOLVE_RESPONSE_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_REQUEST_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_RESPONSE_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_REQUEST_SCHEMA,
    EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_RESPONSE_SCHEMA, EXTENSION_API_V1,
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

    pub fn submit_api(
        &self,
        request: &ExtensionApiDeploymentProviderSubmitRequest,
        context: DeploymentProviderInvocationContext<'_>,
    ) -> ExtensionApiDeploymentProviderSubmitResponse {
        if let Some(failure) = validate_operation_request(
            &request.schema,
            EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_REQUEST_SCHEMA,
            request.api_version,
        ) {
            return submit_failure(request, failure);
        }
        if let Some(failure) = self.failure.clone() {
            return submit_failure(request, failure);
        }
        let effect = match admit_provider_effect(request) {
            Ok(effect) => effect,
            Err(error) if error.message.contains("different action intent") => {
                return submit_diagnostic(
                    request,
                    diagnostic(
                        request,
                        ExtensionApiDeploymentProviderDiagnosticKind::Conflict,
                        "effect id was already submitted with a different immutable request",
                    ),
                )
            }
            Err(error) => return submit_failure(request, internal_failure(error.to_string())),
        };
        if effect.recovery_required {
            return provider_effect_response(
                request,
                ExtensionApiDeploymentProviderEffectState::Unknown,
                None,
            );
        }
        if let Some(terminal) = effect.terminal {
            return provider_terminal_response(request, &terminal);
        }
        let now = chrono::Utc::now();
        let lease =
            match crate::observation::ObservationStore::open_initialized().and_then(|store| {
                store.lease_control_plane_effect_by_id(
                    &request.effect_id,
                    &format!("deployment-provider:{}", std::process::id()),
                    &now.to_rfc3339(),
                    &(now + chrono::Duration::seconds(30)).to_rfc3339(),
                )
            }) {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    return provider_effect_response(
                        request,
                        ExtensionApiDeploymentProviderEffectState::Running,
                        None,
                    )
                }
                Err(error) => return submit_failure(request, internal_failure(error.to_string())),
            };
        let candidate = match self.select(&request.extension_id, &request.provider_id) {
            Ok(candidate) => candidate,
            Err(diagnostic) => return submit_diagnostic(request, diagnostic),
        };
        let Some(provider) = candidate.provider.as_ref() else {
            return submit_diagnostic(
                request,
                diagnostic(
                    request,
                    ExtensionApiDeploymentProviderDiagnosticKind::Invalid,
                    "The installed deployment provider declaration is invalid.",
                ),
            );
        };
        if request.dry_run && provider.dry_run_command.is_none() {
            return submit_diagnostic(
                request,
                diagnostic(
                    request,
                    ExtensionApiDeploymentProviderDiagnosticKind::DryRunUnsupported,
                    &format!(
                        "Provider '{}' does not declare a non-mutating dry-run command",
                        request.provider_id
                    ),
                ),
            );
        }
        let readiness = extension_ready_status(&candidate.extension);
        if readiness.ready != Some(true) {
            return submit_diagnostic(
                request,
                diagnostic(
                    request,
                    ExtensionApiDeploymentProviderDiagnosticKind::NotReady,
                    readiness
                        .detail
                        .or(readiness.reason)
                        .as_deref()
                        .unwrap_or("The deployment extension is not ready."),
                ),
            );
        }
        let Some(extension_path) = candidate.extension.extension_path.as_deref() else {
            return submit_diagnostic(
                request,
                diagnostic(
                    request,
                    ExtensionApiDeploymentProviderDiagnosticKind::Invalid,
                    "The deployment extension has no installation path.",
                ),
            );
        };
        let Some(input_path) = context.input_path.to_str() else {
            return submit_diagnostic(
                request,
                diagnostic(
                    request,
                    ExtensionApiDeploymentProviderDiagnosticKind::InvalidInput,
                    "Deployment provider input path is not valid UTF-8.",
                ),
            );
        };
        let Some(component_path) = context.component_path.to_str() else {
            return submit_diagnostic(
                request,
                diagnostic(
                    request,
                    ExtensionApiDeploymentProviderDiagnosticKind::InvalidInput,
                    "Deployment component path is not valid UTF-8.",
                ),
            );
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
                return submit_diagnostic(
                    request,
                    diagnostic(
                        request,
                        ExtensionApiDeploymentProviderDiagnosticKind::ExecutionFailed,
                        &error.to_string(),
                    ),
                );
            }
        };
        let evidence =
            provider_evidence(&execution.output.stdout, &execution.output.stderr, provider);

        let result = ExtensionApiDeploymentProviderResult {
            exit_code: execution.exit_code,
            evidence,
            error: (execution.exit_code != 0).then(|| {
                if provider.layered_input.is_some() {
                    "Deployment provider failed".to_string()
                } else {
                    format!("{}{}", execution.output.stdout, execution.output.stderr)
                }
            }),
        };
        if let Err(error) = terminalize_provider_effect(request, &lease, result.clone()) {
            return submit_failure(request, internal_failure(error.to_string()));
        }
        provider_effect_response(
            request,
            if result.exit_code == 0 {
                ExtensionApiDeploymentProviderEffectState::Succeeded
            } else {
                ExtensionApiDeploymentProviderEffectState::Failed
            },
            Some(result),
        )
    }

    pub fn status_api(
        &self,
        request: &ExtensionApiDeploymentProviderStatusRequest,
    ) -> ExtensionApiDeploymentProviderStatusResponse {
        if let Some(failure) = validate_operation_request(
            &request.schema,
            EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_REQUEST_SCHEMA,
            request.api_version,
        ) {
            return status_failure(request, failure);
        }
        provider_effect_status(request)
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
    request: &ExtensionApiDeploymentProviderSubmitRequest,
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

fn submit_failure(
    request: &ExtensionApiDeploymentProviderSubmitRequest,
    failure: ExtensionApiOperationFailure,
) -> ExtensionApiDeploymentProviderSubmitResponse {
    ExtensionApiDeploymentProviderSubmitResponse {
        schema: EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        effect_id: request.effect_id.clone(),
        state: ExtensionApiDeploymentProviderEffectState::Unknown,
        result: None,
        diagnostic: None,
        failure: Some(failure),
    }
}

fn submit_diagnostic(
    request: &ExtensionApiDeploymentProviderSubmitRequest,
    diagnostic: ExtensionApiDeploymentProviderDiagnostic,
) -> ExtensionApiDeploymentProviderSubmitResponse {
    ExtensionApiDeploymentProviderSubmitResponse {
        schema: EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        effect_id: request.effect_id.clone(),
        state: ExtensionApiDeploymentProviderEffectState::Unknown,
        result: None,
        diagnostic: Some(diagnostic),
        failure: None,
    }
}

fn provider_effect_response(
    request: &ExtensionApiDeploymentProviderSubmitRequest,
    state: ExtensionApiDeploymentProviderEffectState,
    result: Option<ExtensionApiDeploymentProviderResult>,
) -> ExtensionApiDeploymentProviderSubmitResponse {
    ExtensionApiDeploymentProviderSubmitResponse {
        schema: EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        effect_id: request.effect_id.clone(),
        state,
        result,
        diagnostic: None,
        failure: None,
    }
}

fn provider_effect_status(
    request: &ExtensionApiDeploymentProviderStatusRequest,
) -> ExtensionApiDeploymentProviderStatusResponse {
    let store = match crate::observation::ObservationStore::open_initialized() {
        Ok(store) => store,
        Err(error) => return status_failure(request, internal_failure(error.to_string())),
    };
    if let Err(error) = store.expire_control_plane_effect_leases(&chrono::Utc::now().to_rfc3339()) {
        return status_failure(request, internal_failure(error.to_string()));
    }
    match store.control_plane_effect_status(&request.effect_id) {
        Ok(None) => ExtensionApiDeploymentProviderStatusResponse { schema: EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_RESPONSE_SCHEMA.to_string(), api_version: EXTENSION_API_V1, effect_id: request.effect_id.clone(), state: ExtensionApiDeploymentProviderEffectState::NotStarted, result: None, message: None, failure: None },
        Ok(Some(effect)) if effect.recovery_required => ExtensionApiDeploymentProviderStatusResponse { schema: EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_RESPONSE_SCHEMA.to_string(), api_version: EXTENSION_API_V1, effect_id: request.effect_id.clone(), state: ExtensionApiDeploymentProviderEffectState::Unknown, result: None, message: Some("provider execution lease expired; authoritative provider reconciliation is required".to_string()), failure: None },
        Ok(Some(effect)) => match effect.terminal {
            Some(terminal) => provider_terminal_status(request, &terminal),
            None => ExtensionApiDeploymentProviderStatusResponse { schema: EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_RESPONSE_SCHEMA.to_string(), api_version: EXTENSION_API_V1, effect_id: request.effect_id.clone(), state: ExtensionApiDeploymentProviderEffectState::Running, result: None, message: None, failure: None },
        },
        Err(error) => status_failure(request, internal_failure(error.to_string())),
    }
}

fn provider_terminal_status(
    request: &ExtensionApiDeploymentProviderStatusRequest,
    terminal: &ControlPlaneEffectTerminal,
) -> ExtensionApiDeploymentProviderStatusResponse {
    let result = serde_json::from_value(terminal.acknowledgement.result.data.clone()).ok();
    ExtensionApiDeploymentProviderStatusResponse {
        schema: EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        effect_id: request.effect_id.clone(),
        state: if terminal.acknowledgement.outcome
            == homeboy_control_plane_contract::ControlPlaneActionOutcome::Succeeded
        {
            ExtensionApiDeploymentProviderEffectState::Succeeded
        } else {
            ExtensionApiDeploymentProviderEffectState::Failed
        },
        result,
        message: terminal.acknowledgement.message.clone(),
        failure: None,
    }
}

fn provider_terminal_response(
    request: &ExtensionApiDeploymentProviderSubmitRequest,
    terminal: &ControlPlaneEffectTerminal,
) -> ExtensionApiDeploymentProviderSubmitResponse {
    provider_effect_response(
        request,
        if terminal.acknowledgement.outcome
            == homeboy_control_plane_contract::ControlPlaneActionOutcome::Succeeded
        {
            ExtensionApiDeploymentProviderEffectState::Succeeded
        } else {
            ExtensionApiDeploymentProviderEffectState::Failed
        },
        serde_json::from_value(terminal.acknowledgement.result.data.clone()).ok(),
    )
}

fn admit_provider_effect(
    request: &ExtensionApiDeploymentProviderSubmitRequest,
) -> crate::error::Result<crate::observation::store::ControlPlaneEffectStatus> {
    let store = crate::observation::ObservationStore::open_initialized()?;
    let resource_id = format!(
        "deployment-provider-{}",
        homeboy_engine_primitives::content_hash::sha256_hex(request.effect_id.0.as_bytes())
    );
    let run = RunId::new(resource_id.clone()).map_err(|error| {
        crate::Error::validation_invalid_argument("effect_id", error.to_string(), None, None)
    })?;
    let request_digest = homeboy_engine_primitives::content_hash::sha256_hex(
        &serde_json::to_vec(request)
            .map_err(|error| crate::Error::internal_json(error.to_string(), None))?,
    );
    let projection = crate::observation::ControlPlaneResourceProjection {
        resource_type: "deployment_provider_effect".to_string(),
        resource_id: resource_id.clone(),
        version: request_digest.clone(),
        state: "admitted".to_string(),
        aliases: vec![resource_id.clone()],
        eligibility: serde_json::json!({"provider": request.provider_id}),
        provenance: serde_json::json!({"source": "deployment_provider", "request": request}),
    };
    // The action-outbox foreign key binds every effect to an observation run.
    // Provider effects use this SQLite-owned adapter row, not another ledger.
    store.upsert_imported_run_with_resource_projection(
        &crate::observation::RunRecord {
            id: resource_id.clone(),
            kind: "deployment-provider-effect".to_string(),
            component_id: Some(request.component_id.clone()),
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
            status: "running".to_string(),
            command: Some("deployment provider".to_string()),
            cwd: None,
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: serde_json::json!({ "request": request }),
        },
        &projection,
        true,
    )?;
    let action_request = ControlPlaneActionRequest {
        schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
        effect_id: request.effect_id.clone(),
        action: ControlPlaneAction::Resume,
        idempotency_key: request.effect_id.0.clone(),
        actor: "deployment-provider".to_string(),
        expected_updated_at: None,
        parameters: ControlPlaneActionPayload::empty(),
        confirmed: false,
    };
    let intent = ControlPlaneActionIntent {
        schema: CONTROL_PLANE_ACTION_INTENT_SCHEMA.to_string(),
        effect_id: request.effect_id.clone(),
        resource: ControlPlaneActionResource {
            resource: ControlPlaneRef::Run(run.clone()),
            run,
            original_alias: None,
        },
        request: action_request,
        request_digest: request_digest.clone(),
        accepted_at: chrono::Utc::now().to_rfc3339(),
    };
    let fence = ControlPlaneActionFence {
        schema: CONTROL_PLANE_ACTION_FENCE_SCHEMA.to_string(),
        resource_updated_at: request_digest.clone(),
        eligible: true,
        reason: None,
    };
    match store.enqueue_control_plane_action_intent(
        &intent,
        &fence,
        "deployment_provider_effect",
        &request_digest,
    )? {
        crate::observation::store::ControlPlaneEffectAdmission::Enqueued(effect)
        | crate::observation::store::ControlPlaneEffectAdmission::Duplicate(effect) => Ok(effect),
    }
}

fn terminalize_provider_effect(
    request: &ExtensionApiDeploymentProviderSubmitRequest,
    lease: &crate::observation::store::ControlPlaneEffectStatus,
    result: ExtensionApiDeploymentProviderResult,
) -> crate::error::Result<()> {
    let store = crate::observation::ObservationStore::open_initialized()?;
    let completed_at = chrono::Utc::now().to_rfc3339();
    let run = lease.intent.resource.run.clone();
    let acknowledgement = homeboy_control_plane_contract::ControlPlaneActionAcknowledgement {
        schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA
            .to_string(),
        acknowledgement: format!("{}:provider-effect", request.effect_id.0),
        run: run.clone(),
        action: ControlPlaneAction::Resume,
        idempotency_key: request.effect_id.0.clone(),
        actor: "deployment-provider".to_string(),
        accepted_at: lease.intent.accepted_at.clone(),
        completed_at: completed_at.clone(),
        outcome: if result.exit_code == 0 {
            homeboy_control_plane_contract::ControlPlaneActionOutcome::Succeeded
        } else {
            homeboy_control_plane_contract::ControlPlaneActionOutcome::Failed
        },
        resource: homeboy_control_plane_contract::ControlPlaneRun::new(run),
        result: ControlPlaneActionPayload {
            schema: homeboy_control_plane_contract::CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA
                .to_string(),
            data: serde_json::to_value(&result)
                .map_err(|error| crate::Error::internal_json(error.to_string(), None))?,
        },
        message: result.error.clone(),
    };
    store.terminalize_control_plane_effect(
        &request.effect_id,
        lease.lease_fence,
        &ControlPlaneEffectTerminal {
            schema: CONTROL_PLANE_EFFECT_TERMINAL_SCHEMA.to_string(),
            completed_at: completed_at.clone(),
            acknowledgement,
            audit: ControlPlaneEffectAudit {
                schema: CONTROL_PLANE_EFFECT_AUDIT_SCHEMA.to_string(),
                observed_at: completed_at,
                evidence: serde_json::json!({"provider_result": result}),
            },
        },
    )?;
    Ok(())
}

fn status_failure(
    request: &ExtensionApiDeploymentProviderStatusRequest,
    failure: ExtensionApiOperationFailure,
) -> ExtensionApiDeploymentProviderStatusResponse {
    ExtensionApiDeploymentProviderStatusResponse {
        schema: EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        effect_id: request.effect_id.clone(),
        state: ExtensionApiDeploymentProviderEffectState::Unknown,
        result: None,
        message: None,
        failure: Some(failure),
    }
}

fn internal_failure(message: String) -> ExtensionApiOperationFailure {
    ExtensionApiOperationFailure {
        code: homeboy_extension_contract::api::v1::ExtensionApiOperationFailureCode::CapabilityExecutionFailed,
        message,
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

    fn submit(
        api: &DeploymentProviderApi,
        extension_id: &str,
        provider_id: &str,
        input_path: &std::path::Path,
        component_path: &std::path::Path,
        dry_run: bool,
    ) -> ExtensionApiDeploymentProviderSubmitResponse {
        api.submit_api(
            &ExtensionApiDeploymentProviderSubmitRequest {
                schema: EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: extension_id.to_string(),
                provider_id: provider_id.to_string(),
                effect_id: homeboy_control_plane_contract::EffectId(format!(
                    "test:{extension_id}:{provider_id}:{dry_run}"
                )),
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
                EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_REQUEST_SCHEMA
            );
            assert_eq!(
                capability.output_schema.as_ref().expect("output").schema,
                EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_RESPONSE_SCHEMA
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

            let response = submit(
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

            let response = submit(
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

    #[test]
    fn effect_submission_is_idempotent_conflict_safe_and_status_authoritative() {
        homeboy_core::test_support::with_isolated_home(|context| {
            let counter = context.path().join("provider-calls");
            write_extension(
                "fixture-provider",
                serde_json::json!([{
                    "id": "fixture.deploy",
                    "command": format!("sh {{{{extension_path}}}}/run.sh {}", counter.display())
                }]),
                "#!/bin/sh\nprintf 'call\\n' >> \"$1\"\nprintf '{\"schema\":\"fixture/result/v1\"}'\n",
            );
            let api = std::sync::Arc::new(discover());
            let input = tempfile::NamedTempFile::new().expect("input");
            let component = tempfile::tempdir().expect("component");
            let request = ExtensionApiDeploymentProviderSubmitRequest {
                schema: EXTENSION_API_DEPLOYMENT_PROVIDER_SUBMIT_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                extension_id: "fixture-provider".to_string(),
                provider_id: "fixture.deploy".to_string(),
                effect_id: homeboy_control_plane_contract::EffectId(
                    "test:concurrent-submit".to_string(),
                ),
                project_id: "site".to_string(),
                component_id: "fixture".to_string(),
                dry_run: false,
            };
            std::thread::scope(|scope| {
                let first = scope.spawn(|| {
                    api.submit_api(
                        &request,
                        DeploymentProviderInvocationContext {
                            component_path: component.path(),
                            input_path: input.path(),
                        },
                    )
                });
                let second = scope.spawn(|| {
                    api.submit_api(
                        &request,
                        DeploymentProviderInvocationContext {
                            component_path: component.path(),
                            input_path: input.path(),
                        },
                    )
                });
                let first = first.join().expect("first submit").state;
                let second = second.join().expect("second submit").state;
                assert!(matches!(
                    first,
                    ExtensionApiDeploymentProviderEffectState::Succeeded
                        | ExtensionApiDeploymentProviderEffectState::Running
                ));
                assert!(matches!(
                    second,
                    ExtensionApiDeploymentProviderEffectState::Succeeded
                        | ExtensionApiDeploymentProviderEffectState::Running
                ));
            });
            assert_eq!(
                std::fs::read_to_string(&counter)
                    .expect("external calls")
                    .lines()
                    .count(),
                1
            );

            let status = api.status_api(&ExtensionApiDeploymentProviderStatusRequest {
                schema: EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                effect_id: request.effect_id.clone(),
            });
            assert_eq!(
                status.state,
                ExtensionApiDeploymentProviderEffectState::Succeeded
            );
            assert_eq!(status.result.expect("durable result").exit_code, 0);

            let mut mismatch = request.clone();
            mismatch.component_id = "other".to_string();
            assert_eq!(
                api.submit_api(
                    &mismatch,
                    DeploymentProviderInvocationContext {
                        component_path: component.path(),
                        input_path: input.path()
                    }
                )
                .diagnostic
                .expect("conflict")
                .kind,
                ExtensionApiDeploymentProviderDiagnosticKind::Conflict
            );

            let unknown = homeboy_control_plane_contract::EffectId("test:unknown".to_string());
            assert_eq!(
                api.status_api(&ExtensionApiDeploymentProviderStatusRequest {
                    schema: EXTENSION_API_DEPLOYMENT_PROVIDER_STATUS_REQUEST_SCHEMA.to_string(),
                    api_version: EXTENSION_API_V1,
                    effect_id: unknown
                })
                .state,
                ExtensionApiDeploymentProviderEffectState::NotStarted
            );

            write_extension(
                "failed-provider",
                serde_json::json!([{ "id": "fixture.deploy", "command": "false" }]),
                "",
            );
            let failed = discover().submit_api(
                &ExtensionApiDeploymentProviderSubmitRequest {
                    extension_id: "failed-provider".to_string(),
                    effect_id: homeboy_control_plane_contract::EffectId("test:failed".to_string()),
                    ..request
                },
                DeploymentProviderInvocationContext {
                    component_path: component.path(),
                    input_path: input.path(),
                },
            );
            assert_eq!(
                failed.state,
                ExtensionApiDeploymentProviderEffectState::Failed
            );
            assert_eq!(failed.result.expect("failed result").exit_code, 1);
        });
    }
}
