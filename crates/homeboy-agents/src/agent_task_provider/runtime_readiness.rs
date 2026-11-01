use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs4::fs_std::FileExt;
use homeboy_engine_primitives::content_hash;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent_task_scheduler::AgentTaskPlan;
use homeboy_core::{Error, Result};

use super::command_runner::{
    run_provider_readiness_probe_with_env_and_timeout,
    validate_provider_immediate_failure_patterns, DurableProviderReadinessIdentity,
    ProviderReadinessInvocationResult, PROVIDER_READINESS_CAPACITY_MODE,
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

static PROCESS_READINESS_CACHE: OnceLock<Arc<ProviderRuntimeReadinessCacheShared>> =
    OnceLock::new();
static PROCESS_PROBE_GATE: OnceLock<Arc<ProviderReadinessProbeGate>> = OnceLock::new();

fn process_probe_gate() -> Arc<ProviderReadinessProbeGate> {
    Arc::clone(PROCESS_PROBE_GATE.get_or_init(|| {
        Arc::new(ProviderReadinessProbeGate {
            active: Mutex::new(0),
            changed: Condvar::new(),
        })
    }))
}

#[derive(Debug)]
struct ProviderReadinessProbeGate {
    active: Mutex<usize>,
    changed: Condvar,
}

#[derive(Debug)]
struct ProviderRuntimeReadinessCacheShared {
    state: Mutex<ProviderRuntimeReadinessCacheState>,
    changed: Condvar,
    probe_gate: Arc<ProviderReadinessProbeGate>,
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
                probe_gate: process_probe_gate(),
            }),
        }
    }
}

impl ProviderRuntimeReadinessCache {
    /// Share evidence across compile, admission, and execution phases. The
    /// request identity still includes credential value hashes, so refreshed
    /// credentials cannot reuse old readiness evidence.
    pub fn process_local() -> Self {
        Self {
            shared: Arc::clone(PROCESS_READINESS_CACHE.get_or_init(|| {
                Arc::new(ProviderRuntimeReadinessCacheShared {
                    state: Mutex::new(ProviderRuntimeReadinessCacheState::default()),
                    changed: Condvar::new(),
                    probe_gate: process_probe_gate(),
                })
            })),
        }
    }
}

#[cfg(test)]
impl ProviderRuntimeReadinessCache {
    /// A cache with its own probe gate. The production gate caps concurrent
    /// probes for the whole process, which in a test binary means every
    /// parallel test queues behind unrelated tests' probes and spends its
    /// readiness deadline waiting for a permit (#14832).
    pub(crate) fn isolated() -> Self {
        Self {
            shared: Arc::new(ProviderRuntimeReadinessCacheShared {
                state: Mutex::new(ProviderRuntimeReadinessCacheState::default()),
                changed: Condvar::new(),
                probe_gate: Arc::new(ProviderReadinessProbeGate {
                    active: Mutex::new(0),
                    changed: Condvar::new(),
                }),
            }),
        }
    }

    pub(crate) fn expire_all(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for cached in state.by_request.values_mut() {
            if let CachedProviderRuntimeReadiness::Complete { cached_at, .. } = cached {
                *cached_at =
                    Instant::now() - PROVIDER_RUNTIME_READINESS_READY_TTL - Duration::from_secs(1);
            }
        }
    }
}

const PROVIDER_RUNTIME_READINESS_READY_TTL: Duration = Duration::from_secs(30);
const PROVIDER_RUNTIME_READINESS_NEGATIVE_TTL: Duration = Duration::from_secs(5);
const PROVIDER_RUNTIME_READINESS_ERROR_TTL: Duration = Duration::from_secs(2);
const PROVIDER_RUNTIME_READINESS_TRANSIENT_ATTEMPTS: usize = 2;
const MAX_CONCURRENT_PROVIDER_READINESS_PROBES: usize = 4;
const DURABLE_FANOUT_READINESS_DIR: &str = "runtime/provider-readiness/v1";
const DURABLE_FANOUT_READINESS_LOCK_SHARDS: u8 = 64;
const MAX_DURABLE_FANOUT_READINESS_ENTRIES: usize = 128;

#[derive(Debug, Deserialize, Serialize)]
struct DurableFanoutReadinessEntry {
    schema: String,
    request_identity: String,
    checked_at_unix_ms: u64,
    ready: bool,
    cache_key_sha256: Option<String>,
    identity_sha256: Option<String>,
}

fn durable_fanout_readiness_paths(request_key: &str) -> Option<(PathBuf, PathBuf)> {
    let root = homeboy_core::paths::homeboy_data().ok()?;
    let directory = root.join(DURABLE_FANOUT_READINESS_DIR);
    let name = content_hash::sha256_hex(request_key.as_bytes());
    let shard = u8::from_str_radix(&name[..2], 16).unwrap_or_default()
        % DURABLE_FANOUT_READINESS_LOCK_SHARDS;
    Some((
        directory.join(format!("{name}.json")),
        directory.join(format!("{shard:02x}.lock")),
    ))
}

fn read_durable_fanout_readiness(
    path: &std::path::Path,
    request_key: &str,
) -> Option<ProviderReadinessInvocationResult> {
    let entry: DurableFanoutReadinessEntry =
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    if entry.schema != "homeboy/provider-readiness-cache/v1"
        || entry.request_identity != request_key
        || entry.checked_at_unix_ms > now
        || now.saturating_sub(entry.checked_at_unix_ms)
            > PROVIDER_RUNTIME_READINESS_READY_TTL.as_millis() as u64
        || !entry.ready
    {
        return None;
    }
    Some(ProviderReadinessInvocationResult {
        ready: true,
        classification: "ready".to_string(),
        retryable: false,
        remediation: String::new(),
        reason: String::new(),
        cache_key: String::new(),
        identity: Value::Null,
        capacity: None,
        durable_identity: Some(DurableProviderReadinessIdentity {
            cache_key_sha256: entry.cache_key_sha256,
            identity_sha256: entry.identity_sha256,
        }),
    })
}

fn write_durable_fanout_readiness(
    path: &std::path::Path,
    request_key: &str,
    result: &ProviderReadinessInvocationResult,
) {
    if !result.ready || result.capacity.is_some() || result.classification == "capacity" {
        let _ = std::fs::remove_file(path);
        return;
    }
    let Ok(checked_at_unix_ms) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return;
    };
    let cache_key_sha256 = (!result.cache_key.is_empty())
        .then(|| content_hash::sha256_hex(result.cache_key.as_bytes()));
    let identity_sha256 = (!result.identity.is_null()).then(|| {
        let identity = serde_json::to_vec(&result.identity).unwrap_or_default();
        content_hash::sha256_hex(&identity)
    });
    let Ok(contents) = serde_json::to_vec(&DurableFanoutReadinessEntry {
        schema: "homeboy/provider-readiness-cache/v1".to_string(),
        request_identity: request_key.to_string(),
        checked_at_unix_ms: checked_at_unix_ms.as_millis() as u64,
        ready: true,
        cache_key_sha256,
        identity_sha256,
    }) else {
        return;
    };
    let _ = homeboy_core::io::write_output_file_atomically(
        path,
        contents,
        homeboy_core::io::OutputWriteOptions::artifact(),
    );
    #[cfg(unix)]
    if let Ok(metadata) = std::fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(metadata.permissions().mode() & 0o600),
        );
    }
    if let Some(parent) = path.parent() {
        prune_durable_fanout_readiness(parent, path);
    }
}

