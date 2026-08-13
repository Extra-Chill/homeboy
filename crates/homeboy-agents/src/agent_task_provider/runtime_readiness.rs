use std::collections::BTreeMap;

use homeboy_engine_primitives::content_hash;
use serde_json::{json, Value};

use crate::agent_task_scheduler::AgentTaskPlan;
use homeboy_core::{Error, Result};

use super::command_runner::{run_provider_readiness_invocation, ProviderReadinessInvocationResult};
use super::resolution::select_provider;
use super::AgentTaskExecutorProvider;

/// Process-local cache for one compiled batch. The provider owns the durable
/// cache key; Homeboy also indexes the request identity so siblings do not need
/// to launch a probe merely to learn that key.
#[derive(Default)]
pub struct ProviderRuntimeReadinessCache {
    by_request: BTreeMap<String, ProviderReadinessInvocationResult>,
}

pub fn preflight_plan_provider_runtime_readiness_with_providers(
    plan: &AgentTaskPlan,
    providers: &[AgentTaskExecutorProvider],
    cache: &mut ProviderRuntimeReadinessCache,
) -> Result<()> {
    for task in &plan.tasks {
        let Some(provider) = select_provider(providers, task) else {
            continue;
        };
        if provider.readiness_invocation.is_none() {
            validate_provider_immediate_failure_patterns(provider).map_err(|message| {
                Error::validation_invalid_argument(
                    "immediate_failure_patterns",
                    format!("provider '{}' has invalid immediate failure configuration: {message}", provider.id),
                    Some(provider.backend.clone()),
                    None,
                )
            })?;
            continue;
        }
        validate_provider_immediate_failure_patterns(provider).map_err(|message| {
            Error::validation_invalid_argument(
                "immediate_failure_patterns",
                format!("provider '{}' has invalid immediate failure configuration: {message}", provider.id),
                Some(provider.backend.clone()),
                None,
            )
        })?;
        let config = effective_provider_config(&task.executor.config, task.executor.model());
        let verdict = readiness_verdict(provider, &config, cache)?;
        if !verdict.ready {
            return Err(readiness_error(provider, &config, &verdict));
        }
    }
    Ok(())
}

fn readiness_verdict(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    cache: &mut ProviderRuntimeReadinessCache,
) -> Result<ProviderReadinessInvocationResult> {
    let request_key = readiness_request_key(provider, config)?;
    if let Some(verdict) = cache.by_request.get(&request_key) {
        return Ok(verdict.clone());
    }
    let verdict = run_provider_readiness_invocation(provider, config).map_err(|message| {
        Error::validation_invalid_argument(
            "provider_runtime_readiness",
            format!(
                "provider '{}' readiness invocation failed: {message}",
                provider.id
            ),
            Some(provider.backend.clone()),
            None,
        )
        .with_retryable(true)
    })?;
    cache.by_request.insert(request_key, verdict.clone());
    Ok(verdict)
}

fn effective_provider_config(config: &Value, model: Option<&str>) -> Value {
    let mut config = config.as_object().cloned().unwrap_or_default();
    if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
        // The executor resolves its selected model ahead of config.model.
        // Readiness must probe that same effective model.
        config.insert("model".to_string(), Value::String(model.to_string()));
    }
    Value::Object(config)
}

