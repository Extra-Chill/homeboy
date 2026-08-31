use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use homeboy_engine_primitives::content_hash;
use serde_json::{json, Value};

use crate::agent_task_scheduler::AgentTaskPlan;
use homeboy_core::{Error, Result};

use super::command_runner::{
    run_provider_readiness_invocation, validate_provider_immediate_failure_patterns,
    ProviderReadinessInvocationResult,
};
use super::resolution::select_provider;
use super::AgentTaskExecutorProvider;

/// Process-local cache for one compiled batch. The provider owns the durable
/// cache key; Homeboy also indexes the request identity so siblings do not need
/// to launch a probe merely to learn that key.
#[derive(Debug, Clone)]
pub struct ProviderRuntimeReadinessCache {
    shared: Arc<ProviderRuntimeReadinessCacheShared>,
}

#[derive(Debug)]
struct ProviderRuntimeReadinessCacheShared {
    state: Mutex<ProviderRuntimeReadinessCacheState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct ProviderRuntimeReadinessCacheState {
    by_request: BTreeMap<String, CachedProviderRuntimeReadiness>,
    waiters: BTreeMap<String, usize>,
    next_generation: u64,
}

#[derive(Debug)]
enum CachedProviderRuntimeReadiness {
    InFlight,
    Complete {
        result: std::result::Result<ProviderReadinessInvocationResult, String>,
        cached_at: Instant,
        generation: u64,
    },
}

fn release_cache_waiter(state: &mut ProviderRuntimeReadinessCacheState, request_key: &str) {
    let Some(waiters) = state.waiters.get_mut(request_key) else {
        return;
    };
    *waiters = waiters.saturating_sub(1);
    if *waiters == 0 {
        state.waiters.remove(request_key);
    }
}

impl Default for ProviderRuntimeReadinessCache {
    fn default() -> Self {
        Self {
            shared: Arc::new(ProviderRuntimeReadinessCacheShared {
                state: Mutex::new(ProviderRuntimeReadinessCacheState::default()),
                changed: Condvar::new(),
            }),
        }
    }
}

const PROVIDER_RUNTIME_READINESS_READY_TTL: Duration = Duration::from_secs(30);
const PROVIDER_RUNTIME_READINESS_NEGATIVE_TTL: Duration = Duration::from_secs(5);
const PROVIDER_RUNTIME_READINESS_ERROR_TTL: Duration = Duration::from_secs(2);
const PROVIDER_RUNTIME_READINESS_TRANSIENT_ATTEMPTS: usize = 2;
const MAX_CONCURRENT_PROVIDER_READINESS_PROBES: usize = 4;

#[derive(Debug)]
struct ProviderReadinessProbeGate {
    active: Mutex<usize>,
    changed: Condvar,
}

struct ProviderReadinessProbePermit(&'static ProviderReadinessProbeGate);

impl Drop for ProviderReadinessProbePermit {
    fn drop(&mut self) {
        let mut active = self.0.active.lock().expect("readiness probe gate");
        *active = active.saturating_sub(1);
        self.0.changed.notify_one();
    }
}

fn acquire_probe_permit() -> ProviderReadinessProbePermit {
    static GATE: OnceLock<ProviderReadinessProbeGate> = OnceLock::new();
    let gate = GATE.get_or_init(|| ProviderReadinessProbeGate {
        active: Mutex::new(0),
        changed: Condvar::new(),
    });
    let mut active = gate.active.lock().expect("readiness probe gate");
    while *active >= MAX_CONCURRENT_PROVIDER_READINESS_PROBES {
        active = gate.changed.wait(active).expect("readiness probe gate");
    }
    *active += 1;
    ProviderReadinessProbePermit(gate)
}

fn readiness_invocation_error(provider: &AgentTaskExecutorProvider, message: String) -> Error {
    Error::validation_invalid_argument(
        "provider_runtime_readiness",
        format!(
            "provider '{}' readiness invocation failed: {}",
            provider.id,
            homeboy_core::redaction::redact_string(&message)
        ),
        Some(provider.backend.clone()),
        None,
    )
    .with_retryable(true)
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
            if provider_requires_live_auth_validation(provider) {
                return Err(unverified_provider_auth_error(provider));
            }
            validate_provider_immediate_failure_patterns(provider).map_err(|message| {
                Error::validation_invalid_argument(
                    "immediate_failure_patterns",
                    format!(
                        "provider '{}' has invalid immediate failure configuration: {message}",
                        provider.id
                    ),
                    Some(provider.backend.clone()),
                    None,
                )
            })?;
            continue;
        }
        validate_provider_immediate_failure_patterns(provider).map_err(|message| {
            Error::validation_invalid_argument(
                "immediate_failure_patterns",
                format!(
                    "provider '{}' has invalid immediate failure configuration: {message}",
                    provider.id
                ),
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

pub(crate) fn provider_requires_live_auth_validation(provider: &AgentTaskExecutorProvider) -> bool {
    provider
        .capabilities
        .iter()
        .any(|capability| capability == "provider_owned_auth")
}

pub(crate) fn unverified_provider_auth_error(provider: &AgentTaskExecutorProvider) -> Error {
    Error::validation_invalid_argument(
        "provider_runtime_readiness",
        format!(
            "agent-task backend '{}' is not dispatchable: provider-owned authentication has no live validation route. Update provider '{}' to declare a bounded readiness_invocation that validates its provider-owned authentication, or select a verified backend",
            provider.backend, provider.id
        ),
        Some(provider.backend.clone()),
        Some(vec![format!(
            "Update provider '{}' to declare a bounded readiness_invocation that validates its provider-owned authentication, or select a verified backend.",
            provider.id
        )]),
    )
}

pub(crate) fn readiness_verdict(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    cache: &mut ProviderRuntimeReadinessCache,
) -> Result<ProviderReadinessInvocationResult> {
    readiness_verdict_with_credential_identity(provider, config, &[], cache)
}

pub(crate) fn readiness_verdict_with_credential_identity(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    credential_identity: &[(String, String)],
    cache: &mut ProviderRuntimeReadinessCache,
) -> Result<ProviderReadinessInvocationResult> {
    let base_key = readiness_request_key(provider, config)?;
    let request_key = content_hash::sha256_hex(
        &serde_json::to_vec(&(base_key, credential_identity))
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    );
    let mut registered_waiter = false;
    loop {
        let mut state = cache.shared.state.lock().map_err(|_| {
            Error::validation_invalid_argument(
                "provider_runtime_readiness",
                "provider readiness cache lock was poisoned",
                Some(provider.backend.clone()),
                None,
            )
        })?;
        match state.by_request.get(&request_key) {
            Some(CachedProviderRuntimeReadiness::InFlight) => {
                if !registered_waiter {
                    *state.waiters.entry(request_key.clone()).or_default() += 1;
                    registered_waiter = true;
                }
                drop(cache.shared.changed.wait(state));
                continue;
            }
            Some(CachedProviderRuntimeReadiness::Complete {
                result, cached_at, ..
            }) => {
                let ttl = match result {
                    Ok(verdict) if verdict.ready => PROVIDER_RUNTIME_READINESS_READY_TTL,
                    Ok(_) => PROVIDER_RUNTIME_READINESS_NEGATIVE_TTL,
                    Err(_) => PROVIDER_RUNTIME_READINESS_ERROR_TTL,
                };
                if cached_at.elapsed() <= ttl {
                    let result = result.clone();
                    if registered_waiter {
                        release_cache_waiter(&mut state, &request_key);
                        cache.shared.changed.notify_all();
                    }
                    return result.map_err(|message| readiness_invocation_error(provider, message));
                }
                if registered_waiter {
                    release_cache_waiter(&mut state, &request_key);
                    registered_waiter = false;
                    cache.shared.changed.notify_all();
                }
                state.by_request.remove(&request_key);
            }
            None => {}
        }
        if state.by_request.len() >= 64 {
            if let Some(oldest) = state
                .by_request
                .iter()
                .filter_map(|(key, cached)| match cached {
                    CachedProviderRuntimeReadiness::Complete { generation, .. }
                        if state.waiters.get(key).copied().unwrap_or_default() == 0 =>
                    {
                        Some((key, generation))
                    }
                    CachedProviderRuntimeReadiness::Complete { .. }
                    | CachedProviderRuntimeReadiness::InFlight => None,
                })
                .min_by_key(|(_, generation)| *generation)
                .map(|(key, _)| key.clone())
            {
                state.by_request.remove(&oldest);
            } else {
                drop(cache.shared.changed.wait(state));
                continue;
            }
        }
        state.by_request.insert(
            request_key.clone(),
            CachedProviderRuntimeReadiness::InFlight,
        );
        break;
    }

    let _permit = acquire_probe_permit();
    let mut result = Err("provider readiness invocation did not run".to_string());
    for _ in 0..PROVIDER_RUNTIME_READINESS_TRANSIENT_ATTEMPTS {
        result = run_provider_readiness_invocation(provider, config);
        if !matches!(
            &result,
            Ok(verdict)
                if !verdict.ready
                    && verdict.retryable
                    && verdict.classification == "transient_failure"
        ) {
            break;
        }
    }

    let mut state = cache.shared.state.lock().map_err(|_| {
        Error::validation_invalid_argument(
            "provider_runtime_readiness",
            "provider readiness cache lock was poisoned",
            Some(provider.backend.clone()),
            None,
        )
    })?;
    let generation = state.next_generation;
    state.next_generation = state.next_generation.wrapping_add(1);
    state.by_request.insert(
        request_key,
        CachedProviderRuntimeReadiness::Complete {
            result: result.clone(),
            cached_at: Instant::now(),
            generation,
        },
    );
    cache.shared.changed.notify_all();
    result.map_err(|message| readiness_invocation_error(provider, message))
}

pub(crate) fn effective_provider_config(config: &Value, model: Option<&str>) -> Value {
    let mut config = config.as_object().cloned().unwrap_or_default();
    if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
        // The executor resolves its selected model ahead of config.model.
        // Readiness must probe that same effective model.
        config.insert("model".to_string(), Value::String(model.to_string()));
    }
    Value::Object(config)
}

pub(crate) fn readiness_request_key(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
) -> Result<String> {
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
    environment.extend(super::credential_readiness::provider_required_secret_env_names(provider));
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
    let classification = match verdict.classification.as_str() {
        "ready"
        | "deterministic_incompatibility"
        | "auth_failure"
        | "account"
        | "capacity"
        | "transient_failure" => verdict.classification.as_str(),
        _ => "unknown",
    };
    let reason = if verdict.reason.is_empty() {
        "provider-owned readiness check failed".to_string()
    } else {
        homeboy_core::redaction::redact_string(&verdict.reason)
    };
    let mut hints = vec![json!({
        "kind": "provider_runtime_readiness_failed",
        "provider_id": provider.id,
        "backend": provider.backend,
        "classification": classification,
        "retryable": verdict.retryable,
        "cache_identity": content_hash::sha256_hex(verdict.cache_key.as_bytes()),
        "provider_identity": content_hash::sha256_hex(
            &serde_json::to_vec(&verdict.identity).unwrap_or_default()
        ),
        "effective_model": config.get("model"),
    })
    .to_string()];
    if !verdict.remediation.trim().is_empty() {
        hints.push(homeboy_core::redaction::redact_string(&verdict.remediation));
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
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use homeboy_core::command_invocation::CommandInvocation;

    fn provider(script: &std::path::Path, count: &std::path::Path) -> AgentTaskExecutorProvider {
        let mut provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "runtime.provider",
            "backend": "runtime"
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
    fn retryable_non_transient_verdicts_are_not_immediately_retried() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let script = root.path().join("retryable-auth.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const count=process.argv[2];fs.writeFileSync(count,String(Number(fs.existsSync(count)?fs.readFileSync(count,'utf8'):0)+1));process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:false,classification:'auth_failure',retryable:true,remediation:'switch account',reason:'rejected',cache_key:'account',identity:{account:'test'}}));",
        )
        .expect("readiness script");
        let provider = provider(&script, &count);

        let verdict = readiness_verdict(
            &provider,
            &json!({ "model": "test" }),
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect("readiness result");

        assert_eq!(verdict.classification, "auth_failure");
        assert!(verdict.retryable);
        assert_eq!(std::fs::read_to_string(count).expect("probe count"), "1");
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

    #[test]
    fn provider_owned_auth_without_a_probe_fails_plan_admission() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "unverified.provider",
            "backend": "unverified",
            "capabilities": ["cli_runtime", "provider_owned_auth"]
        }))
        .expect("provider fixture");
        let plan = AgentTaskPlan::new(
            "unverified-plan",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "task".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "unverified".to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "test".to_string(),
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
            }],
        );

        let error = preflight_plan_provider_runtime_readiness_with_providers(
            &plan,
            &[provider],
            &mut ProviderRuntimeReadinessCache::default(),
        )
        .expect_err("provider-owned auth requires a live probe");

        assert!(
            error.message.contains("no live validation route"),
            "{error}"
        );
        assert!(error.message.contains("readiness_invocation"), "{error}");
    }

    #[test]
    fn negative_runtime_evidence_expires_and_reprobes_recovered_credentials() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let recovered = root.path().join("recovered");
        let script = root.path().join("recovering-readiness.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const count=process.argv[2],recovered=process.argv[3];fs.writeFileSync(count,String(Number(fs.existsSync(count)?fs.readFileSync(count,'utf8'):0)+1));const ready=fs.existsSync(recovered);process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready,classification:ready?'ready':'auth_failure',retryable:false,remediation:'repair credentials',reason:ready?'':'credentials rejected',cache_key:'account',identity:{account:'test'}}));",
        )
        .expect("readiness script");
        let mut provider = provider(&script, &count);
        provider
            .readiness_invocation
            .as_mut()
            .expect("readiness invocation")
            .argv
            .push(recovered.display().to_string());
        let mut cache = ProviderRuntimeReadinessCache::default();
        let config = json!({ "model": "recovering" });

        assert!(
            !readiness_verdict(&provider, &config, &mut cache)
                .expect("initial verdict")
                .ready
        );
        std::fs::write(&recovered, "ready").expect("recover credentials");
        assert!(
            !readiness_verdict(&provider, &config, &mut cache)
                .expect("bounded cached verdict")
                .ready
        );
        for cached in cache
            .shared
            .state
            .lock()
            .expect("readiness cache")
            .by_request
            .values_mut()
        {
            if let CachedProviderRuntimeReadiness::Complete { cached_at, .. } = cached {
                *cached_at = Instant::now()
                    - PROVIDER_RUNTIME_READINESS_NEGATIVE_TTL
                    - Duration::from_secs(1);
            }
        }
        assert!(
            readiness_verdict(&provider, &config, &mut cache)
                .expect("recovered verdict")
                .ready
        );
        assert_eq!(std::fs::read_to_string(count).expect("probe count"), "2");
    }