fn prune_durable_fanout_readiness(directory: &std::path::Path, current: &std::path::Path) {
    let index_lock = match OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(directory.join("index.lock"))
    {
        Ok(lock) => lock,
        Err(_) => return,
    };
    if !matches!(index_lock.try_lock_exclusive(), Ok(true)) {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            directory.join("index.lock"),
            std::fs::Permissions::from_mode(0o600),
        );
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64);
    let mut entries = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|entry| {
            let path = entry.path();
            let raw = std::fs::read(&path).ok()?;
            let parsed: DurableFanoutReadinessEntry = serde_json::from_slice(&raw).ok()?;
            let fresh = now.is_some_and(|now| {
                parsed.checked_at_unix_ms <= now
                    && now.saturating_sub(parsed.checked_at_unix_ms)
                        <= PROVIDER_RUNTIME_READINESS_READY_TTL.as_millis() as u64
            });
            if parsed.schema != "homeboy/provider-readiness-cache/v1" || !parsed.ready || !fresh {
                let _ = std::fs::remove_file(path);
                return None;
            }
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            Some((path, modified))
        })
        .collect::<Vec<_>>();
    if entries.len() > MAX_DURABLE_FANOUT_READINESS_ENTRIES {
        entries.sort_by_key(|(_, modified)| *modified);
        let remove_count = entries.len() - MAX_DURABLE_FANOUT_READINESS_ENTRIES;
        for (path, _) in entries.into_iter().take(remove_count) {
            if path != current {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn run_durable_fanout_readiness_probe(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    credential_env: &[(String, String)],
    probe_gate: &ProviderReadinessProbeGate,
    deadline: Instant,
    timeout_ms: u64,
    mode: Option<&str>,
    request_key: &str,
) -> std::result::Result<ProviderReadinessInvocationResult, String> {
    let probe = || {
        run_readiness_probe_with_gate(
            provider,
            config,
            credential_env,
            probe_gate,
            deadline,
            timeout_ms,
            mode,
        )
    };
    let Some((cache_path, lock_path)) = durable_fanout_readiness_paths(request_key) else {
        return probe();
    };
    let Some(parent) = lock_path.parent() else {
        return Err("provider readiness cache has no parent directory".to_string());
    };
    if let Err(error) = std::fs::create_dir_all(parent) {
        return probe().map_err(|probe| format!("{probe}; readiness cache unavailable: {error}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let lock = match OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(lock) => lock,
        Err(_) => return probe(),
    };
    #[cfg(unix)]
    let lock = {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600));
        lock
    };
    let mut reported_wait = false;
    loop {
        match lock.try_lock_exclusive() {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(_) => return probe(),
        }
        if !reported_wait {
            eprintln!(
                "{}",
                json!({"event":"provider_readiness_progress", "phase":"provider_readiness", "state":"waiting", "wait_owner":"durable_cache_lock"})
            );
            reported_wait = true;
        }
        if Instant::now() >= deadline {
            return Err("provider readiness shared-cache wait timed out".to_string());
        }
        std::thread::sleep(
            Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    if let Some(result) = read_durable_fanout_readiness(&cache_path, request_key) {
        return Ok(result);
    }
    let result = probe();
    if let Ok(verdict) = &result {
        write_durable_fanout_readiness(&cache_path, request_key, verdict);
    }
    result
}

pub(crate) fn invalidate_fanout_readiness(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    credential_env: &[(String, String)],
    cache: &ProviderRuntimeReadinessCache,
) {
    let Ok(request_key) = readiness_cache_identity_for_generated_fanout_context(
        provider,
        config,
        credential_env,
        true,
        None,
    ) else {
        return;
    };
    {
        let mut state = cache
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.by_request.remove(&request_key);
        cache.shared.changed.notify_all();
    }
    let Some((cache_path, lock_path)) = durable_fanout_readiness_paths(&request_key) else {
        return;
    };
    let Ok(lock) = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)
    else {
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match lock.try_lock_exclusive() {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(_) => return,
        }
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_file(cache_path);
}

#[derive(Debug)]
struct ProviderReadinessProbePermit<'a>(&'a ProviderReadinessProbeGate);

impl Drop for ProviderReadinessProbePermit<'_> {
    fn drop(&mut self) {
        let mut active = self
            .0
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active = active.saturating_sub(1);
        self.0.changed.notify_one();
    }
}

fn acquire_probe_permit<'a>(
    gate: &'a ProviderReadinessProbeGate,
    deadline: Instant,
    timeout_ms: u64,
) -> std::result::Result<ProviderReadinessProbePermit<'a>, String> {
    acquire_probe_permit_from_gate(gate, deadline, timeout_ms)
}

fn acquire_probe_permit_from_gate<'a>(
    gate: &'a ProviderReadinessProbeGate,
    deadline: Instant,
    timeout_ms: u64,
) -> std::result::Result<ProviderReadinessProbePermit<'a>, String> {
    let mut active = gate
        .active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while *active >= MAX_CONCURRENT_PROVIDER_READINESS_PROBES {
        let Some(wait) = deadline.checked_duration_since(Instant::now()) else {
            return Err(format!(
                "provider readiness invocation timed out after {timeout_ms} ms while waiting for a probe permit"
            ));
        };
        let (next, timeout) = gate
            .changed
            .wait_timeout(active, wait)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active = next;
        if timeout.timed_out() && *active >= MAX_CONCURRENT_PROVIDER_READINESS_PROBES {
            return Err(format!(
                "provider readiness invocation timed out after {timeout_ms} ms while waiting for a probe permit"
            ));
        }
    }
    *active += 1;
    Ok(ProviderReadinessProbePermit(gate))
}

fn run_readiness_probe_with_gate(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    credential_env: &[(String, String)],
    gate: &ProviderReadinessProbeGate,
    probe_deadline: Instant,
    probe_timeout_ms: u64,
    mode: Option<&str>,
) -> std::result::Result<ProviderReadinessInvocationResult, String> {
    homeboy_core::observation::phase_timing::time_with_outcome(
        "provider.readiness_probe",
        || {
            let _permit = acquire_probe_permit(gate, probe_deadline, probe_timeout_ms)
                .map_err(homeboy_core::Error::internal_unexpected)?;
            let mut result = Err("provider readiness invocation did not run".to_string());
            for _ in 0..PROVIDER_RUNTIME_READINESS_TRANSIENT_ATTEMPTS {
                let Some(remaining) = probe_deadline.checked_duration_since(Instant::now()) else {
                    result = Err(format!(
                        "provider '{}' readiness invocation timed out after {} ms",
                        provider.id, probe_timeout_ms
                    ));
                    break;
                };
                result = run_provider_readiness_probe_with_env_and_timeout(
                    provider,
                    config,
                    credential_env,
                    remaining,
                    mode,
                );
                let should_retry = match &result {
                    Err(_) => true,
                    Ok(verdict) => {
                        !verdict.ready
                            && verdict.retryable
                            && verdict.classification == "transient_failure"
                    }
                };
                if !should_retry {
                    break;
                }
            }
            result.map_err(homeboy_core::Error::internal_unexpected)
        },
        |verdict| !verdict.ready,
    )
    .map_err(|error| error.message)
}