fn readiness_request_key(provider: &AgentTaskExecutorProvider, config: &Value) -> Result<String> {
    let mut environment = provider
        .readiness_invocation
        .as_ref()
        .and_then(|invocation| invocation.extra.get("env_allowlist"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    environment.extend(["PATH".to_string(), "HOME".to_string()]);
    environment.sort();
    environment.dedup();
    let environment = environment
        .into_iter()
        .map(|name| {
            let value = std::env::var_os(&name)
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default();
            (name, value)
        })
        .collect::<Vec<_>>();
    let value = json!({
        "provider_id": provider.id,
        "runtime_path": provider.runtime_path,
        "invocation": provider.readiness_invocation,
        "effective_config": config,
        "environment": environment,
    });
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    Ok(content_hash::sha256_hex(&encoded))
}

fn readiness_error(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    verdict: &ProviderReadinessInvocationResult,
) -> Error {
    let classification = if verdict.classification.is_empty() {
        "unknown".to_string()
    } else {
        verdict.classification.clone()
    };
    let reason = if verdict.reason.is_empty() {
        "provider-owned readiness check failed".to_string()
    } else {
        verdict.reason.clone()
    };
    let mut hints = vec![json!({
        "kind": "provider_runtime_readiness_failed",
        "provider_id": provider.id,
        "backend": provider.backend,
        "classification": classification,
        "retryable": verdict.retryable,
        "cache_key": verdict.cache_key,
        "identity": verdict.identity,
        "effective_model": config.get("model"),
    })
    .to_string()];
    if !verdict.remediation.trim().is_empty() {
        hints.push(verdict.remediation.clone());
    }
    Error::validation_invalid_argument(
        "provider_runtime_readiness",
        format!(
            "agent-task backend '{}' is not ready for its selected runtime/model: {} ({reason})",
            provider.backend, classification
        ),
        Some(provider.backend.clone()),
        Some(hints),
    )
    .with_retryable(verdict.retryable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::command_invocation::CommandInvocation;

    fn provider(script: &std::path::Path, count: &std::path::Path) -> AgentTaskExecutorProvider {
        let mut provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "runtime.provider",
            "backend": "runtime"
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

    fn readiness_script(root: &std::path::Path) -> std::path::PathBuf {
        let script = root.join("readiness.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const request=JSON.parse(fs.readFileSync(0,'utf8'));const count=process.argv[2];fs.writeFileSync(count,String((Number(fs.existsSync(count)?fs.readFileSync(count,'utf8'):0))+1));const classification=request.effective_config.model;process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:classification==='ready',classification,retryable:classification==='transient_failure',remediation:'repair '+classification,reason:classification,cache_key:'cache-'+classification,identity:{model:classification}}));",
        )
        .expect("readiness script");
        script
    }

    #[test]
    fn selected_model_overrides_provider_configuration() {
        assert_eq!(
            effective_provider_config(&json!({}), Some("selected-model"))["model"],
            "selected-model"
        );
        assert_eq!(
            effective_provider_config(
                &json!({ "model": "provider-model" }),
                Some("selected-model")
            )["model"],
            "selected-model"
        );
    }

    #[test]
    fn runtime_verdicts_preserve_retryability_and_remediation() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let provider = provider(&readiness_script(root.path()), &count);
        for (classification, ready, retryable) in [
            ("ready", true, false),
            ("deterministic_incompatibility", false, false),
            ("auth_failure", false, false),
            ("unknown_metadata", false, false),
            ("transient_failure", false, true),
        ] {
            let verdict = readiness_verdict(
                &provider,
                &json!({ "model": classification }),
                &mut ProviderRuntimeReadinessCache::default(),
            )
            .expect("readiness result");
            assert_eq!(verdict.ready, ready, "{classification}");
            assert_eq!(verdict.retryable, retryable, "{classification}");
            assert_eq!(verdict.remediation, format!("repair {classification}"));
        }
    }

    #[test]
    fn shared_cache_deduplicates_a_fanout_runtime_probe() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let provider = provider(&readiness_script(root.path()), &count);
        let mut cache = ProviderRuntimeReadinessCache::default();
        let config = json!({ "model": "ready" });

        let first = readiness_verdict(&provider, &config, &mut cache).expect("first verdict");
        let second = readiness_verdict(&provider, &config, &mut cache).expect("cached verdict");

        assert_eq!(first.cache_key, second.cache_key);
        assert_eq!(std::fs::read_to_string(count).expect("probe count"), "1");
    }
}
