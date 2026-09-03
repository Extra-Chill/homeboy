use serde::{Deserialize, Serialize};

use super::{
    effective_provider_config, executor::effective_provider_for_request,
    provider_credential_readiness, readiness_verdict_with_credentials_and_deadline,
    resolve_provider_for_backend, runtime_readiness::provider_requires_live_auth_validation,
    validate_provider_immediate_failure_patterns, AgentTaskProviderCatalog, ProviderResolution,
    ProviderRuntimeReadinessCache,
};
use crate::agent_task_scheduler::{
    AgentTaskPlan, AgentTaskScheduleSupport, ProviderRouteDiagnosticData, ProviderRouteEvidence,
};
use serde_json::Value;

/// One redacted, precedence-ordered answer to whether a provider can accept
/// work. This is deliberately owned below the CLI so inventory and Cook cannot
/// drift into separate definitions of "ready".
#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskProviderDispatchability {
    pub state: &'static str,
    pub ready: bool,
    pub reason: String,
    pub checks: AgentTaskProviderDispatchabilityChecks,
    /// Structural routing and live inference answer different questions. Keep
    /// both scopes in the shared verdict so inventory and Cook cannot claim a
    /// provider-native account check that never ran.
    pub readiness: AgentTaskProviderReadiness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration_diagnosis: Option<AgentTaskProviderConfigurationDiagnosis>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskProviderReadiness {
    pub structural_dispatchability: AgentTaskProviderReadinessScope,
    pub live_inference: AgentTaskProviderLiveInferenceReadiness,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskProviderReadinessScope {
    pub state: &'static str,
    pub ready: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskProviderLiveInferenceReadiness {
    pub state: &'static str,
    pub ready: bool,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<AgentTaskProviderRuntimeEvidence>,
}

/// A static provider contract defect that prevents dispatch. This keeps the
/// provider owner attached to every consumer of the shared verdict.
#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskProviderConfigurationDiagnosis {
    pub kind: &'static str,
    pub message: String,
    pub remediation: String,
    pub owner: AgentTaskProviderOwner,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskProviderOwner {
    pub provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_package_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderRuntimeEvidence {
    pub classification: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderDispatchabilityChecks {
    pub route: AgentTaskProviderDispatchabilityCheck,
    pub model: AgentTaskProviderDispatchabilityCheck,
    pub credentials: AgentTaskProviderDispatchabilityCredentialCheck,
    pub configuration: AgentTaskProviderDispatchabilityCheck,
    pub runtime: AgentTaskProviderDispatchabilityCheck,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderDispatchabilityCheck {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderDispatchabilityCredentialCheck {
    pub status: AgentTaskProviderCredentialStatus,
    pub ready: bool,
    pub missing: Vec<String>,
    /// `true` only when a provider-declared live readiness probe
    /// (`readiness_invocation`) actually ran and confirmed the credential
    /// works. `false` means `ready` reflects presence only: the declared
    /// credential material was found somewhere Homeboy can read it, which is
    /// not proof it is still valid — a revoked or expired token is still
    /// present on disk (#13628). Callers that need a go/no-go signal for
    /// provider-owned auth must not treat presence-only readiness as
    /// equivalent to a live-verified one.
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remediation: Vec<String>,
}

/// Credential evidence is deliberately more precise than a boolean. In
/// particular, readable provider-owned auth is only `Present` until a bounded
/// provider-declared readiness invocation proves it usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskProviderCredentialStatus {
    NotRequired,
    Missing,
    Present,
    Unverified,
    Verified,
    Unusable,
}

pub fn evaluate_provider_dispatchability(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
    probe_runtime: bool,
) -> AgentTaskProviderDispatchability {
    let mut cache = ProviderRuntimeReadinessCache::default();
    evaluate_provider_dispatchability_with_config(
        catalog,
        backend,
        selector,
        model,
        &Value::Object(Default::default()),
        probe_runtime,
        &mut cache,
    )
}

pub fn evaluate_provider_dispatchability_with_cache(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
    probe_runtime: bool,
    cache: &mut ProviderRuntimeReadinessCache,
) -> AgentTaskProviderDispatchability {
    evaluate_provider_dispatchability_with_config(
        catalog,
        backend,
        selector,
        model,
        &Value::Object(Default::default()),
        probe_runtime,
        cache,
    )
}

pub fn evaluate_provider_dispatchability_with_config(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
    config: &Value,
    probe_runtime: bool,
    cache: &mut ProviderRuntimeReadinessCache,
) -> AgentTaskProviderDispatchability {
    let mut runtime_evidence = None;
    evaluate_provider_dispatchability_with_config_and_credentials(
        catalog,
        backend,
        selector,
        model,
        config,
        probe_runtime,
        &[],
        cache,
        &mut runtime_evidence,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "dispatchability preserves each route input"
)]
fn evaluate_provider_dispatchability_with_config_and_credentials(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
    config: &Value,
    probe_runtime: bool,
    credential_env: &[(String, String)],
    cache: &mut ProviderRuntimeReadinessCache,
    runtime_evidence_out: &mut Option<AgentTaskProviderRuntimeEvidence>,
) -> AgentTaskProviderDispatchability {
    evaluate_provider_dispatchability_with_config_credentials_and_deadline(
        catalog,
        backend,
        selector,
        model,
        config,
        probe_runtime,
        credential_env,
        cache,
        runtime_evidence_out,
        None,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "dispatchability preserves each route input"
)]
fn evaluate_provider_dispatchability_with_config_credentials_and_deadline(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
    config: &Value,
    probe_runtime: bool,
    credential_env: &[(String, String)],
    cache: &mut ProviderRuntimeReadinessCache,
    runtime_evidence_out: &mut Option<AgentTaskProviderRuntimeEvidence>,
    deadline_unix_ms: Option<u64>,
) -> AgentTaskProviderDispatchability {
    let candidate_providers = catalog
        .providers()
        .iter()
        .filter(|provider| provider.backend == backend)
        .collect::<Vec<_>>();
    let unresolved = |state: &'static str, reason: &'static str, route_reason: String| {
        // A selector can fail while the backend's declared providers are otherwise
        // usable. Preserve that component evidence while route precedence keeps
        // the aggregate verdict unavailable.
        let credentials_ready = !candidate_providers.is_empty()
            && candidate_providers
                .iter()
                .all(|provider| provider_credential_readiness(provider).dispatchable);
        let configuration_ready = !candidate_providers.is_empty()
            && candidate_providers
                .iter()
                .all(|provider| validate_provider_immediate_failure_patterns(provider).is_ok());
        let checks = AgentTaskProviderDispatchabilityChecks {
            route: AgentTaskProviderDispatchabilityCheck {
                ready: false,
                reason: Some(route_reason),
            },
            model: AgentTaskProviderDispatchabilityCheck {
                ready: false,
                reason: None,
            },
            credentials: AgentTaskProviderDispatchabilityCredentialCheck {
                status: if credentials_ready {
                    AgentTaskProviderCredentialStatus::Unverified
                } else {
                    AgentTaskProviderCredentialStatus::Missing
                },
                ready: credentials_ready,
                missing: Vec::new(),
                // A route that never resolved to one provider was never
                // probed, so nothing here was live-verified.
                verified: false,
                reason: Some(
                    "the provider route did not resolve, so credentials were not live-validated"
                        .to_string(),
                ),
                remediation: Vec::new(),
            },
            configuration: AgentTaskProviderDispatchabilityCheck {
                ready: configuration_ready,
                reason: None,
            },
            runtime: AgentTaskProviderDispatchabilityCheck {
                ready: false,
                reason: None,
            },
        };
        AgentTaskProviderDispatchability {
            state,
            ready: false,
            reason: reason.to_string(),
            readiness: readiness_scopes(&checks, false, false, None),
            checks,
            configuration_diagnosis: None,
        }
    };
    let provider = match resolve_provider_for_backend(catalog.providers(), backend, selector) {
        ProviderResolution::Resolved(provider) => provider,
        ProviderResolution::NotFound => {
            return unresolved(
                "route_unavailable",
                "the requested backend/selector route did not resolve",
                "no matching provider".to_string(),
            );
        }
        ProviderResolution::AmbiguousExtensionAlias { candidate_ids } => {
            return unresolved(
                "route_ambiguous",
                "the requested backend route requires a provider selector",
                format!("multiple matching providers: {}", candidate_ids.join(", ")),
            );
        }
        ProviderResolution::SelectorMismatch { available_ids, .. } => {
            return unresolved(
                "route_unavailable",
                "the requested backend/selector route did not resolve",
                format!(
                    "selector did not match; available providers: {}",
                    available_ids.join(", ")
                ),
            );
        }
    };
    let supported = provider
        .cli
        .profiles
        .iter()
        .filter_map(|profile| profile.model.as_deref())
        .collect::<Vec<_>>();
    let model_ready = model.is_none_or(|model| supported.is_empty() || supported.contains(&model));
    let credentials = provider_credential_readiness(provider);
    let declared_credential_env;
    let credential_env = if credential_env.is_empty() {
        declared_credential_env =
            super::secrets::provider_declared_credential_env(provider).unwrap_or_default();
        declared_credential_env.as_slice()
    } else {
        credential_env
    };
    let configuration_diagnosis = (provider_requires_live_auth_validation(provider)
        && provider.readiness_invocation.is_none())
    .then(|| missing_readiness_invocation_diagnosis(provider));
    let configuration = match validate_provider_immediate_failure_patterns(provider) {
        Ok(()) => AgentTaskProviderDispatchabilityCheck {
            ready: configuration_diagnosis.is_none(),
            reason: configuration_diagnosis
                .as_ref()
                .map(|diagnosis| diagnosis.message.clone()),
        },
        Err(reason) => AgentTaskProviderDispatchabilityCheck {
            ready: false,
            reason: Some(reason),
        },
    };
    let mut runtime_remediation = Vec::new();
    let (runtime, runtime_evidence) =
        if probe_runtime && model_ready && credentials.dispatchable && configuration.ready {
            let config = effective_provider_config(config, model);
            match readiness_verdict_with_credentials_and_deadline(
                provider,
                &config,
                credential_env,
                cache,
                deadline_unix_ms,
            ) {
                Ok(verdict) => {
                    let remediation = (!verdict.remediation.trim().is_empty())
                        .then(|| homeboy_core::redaction::redact_string(&verdict.remediation));
                    if let Some(remediation) = remediation.as_ref() {
                        runtime_remediation.push(remediation.clone());
                    }
                    let classification = if verdict.classification.trim().is_empty() {
                        "unknown"
                    } else {
                        verdict.classification.trim()
                    };
                    let reason = (!verdict.ready).then(|| {
                        if verdict.reason.trim().is_empty() {
                            classification.to_string()
                        } else {
                            format!(
                                "{classification}: {}",
                                homeboy_core::redaction::redact_string(&verdict.reason)
                            )
                        }
                    });
                    let evidence = AgentTaskProviderRuntimeEvidence {
                        classification: sanitize_classification(&verdict.classification),
                        retryable: verdict.retryable,
                        remediation,
                        cache_identity: (!verdict.cache_key.is_empty()).then(|| {
                            homeboy_engine_primitives::content_hash::sha256_hex(
                                verdict.cache_key.as_bytes(),
                            )
                        }),
                        provider_identity: (!verdict.identity.is_null()).then(|| {
                            homeboy_engine_primitives::content_hash::sha256_hex(
                                &serde_json::to_vec(&verdict.identity).unwrap_or_default(),
                            )
                        }),
                    };
                    (
                        AgentTaskProviderDispatchabilityCheck {
                            ready: verdict.ready,
                            reason,
                        },
                        Some(evidence),
                    )
                }
                Err(reason) => {
                    let deadline_exhausted = reason.details["classification"] == "timeout";
                    (
                        AgentTaskProviderDispatchabilityCheck {
                            ready: false,
                            reason: Some(homeboy_core::redaction::redact_string(&reason.message)),
                        },
                        Some(AgentTaskProviderRuntimeEvidence {
                            classification: if deadline_exhausted {
                                "timeout".to_string()
                            } else {
                                "probe_error".to_string()
                            },
                            retryable: !deadline_exhausted,
                            ..Default::default()
                        }),
                    )
                }
            }
        } else {
            (
                AgentTaskProviderDispatchabilityCheck {
                    ready: !probe_runtime,
                    reason: (!probe_runtime).then_some("not requested".to_string()),
                },
                None,
            )
        };
    // `runtime.ready` alone conflates two very different situations: a
    // provider-declared probe actually ran and passed, versus no probe being
    // declared at all (in which case `run_provider_readiness_invocation`
    // trivially returns ready). Only the former is a live verification of the
    // provider's credentials; the latter is still presence-only, and reporting
    // it as "ready" without qualification is exactly how a revoked-but-present
    // credential gets reported dispatchable (#13628).
    let credentials_verified =
        probe_runtime && provider.readiness_invocation.is_some() && runtime.ready;
    let provider_owned_auth = provider_requires_live_auth_validation(provider);
    let credentials_rejected = runtime_evidence
        .as_ref()
        .is_some_and(|evidence| evidence.classification == "auth_failure");
    let account_blocked = !runtime.ready
        && runtime_evidence
            .as_ref()
            .is_some_and(|evidence| evidence.classification == "account");
    let (credential_status, credential_reason) = if !credentials.dispatchable {
        (AgentTaskProviderCredentialStatus::Missing, None)
    } else if credentials_verified {
        (AgentTaskProviderCredentialStatus::Verified, None)
    } else if probe_runtime
        && provider.readiness_invocation.is_some()
        && !runtime.ready
        && credentials_rejected
        && (!credentials.requirements.is_empty() || provider_owned_auth)
    {
        (
            AgentTaskProviderCredentialStatus::Unusable,
            runtime.reason.clone(),
        )
    } else if !provider_owned_auth && credentials.requirements.is_empty() {
        (AgentTaskProviderCredentialStatus::NotRequired, None)
    } else if !provider_owned_auth {
        (
            AgentTaskProviderCredentialStatus::Present,
            Some("declared credential material is present".to_string()),
        )
    } else if !probe_runtime && credentials.requirements.is_empty() {
        (
            AgentTaskProviderCredentialStatus::Unverified,
            Some(
                "the provider owns authentication but declares no credential presence contract; live validation was not requested"
                    .to_string(),
            ),
        )
    } else if !probe_runtime {
        (
            AgentTaskProviderCredentialStatus::Present,
            Some(
                "provider-owned credential material is present; live validation was not requested"
                    .to_string(),
            ),
        )
    } else if provider.readiness_invocation.is_none() {
        (
            AgentTaskProviderCredentialStatus::Unverified,
            Some(
                "the provider declares provider-owned authentication but no live readiness invocation"
                    .to_string(),
            ),
        )
    } else {
        (AgentTaskProviderCredentialStatus::Unverified, None)
    };
    let (state, ready, reason) = if !model_ready {
        (
            "model_unavailable",
            false,
            "the selected model is not declared by the routed provider",
        )
    } else if !configuration.ready {
        (
            "configuration_invalid",
            false,
            if configuration_diagnosis.is_some() {
                "provider-owned authentication has no declared readiness invocation"
            } else {
                "provider immediate-failure configuration is invalid"
            },
        )
    } else if !credentials.dispatchable {
        (
            "credentials_missing",
            false,
            "required provider credentials are not configured",
        )
    } else if provider_owned_auth && !probe_runtime {
        (
            if credentials.requirements.is_empty() {
                "credentials_unverified"
            } else {
                "credentials_present"
            },
            false,
            "provider-owned authentication must be live-validated before dispatch",
        )
    } else if provider_owned_auth && provider.readiness_invocation.is_none() {
        (
            "credentials_unverified",
            false,
            "provider-owned authentication has no live validation route",
        )
    } else if account_blocked {
        (
            "account_unavailable",
            false,
            "provider account quota or billing is unavailable",
        )
    } else if provider_owned_auth && !runtime.ready && credentials_rejected {
        (
            "credentials_unusable",
            false,
            "provider-owned authentication or its runtime is revoked or unusable",
        )
    } else if probe_runtime && !runtime.ready {
        (
            "runtime_unavailable",
            false,
            "provider runtime readiness validation failed",
        )
    } else if !probe_runtime || provider.readiness_invocation.is_none() {
        (
            "structurally_dispatchable",
            true,
            "structural dispatchability checks passed; live inference was not probed",
        )
    } else if !provider_owned_auth || credentials_verified {
        ("ready", true, "all dispatchability checks passed")
    } else {
        unreachable!("provider-owned auth without verification is rejected above")
    };
    let reason = if state == "runtime_unavailable" {
        format!(
            "runtime_unavailable: {}",
            runtime.reason.as_deref().unwrap_or(reason)
        )
    } else {
        reason.to_string()
    };
    let checks = AgentTaskProviderDispatchabilityChecks {
        route: AgentTaskProviderDispatchabilityCheck {
            ready: true,
            reason: Some(provider.id.clone()),
        },
        model: AgentTaskProviderDispatchabilityCheck {
            ready: model_ready,
            reason: (!model_ready).then_some("unsupported selected model".to_string()),
        },
        credentials: AgentTaskProviderDispatchabilityCredentialCheck {
            status: credential_status,
            ready: credentials.dispatchable,
            missing: credentials.missing,
            verified: credentials_verified,
            reason: credential_reason,
            remediation: if provider_owned_auth
                && probe_runtime
                && provider.readiness_invocation.is_none()
            {
                configuration_diagnosis
                    .as_ref()
                    .map(|diagnosis| vec![diagnosis.remediation.clone()])
                    .unwrap_or_default()
            } else {
                runtime_remediation
            },
        },
        configuration,
        runtime,
    };
    let readiness = readiness_scopes(
        &checks,
        probe_runtime,
        provider.readiness_invocation.is_some(),
        runtime_evidence.clone(),
    );
    *runtime_evidence_out = runtime_evidence;
    AgentTaskProviderDispatchability {
        state,
        ready,
        reason,
        readiness,
        checks,
        configuration_diagnosis,
    }
}

fn readiness_scopes(
    checks: &AgentTaskProviderDispatchabilityChecks,
    probe_requested: bool,
    probe_declared: bool,
    evidence: Option<AgentTaskProviderRuntimeEvidence>,
) -> AgentTaskProviderReadiness {
    let structural_ready = checks.route.ready
        && checks.model.ready
        && checks.credentials.ready
        && checks.configuration.ready;
    let structural_reason = if structural_ready {
        "route, model, credentials, and configuration are structurally dispatchable".to_string()
    } else {
        "one or more structural dispatchability checks failed".to_string()
    };
    let live_inference = if !structural_ready {
        AgentTaskProviderLiveInferenceReadiness {
            state: "unavailable",
            ready: false,
            reason: structural_reason.clone(),
            evidence: None,
        }
    } else if !probe_requested {
        AgentTaskProviderLiveInferenceReadiness {
            state: "structurally_dispatchable",
            ready: false,
            reason: "live inference probe was not requested".to_string(),
            evidence: None,
        }
    } else if !probe_declared {
        AgentTaskProviderLiveInferenceReadiness {
            state: "inference_unverified",
            ready: false,
            reason: "the provider declares no bounded live inference probe".to_string(),
            evidence: None,
        }
    } else if checks.runtime.ready {
        AgentTaskProviderLiveInferenceReadiness {
            state: "validated",
            ready: true,
            reason: "a bounded provider-native probe accepted the selected route".to_string(),
            evidence,
        }
    } else {
        AgentTaskProviderLiveInferenceReadiness {
            state: "unavailable",
            ready: false,
            reason: checks.runtime.reason.clone().unwrap_or_else(|| {
                "the bounded live inference probe did not accept work".to_string()
            }),
            evidence,
        }
    };
    AgentTaskProviderReadiness {
        structural_dispatchability: AgentTaskProviderReadinessScope {
            state: if structural_ready {
                "ready"
            } else {
                "unavailable"
            },
            ready: structural_ready,
            reason: structural_reason,
        },
        live_inference,
    }
}

fn missing_readiness_invocation_diagnosis(
    provider: &super::AgentTaskExecutorProvider,
) -> AgentTaskProviderConfigurationDiagnosis {
    AgentTaskProviderConfigurationDiagnosis {
        kind: "missing_readiness_invocation",
        message: "provider-owned authentication requires a bounded readiness_invocation".to_string(),
        remediation: format!(
            "Update provider '{}' to declare a bounded readiness_invocation that validates its provider-owned authentication.",
            provider.id
        ),
        owner: AgentTaskProviderOwner {
            provider_id: provider.id.clone(),
            runtime_id: provider.runtime_id.clone(),
            extension_id: provider.extension_id.clone(),
            runtime_package_source: provider.runtime_package_source.clone(),
            runtime_path: provider.runtime_path.clone(),
            extension_path: provider.extension_path.clone(),
        },
    }
}

fn sanitize_classification(classification: &str) -> String {
    match classification {
        "ready"
        | "deterministic_incompatibility"
        | "auth_failure"
        | "account"
        | "capacity"
        | "unavailable"
        | "transient_failure" => classification.to_string(),
        _ => "unknown".to_string(),
    }
}

pub(crate) struct EvaluatedRequestDispatchability {
    pub(crate) dispatchability: AgentTaskProviderDispatchability,
    pub(crate) runtime_evidence: Option<AgentTaskProviderRuntimeEvidence>,
    pub(crate) diagnostic_data: Option<ProviderRouteDiagnosticData>,
}

pub(crate) fn evaluate_request_dispatchability(
    catalog: &AgentTaskProviderCatalog,
    request: &crate::agent_task::AgentTaskRequest,
    cache: &mut ProviderRuntimeReadinessCache,
) -> EvaluatedRequestDispatchability {
    let provider = match effective_provider_for_request(request, catalog.providers()) {
        Ok(Some(provider)) => provider,
        Ok(None) => {
            let resolved = resolve_provider_for_backend(
                catalog.providers(),
                &request.executor.backend,
                request.executor.selector.as_deref(),
            );
            if let Some(provider) = resolved.clone().resolved() {
                let missing =
                    super::secrets::provider_request_secret_env_missing(request, provider);
                if !missing.is_empty() {
                    let mut verdict = unavailable_request_dispatchability(
                        "credentials_missing",
                        "required provider credentials are not configured for this route",
                        "auth_failure",
                        false,
                        Some("configure the required credential material for this provider route"),
                    );
                    verdict.dispatchability.checks.route = AgentTaskProviderDispatchabilityCheck {
                        ready: true,
                        reason: Some(provider.id.clone()),
                    };
                    verdict.dispatchability.checks.credentials.missing = missing;
                    verdict.dispatchability.checks.credentials.status =
                        AgentTaskProviderCredentialStatus::Missing;
                    return verdict;
                }
            }
            let (state, classification, diagnostic_data) = match resolved {
                ProviderResolution::Resolved(_) => (
                    "provider_capability_unavailable",
                    "capability",
                    ProviderRouteDiagnosticData::ProviderCapabilityUnavailable {
                        layer: "provider".to_string(),
                        required_capabilities: request.executor.required_capabilities.clone(),
                    },
                ),
                ProviderResolution::NotFound => {
                    let diagnostics = super::runtime_discovery_diagnostics_for_backend(
                        &catalog.diagnostics,
                        &request.executor.backend,
                    );
                    (
                        "provider_missing",
                        "capability",
                        ProviderRouteDiagnosticData::ProviderMissing {
                            backend: request.executor.backend.clone(),
                            runtime_discovery_diagnostics: diagnostics,
                        },
                    )
                }
                ProviderResolution::SelectorMismatch {
                    available_ids,
                    selector_hint,
                } => (
                    "provider_selector_mismatch",
                    "capability",
                    ProviderRouteDiagnosticData::ProviderSelectorMismatch {
                        backend: request.executor.backend.clone(),
                        selector: request.executor.selector.clone(),
                        available_provider_ids: available_ids,
                        hint: selector_hint,
                    },
                ),
                ProviderResolution::AmbiguousExtensionAlias { candidate_ids } => (
                    "provider_ambiguous",
                    "capability",
                    ProviderRouteDiagnosticData::ProviderAmbiguous {
                        backend: request.executor.backend.clone(),
                        available_provider_ids: candidate_ids,
                    },
                ),
            };
            let mut verdict = unavailable_request_dispatchability(
                state,
                "no provider satisfies the effective route and required capabilities",
                classification,
                false,
                None,
            );
            verdict.diagnostic_data = Some(diagnostic_data);
            return verdict;
        }
        Err(reason) => {
            return unavailable_request_dispatchability(
                "route_unavailable",
                &reason,
                "route",
                false,
                None,
            )
        }
    };
    let missing = super::secrets::provider_request_secret_env_missing(request, &provider);
    if !missing.is_empty() {
        let mut verdict = unavailable_request_dispatchability(
            "credentials_missing",
            "required provider credentials are not configured for this route",
            "auth_failure",
            false,
            Some("configure the required credential material for this provider route"),
        );
        verdict.dispatchability.checks.route = AgentTaskProviderDispatchabilityCheck {
            ready: true,
            reason: Some(provider.id.clone()),
        };
        verdict.dispatchability.checks.credentials =
            AgentTaskProviderDispatchabilityCredentialCheck {
                status: AgentTaskProviderCredentialStatus::Missing,
                ready: false,
                missing,
                verified: false,
                reason: Some("required credential material is missing".to_string()),
                remediation: Vec::new(),
            };
        return verdict;
    }
    let credential_env = match super::secrets::provider_request_credential_env(request, &provider) {
        Ok(credential_env) => credential_env,
        Err(_) => {
            let mut verdict = unavailable_request_dispatchability(
                "credentials_unusable",
                "provider credentials could not be resolved for live validation",
                "auth_failure",
                false,
                Some("repair the provider credential source or select another account"),
            );
            verdict.dispatchability.checks.route = AgentTaskProviderDispatchabilityCheck {
                ready: true,
                reason: Some(provider.id.clone()),
            };
            verdict.dispatchability.checks.credentials.status =
                AgentTaskProviderCredentialStatus::Unusable;
            verdict.dispatchability.checks.credentials.reason =
                Some("credential material could not be resolved".to_string());
            return verdict;
        }
    };
    evaluate_request_dispatchability_with_credentials(
        catalog,
        request,
        provider,
        &credential_env,
        cache,
    )
}

pub(crate) fn evaluate_request_dispatchability_with_credentials(
    catalog: &AgentTaskProviderCatalog,
    request: &crate::agent_task::AgentTaskRequest,
    provider: super::AgentTaskExecutorProvider,
    credential_env: &[(String, String)],
    cache: &mut ProviderRuntimeReadinessCache,
) -> EvaluatedRequestDispatchability {
    let mut runtime_evidence = None;
    let dispatchability = evaluate_provider_dispatchability_with_config_credentials_and_deadline(
        &AgentTaskProviderCatalog {
            providers: vec![provider],
            diagnostics: Vec::new(),
            version: catalog.version.clone(),
        },
        &request.executor.backend,
        request.executor.selector.as_deref(),
        request.executor.model(),
        &request.executor.config,
        true,
        &credential_env,
        cache,
        &mut runtime_evidence,
        request.limits.execution_deadline_unix_ms,
    );
    EvaluatedRequestDispatchability {
        dispatchability,
        runtime_evidence,
        diagnostic_data: None,
    }
}

fn unavailable_request_dispatchability(
    state: &'static str,
    reason: &str,
    classification: &str,
    retryable: bool,
    remediation: Option<&str>,
) -> EvaluatedRequestDispatchability {
    let unavailable = AgentTaskProviderDispatchabilityCheck {
        ready: false,
        reason: None,
    };
    EvaluatedRequestDispatchability {
        runtime_evidence: Some(AgentTaskProviderRuntimeEvidence {
            classification: classification.to_string(),
            retryable,
            remediation: remediation.map(str::to_string),
            cache_identity: None,
            provider_identity: None,
        }),
        diagnostic_data: None,
        dispatchability: AgentTaskProviderDispatchability {
            state,
            ready: false,
            reason: reason.to_string(),
            configuration_diagnosis: None,
            readiness: AgentTaskProviderReadiness {
                structural_dispatchability: AgentTaskProviderReadinessScope {
                    state: "unavailable",
                    ready: false,
                    reason: reason.to_string(),
                },
                live_inference: AgentTaskProviderLiveInferenceReadiness {
                    state: "unavailable",
                    ready: false,
                    reason: reason.to_string(),
                    evidence: None,
                },
            },
            checks: AgentTaskProviderDispatchabilityChecks {
                route: AgentTaskProviderDispatchabilityCheck {
                    ready: false,
                    reason: Some(reason.to_string()),
                },
                model: unavailable.clone(),
                credentials: AgentTaskProviderDispatchabilityCredentialCheck {
                    status: AgentTaskProviderCredentialStatus::Unverified,
                    ready: false,
                    missing: Vec::new(),
                    verified: false,
                    reason: Some(
                        "credential readiness was not evaluated because the route is unavailable"
                            .to_string(),
                    ),
                    remediation: Vec::new(),
                },
                configuration: unavailable.clone(),
                runtime: unavailable,
            },
        },
    }
}

pub fn preflight_provider_dispatchability(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
) -> homeboy_core::Result<AgentTaskProviderDispatchability> {
    let mut cache = ProviderRuntimeReadinessCache::default();
    preflight_provider_dispatchability_with_config(
        catalog,
        backend,
        selector,
        model,
        &Value::Object(Default::default()),
        &mut cache,
    )
}

pub fn preflight_provider_dispatchability_with_config(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
    config: &Value,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<AgentTaskProviderDispatchability> {
    preflight_provider_dispatchability_with_config_and_probe(
        catalog, backend, selector, model, config, true, cache,
    )
}

/// Reject static route/configuration failures before workspace preparation.
/// Runtime probing belongs to the effective-plan pass because provider config
/// is finalized only while building that plan.
pub fn preflight_provider_dispatchability_without_runtime_with_config(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
    config: &Value,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<AgentTaskProviderDispatchability> {
    preflight_provider_dispatchability_with_config_and_probe(
        catalog, backend, selector, model, config, false, cache,
    )
}

fn preflight_provider_dispatchability_with_config_and_probe(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
    config: &Value,
    probe_runtime: bool,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<AgentTaskProviderDispatchability> {
    let verdict = evaluate_provider_dispatchability_with_config(
        catalog,
        backend,
        selector,
        model,
        config,
        probe_runtime,
        cache,
    );
    if verdict.ready
        || (!probe_runtime
            && matches!(
                verdict.state,
                "credentials_present" | "credentials_unverified"
            ))
    {
        return Ok(verdict);
    }
    let detail = verdict
        .checks
        .configuration
        .reason
        .as_deref()
        .or(verdict.checks.credentials.reason.as_deref())
        .as_deref()
        .or(verdict.checks.runtime.reason.as_deref())
        .filter(|reason| *reason != "not requested")
        .map(|reason| format!(": {reason}"))
        .unwrap_or_default();
    let remediation = verdict
        .checks
        .credentials
        .remediation
        .first()
        .map(|remediation| format!(" Remediation: {remediation}"))
        .unwrap_or_default();
    let mut error = homeboy_core::Error::validation_invalid_argument(
        "provider_dispatchability",
        format!(
            "agent-task backend `{backend}` is not dispatchable ({state}): {reason}{detail}.{remediation}",
            state = verdict.state,
            reason = verdict.reason.trim_end_matches('.'),
        ),
        Some(backend.to_string()),
        None,
    );
    error.details["dispatchability"] = serde_json::to_value(&verdict).unwrap_or(Value::Null);
    Err(error)
}

/// Evaluate every effective executor in a compiled plan. The caller owns the
/// cache so fanout siblings with identical provider/config requests probe once.
pub fn admit_plan_provider_dispatchability_with_providers(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<AgentTaskPlan> {
    selected_plan_provider_dispatchability_with_providers(plan, catalog, cache)
}

pub fn preflight_plan_provider_dispatchability_with_providers(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<()> {
    selected_plan_provider_dispatchability_with_providers(plan, catalog, cache).map(|_| ())
}

fn selected_plan_provider_dispatchability_with_providers(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<AgentTaskPlan> {
    let mut admitted = plan.clone();
    let plan_rotation = plan.options.rotation.clone();
    for (task_index, task) in plan.tasks.iter().enumerate() {
        let admission_deadline = match (
            task.limits.execution_deadline_unix_ms,
            plan.options.execution_budget.deadline_unix_ms,
        ) {
            (Some(task), Some(plan)) => Some(task.min(plan)),
            (task, plan) => task.or(plan),
        };
        let mut skipped = task
            .metadata
            .pointer("/provider_readiness_routing/skipped")
            .and_then(Value::as_array)
            .and_then(|values| {
                values
                    .iter()
                    .cloned()
                    .map(serde_json::from_value)
                    .collect::<std::result::Result<Vec<ProviderRouteEvidence>, _>>()
                    .ok()
            })
            .unwrap_or_default();
        let mut current_skipped = Vec::new();
        let base_secret_env = task
            .metadata
            .get("provider_admission")
            .and_then(|value| value.get("base_secret_env"))
            .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
            .unwrap_or_else(|| task.executor.secret_env.clone());
        let candidates =
            AgentTaskScheduleSupport::provider_route_candidates(task, plan_rotation.as_ref());
        let mut selected = None;
        let mut first_failure = None;
        for (mut candidate, next_rotation_index) in candidates {
            candidate.limits.execution_deadline_unix_ms = admission_deadline;
            if super::is_fixture_backend(&candidate.executor.backend)
                || crate::agent_task_gate_executor::is_repo_local_gate_request(&candidate)
            {
                selected = Some((candidate, next_rotation_index));
                break;
            }
            if let Ok(Some(provider)) =
                effective_provider_for_request(&candidate, catalog.providers())
            {
                super::secrets::apply_provider_runner_secret_env_contract_for_request(
                    &mut candidate,
                    &provider,
                    &base_secret_env,
                );
            }
            let evaluated = evaluate_request_dispatchability(catalog, &candidate, cache);
            let diagnostic_data = evaluated.diagnostic_data;
            let verdict = evaluated.dispatchability;
            if verdict.ready {
                selected = Some((candidate, next_rotation_index));
                break;
            }
            let runtime_evidence = evaluated.runtime_evidence;
            current_skipped.push(ProviderRouteEvidence {
                backend: candidate.executor.backend.clone(),
                selector: candidate.executor.selector.clone(),
                model: candidate.executor.model().map(str::to_string),
                state: verdict.state.to_string(),
                reason: verdict.reason.clone(),
                reset_at: None,
                classification: runtime_evidence
                    .as_ref()
                    .map(|evidence| evidence.classification.clone()),
                retryable: runtime_evidence
                    .as_ref()
                    .is_some_and(|evidence| evidence.retryable),
                remediation: runtime_evidence
                    .as_ref()
                    .and_then(|evidence| evidence.remediation.clone()),
                cache_identity: runtime_evidence
                    .as_ref()
                    .and_then(|evidence| evidence.cache_identity.clone()),
                provider_identity: runtime_evidence
                    .as_ref()
                    .and_then(|evidence| evidence.provider_identity.clone()),
                runtime_evidence: runtime_evidence.clone(),
                checks: Some(verdict.checks.clone()),
                diagnostic_data,
            });
            if first_failure.is_none() {
                first_failure = Some((candidate, verdict));
            }
        }
        skipped.extend(current_skipped.iter().cloned());
        if let Some((mut selected, next_rotation_index)) = selected {
            if !skipped.is_empty() {
                if !selected.metadata.is_object() {
                    selected.metadata = serde_json::json!({});
                }
                selected.metadata["provider_readiness_routing"] = serde_json::json!({
                    "skipped": skipped,
                    "provider_rotations_used": 0,
                    "next_rotation_index": next_rotation_index,
                });
            }
            admitted.tasks[task_index] = selected;
            continue;
        }
        let (candidate, first_verdict) = first_failure.expect("every plan has an initial route");
        let deadline_exhausted = current_skipped
            .iter()
            .any(|route| route.classification.as_deref() == Some("timeout"));
        let retryable = deadline_exhausted || current_skipped.iter().any(|route| route.retryable);
        let hints = if first_verdict.checks.credentials.missing.is_empty() {
            None
        } else {
            Some(vec![format!(
                "Missing provider credentials: {}",
                first_verdict.checks.credentials.missing.join(", ")
            )])
        };
        let mut error = if deadline_exhausted {
            let deadline = admission_deadline;
            let mut error = homeboy_core::Error::validation_invalid_argument(
                "execution_deadline_unix_ms",
                "agent-task execution deadline expired during provider readiness admission",
                deadline.map(|value| value.to_string()),
                hints,
            )
            .with_retryable(false);
            error.details["classification"] = serde_json::json!("timeout");
            error.details["zero_provider_executions"] = serde_json::json!(true);
            error.details["deadline_unix_ms"] = serde_json::json!(deadline);
            error
        } else {
            homeboy_core::Error::validation_invalid_argument(
                "provider_dispatchability",
                format!(
                    "agent-task backend `{}` is not dispatchable across any reachable rotation route: {}",
                    candidate.executor.backend, first_verdict.reason
                ),
                Some(candidate.executor.backend),
                hints,
            )
            .with_retryable(retryable)
        };
        error.details["route_evidence_schema"] =
            serde_json::json!("homeboy/agent-task-provider-route-evidence/v1");
        error.details["route_evidence"] = serde_json::to_value(&skipped).unwrap_or(Value::Null);
        return Err(error);
    }
    Ok(admitted)
}

/// Reject deterministic provider contract defects after the effective executor
/// config has been compiled, without invoking provider-owned live readiness.
pub fn preflight_plan_provider_dispatchability_without_runtime_with_providers(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<()> {
    for task in &plan.tasks {
        preflight_provider_dispatchability_without_runtime_with_config(
            catalog,
            &task.executor.backend,
            task.executor.selector.as_deref(),
            task.executor.model(),
            &task.executor.config,
            cache,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::agent_task_scheduler::{
        AgentTaskProviderRotationEntry, AgentTaskProviderRotationPolicy,
    };
    use homeboy_core::command_invocation::CommandInvocation;
    use serde_json::json;

    fn provider(
        script: &std::path::Path,
        count: &std::path::Path,
    ) -> super::super::AgentTaskExecutorProvider {
        let mut provider: super::super::AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "test.provider",
            "backend": "test"
        }))
        .expect("provider fixture");
        provider.readiness_invocation = Some(
            CommandInvocation {
                argv: vec![
                    "node".to_string(),
                    script.display().to_string(),
                    count.display().to_string(),
                ],
                ..CommandInvocation::default()
            }
            .into(),
        );
        provider
    }

    fn catalog(provider: super::super::AgentTaskExecutorProvider) -> AgentTaskProviderCatalog {
        AgentTaskProviderCatalog {
            providers: vec![provider],
            ..Default::default()
        }
    }

    fn request(model: &str) -> AgentTaskRequest {
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "route".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: Some(model.to_string()),
                config: Value::Null,
            },
            instructions: "run".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
            metadata: Value::Null,
        }
    }

    #[test]
    fn mixed_static_route_failure_and_expired_probe_returns_typed_deadline() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let script = root.path().join("slow-readiness.js");
        std::fs::write(
            &script,
            "setTimeout(()=>process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:true,classification:'ready',retryable:false,remediation:'',reason:'',cache_key:'ready',identity:{}})),200);",
        )
        .expect("readiness script");
        let catalog = catalog(provider(&script, &count));
        let mut task = request("model");
        task.executor.backend = "missing".to_string();
        let deadline = crate::agent_task_timeout::now_unix_ms() + 30;
        task.limits.execution_deadline_unix_ms = Some(deadline);
        let mut plan = AgentTaskPlan::new("deadline", vec![task]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![AgentTaskProviderRotationEntry {
                backend: Some("test".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });

        let error = admit_plan_provider_dispatchability_with_providers(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect_err("deadline must supersede the static route failure");

        assert_eq!(error.details["classification"], "timeout");
        assert_eq!(error.details["zero_provider_executions"], true);
        assert_eq!(error.details["deadline_unix_ms"], deadline);
        assert_eq!(
            error.details["route_evidence"][0]["classification"],
            "capability"
        );
        assert_eq!(
            error.details["route_evidence"][0]["diagnostic_data"]["kind"],
            "provider_missing"
        );
        assert_eq!(
            error.details["route_evidence"][1]["classification"],
            "timeout"
        );
    }

    #[test]
    fn expired_plan_deadline_stops_admission_before_probe() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let script = root.path().join("readiness.js");
        std::fs::write(
            &script,
            "require('fs').appendFileSync(process.argv[2],'probe\\n');process.stdout.write('{}');",
        )
        .expect("readiness script");
        let catalog = catalog(provider(&script, &count));
        let mut plan = AgentTaskPlan::new("plan-deadline", vec![request("model")]);
        let deadline = crate::agent_task_timeout::now_unix_ms().saturating_sub(1);
        plan.options.execution_budget.deadline_unix_ms = Some(deadline);

        let error = admit_plan_provider_dispatchability_with_providers(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect_err("expired plan deadline must stop admission");

        assert_eq!(error.details["classification"], "timeout");
        assert_eq!(error.details["deadline_unix_ms"], deadline);
        assert!(!count.exists());
    }

    /// The exact shape of #13628: a provider whose declared credential is
    /// physically present (a `json-file` source Homeboy can read) but that
    /// declares no live readiness probe of its own. `--validate-readiness`
    /// used to report this as fully "ready" with no way to tell presence
    /// apart from a genuine live check — indistinguishable from a provider
    /// whose token had actually been confirmed to still work. A revoked
    /// refresh token is still present on disk, so presence-only readiness
    /// must not be reported the same way as a live-verified one.
    fn provider_with_present_but_unverifiable_credential(
        auth_path: &std::path::Path,
    ) -> super::super::AgentTaskExecutorProvider {
        serde_json::from_value(json!({
            "id": "revocable.agent-task-executor",
            "backend": "revocable",
            "capabilities": ["cli_runtime", "provider_owned_auth"],
            "provider_defaults": {
                "revocable": {
                    "required_secret_env": ["HOMEBOY_TEST_REVOCABLE_REFRESH_TOKEN"],
                    "secret_env_sources": {
                        "HOMEBOY_TEST_REVOCABLE_REFRESH_TOKEN": {
                            "source": "json-file",
                            "path": auth_path,
                            "field": "token"
                        }
                    }
                }
            }
        }))
        .expect("provider fixture")
    }

    #[test]
    fn present_credentials_are_not_admitted_when_live_validation_is_unavailable() {
        let auth = tempfile::NamedTempFile::new().expect("auth file");
        // The credential material is present, exactly like a revoked-but-still-
        // on-disk refresh token (#13628): Homeboy can read it, but reading it
        // is not proof the issuing account still honors it.
        std::fs::write(auth.path(), r#"{"token":"revoked-but-present-token"}"#)
            .expect("write auth");
        let provider = provider_with_present_but_unverifiable_credential(auth.path());
        let catalog = catalog(provider);

        let verdict = evaluate_provider_dispatchability(&catalog, "revocable", None, None, true);

        assert_eq!(verdict.state, "configuration_invalid");
        assert!(
            !verdict.ready,
            "unverified provider-owned auth must fail closed"
        );
        assert!(
            verdict.checks.credentials.ready,
            "the declared credential is present"
        );
        assert_eq!(
            verdict.checks.credentials.status,
            AgentTaskProviderCredentialStatus::Unverified
        );
        assert!(
            !verdict.checks.credentials.verified,
            "no provider-declared readiness probe ran, so this is presence only, not a live \
             verification — reporting it as verified is exactly the #13628 defect"
        );
        assert!(verdict.checks.credentials.remediation[0].contains("readiness_invocation"));
        let diagnosis = verdict
            .configuration_diagnosis
            .expect("missing invocation has a typed diagnosis");
        assert_eq!(diagnosis.kind, "missing_readiness_invocation");
        assert_eq!(diagnosis.owner.provider_id, "revocable.agent-task-executor");
        assert!(!diagnosis.remediation.contains("select a verified backend"));
    }

    #[test]
    fn static_status_distinguishes_present_provider_owned_credentials() {
        let auth = tempfile::NamedTempFile::new().expect("auth file");
        std::fs::write(auth.path(), r#"{"token":"present-token"}"#).expect("write auth");
        let catalog = catalog(provider_with_present_but_unverifiable_credential(
            auth.path(),
        ));

        let verdict = evaluate_provider_dispatchability(&catalog, "revocable", None, None, false);

        assert_eq!(verdict.state, "configuration_invalid");
        assert!(!verdict.ready);
        assert_eq!(
            verdict.checks.credentials.status,
            AgentTaskProviderCredentialStatus::Present
        );
        assert!(!verdict.checks.credentials.verified);
    }

    #[test]
    fn a_live_readiness_probe_that_passes_reports_verified_credentials() {
        let root = tempfile::tempdir().expect("tempdir");
        let script = root.path().join("readiness.js");
        std::fs::write(
            &script,
            "const ready=process.env.HOMEBOY_TEST_REVOCABLE_REFRESH_TOKEN==='present-and-live-checked';process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready,classification:ready?'ready':'auth_failure',retryable:false,remediation:'',reason:ready?'':'fallback credential was not resolved',cache_key:'k',identity:{credential:ready?'resolved':'missing'}}));",
        )
        .expect("readiness script");
        let mut live_provider = provider_with_present_but_unverifiable_credential(
            root.path().join("unused-auth.json").as_path(),
        );
        std::fs::write(
            root.path().join("unused-auth.json"),
            r#"{"token":"present-and-live-checked"}"#,
        )
        .expect("write auth");
        live_provider.readiness_invocation = Some(
            CommandInvocation {
                argv: vec!["node".to_string(), script.display().to_string()],
                ..CommandInvocation::default()
            }
            .into(),
        );
        let catalog = catalog(live_provider);

        let verdict = evaluate_provider_dispatchability(&catalog, "revocable", None, None, true);

        assert!(verdict.ready);
        assert!(
            verdict.checks.credentials.verified,
            "a provider-declared readiness probe ran and passed, so this is a genuine live \
             verification"
        );
        assert_eq!(verdict.reason, "all dispatchability checks passed");
    }

    #[test]
    fn revoked_or_unusable_credentials_preserve_provider_remediation() {
        let root = tempfile::tempdir().expect("tempdir");
        let auth = root.path().join("auth.json");
        let script = root.path().join("readiness.js");
        std::fs::write(&auth, r#"{"token":"revoked-but-present-token"}"#).expect("write auth");
        std::fs::write(
            &script,
            "process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:false,classification:'auth_failure',retryable:false,remediation:'Log out and sign in again.',reason:'refresh token revoked',cache_key:'k',identity:{}}));",
        )
        .expect("readiness script");
        let mut provider = provider_with_present_but_unverifiable_credential(&auth);
        provider.readiness_invocation = Some(
            CommandInvocation {
                argv: vec!["node".to_string(), script.display().to_string()],
                ..CommandInvocation::default()
            }
            .into(),
        );
        let catalog = catalog(provider);

        let verdict = evaluate_provider_dispatchability(&catalog, "revocable", None, None, true);

        assert_eq!(verdict.state, "credentials_unusable");
        assert!(!verdict.ready);
        assert_eq!(
            verdict.checks.credentials.status,
            AgentTaskProviderCredentialStatus::Unusable
        );
        assert_eq!(
            verdict.checks.credentials.reason.as_deref(),
            Some("auth_failure: refresh token revoked")
        );
        assert_eq!(
            verdict.checks.credentials.remediation,
            vec!["Log out and sign in again."]
        );

        let error = preflight_provider_dispatchability(&catalog, "revocable", None, None)
            .expect_err("revoked credential must fail before dispatch");
        assert!(error.message.contains("refresh token revoked"), "{error}");
        assert!(
            error.message.contains("Log out and sign in again"),
            "{error}"
        );
    }

    #[test]
    fn offline_provider_owned_auth_remains_runtime_unavailable() {
        let root = tempfile::tempdir().expect("tempdir");
        let auth = root.path().join("auth.json");
        let script = root.path().join("readiness.js");
        std::fs::write(&auth, r#"{"token":"present-token"}"#).expect("write auth");
        std::fs::write(
            &script,
            "process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:false,classification:'unavailable',retryable:true,remediation:'Start the provider runtime.',reason:'provider is offline',cache_key:'offline',identity:{}}));",
        )
        .expect("readiness script");
        let mut provider = provider_with_present_but_unverifiable_credential(&auth);
        provider.readiness_invocation = Some(
            CommandInvocation {
                argv: vec!["node".to_string(), script.display().to_string()],
                ..CommandInvocation::default()
            }
            .into(),
        );

        let verdict =
            evaluate_provider_dispatchability(&catalog(provider), "revocable", None, None, true);

        assert_eq!(verdict.state, "runtime_unavailable");
        assert_eq!(
            verdict.reason,
            "runtime_unavailable: unavailable: provider is offline"
        );
        assert_eq!(
            verdict.checks.credentials.status,
            AgentTaskProviderCredentialStatus::Unverified
        );
        assert_eq!(
            verdict.checks.runtime.reason.as_deref(),
            Some("unavailable: provider is offline")
        );
        assert_eq!(
            verdict.checks.credentials.remediation,
            vec!["Start the provider runtime."]
        );
    }

    #[test]
    fn account_blocked_probe_is_unavailable_before_a_provider_execution() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("probe-count");
        let script = root.path().join("readiness.js");
        std::fs::write(
            &script,
            "const fs=require('fs');fs.appendFileSync(process.argv[2],'probe\\n');process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:false,classification:'account',retryable:false,remediation:'restore account quota or billing access',reason:'account spending limit is exhausted',cache_key:'blocked-account',identity:{account:'blocked'}}));",
        )
        .expect("readiness script");
        let mut provider = provider(&script, &count);
        provider.cli.profiles = serde_json::from_value(json!([
            { "name": "blocked", "model": "selected-model" }
        ]))
        .expect("model profile");
        let catalog = catalog(provider);

        let verdict =
            evaluate_provider_dispatchability(&catalog, "test", None, Some("selected-model"), true);

        assert_eq!(verdict.state, "account_unavailable");
        assert!(!verdict.ready);
        assert!(verdict.readiness.structural_dispatchability.ready);
        assert_ne!(
            verdict.checks.credentials.status,
            AgentTaskProviderCredentialStatus::Unusable,
            "an account quota or billing block is not a rejected credential"
        );
        assert_eq!(verdict.readiness.live_inference.state, "unavailable");
        assert_eq!(
            verdict
                .readiness
                .live_inference
                .evidence
                .as_ref()
                .map(|evidence| evidence.classification.as_str()),
            Some("account")
        );
        assert!(verdict.reason.contains("quota or billing"));
        assert_eq!(
            std::fs::read_to_string(&count).expect("one bounded probe"),
            "probe\n"
        );

        std::fs::remove_file(&count).expect("reset probe evidence for Cook admission");

        let error = admit_plan_provider_dispatchability_with_providers(
            &AgentTaskPlan::new("account-blocked", vec![request("selected-model")]),
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect_err("Cook admission must reject an account-blocked route");
        assert_eq!(
            error.details["route_evidence"][0]["classification"],
            "account"
        );
        assert_eq!(
            error.details["route_evidence"][0]["state"],
            "account_unavailable"
        );
        assert_eq!(
            std::fs::read_to_string(&count).expect("one bounded Cook admission probe"),
            "probe\n"
        );
    }

    #[test]
    fn route_failures_always_produce_a_machine_verdict() {
        let catalog = AgentTaskProviderCatalog::default();
        let missing = evaluate_provider_dispatchability(&catalog, "missing", None, None, false);
        assert_eq!(missing.state, "route_unavailable");
        assert!(!missing.checks.route.ready);

        let mut first: super::super::AgentTaskExecutorProvider = serde_json::from_value(
            json!({ "id": "one", "backend": "one", "extension_id": "shared" }),
        )
        .expect("first provider");
        first.extension_id = Some("shared".to_string());
        let mut second = first.clone();
        second.id = "two".to_string();
        let ambiguous_catalog = AgentTaskProviderCatalog {
            providers: vec![first, second],
            ..Default::default()
        };
        let ambiguous =
            evaluate_provider_dispatchability(&ambiguous_catalog, "shared", None, None, false);
        assert_eq!(ambiguous.state, "route_ambiguous");
        assert!(!ambiguous.checks.route.ready);
    }

    #[test]
    fn aggregate_precedence_keeps_each_failed_dimension_unavailable() {
        let credential_provider: super::super::AgentTaskExecutorProvider =
            serde_json::from_value(json!({
                "id": "credential.provider",
                "backend": "credential",
                "provider_defaults": {
                    "credential": {
                        "required_secret_env": ["HOMEBOY_TEST_DISPATCHABILITY_MISSING_CREDENTIAL"]
                    }
                }
            }))
            .expect("credential provider");
        let model_provider: super::super::AgentTaskExecutorProvider =
            serde_json::from_value(json!({
                "id": "model.provider",
                "backend": "model",
                "cli": { "profiles": [{ "name": "supported", "model": "supported" }] }
            }))
            .expect("model provider");
        let ready_provider: super::super::AgentTaskExecutorProvider =
            serde_json::from_value(json!({ "id": "ready.provider", "backend": "ready" }))
                .expect("ready provider");
        let catalog = AgentTaskProviderCatalog {
            providers: vec![credential_provider, model_provider, ready_provider],
            ..Default::default()
        };

        let credentials_missing =
            evaluate_provider_dispatchability(&catalog, "credential", None, None, false);
        assert_eq!(credentials_missing.state, "credentials_missing");
        assert!(!credentials_missing.ready);
        assert!(credentials_missing.checks.route.ready);
        assert!(!credentials_missing.checks.credentials.ready);

        let route_failed = evaluate_provider_dispatchability(
            &catalog,
            "ready",
            Some("missing.provider"),
            None,
            false,
        );
        assert_eq!(route_failed.state, "route_unavailable");
        assert!(!route_failed.ready);
        assert!(!route_failed.checks.route.ready);
        assert!(route_failed.checks.credentials.ready);

        let model_unavailable =
            evaluate_provider_dispatchability(&catalog, "model", None, Some("unsupported"), false);
        assert_eq!(model_unavailable.state, "model_unavailable");
        assert!(!model_unavailable.ready);
        assert!(!model_unavailable.checks.model.ready);

        let all_ready = evaluate_provider_dispatchability(&catalog, "ready", None, None, false);
        assert_eq!(all_ready.state, "ready");
        assert!(all_ready.ready);
    }

    #[test]
    fn shared_evaluator_uses_effective_config_redacts_runtime_reason_and_deduplicates() {
        let root = tempfile::tempdir().expect("tempdir");
        let script = root.path().join("readiness.js");
        let count = root.path().join("count");
        std::fs::write(
            &script,
            "const fs=require('fs');const input=JSON.parse(fs.readFileSync(0,'utf8'));const count=process.argv[2];fs.writeFileSync(count,String(Number(fs.existsSync(count)?fs.readFileSync(count,'utf8'):0)+1));const ready=input.effective_config.marker==='ready';process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready,classification:'configuration',retryable:false,remediation:'token=runtime-secret',reason:'token=runtime-secret',cache_key:'test',identity:{}}));",
        )
        .expect("readiness script");
        let catalog = catalog(provider(&script, &count));
        let mut cache = ProviderRuntimeReadinessCache::default();

        let blocked = evaluate_provider_dispatchability_with_config(
            &catalog,
            "test",
            None,
            None,
            &json!({ "marker": "blocked" }),
            true,
            &mut cache,
        );
        assert_eq!(blocked.state, "runtime_unavailable");
        assert_eq!(
            blocked.checks.runtime.reason.as_deref(),
            Some("configuration: token=[REDACTED]")
        );

        let ready = evaluate_provider_dispatchability_with_config(
            &catalog,
            "test",
            None,
            None,
            &json!({ "marker": "ready" }),
            true,
            &mut cache,
        );
        assert!(ready.ready);
        let ready_again = evaluate_provider_dispatchability_with_config(
            &catalog,
            "test",
            None,
            None,
            &json!({ "marker": "ready" }),
            true,
            &mut cache,
        );
        assert!(ready_again.ready);
        assert_eq!(std::fs::read_to_string(count).expect("probe count"), "2");
    }

    #[test]
    fn shared_evaluator_rejects_invalid_immediate_failure_configuration() {
        let mut provider: super::super::AgentTaskExecutorProvider =
            serde_json::from_value(json!({ "id": "test.provider", "backend": "test" }))
                .expect("provider fixture");
        provider.immediate_failure_patterns = vec![serde_json::from_value(json!({
            "id": "",
            "error_contains_any": []
        }))
        .expect("invalid immediate failure fixture")];
        let verdict =
            evaluate_provider_dispatchability(&catalog(provider), "test", None, None, true);

        assert_eq!(verdict.state, "configuration_invalid");
        assert!(!verdict.checks.configuration.ready);
    }

    #[test]
    fn plan_preflight_accepts_the_first_dispatchable_rotation_route() {
        let root = tempfile::tempdir().expect("tempdir");
        let script = root.path().join("readiness.js");
        let count = root.path().join("count");
        std::fs::write(
            &script,
            "const fs=require('fs');const input=JSON.parse(fs.readFileSync(0,'utf8'));const count=process.argv[2];fs.writeFileSync(count,String(Number(fs.existsSync(count)?fs.readFileSync(count,'utf8'):0)+1));const ready=input.effective_config.model==='ready-model';process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready,classification:ready?'ready':'account',retryable:false,remediation:'',reason:ready?'':'account blocked',cache_key:input.effective_config.model,identity:{model:input.effective_config.model}}));",
        )
        .expect("readiness script");
        let catalog = catalog(provider(&script, &count));
        let mut plan = AgentTaskPlan::new("rotation-readiness", vec![request("blocked-model")]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    model: Some("blocked-model".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    model: Some("ready-model".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        preflight_plan_provider_dispatchability_with_providers(
            &mut plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("ready fallback admits the plan");

        assert_eq!(std::fs::read_to_string(count).expect("probe count"), "2");
        assert_eq!(plan.tasks[0].executor.model(), Some("blocked-model"));
        assert!(plan.tasks[0]
            .metadata
            .get("provider_readiness_routing")
            .is_none());
    }

    #[test]
    fn readiness_fallback_uses_rotation_budget_not_dispatch_attempt_budget() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let script = root.path().join("readiness.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const input=JSON.parse(fs.readFileSync(0,'utf8'));const ready=input.effective_config.model==='ready-model';process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready,classification:ready?'ready':'account',retryable:false,remediation:'',reason:ready?'':'blocked',cache_key:input.effective_config.model,identity:{model:input.effective_config.model}}));",
        )
        .expect("readiness script");
        let catalog = catalog(provider(&script, &count));
        let mut plan = AgentTaskPlan::new("bounded-readiness", vec![request("blocked-model")]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![AgentTaskProviderRotationEntry {
                model: Some("ready-model".to_string()),
                ..Default::default()
            }],
            max_attempts: Some(1),
            ..Default::default()
        });
        plan.options.execution_budget.max_provider_rotations = 1;

        preflight_plan_provider_dispatchability_with_providers(
            &mut plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("readiness transition consumes no dispatch attempt");

        assert_eq!(plan.tasks[0].executor.model(), Some("blocked-model"));
    }

    #[test]
    fn repeated_admission_reconsiders_a_recovered_primary_from_the_durable_plan() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let block_fallback = root.path().join("block-fallback");
        let script = root.path().join("readiness.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const input=JSON.parse(fs.readFileSync(0,'utf8'));const model=input.effective_config.model;const ready=model==='fallback-one'&&!fs.existsSync(process.argv[3])||model==='fallback-two';fs.appendFileSync(process.argv[2],model+'\\n');process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready,classification:ready?'ready':'account',retryable:false,remediation:'switch',reason:ready?'':'blocked',cache_key:model,identity:{model}}));",
        )
        .expect("readiness script");
        let mut provider = provider(&script, &count);
        provider
            .readiness_invocation
            .as_mut()
            .expect("readiness invocation")
            .command
            .argv
            .push(block_fallback.display().to_string());
        let catalog = catalog(provider);
        let mut plan = AgentTaskPlan::new("repeated-admission", vec![request("primary")]);
        plan.options.execution_budget.max_provider_rotations = 1;
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    model: Some("fallback-one".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    model: Some("fallback-two".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        let admitted = admit_plan_provider_dispatchability_with_providers(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("first fallback is admitted");
        assert_eq!(admitted.tasks[0].executor.model(), Some("fallback-one"));
        assert_eq!(admitted.options.rotation.as_ref().unwrap().entries.len(), 2);
        std::fs::write(
            &script,
            "const fs=require('fs');const input=JSON.parse(fs.readFileSync(0,'utf8'));const model=input.effective_config.model;fs.appendFileSync(process.argv[2],model+'\\n');process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:model==='primary',classification:model==='primary'?'ready':'account',retryable:false,remediation:'',reason:'',cache_key:model,identity:{model}}));",
        )
        .expect("recover primary probe");

        admit_plan_provider_dispatchability_with_providers(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("restart-style re-admission sees the recovered primary");
        let probes = std::fs::read_to_string(count).expect("probe log");
        assert_eq!(
            probes.lines().filter(|model| *model == "primary").count(),
            2
        );
    }

    #[test]
    fn repeated_admission_of_a_bound_fallback_never_reconsiders_primary() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let script = root.path().join("readiness.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const input=JSON.parse(fs.readFileSync(0,'utf8'));const model=input.effective_config.model;fs.appendFileSync(process.argv[2],model+'\\n');process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:model==='fallback',classification:model==='fallback'?'ready':'account',retryable:false,remediation:'',reason:'blocked',cache_key:model,identity:{model}}));",
        )
        .expect("readiness script");
        let catalog = catalog(provider(&script, &count));
        let mut plan = AgentTaskPlan::new("bound-admission", vec![request("primary")]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![AgentTaskProviderRotationEntry {
                model: Some("fallback".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });
        let admitted = admit_plan_provider_dispatchability_with_providers(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("fallback admitted");
        std::fs::write(
            &script,
            "const fs=require('fs');const input=JSON.parse(fs.readFileSync(0,'utf8'));const model=input.effective_config.model;fs.appendFileSync(process.argv[2],model+'\\n');process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:true,classification:'ready',retryable:false,remediation:'',reason:'',cache_key:model,identity:{model}}));",
        )
        .expect("recover primary");

        let readmitted = admit_plan_provider_dispatchability_with_providers(
            &admitted,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("bound fallback remains admitted");

        assert_eq!(readmitted.tasks[0].executor.model(), Some("fallback"));
        assert_eq!(
            std::fs::read_to_string(count)
                .expect("probe log")
                .lines()
                .filter(|model| *model == "primary")
                .count(),
            1
        );
    }

    #[test]
    fn request_capabilities_select_the_same_provider_for_readiness_as_execution() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let script = root.path().join("unready.js");
        std::fs::write(
            &script,
            "process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:false,classification:'account',retryable:false,remediation:'',reason:'wrong provider',cache_key:'wrong',identity:{}}));",
        )
        .expect("readiness script");
        let incapable = provider(&script, &count);
        let mut capable = incapable.clone();
        capable.id = "test.capable-provider".to_string();
        capable.capabilities = vec!["required-capability".to_string()];
        capable.readiness_invocation = None;
        let mut task = request("model");
        task.executor.required_capabilities = vec!["required-capability".to_string()];
        let mut plan = AgentTaskPlan::new("capability-readiness", vec![task]);

        preflight_plan_provider_dispatchability_with_providers(
            &mut plan,
            &AgentTaskProviderCatalog {
                providers: vec![incapable, capable],
                ..Default::default()
            },
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("the capable provider is the readiness and execution identity");
    }

    #[test]
    fn transient_primary_recovers_before_fallback_selection() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let script = root.path().join("recover-primary.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const count=process.argv[2],n=Number(fs.existsSync(count)?fs.readFileSync(count,'utf8'):0)+1;fs.writeFileSync(count,String(n));const ready=n>1;process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready,classification:ready?'ready':'transient_failure',retryable:!ready,remediation:ready?'':'retry',reason:ready?'':'temporary',cache_key:'primary',identity:{account:'primary'}}));",
        )
        .expect("readiness script");
        let catalog = catalog(provider(&script, &count));
        let mut plan = AgentTaskPlan::new("recovered-primary", vec![request("primary")]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    model: Some("primary".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    model: Some("fallback".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let admitted = admit_plan_provider_dispatchability_with_providers(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("transient primary recovers");
        assert_eq!(admitted.tasks[0].executor.model(), Some("primary"));
        assert_eq!(std::fs::read_to_string(count).expect("probe count"), "2");
        assert!(admitted.tasks[0]
            .metadata
            .get("provider_readiness_routing")
            .is_none());
    }

    #[test]
    fn conditional_credentials_are_recomputed_for_each_rotation_route() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let script = root.path().join("blocked.js");
        std::fs::write(
            &script,
            "process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:false,classification:'account',retryable:false,remediation:'switch',reason:'blocked',cache_key:'primary',identity:{account:'primary'}}));",
        )
        .expect("readiness script");
        let mut primary = provider(&script, &count);
        primary.id = "test.primary".to_string();
        let mut conditional = primary.clone();
        conditional.id = "test.conditional".to_string();
        conditional.readiness_invocation = None;
        conditional.secret_env_requirements =
            vec![super::super::AgentTaskProviderSecretEnvRequirement {
                env: vec![format!("HOMEBOY_MISSING_{}", uuid::Uuid::new_v4())],
                when: Some(json!({"path":"executor.config.provider","equals":"conditional"})),
                ..Default::default()
            }];
        let mut fallback = conditional.clone();
        fallback.id = "test.fallback".to_string();
        fallback.secret_env_requirements.clear();
        let catalog = AgentTaskProviderCatalog {
            providers: vec![primary, conditional, fallback],
            ..Default::default()
        };
        let mut task = request("model");
        task.executor.selector = Some("test.primary".to_string());
        let mut plan = AgentTaskPlan::new("conditional-credentials", vec![task]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    selector: Some("test.primary".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    selector: Some("test.conditional".to_string()),
                    provider_config: json!({"provider":"conditional"}),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    selector: Some("test.fallback".to_string()),
                    provider_config: json!({"provider":"fallback"}),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let admitted = admit_plan_provider_dispatchability_with_providers(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("route after missing conditional credential is selected");
        assert_eq!(
            admitted.tasks[0].executor.selector.as_deref(),
            Some("test.fallback")
        );
        assert!(admitted.tasks[0].executor.secret_env.is_empty());
        assert_eq!(
            admitted.tasks[0].metadata["provider_readiness_routing"]["provider_rotations_used"],
            0
        );
    }

    #[test]
    fn live_verification_receives_the_selected_fallback_credential() {
        let root = tempfile::tempdir().expect("tempdir");
        let credential = root.path().join("account.json");
        let count = root.path().join("count");
        let script = root.path().join("readiness.js");
        std::fs::write(&credential, r#"{"token":"fallback-account"}"#)
            .expect("fallback credential");
        std::fs::write(
            &script,
            "const fs=require('fs');fs.appendFileSync(process.argv[2],'probe\\n');const token=process.env.TEST_ACCOUNT_TOKEN||'';const ready=token==='fallback-account';process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready,classification:ready?'ready':'auth_failure',retryable:false,remediation:token,reason:token,cache_key:token,identity:{account:token}}));",
        )
        .expect("readiness script");
        let mut provider = provider(&script, &count);
        provider.capabilities = vec!["provider_owned_auth".to_string()];
        provider.provider_defaults = serde_json::from_value(json!({
            "conditional-account": {
                "secret_env": ["TEST_ACCOUNT_TOKEN"],
                "secret_env_sources": {
                    "TEST_ACCOUNT_TOKEN": {
                        "source": "json-file",
                        "path": credential,
                        "field": "token"
                    }
                }
            }
        }))
        .expect("provider defaults");
        let mut task = request("model");
        task.executor.config = json!({"provider":"conditional-account"});
        task.executor
            .secret_env
            .push(format!("UNRELATED_TASK_SECRET_{}", uuid::Uuid::new_v4()));

        let verdict = evaluate_request_dispatchability(
            &catalog(provider),
            &task,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .dispatchability;

        assert!(verdict.ready, "{verdict:?}");
        assert_eq!(
            verdict.checks.credentials.status,
            AgentTaskProviderCredentialStatus::Verified
        );
        assert_eq!(
            std::fs::read_to_string(count).expect("probe count"),
            "probe\n"
        );
        assert!(!serde_json::to_string(&verdict)
            .expect("serialized verdict")
            .contains("fallback-account"));
    }
}