fn remaining_deadline_duration(deadline_unix_ms: Option<u64>) -> Option<Duration> {
    match crate::agent_task_timeout::remaining_execution_deadline_ms(deadline_unix_ms) {
        Some(0) => None,
        Some(remaining) => Some(Duration::from_millis(remaining)),
        None => Some(Duration::from_secs(24 * 60 * 60)),
    }
}

fn readiness_deadline_error(boundary: &str, deadline_unix_ms: Option<u64>) -> Error {
    let mut error = Error::validation_invalid_argument(
        "execution_deadline_unix_ms",
        format!("agent-task execution deadline expired during provider readiness {boundary}"),
        deadline_unix_ms.map(|value| value.to_string()),
        None,
    )
    // Readiness is an admission check, not provider execution. Exhausting its
    // bounded probe window is transient and must remain eligible for a later
    // admission attempt rather than being classified as invalid credentials.
    .with_retryable(true);
    error.details["classification"] = json!("timeout");
    error.details["zero_provider_executions"] = json!(true);
    error.details["deadline_unix_ms"] = json!(deadline_unix_ms);
    error
}

fn ensure_readiness_deadline(boundary: &str, deadline_unix_ms: Option<u64>) -> Result<()> {
    if remaining_deadline_duration(deadline_unix_ms).is_none() {
        Err(readiness_deadline_error(boundary, deadline_unix_ms))
    } else {
        Ok(())
    }
}

fn map_readiness_result(
    provider: &AgentTaskExecutorProvider,
    result: std::result::Result<ProviderReadinessInvocationResult, String>,
) -> Result<ProviderReadinessInvocationResult> {
    result.map_err(|message| readiness_probe_error(provider, message))
}

fn readiness_probe_error(provider: &AgentTaskExecutorProvider, message: String) -> Error {
    let mut error = Error::validation_invalid_argument(
        "provider_runtime_readiness",
        format!(
            "provider '{}' readiness invocation failed: {}",
            provider.id,
            homeboy_core::redaction::redact_string(&message)
        ),
        Some(provider.backend.clone()),
        None,
    )
    .with_retryable(true);
    error.details["classification"] = json!("transient_failure");
    error.details["zero_provider_executions"] = json!(true);
    error
}

fn publish_readiness_result(
    shared: &ProviderRuntimeReadinessCacheShared,
    request_key: String,
    result: std::result::Result<ProviderReadinessInvocationResult, String>,
) {
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let generation = state.next_generation;
    state.next_generation = state.next_generation.wrapping_add(1);
    state.by_request.insert(
        request_key,
        CachedProviderRuntimeReadiness::Complete {
            result,
            cached_at: Instant::now(),
            generation,
        },
    );
    shared.changed.notify_all();
}

fn discard_readiness_result(shared: &ProviderRuntimeReadinessCacheShared, request_key: &str) {
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.by_request.remove(request_key);
    shared.changed.notify_all();
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
        let credential_env = super::secrets::provider_request_credential_env(task, provider)
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "provider_runtime_readiness",
                    error.message,
                    Some(provider.backend.clone()),
                    None,
                )
            })?;
        let verdict =
            readiness_verdict_with_credentials(provider, &config, &credential_env, cache)?;
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

#[cfg(test)]
pub(crate) fn readiness_verdict(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    cache: &mut ProviderRuntimeReadinessCache,
) -> Result<ProviderReadinessInvocationResult> {
    readiness_verdict_with_credentials(provider, config, &[], cache)
}

pub(crate) fn readiness_verdict_with_credentials(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    credential_env: &[(String, String)],
    cache: &mut ProviderRuntimeReadinessCache,
) -> Result<ProviderReadinessInvocationResult> {
    readiness_verdict_with_credentials_and_deadline(provider, config, credential_env, cache, None)
}

pub(crate) fn readiness_verdict_with_credentials_and_deadline(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    credential_env: &[(String, String)],
    cache: &mut ProviderRuntimeReadinessCache,
    deadline_unix_ms: Option<u64>,
) -> Result<ProviderReadinessInvocationResult> {
    readiness_verdict_with_credentials_and_deadline_for_generated_fanout_context(
        provider,
        config,
        credential_env,
        cache,
        deadline_unix_ms,
        false,
        None,
    )
}

