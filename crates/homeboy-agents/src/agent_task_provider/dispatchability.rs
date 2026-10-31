use serde::{Deserialize, Serialize};

use super::{
    capacity_readiness::{capacity_readiness_from_probe_result, capacity_readiness_unknown},
    effective_provider_config,
    executor::effective_provider_for_request,
    provider_credential_readiness, resolve_provider_for_backend,
    runtime_readiness::provider_requires_live_auth_validation,
    validate_provider_immediate_failure_patterns, AgentTaskProviderCapacityReadiness,
    AgentTaskProviderCatalog, ProviderResolution, ProviderRuntimeReadinessCache,
};
use crate::agent_task_scheduler::{
    AgentTaskPlan, AgentTaskScheduleSupport, ProviderRouteDiagnosticData, ProviderRouteEvidence,
};
use homeboy_core::Error;
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
    /// Per-connected-account capacity this probe observed, typed distinctly
    /// from a generic runtime failure (#14858). Always present: a route that
    /// was never probed carries `Unknown` with the reason it was not probed,
    /// rather than omitting the field.
    #[serde(default)]
    pub capacity: AgentTaskProviderCapacityReadiness,
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

/// Capacity-only readiness for one route (#15024).
///
/// This is the per-route building block behind `homeboy agent-task capacity`:
/// it resolves the route exactly the way dispatchability does, then runs the
/// provider's declared readiness invocation in capacity mode (`mode:
/// "capacity"`, a distinct cache identity that can never satisfy a
/// live-inference probe). Capacity support is a runtime opt-in declared by
/// the runtime extension: a runtime that ignores the mode still
/// answers, but this function reports only whatever `capacity` object that
/// answer carries — never a dispatchability verdict — so an ignoring runtime
/// cannot pass its normal inference probe off as capacity evidence.
///
/// Failure is a value, not an error: a route that does not resolve, fails a
/// structural check, or whose probe fails reports `Unknown` capacity naming
/// why, so one broken route cannot hide the others in a rotation sweep.
pub fn evaluate_provider_capacity_with_config(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
    config: &Value,
    cache: &mut ProviderRuntimeReadinessCache,
    deadline_unix_ms: Option<u64>,
) -> AgentTaskProviderCapacityReadiness {
    let provider = match resolve_provider_for_backend(catalog.providers(), backend, selector) {
        ProviderResolution::Resolved(provider) => provider,
        ProviderResolution::NotFound
        | ProviderResolution::AmbiguousExtensionAlias { .. }
        | ProviderResolution::SelectorMismatch { .. } => {
            return capacity_readiness_unknown(
                "the requested backend/selector route did not resolve",
            );
        }
    };
    let supported = provider
        .cli
        .profiles
        .iter()
        .filter_map(|profile| profile.model.as_deref())
        .collect::<Vec<_>>();
    if !model.is_none_or(|model| supported.is_empty() || supported.contains(&model)) {
        return capacity_readiness_unknown(
            "the selected model is not declared by the routed provider",
        );
    }
    let credentials = provider_credential_readiness(provider);
    if !credentials.dispatchable {
        return capacity_readiness_unknown(
            "required provider credentials are not configured, so capacity was not probed",
        );
    }
    if provider_requires_live_auth_validation(provider) && provider.readiness_invocation.is_none() {
        return capacity_readiness_unknown(
            "provider-owned authentication has no declared readiness invocation",
        );
    }
    if validate_provider_immediate_failure_patterns(provider).is_err() {
        return capacity_readiness_unknown(
            "provider immediate-failure configuration is invalid, so capacity was not probed",
        );
    }
    if provider.readiness_invocation.is_none() {
        return capacity_readiness_unknown(
            "the provider declares no readiness invocation, so capacity cannot be observed",
        );
    }
    let effective_config = effective_provider_config(config, model);
    let declared_credential_env = match super::secrets::provider_declared_credential_env(provider) {
        Ok(credential_env) => credential_env,
        Err(error) => {
            return capacity_readiness_unknown(format!(
                "provider credentials did not resolve, so capacity was not probed: {}",
                error.message
            ));
        }
    };
    match super::runtime_readiness::capacity_mode_readiness_verdict_with_deadline(
        provider,
        &effective_config,
        &declared_credential_env,
        cache,
        deadline_unix_ms,
    ) {
        Ok(verdict) => {
            let readiness = capacity_readiness_from_probe_result(&verdict);
            if matches!(
                readiness,
                AgentTaskProviderCapacityReadiness::Unknown { .. }
            ) && !verdict.ready
                && !verdict.reason.trim().is_empty()
            {
                // The probe ran but the route rejected it (auth, account,
                // unavailable, ...) without publishing capacity. The route's
                // own reason is the diagnostic an operator needs; a generic
                // "does not report capacity" would hide the actual failure.
                let classification = verdict.classification.trim();
                let reason = homeboy_core::redaction::redact_string(&verdict.reason);
                return capacity_readiness_unknown(if classification.is_empty() {
                    reason
                } else {
                    format!("{classification}: {reason}")
                });
            }
            readiness
        }
        Err(error) => {
            let timed_out = error.details["classification"] == "timeout";
            capacity_readiness_unknown(if timed_out {
                "the capacity probe timed out before capacity could be observed".to_string()
            } else {
                format!("the capacity probe failed to run: {}", error.message)
            })
        }
    }
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
        false,
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
    generated_fanout_context: bool,
) -> AgentTaskProviderDispatchability {
    let candidate_providers = catalog
        .providers()
        .iter()
        .filter(|provider| provider.backend == backend)
        .collect::<Vec<_>>();
    let unresolved = |state: &'static str, reason: &'static str, route_reason: String| {
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
                status: AgentTaskProviderCredentialStatus::Unverified,
                ready: false,
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
    let (runtime, runtime_evidence) = if probe_runtime
        && model_ready
        && credentials.dispatchable
        && configuration.ready
    {
        let config = effective_provider_config(config, model);
        match super::runtime_readiness::readiness_verdict_with_credentials_and_deadline_for_generated_fanout_context(
                provider,
                &config,
                credential_env,
                cache,
                deadline_unix_ms,
                generated_fanout_context,
                // Live dispatchability never probes in capacity mode; the two
                // verdicts are distinct cache identities by design (#15024).
                None,
            ) {
                Ok(verdict) => {
                    let remediation = (!verdict.remediation.trim().is_empty())
                        .then(|| homeboy_core::redaction::redact_string(&verdict.remediation));
                    if let Some(remediation) = remediation.as_ref() {
                        runtime_remediation.push(remediation.clone());
                    }
                    let classification = match verdict.classification.trim() {
                        "" => "unknown".to_string(),
                        "provider_account_blocked" => "account".to_string(),
                        classification => classification.to_string(),
                    };
                    let reason = (!verdict.ready).then(|| {
                        if verdict.reason.trim().is_empty() {
                            classification.clone()
                        } else {
                            let mut cause = homeboy_core::redaction::redact_string(&verdict.reason);
                            let prefix = format!("{}:", verdict.classification.trim());
                            while let Some(next) = cause.strip_prefix(&prefix) {
                                cause = next.trim_start().to_string();
                            }
                            format!("{classification}: {cause}")
                        }
                    });
                    let evidence = AgentTaskProviderRuntimeEvidence {
                        classification: sanitize_classification(&verdict.classification),
                        retryable: verdict.retryable,
                        remediation,
                        cache_identity: verdict
                            .durable_identity
                            .as_ref()
                            .and_then(|identity| identity.cache_key_sha256.clone())
                            .or_else(|| {
                                (!verdict.cache_key.is_empty()).then(|| {
                                    homeboy_engine_primitives::content_hash::sha256_hex(
                                        verdict.cache_key.as_bytes(),
                                    )
                                })
                            }),
                        provider_identity: verdict
                            .durable_identity
                            .as_ref()
                            .and_then(|identity| identity.identity_sha256.clone())
                            .or_else(|| {
                                (!verdict.identity.is_null()).then(|| {
                                    homeboy_engine_primitives::content_hash::sha256_hex(
                                        &serde_json::to_vec(&verdict.identity).unwrap_or_default(),
                                    )
                                })
                            }),
                        capacity: capacity_readiness_from_probe_result(&verdict),
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
                            capacity: capacity_readiness_unknown(if deadline_exhausted {
                                "the provider readiness probe timed out before capacity could be observed"
                            } else {
                                "the provider readiness probe failed to run"
                            }),
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
    // Capacity exhaustion is orthogonal to `provider_owned_auth`: a provider
    // with perfectly valid, non-provider-owned credentials can still be out
    // of capacity, so this check does not gate on it the way
    // `credentials_rejected`/`account_blocked` do (#14858).
    let capacity_exhausted = !runtime.ready
        && runtime_evidence
            .as_ref()
            .is_some_and(|evidence| evidence.capacity.is_exhausted());
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
    } else if capacity_exhausted {
        (
            "capacity_exhausted",
            false,
            "the connected account has no capacity remaining right now",
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
    } else if state == "credentials_unusable" {
        // The generic sentence alone reads as "the login was revoked" even
        // when the provider reported a concrete, different cause (for example
        // an agent directory with no models). Carry the provider's own
        // classification through so operators see what actually failed
        // (Extra-Chill/homeboy#16841).
        match runtime.reason.as_deref() {
            Some(cause) if !cause.trim().is_empty() => format!("{reason} ({cause})"),
            _ => reason.to_string(),
        }
    } else if state == "capacity_exhausted" {
        let detail = runtime.reason.as_deref().unwrap_or(reason);
        let capacity = runtime_evidence.as_ref().map(|evidence| &evidence.capacity);
        let mut message = match capacity.and_then(|capacity| capacity.reset_at()) {
            Some(reset_at) => format!("capacity_exhausted: {detail} (resets at {reset_at})"),
            None => format!("capacity_exhausted: {detail}"),
        };
        // A route that rotates across accounts is only exhausted when every
        // account is; name each one's reset so operators see the whole pool.
        let accounts = capacity
            .map(|capacity| capacity.accounts())
            .unwrap_or_default()
            .iter()
            .map(|account| match account.reset_at.as_deref() {
                Some(reset_at) => format!("{} {} until {reset_at}", account.account, account.state),
                None => format!("{} {}", account.account, account.state),
            })
            .collect::<Vec<_>>();
        if !accounts.is_empty() {
            message.push_str(&format!("; accounts: {}", accounts.join(", ")));
        }
        message
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
        "provider_account_blocked" => "account".to_string(),
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

/// Returns the runtime-readiness cache identity for a plan's first selected
/// route, using the same route candidate and secret normalization as admission.
pub fn provider_runtime_readiness_cache_identity_for_plan(
    catalog: &AgentTaskProviderCatalog,
    plan: &AgentTaskPlan,
) -> homeboy_core::Result<String> {
    let task = plan.tasks.first().ok_or_else(|| {
        Error::internal_unexpected("provider readiness identity requires a provider task")
    })?;
    let (mut request, _) =
        AgentTaskScheduleSupport::provider_route_candidates(task, plan.options.rotation.as_ref())
            .into_iter()
            .next()
            .ok_or_else(|| {
                Error::internal_unexpected("provider readiness identity requires a route")
            })?;
    let provider = effective_provider_for_request(&request, catalog.providers())
        .map_err(|reason| {
            Error::validation_invalid_argument(
                "provider_runtime_readiness",
                reason,
                Some(request.executor.backend.clone()),
                None,
            )
        })?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "provider_runtime_readiness",
                "no provider satisfies the effective route and required capabilities",
                Some(request.executor.backend.clone()),
                None,
            )
        })?;
    let base_secret_env = request
        .metadata
        .get("provider_admission")
        .and_then(|value| value.get("base_secret_env"))
        .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
        .unwrap_or_else(|| request.executor.secret_env.clone());
    super::secrets::apply_provider_runner_secret_env_contract_for_request(
        &mut request,
        &provider,
        &base_secret_env,
    );
    let credential_env = super::secrets::provider_request_credential_env(&request, &provider)
        .map_err(|error| {
            Error::validation_invalid_argument(
                "provider_runtime_readiness",
                error.message,
                Some(request.executor.backend.clone()),
                None,
            )
        })?;
    let config = effective_provider_config(&request.executor.config, request.executor.model());
    super::runtime_readiness::readiness_cache_identity_for_generated_fanout_context(
        &provider,
        &config,
        &credential_env,
        task.metadata["provider_readiness_generated_fanout_context"] == true,
        None,
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
        request.metadata["provider_readiness_generated_fanout_context"] == true,
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
            capacity: capacity_readiness_unknown(
                "the provider route did not resolve, so capacity was not evaluated",
            ),
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

/// Whether this process already selected Lab placement for its workload.
///
/// A Lab-placed Cook runs its provider on the runner, with the runner's own
/// credentials. Lab staging proves those through the runner-side readiness
/// preflight (`providers --validate-readiness` on the runner). Controller-local
/// live readiness only proves the controller's credentials, so it must not
/// gate Lab admission (#15198).
pub fn provider_readiness_is_runner_owned() -> bool {
    homeboy_core::parsed_command_preflight::captured_result().is_some_and(|result| {
        result.placement.selected == homeboy_lab_contract::EffectiveExecutionPlacement::Lab
    })
}

/// Admit a compiled plan for the execution placement this process selected.
///
/// Local placement keeps full live admission and route selection. Lab placement
/// defers live readiness to the runner and applies only the deterministic,
/// probe-free contract checks here, returning the plan unchanged so the runner
/// selects the effective route.
pub fn admit_plan_provider_dispatchability_for_placement(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<AgentTaskPlan> {
    admit_plan_provider_dispatchability_for_runner_ownership(
        plan,
        catalog,
        cache,
        provider_readiness_is_runner_owned(),
    )
}

/// Placement-explicit form of [`admit_plan_provider_dispatchability_for_placement`]
/// so callers and tests do not depend on process-global preflight state.
pub fn admit_plan_provider_dispatchability_for_runner_ownership(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
    cache: &mut ProviderRuntimeReadinessCache,
    runner_owned: bool,
) -> homeboy_core::Result<AgentTaskPlan> {
    if !runner_owned {
        return admit_plan_provider_dispatchability_with_providers(plan, catalog, cache);
    }
    // A rotation plan is judged as a complete effective chain on the runner, so
    // an unavailable primary cannot hide a valid fallback here either.
    let has_rotation = plan
        .options
        .rotation
        .as_ref()
        .is_some_and(|rotation| !rotation.entries.is_empty());
    if !has_rotation {
        preflight_plan_provider_dispatchability_without_runtime_with_providers(
            plan, catalog, cache,
        )?;
    }
    Ok(plan.clone())
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
            // Readiness has its own bounded invocation budget (the provider
            // declaration, normally 20s). A task execution deadline is not a
            // per-phase readiness deadline: passing it here can leave only a
            // few seconds after preview/materialization and falsely reject a
            // route that validates within its declared probe window. Retain a
            // plan-level deadline, which bounds admission as a whole.
            candidate.limits.execution_deadline_unix_ms =
                plan.options.execution_budget.deadline_unix_ms;
            if super::is_fixture_backend(&candidate.executor.backend) {
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
                // The bound fallback is the model this task runs; keep the
                // recorded selection in agreement with the executor.
                if selected.metadata["model_selection"].is_object() {
                    selected.metadata["model_selection"]["selected"] =
                        serde_json::json!(selected.executor.model());
                }
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
            .with_retryable(true);
            error.details["classification"] = serde_json::json!("timeout");
            error.details["zero_provider_executions"] = serde_json::json!(true);
            error.details["deadline_unix_ms"] = serde_json::json!(deadline);
            error
        } else {
            homeboy_core::Error::validation_invalid_argument(
                "provider_dispatchability",
                format!(
                    "agent-task backend `{}` is not dispatchable across any reachable rotation route: {}",
                    candidate.executor.backend,
                    rotation_failure_detail(
                        &first_verdict.reason,
                        &current_skipped,
                        homeboy_core::process::host_identity().as_deref(),
                    )
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

/// Render the rotation-level admission failure so every route's own cause, the
/// provider's remediation, and the host the readiness was evaluated on reach
/// the operator. Admission can run on the controller or on a runner; without
/// the host an operator cannot tell a controller-local catalog gap from a
/// broken runner (Extra-Chill/homeboy#16841, #15198).
fn rotation_failure_detail(
    first_reason: &str,
    routes: &[ProviderRouteEvidence],
    host: Option<&str>,
) -> String {
    fn route_label(route: &ProviderRouteEvidence) -> String {
        route
            .model
            .clone()
            .or_else(|| route.selector.clone())
            .unwrap_or_else(|| route.backend.clone())
    }
    let mut detail = match routes.first() {
        Some(first) => format!("{}: {first_reason}", route_label(first)),
        None => first_reason.to_string(),
    };
    for route in routes.iter().skip(1) {
        detail.push_str(&format!("; {}: {}", route_label(route), route.reason));
    }
    if let Some(remediation) = routes
        .iter()
        .filter_map(|route| route.remediation.as_deref())
        .find(|remediation| !remediation.trim().is_empty())
    {
        detail.push_str(&format!(". Remediation: {remediation}"));
    }
    if let Some(host) = host.filter(|host| !host.trim().is_empty()) {
        detail.push_str(&format!(" (readiness evaluated on host `{host}`)"));
    }
    detail
}

/// Reject deterministic provider contract defects after the effective executor
/// config has been compiled, without invoking provider-owned live readiness.
pub fn preflight_plan_provider_dispatchability_without_runtime_with_providers(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<()> {
    for task in &plan.tasks {
        // The in-tree test double has no catalog manifest; plan admission
        // selects it through the `is_fixture_backend` gate
        // [`selected_plan_provider_dispatchability_with_providers`] applies, so
        // the runner-owned static preflight must not reject it first.
        if super::is_fixture_backend(&task.executor.backend) {
            continue;
        }
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
    use super::super::test_pi::FakePi;
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

    fn route_pi(models: &str) -> FakePi {
        FakePi::with_probes("exit 99", "printf 'test-pi\\n'", models)
    }

    fn pi_task(fake: &FakePi, model: &str) -> AgentTaskRequest {
        let mut task = fake.request("route");
        task.executor.model = Some(model.into());
        task
    }

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
        let mut plan = AgentTaskPlan::new("deadline", vec![task]);
        plan.options.execution_budget.deadline_unix_ms = Some(deadline);
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
            "secret_env_requirements": [{
                "env": ["HOMEBOY_TEST_REVOCABLE_REFRESH_TOKEN"],
                "secret_env_sources": {
                    "HOMEBOY_TEST_REVOCABLE_REFRESH_TOKEN": {
                        "source": "json-file",
                        "path": auth_path,
                        "field": "token"
                    }
                }
            }]
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
        let fake = route_pi(
            "if [ \"$HOMEBOY_TEST_REVOCABLE_REFRESH_TOKEN\" = present-and-live-checked ]; then printf 'test model\\n'; else printf 'No models available\\n'; fi",
        );
        let auth = fake.root.path().join("auth.json");
        std::fs::write(&auth, r#"{"token":"present-and-live-checked"}"#).unwrap();
        let mut live_provider = fake.provider();
        live_provider.secret_env_requirements =
            provider_with_present_but_unverifiable_credential(&auth).secret_env_requirements;
        let catalog = catalog(live_provider);

        let verdict = evaluate_provider_dispatchability_with_config(
            &catalog,
            "pi",
            None,
            Some("test/model"),
            &fake.config(),
            true,
            &mut ProviderRuntimeReadinessCache::default(),
        );

        assert!(verdict.ready);
        assert!(
            verdict.checks.credentials.verified,
            "the Pi probe received the resolved credential and passed live verification"
        );
        assert_eq!(verdict.reason, "all dispatchability checks passed");
    }

    /// Controller whose provider-owned credential is present but whose live
    /// readiness probe rejects it (the #15198 shape: a controller-local OAuth
    /// token returning 401). The probe appends to `count` on every run.
    fn controller_with_rejected_live_credentials(
        root: &std::path::Path,
    ) -> (AgentTaskProviderCatalog, std::path::PathBuf) {
        let auth = root.join("auth.json");
        let count = root.join("count");
        let script = root.join("readiness.js");
        std::fs::write(&auth, r#"{"token":"controller-token-rejected"}"#).expect("write auth");
        std::fs::write(
            &script,
            "require('fs').appendFileSync(process.argv[2],'probe\\n');process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:false,classification:'auth_failure',retryable:false,remediation:'Sign in again.',reason:'Token refresh failed: 401',cache_key:'k',identity:{}}));",
        )
        .expect("readiness script");
        let mut provider = provider_with_present_but_unverifiable_credential(&auth);
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
        (catalog(provider), count)
    }

    fn revocable_plan() -> AgentTaskPlan {
        let mut task = request("model");
        task.executor.backend = "revocable".to_string();
        AgentTaskPlan::new("lab-admission", vec![task])
    }

    #[test]
    fn local_placement_still_rejects_controller_credentials_the_live_probe_refuses() {
        let root = tempfile::tempdir().unwrap();
        let count = root.path().join("count");
        let fake = route_pi(&format!(
            "printf 'probe\\n' >> '{}'; printf 'No models available\\n'",
            count.display()
        ));
        let catalog = catalog(fake.provider());
        let plan = AgentTaskPlan::new("local-admission", vec![pi_task(&fake, "test/model")]);

        let error = admit_plan_provider_dispatchability_for_runner_ownership(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            false,
        )
        .expect_err("local placement must keep live admission");

        assert_eq!(error.details["field"], "provider_dispatchability");
        assert!(count.exists(), "local admission must run the live probe");
    }

    #[test]
    fn lab_placement_does_not_gate_on_controller_live_credentials() {
        let root = tempfile::tempdir().expect("tempdir");
        let (catalog, count) = controller_with_rejected_live_credentials(root.path());
        let plan = revocable_plan();

        let admitted = admit_plan_provider_dispatchability_for_runner_ownership(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            true,
        )
        .expect("Lab placement defers live readiness to the runner");

        assert!(
            !count.exists(),
            "Lab admission must not probe controller-local credentials"
        );
        assert_eq!(admitted.tasks.len(), plan.tasks.len());
        assert_eq!(admitted.tasks[0].executor.backend, "revocable");
    }

    #[test]
    fn lab_placement_still_rejects_static_route_defects() {
        let root = tempfile::tempdir().expect("tempdir");
        let (catalog, count) = controller_with_rejected_live_credentials(root.path());
        let mut plan = revocable_plan();
        plan.tasks[0].executor.backend = "missing".to_string();

        admit_plan_provider_dispatchability_for_runner_ownership(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            true,
        )
        .expect_err("an unresolvable route is a deterministic defect on any placement");

        assert!(!count.exists());
    }

    fn skipped_route(
        model: &str,
        state: &str,
        reason: &str,
        remediation: Option<&str>,
    ) -> ProviderRouteEvidence {
        ProviderRouteEvidence {
            backend: "pi".to_string(),
            selector: None,
            model: Some(model.to_string()),
            state: state.to_string(),
            reason: reason.to_string(),
            reset_at: None,
            classification: None,
            retryable: false,
            remediation: remediation.map(str::to_string),
            cache_identity: None,
            provider_identity: None,
            runtime_evidence: None,
            checks: None,
            diagnostic_data: None,
        }
    }

    #[test]
    fn rotation_failure_names_every_route_cause_remediation_and_host() {
        let routes = vec![
            skipped_route(
                "roadie/cook-heavy",
                "runtime_unavailable",
                "runtime_unavailable: deterministic_incompatibility: model_not_available",
                Some("Select a model available in Pi agent directory /home/op/.roadie/pi-agent."),
            ),
            skipped_route(
                "roadie/sol",
                "credentials_unusable",
                "provider-owned authentication or its runtime is revoked or unusable (auth_failure: no_models_available)",
                None,
            ),
        ];

        let detail = rotation_failure_detail(&routes[0].reason, &routes, Some("controller-1"));

        assert_eq!(
            detail,
            "roadie/cook-heavy: runtime_unavailable: deterministic_incompatibility: model_not_available; \
             roadie/sol: provider-owned authentication or its runtime is revoked or unusable (auth_failure: no_models_available). \
             Remediation: Select a model available in Pi agent directory /home/op/.roadie/pi-agent. \
             (readiness evaluated on host `controller-1`)"
        );
    }

    #[test]
    fn rotation_failure_without_route_evidence_or_host_is_the_first_reason() {
        assert_eq!(
            rotation_failure_detail("plain cause", &[], None),
            "plain cause"
        );
    }

    #[test]
    fn revoked_or_unusable_credentials_preserve_provider_remediation() {
        let fake = route_pi("printf 'No models available\\n'");
        let catalog = catalog(fake.provider());
        let verdict = evaluate_provider_dispatchability_with_config(
            &catalog,
            "pi",
            None,
            None,
            &fake.config(),
            true,
            &mut ProviderRuntimeReadinessCache::default(),
        );

        assert_eq!(verdict.state, "credentials_unusable");
        assert!(!verdict.ready);
        // The provider's concrete cause travels with the generic sentence so
        // an empty agent dir is not reported as a bare revoked login (#16841).
        assert!(
            verdict
                .reason
                .contains("(auth_failure: no_models_available)"),
            "{}",
            verdict.reason
        );
        assert_eq!(
            verdict.checks.credentials.status,
            AgentTaskProviderCredentialStatus::Unusable
        );
        assert_eq!(
            verdict.checks.credentials.reason.as_deref(),
            Some("auth_failure: no_models_available")
        );
        assert!(verdict.checks.credentials.remediation[0].contains("Log in to a provider"));

        let error = preflight_provider_dispatchability_with_config(
            &catalog,
            "pi",
            None,
            None,
            &fake.config(),
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect_err("unusable credential must fail before dispatch");
        assert!(error.message.contains("no_models_available"), "{error}");
        assert!(error.message.contains("Log in to a provider"), "{error}");
    }

    #[test]
    fn offline_provider_owned_auth_remains_runtime_unavailable() {
        let fake = route_pi("exit 1");
        let verdict = evaluate_provider_dispatchability_with_config(
            &catalog(fake.provider()),
            "pi",
            None,
            None,
            &fake.config(),
            true,
            &mut ProviderRuntimeReadinessCache::default(),
        );

        assert_eq!(verdict.state, "runtime_unavailable");
        assert_eq!(
            verdict.reason,
            "runtime_unavailable: transient_failure: model_list_probe_interrupted"
        );
        assert_eq!(
            verdict.checks.credentials.status,
            AgentTaskProviderCredentialStatus::Unverified
        );
        assert_eq!(
            verdict.checks.runtime.reason.as_deref(),
            Some("transient_failure: model_list_probe_interrupted")
        );
        assert!(verdict.checks.credentials.remediation[0]
            .contains("Retry the bounded Pi model catalog probe"));
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
    fn unresolved_route_never_claims_credentials_are_missing() {
        let provider: super::super::AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "credential.provider",
            "backend": "credential",
            "provider_defaults": {
                "credential": {
                    "required_secret_env": ["HOMEBOY_TEST_UNRESOLVED_ROUTE_CREDENTIAL"]
                }
            }
        }))
        .expect("provider fixture");
        let verdict = evaluate_provider_dispatchability(
            &catalog(provider),
            "credential",
            Some("missing.provider"),
            None,
            false,
        );

        assert_eq!(verdict.state, "route_unavailable");
        assert_eq!(
            verdict.checks.credentials.status,
            AgentTaskProviderCredentialStatus::Unverified
        );
        assert!(!verdict.checks.credentials.ready);
        assert!(verdict.checks.credentials.missing.is_empty());
    }

    #[test]
    fn aggregate_precedence_keeps_each_failed_dimension_unavailable() {
        let credential_provider: super::super::AgentTaskExecutorProvider =
            serde_json::from_value(json!({
                "id": "credential.provider",
                "backend": "credential",
                "secret_env_requirements": [{
                    "env": ["HOMEBOY_TEST_DISPATCHABILITY_MISSING_CREDENTIAL"]
                }]
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
        // An unresolved route was never probed: credentials are unverified,
        // not ready, and never reported as missing.
        assert!(!route_failed.checks.credentials.ready);
        assert_eq!(
            route_failed.checks.credentials.status,
            AgentTaskProviderCredentialStatus::Unverified
        );
        assert!(route_failed.checks.credentials.missing.is_empty());

        let model_unavailable =
            evaluate_provider_dispatchability(&catalog, "model", None, Some("unsupported"), false);
        assert_eq!(model_unavailable.state, "model_unavailable");
        assert!(!model_unavailable.ready);
        assert!(!model_unavailable.checks.model.ready);

        // Without a live probe, a route whose every static check passes is
        // structurally dispatchable rather than live-verified ready.
        let all_ready = evaluate_provider_dispatchability(&catalog, "ready", None, None, false);
        assert_eq!(all_ready.state, "structurally_dispatchable");
        assert!(all_ready.ready);
    }

    #[test]
    fn shared_evaluator_uses_effective_config_redacts_runtime_reason_and_deduplicates() {
        let _home = crate::agent_task_provider::runtime_readiness::stable_identity_home();
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let fake = route_pi(&format!("printf 'probe\\n' >> '{}'; if [ \"$MARKER\" != ready ]; then printf 'token=runtime-secret' >&2; exit 1; fi; printf 'test model\\n'", count.display()));
        let catalog = catalog(fake.provider());
        let mut blocked_config = fake.config();
        blocked_config["runtime_env"] = json!({"MARKER":"blocked"});
        let mut ready_config = fake.config();
        ready_config["runtime_env"] = json!({"MARKER":"ready"});
        let mut cache = ProviderRuntimeReadinessCache::default();

        let blocked = evaluate_provider_dispatchability_with_config(
            &catalog,
            "pi",
            None,
            None,
            &blocked_config,
            true,
            &mut cache,
        );
        assert_eq!(blocked.state, "runtime_unavailable");
        assert_eq!(
            blocked.checks.runtime.reason.as_deref(),
            Some("transient_failure: model_list_probe_interrupted")
        );

        let ready = evaluate_provider_dispatchability_with_config(
            &catalog,
            "pi",
            None,
            None,
            &ready_config,
            true,
            &mut cache,
        );
        assert!(ready.ready);
        let ready_again = evaluate_provider_dispatchability_with_config(
            &catalog,
            "pi",
            None,
            None,
            &ready_config,
            true,
            &mut cache,
        );
        assert!(ready_again.ready);
        let probes = std::fs::read_to_string(count).expect("probe count");
        assert_eq!(probes.lines().count(), 3); // transient failure retries once
        let serialized = serde_json::to_string(&blocked).unwrap();
        assert!(!serialized.contains("runtime-secret"));
        // Pi exposes a stable reason code instead of echoing raw probe stderr.
        assert!(serialized.contains("model_list_probe_interrupted"));
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
        let fake = route_pi("printf 'test ready-model\\n'");
        let catalog = catalog(fake.provider());
        let mut plan = AgentTaskPlan::new(
            "rotation-readiness",
            vec![pi_task(&fake, "test/blocked-model")],
        );
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    model: Some("test/blocked-model".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    model: Some("test/ready-model".to_string()),
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

        assert_eq!(plan.tasks[0].executor.model(), Some("test/blocked-model"));
        assert!(plan.tasks[0]
            .metadata
            .get("provider_readiness_routing")
            .is_none());
    }

    #[test]
    fn readiness_fallback_uses_rotation_budget_not_dispatch_attempt_budget() {
        let fake = route_pi("printf 'test ready-model\\n'");
        let catalog = catalog(fake.provider());
        let mut plan = AgentTaskPlan::new(
            "bounded-readiness",
            vec![pi_task(&fake, "test/blocked-model")],
        );
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![AgentTaskProviderRotationEntry {
                model: Some("test/ready-model".to_string()),
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

        assert_eq!(plan.tasks[0].executor.model(), Some("test/blocked-model"));
    }

    /// An unpinned rotation may bind a fallback during readiness; the bound
    /// task must then record that fallback as its selected model so the plan,
    /// the executor, and the recorded provider model agree.
    #[test]
    fn readiness_fallback_records_the_bound_model_as_selected() {
        let fake = route_pi("printf 'test fallback\\n'");
        let catalog = catalog(fake.provider());
        let mut task = pi_task(&fake, "test/primary");
        task.metadata = json!({
            "model_selection": {
                "requested": null,
                "selected": "test/primary",
                "reason": "policy-rotation"
            }
        });
        let mut plan = AgentTaskPlan::new("rotatable-readiness", vec![task]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![AgentTaskProviderRotationEntry {
                model: Some("test/fallback".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });
        plan.options.execution_budget.max_provider_rotations = 1;

        let admitted = admit_plan_provider_dispatchability_with_providers(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("rotatable plan binds the ready fallback");

        let task = &admitted.tasks[0];
        assert_eq!(task.executor.model(), Some("test/fallback"));
        assert_eq!(
            task.metadata["model_selection"]["selected"],
            "test/fallback"
        );
        assert_eq!(
            task.metadata["provider_readiness_routing"]["skipped"][0]["model"],
            "test/primary"
        );
    }

    #[test]
    fn repeated_admission_reconsiders_a_recovered_primary_from_the_durable_plan() {
        let _home = crate::agent_task_provider::runtime_readiness::stable_identity_home();
        let fake = route_pi("printf 'test fallback-one\\ntest fallback-two\\n'");
        let catalog = catalog(fake.provider());
        let mut plan =
            AgentTaskPlan::new("repeated-admission", vec![pi_task(&fake, "test/primary")]);
        plan.options.execution_budget.max_provider_rotations = 1;
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    model: Some("test/fallback-one".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    model: Some("test/fallback-two".to_string()),
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
        assert_eq!(
            admitted.tasks[0].executor.model(),
            Some("test/fallback-one")
        );
        assert_eq!(admitted.options.rotation.as_ref().unwrap().entries.len(), 2);
        let recovered = route_pi("printf 'test primary\\n'");
        std::fs::copy(&recovered.command, &fake.command).unwrap();

        let readmitted = admit_plan_provider_dispatchability_with_providers(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("restart-style re-admission sees the recovered primary");
        assert_eq!(readmitted.tasks[0].executor.model(), Some("test/primary"));
        assert_eq!(plan.tasks[0].executor.model(), Some("test/primary"));
    }

    #[test]
    fn repeated_admission_of_a_bound_fallback_never_reconsiders_primary() {
        let _home = crate::agent_task_provider::runtime_readiness::stable_identity_home();
        let fake = route_pi("printf 'test fallback\\n'");
        let catalog = catalog(fake.provider());
        let mut plan = AgentTaskPlan::new("bound-admission", vec![pi_task(&fake, "test/primary")]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![AgentTaskProviderRotationEntry {
                model: Some("test/fallback".to_string()),
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
        let recovered = route_pi("printf 'test primary\\ntest fallback\\n'");
        std::fs::copy(&recovered.command, &fake.command).unwrap();

        let readmitted = admit_plan_provider_dispatchability_with_providers(
            &admitted,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("bound fallback remains admitted");

        assert_eq!(readmitted.tasks[0].executor.model(), Some("test/fallback"));
        assert_eq!(
            readmitted.tasks[0].metadata["provider_readiness_routing"],
            admitted.tasks[0].metadata["provider_readiness_routing"]
        );
    }

    #[test]
    fn transient_primary_recovers_before_fallback_selection() {
        let root = tempfile::tempdir().unwrap();
        let count = root.path().join("count");
        let fake = route_pi(&format!("if [ ! -f '{}' ]; then printf '1' > '{}'; exit 1; fi; printf '2' > '{}'; printf 'test primary\\n'", count.display(), count.display(), count.display()));
        let catalog = catalog(fake.provider());
        let mut plan =
            AgentTaskPlan::new("recovered-primary", vec![pi_task(&fake, "test/primary")]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    model: Some("test/primary".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    model: Some("test/fallback".to_string()),
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
        assert_eq!(admitted.tasks[0].executor.model(), Some("test/primary"));
        assert_eq!(std::fs::read_to_string(count).expect("probe count"), "2");
        assert!(admitted.tasks[0]
            .metadata
            .get("provider_readiness_routing")
            .is_none());
    }

    #[test]
    fn conditional_credentials_are_recomputed_for_each_rotation_route() {
        let fake = route_pi("printf 'test conditional\\ntest fallback\\n'");
        let mut provider = fake.provider();
        provider.secret_env_requirements =
            vec![super::super::AgentTaskProviderSecretEnvRequirement {
                env: vec![format!("HOMEBOY_MISSING_{}", uuid::Uuid::new_v4())],
                when: Some(json!({"path":"executor.config.provider","equals":"conditional"})),
                ..Default::default()
            }];
        let catalog = catalog(provider);
        let task = pi_task(&fake, "test/primary");
        let mut plan = AgentTaskPlan::new("conditional-credentials", vec![task]);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    model: Some("test/primary".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    model: Some("test/conditional".to_string()),
                    provider_config: json!({"provider":"conditional"}),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    model: Some("test/fallback".to_string()),
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
        assert_eq!(admitted.tasks[0].executor.model(), Some("test/fallback"));
        assert_eq!(
            admitted.tasks[0].metadata["provider_readiness_routing"]["skipped"][1]["state"],
            "credentials_missing"
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
        std::fs::write(&credential, r#"{"token":"fallback-account"}"#).unwrap();
        let fake = route_pi(&format!("printf 'probe\\n' >> '{}'; if [ \"$TEST_ACCOUNT_TOKEN\" = fallback-account ]; then printf 'test model\\n'; else printf 'No models available\\n'; fi", count.display()));
        let mut provider = fake.provider();
        provider.provider_defaults.extend(
            serde_json::from_value::<std::collections::BTreeMap<String, Value>>(json!({
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
            .expect("provider defaults"),
        );
        let mut task = pi_task(&fake, "test/model");
        task.executor.config["provider"] = json!("conditional-account");
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

    // Extension readiness JSON could publish known capacity and per-account
    // resets. Pi's CLI catalog probe publishes neither; those protocol-only
    // fixtures are retired rather than teaching FakePi the removed protocol.

    /// Pi publishes no capacity evidence: report Unknown without blocking an
    /// otherwise dispatchable route or silently claiming known availability.
    #[test]
    fn a_provider_that_never_publishes_capacity_stays_dispatchable_and_reports_unknown() {
        let fake = route_pi("printf 'test model\\n'");
        let catalog = catalog(fake.provider());
        let mut cache = ProviderRuntimeReadinessCache::default();

        let verdict = evaluate_provider_dispatchability_with_config(
            &catalog,
            "pi",
            None,
            Some("test/model"),
            &fake.config(),
            true,
            &mut cache,
        );

        assert!(
            verdict.ready,
            "silence about capacity must never block dispatch: {verdict:?}"
        );
        let evidence = verdict
            .readiness
            .live_inference
            .evidence
            .as_ref()
            .expect("live probe ran");
        assert_eq!(
            evidence.capacity,
            AgentTaskProviderCapacityReadiness::Unknown {
                reason: "the provider's readiness invocation does not report capacity".to_string(),
            }
        );
    }
}
