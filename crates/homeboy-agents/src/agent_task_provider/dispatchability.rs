use serde::Serialize;

use super::{
    effective_provider_config, provider_credential_readiness, readiness_verdict,
    resolve_provider_for_backend, validate_provider_immediate_failure_patterns,
    AgentTaskProviderCatalog, ProviderResolution, ProviderRuntimeReadinessCache,
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
    pub ready: bool,
    pub missing: Vec<String>,
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
                    ready: credentials_ready,
                    missing: Vec::new(),
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
    let configuration = match validate_provider_immediate_failure_patterns(provider) {
        Ok(()) => AgentTaskProviderDispatchabilityCheck {
            ready: true,
            reason: None,
        },
        Err(reason) => AgentTaskProviderDispatchabilityCheck {
            ready: false,
            reason: Some(reason),
        },
    };
    let runtime = if probe_runtime && model_ready && credentials.dispatchable && configuration.ready
    {
        let config = effective_provider_config(config, model);
        match readiness_verdict(provider, &config, cache) {
            Ok(verdict) if verdict.ready => AgentTaskProviderDispatchabilityCheck {
                ready: true,
                reason: None,
            },
            Ok(verdict) => AgentTaskProviderDispatchabilityCheck {
                ready: false,
                reason: Some(homeboy_core::redaction::redact_string(&verdict.reason)),
            },
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
            "provider immediate-failure configuration is invalid",
        )
    } else if !credentials.dispatchable {
        (
            "credentials_missing",
            false,
            "required provider credentials are not configured",
        )
    } else if probe_runtime && !runtime.ready {
        (
            "runtime_unavailable",
            false,
            "provider runtime readiness validation failed",
        )
    } else if !probe_runtime {
        ("ready", true, "live runtime readiness was not requested")
    } else {
        ("ready", true, "all dispatchability checks passed")
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
                ready: credentials.dispatchable,
                missing: credentials.missing,
            },
            configuration,
            runtime,
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
    if verdict.ready {
        return Ok(verdict);
    }
    let detail = verdict
        .checks
        .runtime
        .reason
        .as_deref()
        .filter(|reason| *reason != "not requested")
        .map(|reason| format!(": {reason}"))
        .unwrap_or_default();
    Err(homeboy_core::Error::validation_invalid_argument(
        "provider_dispatchability",
        format!(
            "agent-task backend `{backend}` is not dispatchable: {}{detail}",
            verdict.reason,
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
        provider.readiness_invocation = Some(CommandInvocation {
            argv: vec![
                "node".to_string(),
                script.display().to_string(),
                count.display().to_string(),
            ],
            ..CommandInvocation::default()
        });
        provider
    }

    fn catalog(provider: super::super::AgentTaskExecutorProvider) -> AgentTaskProviderCatalog {
        AgentTaskProviderCatalog {
            providers: vec![provider],
            ..Default::default()
        }
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
            Some("token=[REDACTED]")
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