pub(crate) fn readiness_verdict_with_credentials_and_deadline_for_generated_fanout_context(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    credential_env: &[(String, String)],
    cache: &mut ProviderRuntimeReadinessCache,
    deadline_unix_ms: Option<u64>,
    generated_fanout_context: bool,
    mode: Option<&str>,
) -> Result<ProviderReadinessInvocationResult> {
    homeboy_core::observation::phase_timing::time_with_outcome(
        "admit.provider_readiness",
        || {
            ensure_readiness_deadline("probe", deadline_unix_ms)?;
            let request_key = readiness_cache_identity_for_generated_fanout_context(
                provider,
                config,
                credential_env,
                generated_fanout_context,
                mode,
            )?;
            ensure_readiness_deadline("cache_key", deadline_unix_ms)?;
            let mut registered_waiter = false;
            loop {
                let mut state = cache
                    .shared
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Err(error) = ensure_readiness_deadline("cache_lock", deadline_unix_ms) {
                    if registered_waiter {
                        release_cache_waiter(&mut state, &request_key);
                        cache.shared.changed.notify_all();
                    }
                    return Err(error);
                }
                match state.by_request.get(&request_key) {
                    Some(CachedProviderRuntimeReadiness::InFlight) => {
                        if !registered_waiter {
                            *state.waiters.entry(request_key.clone()).or_default() += 1;
                            registered_waiter = true;
                        }
                        let Some(wait) = remaining_deadline_duration(deadline_unix_ms) else {
                            release_cache_waiter(&mut state, &request_key);
                            return Err(readiness_deadline_error("cache_wait", deadline_unix_ms));
                        };
                        let (mut state, timeout) = cache
                            .shared
                            .changed
                            .wait_timeout(state, wait)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if timeout.timed_out() {
                            release_cache_waiter(&mut state, &request_key);
                            return Err(readiness_deadline_error("cache_wait", deadline_unix_ms));
                        }
                        drop(state);
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
                            if let Err(error) =
                                ensure_readiness_deadline("cached_result", deadline_unix_ms)
                            {
                                if registered_waiter {
                                    release_cache_waiter(&mut state, &request_key);
                                    cache.shared.changed.notify_all();
                                }
                                return Err(error);
                            }
                            let result = result.clone();
                            if registered_waiter {
                                release_cache_waiter(&mut state, &request_key);
                                cache.shared.changed.notify_all();
                            }
                            return map_readiness_result(provider, result);
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
                        let Some(wait) = remaining_deadline_duration(deadline_unix_ms) else {
                            return Err(readiness_deadline_error(
                                "cache_capacity",
                                deadline_unix_ms,
                            ));
                        };
                        let (state, timeout) = cache
                            .shared
                            .changed
                            .wait_timeout(state, wait)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        drop(state);
                        if timeout.timed_out() {
                            return Err(readiness_deadline_error(
                                "cache_capacity",
                                deadline_unix_ms,
                            ));
                        }
                        continue;
                    }
                }
                state.by_request.insert(
                    request_key.clone(),
                    CachedProviderRuntimeReadiness::InFlight,
                );
                if let Err(error) = ensure_readiness_deadline("probe_publication", deadline_unix_ms)
                {
                    state.by_request.remove(&request_key);
                    cache.shared.changed.notify_all();
                    return Err(error);
                }
                let provider = provider.clone();
                let config = config.clone();
                let credential_env = credential_env.to_vec();
                let probe_timeout = Duration::from_millis(provider.pi_readiness.timeout_ms);
                // The worker owns a permit and a child process, so it must share the
                // caller's absolute deadline rather than merely letting the caller stop
                // waiting while the work continues in the background.
                let invocation_deadline = Instant::now() + probe_timeout;
                let execution_deadline = remaining_deadline_duration(deadline_unix_ms)
                    .map(|remaining| Instant::now() + remaining);
                let deadline_limited =
                    execution_deadline.is_some_and(|deadline| deadline < invocation_deadline);
                let probe_deadline = execution_deadline
                    .map(|deadline| invocation_deadline.min(deadline))
                    .unwrap_or(invocation_deadline);
                let probe_timeout_ms = u64::try_from(
                    probe_deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis(),
                )
                .unwrap_or(u64::MAX);
                let shared = Arc::clone(&cache.shared);
                let probe_gate = Arc::clone(&cache.shared.probe_gate);
                let probe_request_key = request_key.clone();
                let probe_mode = mode.map(str::to_string);
                let persistent_fanout = generated_fanout_context;
                drop(state);
                let spawn_result = std::thread::Builder::new()
                    .name("provider-readiness-probe".to_string())
                    .spawn(homeboy_core::observation::phase_timing::inherit(
                        move || {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    if persistent_fanout {
                                        run_durable_fanout_readiness_probe(
                                            &provider,
                                            &config,
                                            &credential_env,
                                            probe_gate.as_ref(),
                                            probe_deadline,
                                            probe_timeout_ms,
                                            probe_mode.as_deref(),
                                            &probe_request_key,
                                        )
                                    } else {
                                        run_readiness_probe_with_gate(
                                            &provider,
                                            &config,
                                            &credential_env,
                                            probe_gate.as_ref(),
                                            probe_deadline,
                                            probe_timeout_ms,
                                            probe_mode.as_deref(),
                                        )
                                    }
                                }))
                                .unwrap_or_else(|_| {
                                    Err("provider readiness invocation panicked".to_string())
                                });

                            if deadline_limited
                                && result
                                    .as_ref()
                                    .err()
                                    .is_some_and(|error| error.contains("timed out"))
                            {
                                // The result is about one caller's exhausted deadline,
                                // not this provider's reusable readiness state.
                                discard_readiness_result(&shared, &probe_request_key);
                            } else {
                                publish_readiness_result(&shared, probe_request_key, result);
                            }
                        },
                    ));
                if let Err(error) = spawn_result {
                    publish_readiness_result(
                        &cache.shared,
                        request_key.clone(),
                        Err(format!("failed to start provider readiness probe: {error}")),
                    );
                }
            }
        },
        |verdict| !verdict.ready,
    )
}

/// Run one route's readiness invocation in capacity-only mode (#15024): the
/// request carries `mode: "capacity"` so a runtime that supports it returns
/// capacity evidence without spending its bounded inference probe, and the
/// cache identity is mode-specific so the answer can never satisfy — or be
/// satisfied by — live-inference readiness. Failure is a normal outcome for
/// callers: a route whose runtime lookup fails reports `unknown` capacity and
/// must not block the other routes.
pub(crate) fn capacity_mode_readiness_verdict_with_deadline(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    credential_env: &[(String, String)],
    cache: &mut ProviderRuntimeReadinessCache,
    deadline_unix_ms: Option<u64>,
) -> Result<ProviderReadinessInvocationResult> {
    readiness_verdict_with_credentials_and_deadline_for_generated_fanout_context(
        provider,
        config,
        credential_env,
        cache,
        deadline_unix_ms,
        false,
        Some(PROVIDER_READINESS_CAPACITY_MODE),
    )
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
    readiness_request_key_for_generated_fanout_context(provider, config, false, None)
}

