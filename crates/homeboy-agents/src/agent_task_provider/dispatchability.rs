use serde::Serialize;

use super::{
    effective_provider_config, provider_credential_readiness, readiness_verdict,
    resolve_provider_for_backend, runtime_readiness::provider_requires_live_auth_validation,
    validate_provider_immediate_failure_patterns, AgentTaskProviderCatalog, ProviderResolution,
    ProviderRuntimeReadinessCache,
};
use crate::agent_task_scheduler::AgentTaskPlan;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration_diagnosis: Option<AgentTaskProviderConfigurationDiagnosis>,
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

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskProviderDispatchabilityChecks {
    pub route: AgentTaskProviderDispatchabilityCheck,
    pub model: AgentTaskProviderDispatchabilityCheck,
    pub credentials: AgentTaskProviderDispatchabilityCredentialCheck,
    pub configuration: AgentTaskProviderDispatchabilityCheck,
    pub runtime: AgentTaskProviderDispatchabilityCheck,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskProviderDispatchabilityCheck {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

pub fn evaluate_provider_dispatchability_with_config(
    catalog: &AgentTaskProviderCatalog,
    backend: &str,
    selector: Option<&str>,
    model: Option<&str>,
    config: &Value,
    probe_runtime: bool,
    cache: &mut ProviderRuntimeReadinessCache,
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
        AgentTaskProviderDispatchability {
            state,
            ready: false,
            reason: reason.to_string(),
            checks: AgentTaskProviderDispatchabilityChecks {
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
                    reason: Some("the provider route did not resolve, so credentials were not live-validated".to_string()),
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
            },
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
    let runtime = if probe_runtime && model_ready && credentials.dispatchable && configuration.ready
    {
        let config = effective_provider_config(config, model);
        match readiness_verdict(provider, &config, cache) {
            Ok(verdict) if verdict.ready => AgentTaskProviderDispatchabilityCheck {
                ready: true,
                reason: None,
            },
            Ok(verdict) => {
                if !verdict.remediation.trim().is_empty() {
                    runtime_remediation
                        .push(homeboy_core::redaction::redact_string(&verdict.remediation));
                }
                let classification = if verdict.classification.trim().is_empty() {
                    "unknown"
                } else {
                    verdict.classification.trim()
                };
                let reason = if verdict.reason.trim().is_empty() {
                    classification.to_string()
                } else {
                    format!(
                        "{classification}: {}",
                        homeboy_core::redaction::redact_string(&verdict.reason)
                    )
                };
                AgentTaskProviderDispatchabilityCheck {
                    ready: false,
                    reason: Some(reason),
                }
            }
            Err(reason) => AgentTaskProviderDispatchabilityCheck {
                ready: false,
                reason: Some(homeboy_core::redaction::redact_string(&reason.message)),
            },
        }
    } else {
        AgentTaskProviderDispatchabilityCheck {
            ready: !probe_runtime,
            reason: (!probe_runtime).then_some("not requested".to_string()),
        }
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
    let (credential_status, credential_reason) = if !credentials.dispatchable {
        (AgentTaskProviderCredentialStatus::Missing, None)
    } else if credentials_verified {
        (AgentTaskProviderCredentialStatus::Verified, None)
    } else if probe_runtime
        && provider.readiness_invocation.is_some()
        && !runtime.ready
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
    } else if provider_owned_auth && !runtime.ready {
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
    } else if !probe_runtime {
        ("ready", true, "live runtime readiness was not requested")
    } else if !provider_owned_auth || credentials_verified {
        ("ready", true, "all dispatchability checks passed")
    } else {
        unreachable!("provider-owned auth without verification is rejected above")
    };
    AgentTaskProviderDispatchability {
        state,
        ready,
        reason: reason.to_string(),
        checks: AgentTaskProviderDispatchabilityChecks {
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
        },
        configuration_diagnosis,
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
    Err(homeboy_core::Error::validation_invalid_argument(
        "provider_dispatchability",
        format!(
            "agent-task backend `{backend}` is not dispatchable ({state}): {reason}{detail}.{remediation}",
            state = verdict.state,
            reason = verdict.reason.trim_end_matches('.'),
        ),
        Some(backend.to_string()),
        Some(vec![serde_json::to_string(&verdict).unwrap_or_default()]),
    ))
}

/// Evaluate every effective executor in a compiled plan. The caller owns the
/// cache so fanout siblings with identical provider/config requests probe once.
pub fn preflight_plan_provider_dispatchability_with_providers(
    plan: &AgentTaskPlan,
    catalog: &AgentTaskProviderCatalog,
    cache: &mut ProviderRuntimeReadinessCache,
) -> homeboy_core::Result<()> {
    for task in &plan.tasks {
        preflight_provider_dispatchability_with_config(
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
            "process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:true,classification:'ready',retryable:false,remediation:'',reason:'',cache_key:'k',identity:{}}));",
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
}
