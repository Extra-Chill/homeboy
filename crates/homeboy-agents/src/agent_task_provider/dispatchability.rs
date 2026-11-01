use serde::{Deserialize, Serialize};

use super::{
    capacity_readiness::{capacity_readiness_from_probe_result, capacity_readiness_unknown},
    effective_provider_config,
    executor::effective_provider_for_request,
    resolve_provider_for_backend,
    runtime_readiness::provider_requires_live_auth_validation,
    AgentTaskProviderCapacityReadiness, AgentTaskProviderCatalog, ProviderResolution,
    ProviderRuntimeReadinessCache,
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
    /// (the built-in Pi probe) actually ran and confirmed the credential
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
        ProviderResolution::NotFound | ProviderResolution::SelectorMismatch { .. } => {
            return capacity_readiness_unknown(
                "the requested backend/selector route did not resolve",
            );
        }
    };
    let effective_config = effective_provider_config(config, model);
    match super::runtime_readiness::readiness_verdict(
        provider,
        &effective_config,
        &[],
        cache,
        deadline_unix_ms,
        false,
        Some(super::command_runner::PROVIDER_READINESS_CAPACITY_MODE),
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
pub fn evaluate_provider_dispatchability(
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
    let unresolved = |state: &'static str, reason: &'static str, route_reason: String| {
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
            runtime: AgentTaskProviderDispatchabilityCheck {
                ready: false,
                reason: None,
            },
        };
        AgentTaskProviderDispatchability {
            state,
            ready: false,
            reason: reason.to_string(),
            readiness: readiness_scopes(&checks, false, None),
            checks,
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
    let mut runtime_remediation = Vec::new();
    let (runtime, runtime_evidence) = if probe_runtime {
        let config = effective_provider_config(config, model);
        match super::runtime_readiness::readiness_verdict(
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
    // Only a successful live Pi probe verifies credentials.
    let credentials_verified = probe_runtime && runtime.ready;
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
    let (credential_status, credential_reason) = if credentials_verified {
        (AgentTaskProviderCredentialStatus::Verified, None)
    } else if probe_runtime && !runtime.ready && credentials_rejected && provider_owned_auth {
        (
            AgentTaskProviderCredentialStatus::Unusable,
            runtime.reason.clone(),
        )
    } else if !provider_owned_auth {
        (AgentTaskProviderCredentialStatus::NotRequired, None)
    } else if !probe_runtime {
        (
            AgentTaskProviderCredentialStatus::Unverified,
            Some("live validation of provider-owned authentication was not requested".to_string()),
        )
    } else {
        (AgentTaskProviderCredentialStatus::Unverified, None)
    };
    let (state, ready, reason) = if provider_owned_auth && !probe_runtime {
        (
            "credentials_unverified",
            false,
            "provider-owned authentication must be live-validated before dispatch",
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
    } else if !probe_runtime {
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
            ready: true,
            reason: None,
        },
        credentials: AgentTaskProviderDispatchabilityCredentialCheck {
            status: credential_status,
            ready: true,
            missing: Vec::new(),
            verified: credentials_verified,
            reason: credential_reason,
            remediation: runtime_remediation,
        },
        runtime,
    };
    let readiness = readiness_scopes(&checks, probe_runtime, runtime_evidence.clone());
    *runtime_evidence_out = runtime_evidence;
    AgentTaskProviderDispatchability {
        state,
        ready,
        reason,
        readiness,
        checks,
    }
}

fn readiness_scopes(
    checks: &AgentTaskProviderDispatchabilityChecks,
    probe_requested: bool,
    evidence: Option<AgentTaskProviderRuntimeEvidence>,
) -> AgentTaskProviderReadiness {
    let structural_ready = checks.route.ready && checks.model.ready && checks.credentials.ready;
    let structural_reason = if structural_ready {
        "route, model, and credentials are structurally dispatchable".to_string()
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
    credential_env: &[(String, String)],
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
    let mut runtime_evidence = None;
    let dispatchability = evaluate_provider_dispatchability(
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

/// Returns the runtime-readiness cache identity for a plan's first selected
/// route, using the same route candidate and secret normalization as admission.
pub fn provider_runtime_readiness_cache_identity_for_plan(
    catalog: &AgentTaskProviderCatalog,
    plan: &AgentTaskPlan,
) -> homeboy_core::Result<String> {
    let task = plan.tasks.first().ok_or_else(|| {
        Error::internal_unexpected("provider readiness identity requires a provider task")
    })?;
    let (request, _) =
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
    let credential_env = Vec::new();
    let config = effective_provider_config(&request.executor.config, request.executor.model());
    super::runtime_readiness::readiness_cache_identity_for_generated_fanout_context(
        &provider,
        &config,
        &credential_env,
        task.metadata["provider_readiness_generated_fanout_context"] == true,
        None,
    )
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
    config: &Value,
    probe_runtime: bool,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<AgentTaskProviderDispatchability> {
    let verdict = evaluate_provider_dispatchability(
        catalog,
        backend,
        selector,
        model,
        config,
        probe_runtime,
        &[],
        cache,
        &mut None,
        None,
        false,
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
        .credentials
        .reason
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

/// Admit a compiled plan for an explicit placement (or local admission with `None`).
///
/// Required-but-unavailable Lab placement fails before provider admission.
/// Local placement keeps full live admission and route selection. Lab placement
/// defers live readiness to the runner and applies only the deterministic,
/// probe-free contract checks here, returning the plan unchanged so the runner
/// selects the effective route. The caller owns the cache for probe deduplication.
///
/// Admit only executable placements before checking any provider. A required
/// Lab that resolved to Local is a placement failure, not a local workload.
/// Accept the directive explicitly so callers need not mutate captured state.
pub fn admit_plan_provider_dispatchability(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
    cache: &mut ProviderRuntimeReadinessCache,
    placement: Option<&homeboy_core::parsed_command_preflight::PlacementDirective>,
) -> homeboy_core::Result<AgentTaskPlan> {
    use homeboy_lab_contract::{EffectiveExecutionPlacement, ExecutionPlacementRequirement};

    if let Some(placement) = placement {
        if placement.required == ExecutionPlacementRequirement::Lab
            && placement.selected == EffectiveExecutionPlacement::Local
            && !placement.fallback.local_allowed
        {
            let reason = placement
                .fallback
                .reason
                .as_deref()
                .unwrap_or("no selected ready Lab runner");
            return Err(homeboy_core::Error::validation_invalid_argument(
                "placement",
                format!("required Lab placement is unavailable: {reason}"),
                Some("lab".to_string()),
                Some(vec![reason.to_string()]),
            ));
        }
    }
    let runner_owned =
        placement.is_some_and(|placement| placement.selected == EffectiveExecutionPlacement::Lab);
    if runner_owned {
        // A rotation plan is judged as a complete effective chain on the runner, so
        // an unavailable primary cannot hide a valid fallback here either.
        let has_rotation = plan
            .options
            .rotation
            .as_ref()
            .is_some_and(|rotation| !rotation.entries.is_empty());
        if !has_rotation {
            preflight_plan_provider_dispatchability_without_runtime(plan, catalog, cache)?;
        }
        return Ok(plan.clone());
    }
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
            let evaluated = evaluate_request_dispatchability(catalog, &candidate, &[], cache);
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
pub fn preflight_plan_provider_dispatchability_without_runtime(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<()> {
    for task in &plan.tasks {
        // The in-tree test double has no catalog manifest; plan admission
        // selects it through the `is_fixture_backend` gate
        // [`admit_plan_provider_dispatchability`] applies, so
        // the runner-owned static preflight must not reject it first.
        if super::is_fixture_backend(&task.executor.backend) {
            continue;
        }
        preflight_provider_dispatchability(
            catalog,
            &task.executor.backend,
            task.executor.selector.as_deref(),
            task.executor.model(),
            &task.executor.config,
            false,
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
    use serde_json::json;

    fn route_pi(models: &str) -> FakePi {
        FakePi::with_probes("exit 99", "printf 'test-pi\\n'", models)
    }

    fn pi_task(fake: &FakePi, model: &str) -> AgentTaskRequest {
        let mut task = fake.request("route");
        task.executor.model = Some(model.into());
        task
    }

    fn provider() -> super::super::AgentTaskExecutorProvider {
        serde_json::from_value(json!({
            "id": "test.provider",
            "backend": "test"
        }))
        .expect("provider fixture")
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
        let catalog = catalog(provider());
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

        let error = admit_plan_provider_dispatchability(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            None,
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
        let catalog = catalog(provider());
        let mut plan = AgentTaskPlan::new("plan-deadline", vec![request("model")]);
        let deadline = crate::agent_task_timeout::now_unix_ms().saturating_sub(1);
        plan.options.execution_budget.deadline_unix_ms = Some(deadline);

        let error = admit_plan_provider_dispatchability(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            None,
        )
        .expect_err("expired plan deadline must stop admission");

        assert_eq!(error.details["classification"], "timeout");
        assert_eq!(error.details["deadline_unix_ms"], deadline);
        assert_eq!(error.details["zero_provider_executions"], true);
    }

    /// Controller whose provider-owned credential is present but whose live
    /// readiness probe rejects it (the #15198 shape: a controller-local OAuth
    /// token returning 401). The probe appends to `count` on every run.
    fn controller_with_rejected_live_credentials(
        root: &std::path::Path,
    ) -> (AgentTaskProviderCatalog, std::path::PathBuf) {
        let auth = root.join("auth.json");
        let count = root.join("count");
        std::fs::write(&auth, r#"{"token":"controller-token-rejected"}"#).expect("write auth");
        let provider: super::super::AgentTaskExecutorProvider =
            serde_json::from_value(serde_json::json!({
                "id": "revocable.agent-task-executor",
                "backend": "revocable",
                "capabilities": ["cli_runtime", "provider_owned_auth"]
            }))
            .expect("provider fixture");
        (catalog(provider), count)
    }

    fn revocable_plan() -> AgentTaskPlan {
        let mut task = request("model");
        task.executor.backend = "revocable".to_string();
        AgentTaskPlan::new("lab-admission", vec![task])
    }

    fn placement_directive(
        required: homeboy_lab_contract::ExecutionPlacementRequirement,
        selected: homeboy_lab_contract::EffectiveExecutionPlacement,
    ) -> homeboy_core::parsed_command_preflight::PlacementDirective {
        use homeboy_lab_contract::*;
        homeboy_core::parsed_command_preflight::PlacementDirective {
            requested: if required == ExecutionPlacementRequirement::Lab {
                Placement::Lab
            } else {
                Placement::Local
            },
            required,
            selected,
            runner: None,
            fallback: ExecutionPlacementFallback {
                local_allowed: required != ExecutionPlacementRequirement::Lab,
                reason: None,
            },
            override_authorization: ExecutionPlacementOverrideAuthorization {
                authorized: false,
                authority: None,
            },
        }
    }

    #[test]
    fn required_unavailable_lab_fails_before_provider_probe() {
        use homeboy_lab_contract::{EffectiveExecutionPlacement, ExecutionPlacementRequirement};
        let root = tempfile::tempdir().unwrap();
        let (catalog, count) = controller_with_rejected_live_credentials(root.path());
        let mut placement = placement_directive(
            ExecutionPlacementRequirement::Lab,
            EffectiveExecutionPlacement::Local,
        );
        let reason = "stale_daemon; active_daemon_control_plane_version != job_command_binary_version; run homeboy runner refresh-homeboy homeboy-lab --ref 0.438.1 --reconnect";
        placement.fallback.reason = Some(reason.to_string());

        let error = admit_plan_provider_dispatchability(
            &revocable_plan(),
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            Some(&placement),
        )
        .expect_err("required unavailable Lab must fail placement admission");

        assert_eq!(error.details["field"], "placement");
        assert!(error.message.contains(reason), "{error}");
        assert!(
            !count.exists(),
            "placement failure must precede live probes"
        );
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

        let placement = placement_directive(
            homeboy_lab_contract::ExecutionPlacementRequirement::Local,
            homeboy_lab_contract::EffectiveExecutionPlacement::Local,
        );
        let error = admit_plan_provider_dispatchability(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            Some(&placement),
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

        let placement = placement_directive(
            homeboy_lab_contract::ExecutionPlacementRequirement::Lab,
            homeboy_lab_contract::EffectiveExecutionPlacement::Lab,
        );
        let admitted = admit_plan_provider_dispatchability(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            Some(&placement),
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

        admit_plan_provider_dispatchability(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            Some(&placement_directive(
                homeboy_lab_contract::ExecutionPlacementRequirement::Lab,
                homeboy_lab_contract::EffectiveExecutionPlacement::Lab,
            )),
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
        let verdict = evaluate_provider_dispatchability(
            &catalog,
            "pi",
            None,
            None,
            &fake.config(),
            true,
            &[],
            &mut ProviderRuntimeReadinessCache::default(),
            &mut None,
            None,
            false,
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

        let error = preflight_provider_dispatchability(
            &catalog,
            "pi",
            None,
            None,
            &fake.config(),
            true,
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect_err("unusable credential must fail before dispatch");
        assert!(error.message.contains("no_models_available"), "{error}");
        assert!(error.message.contains("Log in to a provider"), "{error}");
    }

    #[test]
    fn offline_provider_owned_auth_remains_runtime_unavailable() {
        let fake = route_pi("exit 1");
        let verdict = evaluate_provider_dispatchability(
            &catalog(fake.provider()),
            "pi",
            None,
            None,
            &fake.config(),
            true,
            &[],
            &mut ProviderRuntimeReadinessCache::default(),
            &mut None,
            None,
            false,
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
        let missing = evaluate_provider_dispatchability(
            &catalog,
            "missing",
            None,
            None,
            &serde_json::json!({}),
            false,
            &[],
            &mut ProviderRuntimeReadinessCache::default(),
            &mut None,
            None,
            false,
        );
        assert_eq!(missing.state, "route_unavailable");
        assert!(!missing.checks.route.ready);
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
            &serde_json::json!({}),
            false,
            &[],
            &mut ProviderRuntimeReadinessCache::default(),
            &mut None,
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

        let blocked = evaluate_provider_dispatchability(
            &catalog,
            "pi",
            None,
            None,
            &blocked_config,
            true,
            &[],
            &mut cache,
            &mut None,
            None,
            false,
        );
        assert_eq!(blocked.state, "runtime_unavailable");
        assert_eq!(
            blocked.checks.runtime.reason.as_deref(),
            Some("transient_failure: model_list_probe_interrupted")
        );

        let ready = evaluate_provider_dispatchability(
            &catalog,
            "pi",
            None,
            None,
            &ready_config,
            true,
            &[],
            &mut cache,
            &mut None,
            None,
            false,
        );
        assert!(ready.ready);
        let ready_again = evaluate_provider_dispatchability(
            &catalog,
            "pi",
            None,
            None,
            &ready_config,
            true,
            &[],
            &mut cache,
            &mut None,
            None,
            false,
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

        admit_plan_provider_dispatchability(
            &mut plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            None,
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

        admit_plan_provider_dispatchability(
            &mut plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            None,
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

        let admitted = admit_plan_provider_dispatchability(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            None,
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

        let admitted = admit_plan_provider_dispatchability(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            None,
        )
        .expect("first fallback is admitted");
        assert_eq!(
            admitted.tasks[0].executor.model(),
            Some("test/fallback-one")
        );
        assert_eq!(admitted.options.rotation.as_ref().unwrap().entries.len(), 2);
        let recovered = route_pi("printf 'test primary\\n'");
        std::fs::copy(&recovered.command, &fake.command).unwrap();

        let readmitted = admit_plan_provider_dispatchability(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            None,
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
        let admitted = admit_plan_provider_dispatchability(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            None,
        )
        .expect("fallback admitted");
        let recovered = route_pi("printf 'test primary\\ntest fallback\\n'");
        std::fs::copy(&recovered.command, &fake.command).unwrap();

        let readmitted = admit_plan_provider_dispatchability(
            &admitted,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            None,
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
        let admitted = admit_plan_provider_dispatchability(
            &plan,
            &catalog,
            &mut ProviderRuntimeReadinessCache::default(),
            None,
        )
        .expect("transient primary recovers");
        assert_eq!(admitted.tasks[0].executor.model(), Some("test/primary"));
        assert_eq!(std::fs::read_to_string(count).expect("probe count"), "2");
        assert!(admitted.tasks[0]
            .metadata
            .get("provider_readiness_routing")
            .is_none());
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

        let verdict = evaluate_provider_dispatchability(
            &catalog,
            "pi",
            None,
            Some("test/model"),
            &fake.config(),
            true,
            &[],
            &mut cache,
            &mut None,
            None,
            false,
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