/// Child-specific fanout identity is orchestration metadata rather than a
/// provider readiness input. Keep the shared fanout context and all caller
/// values, including an explicit provider-config `client_context`.
fn normalize_generated_fanout_client_context(provider_config: &mut serde_json::Map<String, Value>) {
    let Some(fanout) = provider_config
        .get_mut("client_context")
        .and_then(Value::as_object_mut)
        .and_then(|context| context.get_mut("fanout"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    if fanout.get("semantics") != Some(&Value::String("batch_cook".to_string())) {
        return;
    }
    for field in [
        "cook_id",
        "to_worktree",
        "head",
        "workspace_materialization",
    ] {
        fanout.remove(field);
    }
}

/// The complete process-local readiness cache identity. Callers that coalesce
/// readiness work must use this rather than approximating a provider route.
pub(crate) fn readiness_cache_identity_for_generated_fanout_context(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    credential_env: &[(String, String)],
    generated_fanout_context: bool,
    mode: Option<&str>,
) -> Result<String> {
    let base_key = readiness_request_key_for_generated_fanout_context(
        provider,
        config,
        generated_fanout_context,
        mode,
    )?;
    let credential_identity = credential_env
        .iter()
        .map(|(name, value)| (name, content_hash::sha256_hex(value.as_bytes())))
        .collect::<Vec<_>>();
    let provider_contract = serde_json::to_vec(provider)
        .map(|bytes| content_hash::sha256_hex(&bytes))
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    let encoded = serde_json::to_vec(&(base_key, credential_identity, provider_contract))
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    Ok(content_hash::sha256_hex(&encoded))
}

fn readiness_request_key_for_generated_fanout_context(
    provider: &AgentTaskExecutorProvider,
    config: &Value,
    generated_fanout_context: bool,
    mode: Option<&str>,
) -> Result<String> {
    let mut provider_config = config.as_object().cloned().unwrap_or_default();
    if generated_fanout_context {
        normalize_generated_fanout_client_context(&mut provider_config);
    }
    let environment = provider
        .pi_readiness
        .env_allowlist
        .iter()
        .cloned()
        .chain(super::credential_readiness::provider_required_secret_env_names(provider))
        .chain(["PATH".to_string(), "HOME".to_string()])
        .collect::<Vec<_>>();
    let mut environment = environment;
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
    // The readiness request mode belongs in the cache identity: a capacity
    // probe and a live-inference probe of the same route are different
    // questions, so a capacity answer must never be served to — or satisfy —
    // a live-inference caller (#15024). Absent mode keeps legacy keys
    // byte-identical with pre-#15024 identities.
    let mut value = json!({
        "provider_id": provider.id,
        "runtime_path": provider.runtime_path,
        "pi_readiness": provider.pi_readiness,
        "effective_config": provider_config,
        "environment": environment,
    });
    if let Some(mode) = mode {
        value["mode"] = Value::String(mode.to_string());
    }
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
        | "unavailable"
        | "transient_failure" => verdict.classification.as_str(),
        _ => "unknown",
    };
    let reason = if verdict.reason.is_empty() {
        "provider-owned readiness check failed".to_string()
    } else {
        homeboy_core::redaction::redact_string(&verdict.reason)
    };
    let evidence = json!({
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
    });
    let mut hints = Vec::new();
    if !verdict.remediation.trim().is_empty() {
        hints.push(homeboy_core::redaction::redact_string(&verdict.remediation));
    }
    let mut error = Error::validation_invalid_argument(
        "provider_runtime_readiness",
        format!(
            "agent-task backend '{}' is not ready for its selected runtime/model: {} ({reason})",
            provider.backend, classification
        ),
        Some(provider.backend.clone()),
        Some(hints),
    )
    .with_retryable(verdict.retryable);
    error.details["runtime_readiness_evidence"] = evidence;
    error
}

/// The readiness cache identity includes the probe's `HOME`. A parallel test
/// repointing `HOME` between two probes turns them into different identities,
/// so the second one re-probes (#14832). Tests that assert readiness reuse hold
/// an isolated home, which fixes `HOME` for their whole duration and keeps the
/// durable fan-out cache out of the real data root.
#[cfg(test)]
pub(crate) fn stable_identity_home() -> homeboy_core::test_support::HomeGuard {
    homeboy_core::test_support::HomeGuard::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_pi::FakePi;

    fn pi_fixture(models: &str) -> (FakePi, AgentTaskExecutorProvider, Value, PathBuf) {
        let fake = FakePi::with_probes("exit 1", "printf 'test-pi\\n'", models);
        let count = fake.root.path().join("count");
        let provider = fake.provider();
        let mut config = fake.config();
        config["model"] = json!("test/model");
        config["runtime_env"] = json!({"COUNT": count});
        (fake, provider, config, count)
    }

    fn probe_count(path: &std::path::Path) -> usize {
        std::fs::read_to_string(path)
            .expect("probe count")
            .lines()
            .count()
    }

    fn test_cache() -> ProviderRuntimeReadinessCache {
        ProviderRuntimeReadinessCache::isolated()
    }

    fn provider() -> AgentTaskExecutorProvider {
        serde_json::from_value(json!({
            "id": "runtime.provider",
            "backend": "runtime"
        }))
        .expect("provider fixture")
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
        let store = homeboy_core::observation::ObservationStore::open_initialized_at(
            root.path().join("observe.db"),
        )
        .unwrap();
        let run_id = store
            .start_run(homeboy_core::observation::NewRunRecord::builder("agent-task").build())
            .unwrap()
            .id;
        let _timing = homeboy_core::observation::phase_timing::for_run(&store, &run_id);
        let (_fake, provider, mut config, _) = pi_fixture("printf 'test model\\n'");
        for (model, classification, ready) in [
            ("test/model", "ready", true),
            ("model", "deterministic_incompatibility", false),
            ("test/missing", "deterministic_incompatibility", false),
        ] {
            config["model"] = json!(model);
            let verdict = readiness_verdict(&provider, &config, &mut test_cache()).unwrap();
            assert_eq!(verdict.classification, classification);
            assert_eq!(verdict.ready, ready);
            assert!(!verdict.retryable);
            assert_eq!(verdict.remediation.is_empty(), ready);
            if !ready {
                assert!(verdict.remediation.contains("Select a"));
            }
        }
        for (models, classification, retryable, remediation) in [
            (
                "printf 'No models available\\n'",
                "auth_failure",
                false,
                "Log in",
            ),
            (
                "exit 1",
                "transient_failure",
                true,
                "Retry the bounded Pi model catalog probe",
            ),
        ] {
            let (_fake, provider, config, _) = pi_fixture(models);
            let verdict = readiness_verdict(&provider, &config, &mut test_cache()).unwrap();
            assert!(!verdict.ready);
            assert_eq!(verdict.classification, classification);
            assert_eq!(verdict.retryable, retryable);
            assert!(verdict.remediation.contains(remediation));
        }
        let spans = store.phase_spans(&run_id).unwrap();
        for phase in ["admit.provider_readiness", "provider.readiness_probe"] {
            let samples: Vec<_> = spans.spans.iter().filter(|span| span.id == phase).collect();
            assert_eq!(samples.len(), 5);
            assert!(samples.iter().all(|span| span.duration_ms.unwrap() > 0.0));
            assert_eq!(
                samples
                    .iter()
                    .filter(|span| span.status
                        == homeboy_engine_primitives::phase_timing::PhaseStatus::Failed)
                    .count(),
                4
            );
        }
        // An unopenable timing store cannot change the actual readiness result.
        let _broken = homeboy_core::observation::phase_timing::enter(Some((
            root.path().to_path_buf(),
            run_id,
        )));
        assert!(
            readiness_verdict(
                &provider,
                &{
                    config["model"] = json!("test/model");
                    config
                },
                &mut test_cache()
            )
            .unwrap()
            .ready
        );
    }

    #[test]
    fn shared_cache_deduplicates_a_fanout_runtime_probe() {
        let _home = stable_identity_home();
        let (_fake, provider, config, count) =
            pi_fixture("printf 'probe\\n' >> \"$COUNT\"; printf 'test model\\n'");
        let mut cache = test_cache();

        let first = readiness_verdict(&provider, &config, &mut cache).expect("first verdict");
        let second = readiness_verdict(&provider, &config, &mut cache).expect("cached verdict");

        assert_eq!(first.cache_key, second.cache_key);
        assert_eq!(probe_count(&count), 1);
    }

    #[test]
    fn process_local_cache_deduplicates_phases_without_reusing_stale_credentials() {
        let _home = stable_identity_home();
        let fake = super::super::test_pi::FakePi::with_probes(
            "exit 1",
            "printf 'test-pi\\n'",
            "printf 'probe\\n' >> \"$COUNT\"; printf 'test model\\n'",
        );
        let count = fake.root.path().join("count");
        let provider = fake.provider();
        let mut config = fake.config();
        config["model"] = json!("test/model");
        config["runtime_env"] = json!({"COUNT":count});
        let mut compile_cache = ProviderRuntimeReadinessCache::process_local();
        let mut admission_cache = ProviderRuntimeReadinessCache::process_local();

        readiness_verdict_with_credentials(
            &provider,
            &config,
            &[("TOKEN".to_string(), "first".to_string())],
            &mut compile_cache,
        )
        .expect("first phase verdict");
        readiness_verdict_with_credentials(
            &provider,
            &config,
            &[("TOKEN".to_string(), "first".to_string())],
            &mut admission_cache,
        )
        .expect("same-credential phase verdict");
        readiness_verdict_with_credentials(
            &provider,
            &config,
            &[("TOKEN".to_string(), "rotated".to_string())],
            &mut admission_cache,
        )
        .expect("rotated-credential phase verdict");

        assert_eq!(
            std::fs::read_to_string(count)
                .expect("probe count")
                .lines()
                .count(),
            2
        );
    }

    #[test]
    fn durable_fanout_cache_hashes_provider_identity_and_expires() {
        let _home = stable_identity_home();
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("entry.json");
        let request_key = content_hash::sha256_hex(b"test-request");
        let result = ProviderReadinessInvocationResult {
            ready: true,
            classification: "ready".to_string(),
            retryable: false,
            remediation: String::new(),
            reason: String::new(),
            cache_key: "private-provider-cache-key".to_string(),
            identity: json!({ "account": "private-account-label" }),
            capacity: None,
            durable_identity: None,
        };

        write_durable_fanout_readiness(&path, &request_key, &result);
        let bytes = std::fs::read_to_string(&path).expect("durable cache file");
        assert!(!bytes.contains("private-provider-cache-key"));
        assert!(!bytes.contains("private-account-label"));
        let restored =
            read_durable_fanout_readiness(&path, &request_key).expect("fresh canonical readiness");
        let identity = restored.durable_identity.expect("opaque identity hashes");
        assert_eq!(
            identity.cache_key_sha256.as_deref(),
            Some(content_hash::sha256_hex(b"private-provider-cache-key").as_str())
        );
        assert_eq!(
            identity.identity_sha256.as_deref(),
            Some(
                content_hash::sha256_hex(&serde_json::to_vec(&result.identity).expect("identity"))
                    .as_str()
            )
        );

        let mut stale: Value = serde_json::from_str(&bytes).expect("cache entry");
        stale["checked_at_unix_ms"] = json!(0);
        std::fs::write(&path, serde_json::to_vec(&stale).expect("stale entry"))
            .expect("age cache entry");
        assert!(read_durable_fanout_readiness(&path, &request_key).is_none());

        let capacity_path = root.path().join("capacity.json");
        let mut capacity_result = result;
        capacity_result.capacity = Some(Default::default());
        write_durable_fanout_readiness(&capacity_path, &request_key, &capacity_result);
        assert!(
            !capacity_path.exists(),
            "capacity evidence is never reusable"
        );
    }

    #[test]
    fn explicit_provider_config_client_contexts_do_not_share_fanout_readiness() {
        let _home = stable_identity_home();
        let (_fake, provider, mut config, count) =
            pi_fixture("printf 'probe\\n' >> \"$COUNT\"; printf 'test model\\n'");
        let mut cache = test_cache();

        for account in ["first", "second"] {
            config["client_context"] = json!({"account": account});
            assert!(
                readiness_verdict(&provider, &config, &mut cache,)
                    .expect("readiness verdict")
                    .ready
            );
        }

        assert_eq!(probe_count(&count), 2);
    }

    #[test]
    fn generated_fanout_child_identity_deduplicates_readiness() {
        let _home = stable_identity_home();
        let (_fake, provider, mut config, count) =
            pi_fixture("printf 'probe\\n' >> \"$COUNT\"; printf 'test model\\n'");
        let mut cache = test_cache();

        for (cook_id, to_worktree) in [("first", "homeboy@first"), ("second", "homeboy@second")] {
            config["client_context"] = json!({"fanout": {
                "id": "shared-fanout", "semantics": "batch_cook",
                "cook_id": cook_id, "to_worktree": to_worktree,
                "head": format!("fanout/{cook_id}"),
                "workspace_materialization": [{"child": cook_id}],
            }});
            assert!(
                readiness_verdict_with_credentials_and_deadline_for_generated_fanout_context(
                    &provider,
                    &config,
                    &[],
                    &mut cache,
                    None,
                    true,
                    None,
                )
                .expect("readiness verdict")
                .ready
            );
        }

        assert_eq!(probe_count(&count), 1);
    }

    #[test]
    fn caller_fanout_shape_without_the_internal_marker_remains_distinct() {
        let _home = stable_identity_home();
        let (_fake, provider, mut config, count) =
            pi_fixture("printf 'probe\\n' >> \"$COUNT\"; printf 'test model\\n'");
        let mut cache = test_cache();

        for cook_id in ["first", "second"] {
            config["client_context"] = json!({"fanout": {
                "id": "shared-fanout", "semantics": "batch_cook", "cook_id": cook_id,
            }});
            assert!(
                readiness_verdict(&provider, &config, &mut cache,)
                    .expect("readiness verdict")
                    .ready
            );
        }

        assert_eq!(probe_count(&count), 2);
    }

    #[test]
    fn negative_runtime_evidence_expires_and_reprobes_recovered_credentials() {
        let _home = stable_identity_home();
        let root = tempfile::tempdir().expect("tempdir");
        let recovered = root.path().join("recovered");
        let (fake, provider, mut config, count) = pi_fixture(
            "printf 'probe\\n' >> \"$COUNT\"; if [ -f \"$RECOVERED\" ]; then printf 'test model\\n'; else printf 'No models available\\n'; fi",
        );
        config["runtime_env"]["RECOVERED"] = json!(recovered);
        let mut cache = test_cache();
        let _keep_alive = fake;

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
        assert_eq!(probe_count(&count), 2);
    }

    #[test]
    fn concurrent_exact_key_probes_singleflight() {
        let _home = stable_identity_home();
        let fake = super::super::test_pi::FakePi::with_probes(
            "exit 1",
            "printf 'test-pi\\n'",
            "printf 'probe\\n' >> \"$COUNT\"; sleep 0.1; printf 'test model\\n'",
        );
        let count = fake.root.path().join("count");
        let provider = fake.provider();
        let mut config = fake.config();
        config["model"] = json!("test/model");
        config["runtime_env"] = json!({"COUNT": count});
        let cache = test_cache();
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let provider = provider.clone();
                let mut cache = cache.clone();
                let config = config.clone();
                scope.spawn(move || {
                    assert!(
                        readiness_verdict(&provider, &config, &mut cache)
                            .expect("readiness")
                            .ready
                    );
                });
            }
        });
        assert_eq!(
            std::fs::read_to_string(&count)
                .expect("probe count")
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn short_deadline_waiter_does_not_poison_long_deadline_singleflight() {
        let _home = stable_identity_home();
        let root = tempfile::tempdir().expect("tempdir");
        let release = root.path().join("release");
        // Release explicitly rather than racing a fixed probe delay.
        let (_fake, provider, mut config, count) = pi_fixture(
            "printf 'probe\\n' >> \"$COUNT\"; while [ ! -f \"$RELEASE\" ]; do sleep 0.01; done; printf 'test model\\n'",
        );
        config["runtime_env"]["RELEASE"] = json!(release);
        let cache = test_cache();
        let long_provider = provider.clone();
        let mut long_cache = cache.clone();
        let long_config = config.clone();
        // The long deadline only has to outlive the handshake; it bounds a
        // hung probe, not the code under test.
        let long = std::thread::spawn(move || {
            readiness_verdict_with_credentials_and_deadline(
                &long_provider,
                &long_config,
                &[],
                &mut long_cache,
                Some(crate::agent_task_timeout::now_unix_ms() + 60_000),
            )
        });
        let started = Instant::now() + Duration::from_secs(5);
        loop {
            let in_flight = cache
                .shared
                .state
                .lock()
                .expect("readiness cache")
                .by_request
                .values()
                .any(|entry| matches!(entry, CachedProviderRuntimeReadiness::InFlight));
            if in_flight {
                break;
            }
            assert!(
                !long.is_finished(),
                "long probe finished before publication"
            );
            assert!(Instant::now() < started, "long probe did not start");
            std::thread::sleep(Duration::from_millis(5));
        }

        let short_deadline = crate::agent_task_timeout::now_unix_ms() + 25;
        let error = readiness_verdict_with_credentials_and_deadline(
            &provider,
            &config,
            &[],
            &mut cache.clone(),
            Some(short_deadline),
        )
        .expect_err("short waiter must time out locally");
        assert_eq!(error.details["classification"], "timeout");
        assert_eq!(error.details["deadline_unix_ms"], short_deadline);
        std::fs::write(&release, "release").expect("release probe");
        assert!(
            long.join()
                .expect("long waiter")
                .expect("long readiness")
                .ready
        );

        let mut cache = cache;
        assert!(
            readiness_verdict(&provider, &config, &mut cache)
                .expect("published long result")
                .ready
        );
        assert_eq!(
            std::fs::read_to_string(count)
                .expect("probe count")
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn expired_probe_owner_releases_singleflight_for_a_long_deadline_retry() {
        let _home = stable_identity_home();
        let root = tempfile::tempdir().expect("tempdir");
        let release = root.path().join("release");
        let (_fake, provider, mut config, count) = pi_fixture(
            "printf 'probe\\n' >> \"$COUNT\"; while [ ! -f \"$RELEASE\" ]; do sleep 0.01; done; printf 'test model\\n'",
        );
        config["runtime_env"]["RELEASE"] = json!(release);
        let mut cache = test_cache();

        // The short deadline must only outlast the probe's process startup,
        // which on a loaded host is scheduling latency rather than readiness
        // work; the probe itself never answers before the deadline.
        let short_deadline = crate::agent_task_timeout::now_unix_ms() + 5_000;
        let short_provider = provider.clone();
        let mut short_cache = cache.clone();
        let short_config = config.clone();
        let short = std::thread::spawn(move || {
            readiness_verdict_with_credentials_and_deadline(
                &short_provider,
                &short_config,
                &[],
                &mut short_cache,
                Some(short_deadline),
            )
        });
        while !count.exists() {
            assert!(
                crate::agent_task_timeout::now_unix_ms() < short_deadline,
                "short deadline expired before its probe started"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let error = short
            .join()
            .expect("short probe owner")
            .expect_err("short probe owner must time out locally");
        assert_eq!(error.details["classification"], "timeout");
        assert_eq!(error.details["deadline_unix_ms"], short_deadline);

        std::fs::write(&release, "release").expect("release probe");
        let verdict = readiness_verdict_with_credentials_and_deadline(
            &provider,
            &config,
            &[],
            &mut cache,
            Some(crate::agent_task_timeout::now_unix_ms() + 60_000),
        )
        .expect("long waiter starts a fresh readiness probe");
        assert!(verdict.ready);
        assert_eq!(
            std::fs::read_to_string(count)
                .expect("probe count")
                .lines()
                .count(),
            2
        );
    }

    #[test]
    fn distinct_readiness_keys_probe_concurrently() {
        let root = tempfile::tempdir().expect("tempdir");
        let (_fake, provider, config, _) = pi_fixture(
            "touch \"$ROOT/started-$KEY\"; while [ ! -f \"$ROOT/started-$OTHER\" ]; do sleep 0.01; done; printf 'test model\\n'",
        );
        let cache = test_cache();
        std::thread::scope(|scope| {
            for (key, other) in [("a", "b"), ("b", "a")] {
                let provider = provider.clone();
                let mut cache = cache.clone();
                let mut config = config.clone();
                config["runtime_env"] = json!({"ROOT": root.path(), "KEY": key, "OTHER": other});
                scope.spawn(move || {
                    assert!(
                        readiness_verdict(&provider, &config, &mut cache)
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
        let _home = stable_identity_home();
        let root = tempfile::tempdir().expect("tempdir");
        let release = root.path().join("release");
        let probes = root.path().join("probes");
        let (_fake, provider, mut config, _) = pi_fixture(
            "printf '%s\\n' \"$KEY\" >> \"$PROBES\"; while [ ! -f \"$RELEASE\" ]; do sleep 0.01; done; printf 'test model\\n'",
        );
        config["runtime_env"] = json!({"RELEASE": release, "PROBES": probes});
        let cache = test_cache();
        std::thread::scope(|scope| {
            for index in (0..65).chain(std::iter::once(0)) {
                let provider = provider.clone();
                let mut cache = cache.clone();
                let mut config = config.clone();
                config["runtime_env"]["KEY"] = json!(format!("model-{index}"));
                scope.spawn(move || {
                    assert!(
                        readiness_verdict(&provider, &config, &mut cache,)
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

    /// #15024: a capacity-mode probe and a live-inference probe of the same
    /// route are different questions. The mode belongs to the cache identity,
    /// so the two probes each run once against one shared cache, neither
    /// answer is served to the other caller, and a capacity result can never
    /// satisfy live-inference readiness (or vice versa).
    #[test]
    fn capacity_mode_probes_are_a_distinct_cache_identity_from_live_inference() {
        let _home = stable_identity_home();
        let (_fake, provider, config, count) =
            pi_fixture("printf 'probe\\n' >> \"$COUNT\"; printf 'test model\\n'");
        let mut cache = test_cache();

        let capacity = capacity_mode_readiness_verdict_with_deadline(
            &provider,
            &config,
            &[],
            &mut cache,
            None,
        )
        .expect("capacity verdict");
        assert!(capacity.ready);
        assert!(capacity.capacity.is_none());
        assert_eq!(capacity.reason, "capacity_not_published");
        assert!(!count.exists(), "capacity mode does not run a live probe");

        let live = readiness_verdict(&provider, &config, &mut cache).expect("live verdict");
        assert!(live.ready);
        assert_ne!(live.cache_key, capacity.cache_key);
        assert!(live.reason.is_empty());
        assert!(
            live.capacity.is_none(),
            "a live-inference answer must never be served from capacity-mode cache"
        );

        capacity_mode_readiness_verdict_with_deadline(&provider, &config, &[], &mut cache, None)
            .expect("capacity answer is cached under its own identity");
        readiness_verdict(&provider, &config, &mut cache).expect("live answer is cached");

        assert_eq!(
            probe_count(&count),
            1,
            "only live mode runs the catalog probe"
        );
    }

    #[test]
    fn readiness_probe_errors_are_cached_briefly() {
        let _home = stable_identity_home();
        let (fake, provider, mut config, count) = pi_fixture(
            "printf 'probe\\n' >> \"$COUNT\"; if [ -f \"$RECOVERED\" ]; then printf 'test model\\n'; else exit 1; fi",
        );
        let recovered = fake.root.path().join("recovered");
        config["runtime_env"]["RECOVERED"] = json!(recovered);
        let mut cache = test_cache();
        for _ in 0..2 {
            let verdict = readiness_verdict(&provider, &config, &mut cache).unwrap();
            assert!(!verdict.ready);
            assert_eq!(verdict.classification, "transient_failure");
            assert!(verdict.retryable);
        }
        assert_eq!(
            std::fs::read_to_string(&count)
                .expect("probe count")
                .lines()
                .count(),
            PROVIDER_RUNTIME_READINESS_TRANSIENT_ATTEMPTS
        );
        std::fs::write(&recovered, "ready").expect("recover probe");
        cache.expire_all();
        assert!(
            readiness_verdict(&provider, &config, &mut cache)
                .expect("recovered probe")
                .ready
        );
        assert_eq!(
            std::fs::read_to_string(&count)
                .expect("probe count")
                .lines()
                .count(),
            PROVIDER_RUNTIME_READINESS_TRANSIENT_ATTEMPTS + 1
        );
    }

    #[test]
    fn readiness_timeout_is_shared_across_retries() {
        let (_fake, provider, config, count) = pi_fixture(
            "if [ ! -f \"$COUNT\" ]; then printf 'probe\\n' >> \"$COUNT\"; sleep 4; exit 1; fi; printf 'probe\\n' >> \"$COUNT\"; sleep 600",
        );
        // The budget only has to cover the first attempt's process startup on
        // a loaded host; the retry never answers, so it always runs into the
        // shared deadline whatever the budget is (#14832).
        const BUDGET_MS: u64 = 10_000;
        let gate = Box::leak(Box::new(ProviderReadinessProbeGate {
            active: Mutex::new(0),
            changed: Condvar::new(),
        }));
        let started = Instant::now();
        let result = run_readiness_probe_with_gate(
            &provider,
            &config,
            &[],
            gate,
            Instant::now() + Duration::from_millis(BUDGET_MS),
            BUDGET_MS,
            None,
        );
        match result {
            Ok(verdict) => {
                assert!(!verdict.ready);
                assert_eq!(verdict.classification, "transient_failure");
                assert!(verdict.remediation.contains("timed out"));
            }
            Err(error) => assert!(error.contains("timed out"), "{error}"),
        }
        // A fresh ten-second budget for the retry would take at least fourteen
        // seconds, rather than sharing the original ten-second deadline.
        assert!(started.elapsed() >= Duration::from_millis(BUDGET_MS - 500));
        assert!(started.elapsed() < Duration::from_secs(12));
        assert_eq!(probe_count(&count), 2);
    }

    #[test]
    fn readiness_permit_wait_uses_the_total_probe_deadline() {
        let gate = Box::leak(Box::new(ProviderReadinessProbeGate {
            active: Mutex::new(0),
            changed: Condvar::new(),
        }));
        let deadline = Instant::now() + Duration::from_secs(1);
        let permits = (0..MAX_CONCURRENT_PROVIDER_READINESS_PROBES)
            .map(|_| {
                acquire_probe_permit_from_gate(gate, deadline, 1_000)
                    .expect("fill readiness permits")
            })
            .collect::<Vec<_>>();

        let error =
            acquire_probe_permit_from_gate(gate, Instant::now() + Duration::from_millis(25), 25)
                .expect_err("saturated readiness queue must be bounded");

        assert!(error.contains("timed out after 25 ms"), "{error}");
        drop(permits);
    }

    #[test]
    fn expired_execution_deadline_returns_typed_timeout_without_a_probe() {
        let provider = provider();

        let error = readiness_verdict_with_credentials_and_deadline(
            &provider,
            &json!({"model":"ready"}),
            &[],
            &mut test_cache(),
            Some(crate::agent_task_timeout::now_unix_ms().saturating_sub(1)),
        )
        .expect_err("expired deadline stops before probing");

        assert_eq!(error.details["classification"], "timeout");
        assert_eq!(error.details["zero_provider_executions"], true);
    }

    #[test]
    fn readiness_command_timeout_is_capped_to_the_absolute_deadline() {
        let root = tempfile::tempdir().expect("tempdir");
        let pid_file = root.path().join("pid");
        let fake = FakePi::with_probes(
            "exit 1",
            "printf '%s' \"$$\" > \"$PID_FILE\"; exec sleep 600",
            "exit 1",
        );
        let provider = fake.provider();
        let mut config = fake.config();
        config["readiness_timeout_ms"] = json!(120_000);
        config["runtime_env"] = json!({"PID_FILE": pid_file});
        // The command's own timeout is far beyond the absolute deadline, so
        // returning well before it proves the cap. The deadline itself only
        // has to let the probe start and record its pid on a loaded host.
        let started = Instant::now();

        let error = readiness_verdict_with_credentials_and_deadline(
            &provider,
            &config,
            &[],
            &mut test_cache(),
            Some(crate::agent_task_timeout::now_unix_ms() + 5_000),
        )
        .expect_err("absolute deadline interrupts readiness");

        assert_eq!(error.details["classification"], "timeout");
        assert!(started.elapsed() < Duration::from_secs(60));
        let pid = std::fs::read_to_string(&pid_file)
            .expect("probe recorded its pid")
            .trim()
            .parse::<u32>()
            .expect("numeric probe pid");
        let cleanup_deadline = Instant::now() + Duration::from_secs(2);
        while homeboy_core::process::pid_is_running(pid) {
            assert!(
                Instant::now() < cleanup_deadline,
                "readiness probe {pid} survived its absolute deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn delayed_ready_route_is_cached_across_repeated_admission_probes() {
        let _home = stable_identity_home();
        let (_fake, provider, config, count) =
            pi_fixture("printf 'probe\\n' >> \"$COUNT\"; sleep 4; printf 'test model\\n'");
        let mut cache = test_cache();
        let started = Instant::now();

        assert!(
            readiness_verdict_with_credentials_and_deadline(
                &provider,
                &config,
                &[],
                &mut cache,
                None,
            )
            .expect("delayed readiness")
            .ready
        );
        assert!(started.elapsed() >= Duration::from_secs(3));
        assert!(
            readiness_verdict_with_credentials_and_deadline(
                &provider,
                &config,
                &[],
                &mut cache,
                None,
            )
            .expect("cached readiness")
            .ready
        );
        assert_eq!(
            std::fs::read_to_string(count)
                .expect("probe count")
                .lines()
                .count(),
            1,
            "a route already proven ready must not be re-probed during rotation"
        );
    }
}