    #[test]
    fn concurrent_exact_key_probes_singleflight() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let script = root.path().join("singleflight.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const count=process.argv[2];fs.appendFileSync(count,'probe\\n');Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,100);process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:true,classification:'ready',retryable:false,remediation:'',reason:'',cache_key:'shared',identity:{account:'shared'}}));",
        )
        .expect("readiness script");
        let provider = provider(&script, &count);
        let cache = ProviderRuntimeReadinessCache::default();
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let provider = provider.clone();
                let mut cache = cache.clone();
                scope.spawn(move || {
                    assert!(
                        readiness_verdict(&provider, &json!({"model":"same"}), &mut cache)
                            .expect("readiness")
                            .ready
                    );
                });
            }
        });
        assert_eq!(
            std::fs::read_to_string(count)
                .expect("probe count")
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn distinct_readiness_keys_probe_concurrently() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("unused-count");
        let script = root.path().join("concurrent.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const req=JSON.parse(fs.readFileSync(0,'utf8'));const root=process.argv[3],model=req.effective_config.model,other=model==='a'?'b':'a';fs.writeFileSync(root+'/started-'+model,'');while(!fs.existsSync(root+'/started-'+other)){Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,10)}process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:true,classification:'ready',retryable:false,remediation:'',reason:'',cache_key:model,identity:{model}}));",
        )
        .expect("readiness script");
        let mut provider = provider(&script, &count);
        provider
            .readiness_invocation
            .as_mut()
            .expect("readiness invocation")
            .argv
            .push(root.path().display().to_string());
        let cache = ProviderRuntimeReadinessCache::default();
        std::thread::scope(|scope| {
            for model in ["a", "b"] {
                let provider = provider.clone();
                let mut cache = cache.clone();
                scope.spawn(move || {
                    assert!(
                        readiness_verdict(&provider, &json!({"model":model}), &mut cache)
                            .expect("readiness")
                            .ready
                    );
                });
            }
        });
        assert!(root.path().join("started-a").exists());
        assert!(root.path().join("started-b").exists());
    }

    #[test]
    fn concurrent_distinct_keys_keep_pending_cache_entries_bounded() {
        let root = tempfile::tempdir().expect("tempdir");
        let release = root.path().join("release");
        let probes = root.path().join("probes");
        let script = root.path().join("bounded.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const req=JSON.parse(fs.readFileSync(0,'utf8'));const release=process.argv[2];fs.appendFileSync(process.argv[3],req.effective_config.model+'\\n');const finish=()=>process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:true,classification:'ready',retryable:false,remediation:'',reason:'',cache_key:req.effective_config.model,identity:{model:req.effective_config.model}}));if(fs.existsSync(release)){finish()}else{const timer=setInterval(()=>{if(fs.existsSync(release)){clearInterval(timer);finish()}},10)}",
        )
        .expect("readiness script");
        let mut provider = provider(&script, &release);
        provider
            .readiness_invocation
            .as_mut()
            .expect("readiness invocation")
            .argv
            .push(probes.display().to_string());
        let cache = ProviderRuntimeReadinessCache::default();
        std::thread::scope(|scope| {
            for index in (0..65).chain(std::iter::once(0)) {
                let provider = provider.clone();
                let mut cache = cache.clone();
                scope.spawn(move || {
                    assert!(
                        readiness_verdict(
                            &provider,
                            &json!({"model": format!("model-{index}")}),
                            &mut cache,
                        )
                        .expect("readiness")
                        .ready
                    );
                });
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let entries = cache
                    .shared
                    .state
                    .lock()
                    .expect("readiness cache")
                    .by_request
                    .len();
                assert!(entries <= 64);
                if entries == 64 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "pending probes did not fill cache"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            std::fs::write(&release, "release").expect("release probes");
        });
        assert!(
            cache
                .shared
                .state
                .lock()
                .expect("readiness cache")
                .by_request
                .len()
                <= 64
        );
        assert_eq!(
            std::fs::read_to_string(probes)
                .expect("probe log")
                .lines()
                .filter(|model| *model == "model-0")
                .count(),
            1,
            "same-key waiters must retain singleflight while distinct keys saturate the cache"
        );
    }

    #[test]
    fn readiness_probe_errors_are_cached_briefly() {
        let root = tempfile::tempdir().expect("tempdir");
        let count = root.path().join("count");
        let script = root.path().join("failure.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const count=process.argv[2];fs.appendFileSync(count,'probe\\n');process.exit(1);",
        )
        .expect("readiness script");
        let provider = provider(&script, &count);
        let mut cache = ProviderRuntimeReadinessCache::default();
        for _ in 0..2 {
            assert!(readiness_verdict(&provider, &json!({"model":"error"}), &mut cache).is_err());
        }
        assert_eq!(
            std::fs::read_to_string(count)
                .expect("probe count")
                .lines()
                .count(),
            1
        );
    }
}
