use std::cell::RefCell;
use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::defaults::{
    self, HomeboyConfig, WorktreeProviderConfig, WorktreeProviderKind,
    WorktreeProviderListResultMapping,
};
use crate::error::{CommandEvidence, Error, Result};

/// `HomeboyConfig.settings` key containing provider lifecycle command settings.
pub const WORKTREE_PROVIDER_LIFECYCLE_SETTINGS_KEY: &str = "worktree_provider_lifecycle";

#[derive(Debug, Deserialize)]
struct WorktreeProviderLifecycleSettings {
    finalize: Vec<String>,
}

/// Resolve the provider-agnostic terminal lifecycle argv configured through the
/// generic settings extension map. Keeping this separate from provider command
/// discovery preserves the public provider configuration struct.
pub fn worktree_provider_lifecycle_finalizer_argv_from_config(
    provider_id: &str,
    config: &HomeboyConfig,
) -> Result<Option<Vec<String>>> {
    let Some(value) = config
        .settings
        .get(WORKTREE_PROVIDER_LIFECYCLE_SETTINGS_KEY)
    else {
        return Ok(None);
    };
    let settings = serde_json::from_value::<BTreeMap<String, WorktreeProviderLifecycleSettings>>(
        value.clone(),
    )
    .map_err(|error| {
        Error::validation_invalid_argument(
            format!("settings.{WORKTREE_PROVIDER_LIFECYCLE_SETTINGS_KEY}"),
            format!("provider lifecycle settings must map provider ids to finalize argv: {error}"),
            Some(provider_id.to_string()),
            None,
        )
    })?;
    Ok(settings
        .get(provider_id)
        .map(|settings| settings.finalize.clone()))
}

#[cfg(not(test))]
const PROVIDER_CLEANUP_HEARTBEAT: Duration = Duration::from_secs(5);
#[cfg(test)]
const PROVIDER_CLEANUP_HEARTBEAT: Duration = Duration::from_millis(100);
const PROVIDER_CLEANUP_OUTPUT_LIMIT: usize = 64 * 1024;
#[cfg(not(test))]
const PROVIDER_LOOKUP_HEARTBEAT: Duration = Duration::from_millis(100);
#[cfg(test)]
const PROVIDER_LOOKUP_HEARTBEAT: Duration = Duration::from_millis(1);

/// Cancellation is scoped to the controller thread issuing provider commands.
/// This keeps provider supervision generic while letting a durable owner stop
/// an isolated process group when its own lifecycle is cancelled.
#[derive(Clone, Default)]
pub struct WorktreeProviderCommandControl(Arc<AtomicBool>);

impl WorktreeProviderCommandControl {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

thread_local! {
    static PROVIDER_COMMAND_CONTROL: RefCell<Option<WorktreeProviderCommandControl>> = const { RefCell::new(None) };
}

pub fn with_worktree_provider_command_control<T>(
    control: WorktreeProviderCommandControl,
    run: impl FnOnce() -> T,
) -> T {
    PROVIDER_COMMAND_CONTROL.with(|active| {
        let previous = active.replace(Some(control));
        let result = run();
        active.replace(previous);
        result
    })
}

#[derive(Debug, Clone)]
pub struct WorktreeProviderCleanupOptions {
    pub provider: Vec<String>,
    pub all_providers: bool,
    pub apply: bool,
    /// An aggregate owner may cap this provider's normal cleanup timeout.
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeProviderCleanupMode {
    Preview,
    Apply,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeProviderCleanupOutcome {
    Completed,
    TimedOut,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeProviderInventoryCompleteness {
    Complete,
    Partial,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorktreeProviderCleanupOutput {
    pub command: &'static str,
    pub mode: WorktreeProviderCleanupMode,
    pub provider_count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub inventory_completeness: WorktreeProviderInventoryCompleteness,
    pub providers: Vec<WorktreeProviderCleanupResult>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorktreeProviderCleanupResult {
    pub provider_id: String,
    pub success: bool,
    pub outcome: WorktreeProviderCleanupOutcome,
    pub inventory_completeness: WorktreeProviderInventoryCompleteness,
    pub elapsed_ms: u128,
    pub heartbeat_count: usize,
    pub timeout_ms: u128,
    pub mode: WorktreeProviderCleanupMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_run: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i32>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_progress: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub run_refs: Vec<WorktreeProviderRunRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeProviderRunRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_command: Option<String>,
}

/// A workspace returned by a command worktree provider's `list` command.
///
/// The configured result mapping must project every field below so Homeboy
/// never guesses safety state for an externally managed destination.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorktreeProviderHandle {
    pub handle: String,
    pub path: String,
    pub branch: String,
    pub task_url: Option<String>,
    pub safety: WorktreeProviderHandleSafety,
}

/// A provider-managed workspace together with the provider that resolved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProviderResolution {
    pub provider_id: String,
    pub worktree: WorktreeProviderHandle,
}

/// Non-mutating provider answer for one explicit workspace creation intent.
/// Existing destinations retain their resolved metadata; absent destinations
/// require a provider-declared path projection before they can be represented
/// as a runnable plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeProviderCreatePlan {
    Existing(WorktreeProviderResolution),
    WouldCreate(WorktreeProviderResolution),
}

/// Exact provider identity, intentionally separate from mutable workspace
/// safety. `token` is opaque to Homeboy and is the sole value accepted by a
/// versioned provider safety command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeProviderExactIdentity {
    pub schema: String,
    pub provider_id: String,
    pub token: String,
    pub handle: String,
    pub path: String,
    pub branch: String,
    pub primary: bool,
    pub latency_ms: u128,
    pub budget_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeProviderSafetyAttestation {
    pub schema: String,
    pub identity_token: String,
    pub observed_at: String,
    pub dirty: bool,
    pub unpushed: bool,
    pub fresh: bool,
    pub latency_ms: u128,
    pub budget_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProviderSplitResolution {
    pub identity: WorktreeProviderExactIdentity,
    pub safety: WorktreeProviderSafetyAttestation,
}

/// Explicit destination inputs required to create a managed worktree without
/// inferring repository or branch policy from a product-specific provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProviderCreateIntent {
    pub handle: String,
    pub repo: String,
    pub base: String,
    pub head: String,
    pub task_url: String,
}

/// Typed lifecycle intent attached to a provider-owned workspace request.
/// Providers own any product-specific interpretation; Homeboy only preserves
/// this generic ownership contract through argv substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProviderLifecycleIntent {
    pub purpose: String,
    pub owner_run_ref: String,
    pub cleanup_policy: WorktreeProviderCleanupPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeProviderCleanupPolicy {
    RemoveOnSuccess,
    PreserveOnFailure,
}

impl WorktreeProviderCleanupPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RemoveOnSuccess => "remove_on_success",
            Self::PreserveOnFailure => "preserve_on_failure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeProviderTerminalDisposition {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Interrupted,
}

impl WorktreeProviderTerminalDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Interrupted => "interrupted",
        }
    }

    fn owner_outcome(self) -> &'static str {
        match self {
            Self::Succeeded => "success",
            Self::Failed | Self::Cancelled | Self::TimedOut | Self::Interrupted => "failure",
        }
    }

    fn lifecycle_state(self) -> &'static str {
        match self {
            Self::Succeeded => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProviderFinalization {
    pub provider_id: String,
    pub handle: String,
    pub disposition: WorktreeProviderTerminalDisposition,
    pub owner_outcome: String,
    pub lifecycle_state: String,
    pub inspection_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProviderProvision {
    pub resolution: WorktreeProviderResolution,
    pub action: &'static str,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorktreeProviderHandleSafety {
    pub dirty: bool,
    pub unpushed: bool,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProviderConvergence {
    pub provider_id: String,
    pub handle: String,
    pub path: String,
    pub base_sha: String,
    pub evidence: Value,
}

#[derive(Debug, Deserialize)]
struct WorktreeProviderConvergenceEvidence {
    schema: String,
    identity_token: String,
    base_sha: String,
}

/// Converge a provider-owned worktree only through its declared mutation
/// capability. The split resolver proves the handle is currently clean,
/// pushed, non-primary, and owned by an apply-enabled provider before the
/// provider receives the immutable base SHA.
pub fn converge_apply_enabled_worktree_provider_to_base_from_config(
    handle: &str,
    base_sha: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderConvergence> {
    let resolution = resolve_apply_enabled_worktree_provider_split_from_config(handle, config)?;
    let provider = config
        .worktree_providers
        .get(&resolution.identity.provider_id)
        .expect("split resolution selects a configured provider");
    let command = provider.commands.converge.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "provider `{}` owns `{handle}` but does not configure token-bound commands.converge",
                resolution.identity.provider_id
            ),
            Some(handle.to_string()),
            None,
        )
    })?;
    let command = command
        .iter()
        .map(|argument| {
            argument
                .replace("{handle}", handle)
                .replace("{identity}", &resolution.identity.token)
                .replace("{base}", base_sha)
        })
        .collect::<Vec<_>>();
    let output = run_provider_mutation_command(
        &resolution.identity.provider_id,
        provider,
        &command,
        "converge",
    )?;
    let evidence_value: Value = serde_json::from_slice(&output).map_err(|error| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "provider `{}` returned invalid convergence evidence: {error}",
                resolution.identity.provider_id
            ),
            Some(handle.to_string()),
            None,
        )
    })?;
    let evidence: WorktreeProviderConvergenceEvidence =
        serde_json::from_value(evidence_value.clone()).map_err(|error| {
            Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "provider `{}` returned incomplete convergence evidence: {error}",
                    resolution.identity.provider_id
                ),
                Some(handle.to_string()),
                None,
            )
        })?;
    if evidence.schema != "homeboy/worktree-provider-convergence/v1"
        || evidence.identity_token != resolution.identity.token
        || evidence.base_sha != base_sha
    {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "provider convergence evidence does not bind the attested identity token and pinned base",
            Some(handle.to_string()),
            None,
        ));
    }
    // The mutation command is bound to the pre-mutation opaque token. Re-attest
    // afterward only to detect a changed safety state, never as authorization.
    let safety =
        attest_apply_enabled_worktree_provider_safety_from_config(&resolution.identity, config)?;
    if !safety.fresh || safety.dirty || safety.unpushed || resolution.identity.primary {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "provider safety attestation is not safe after pinned Cook convergence",
            Some(handle.to_string()),
            None,
        ));
    }
    Ok(WorktreeProviderConvergence {
        provider_id: resolution.identity.provider_id,
        handle: resolution.identity.handle,
        path: resolution.identity.path,
        base_sha: base_sha.to_string(),
        evidence: evidence_value,
    })
}

/// Resolve an externally managed worktree handle without creating or adopting
/// a Homeboy record. This is intentionally a lookup-only boundary.
pub fn resolve_worktree_provider_handle(handle: &str) -> Result<WorktreeProviderHandle> {
    resolve_worktree_provider(handle).map(|resolution| resolution.worktree)
}

pub fn resolve_worktree_provider_handle_from_config(
    handle: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderHandle> {
    resolve_worktree_provider_from_config(handle, config).map(|resolution| resolution.worktree)
}

/// Resolve a provider-managed workspace and retain the selected provider id.
pub fn resolve_worktree_provider(handle: &str) -> Result<WorktreeProviderResolution> {
    resolve_worktree_provider_from_config(handle, &defaults::load_config())
}

/// Resolve an effective checkout path through configured providers.
///
/// Unlike handle resolution this is intentionally optional: a rig may declare
/// an ordinary local checkout. When a provider owns the exact path, its safety
/// contract is validated before callers start expensive work.
pub fn resolve_worktree_provider_path(
    path: &std::path::Path,
) -> Result<Option<WorktreeProviderResolution>> {
    resolve_worktree_provider_path_from_config(path, &defaults::load_config())
}

pub fn resolve_worktree_provider_path_from_config(
    path: &std::path::Path,
    config: &HomeboyConfig,
) -> Result<Option<WorktreeProviderResolution>> {
    resolve_worktree_provider_path_with_policy_from_config(path, config, false, None, None)
}

/// Resolve an exact provider-owned checkout path for mutation while applying
/// the same destination safety policy as handle-based promotion.
pub fn resolve_apply_enabled_worktree_provider_path_from_config(
    path: &std::path::Path,
    config: &HomeboyConfig,
    gate_feedback_baseline: Option<&serde_json::Value>,
    trusted_unpushed_destination: Option<&TrustedUnpushedWorktree>,
) -> Result<Option<WorktreeProviderResolution>> {
    resolve_worktree_provider_path_with_policy_from_config(
        path,
        config,
        true,
        gate_feedback_baseline,
        trusted_unpushed_destination,
    )
}

fn resolve_worktree_provider_path_with_policy_from_config(
    path: &std::path::Path,
    config: &HomeboyConfig,
    require_apply_enabled: bool,
    gate_feedback_baseline: Option<&serde_json::Value>,
    trusted_unpushed_destination: Option<&TrustedUnpushedWorktree>,
) -> Result<Option<WorktreeProviderResolution>> {
    let requested = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    let mut provider_ids = config
        .worktree_providers
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    provider_ids.sort();
    for provider_id in provider_ids {
        let provider = &config.worktree_providers[&provider_id];
        if !provider.enabled || (require_apply_enabled && !provider.apply_enabled) {
            continue;
        }
        let worktree = if let Some(command) = provider.commands.resolve_path.as_ref() {
            let worktrees = run_provider_resolve_path_command(
                &provider_id,
                provider,
                command,
                requested.as_path(),
            )?;
            targeted_path_result(&provider_id, worktrees, requested.as_path())?
        } else {
            let Some(command) = provider.commands.list.as_ref() else {
                continue;
            };
            run_provider_list_command(&provider_id, provider, command)?
                .into_iter()
                .find(|worktree| {
                    std::fs::canonicalize(&worktree.path).ok().as_deref()
                        == Some(requested.as_path())
                })
        };
        let Some(worktree) = worktree else {
            continue;
        };
        validate_provider_handle(
            &provider_id,
            &worktree,
            gate_feedback_baseline,
            trusted_unpushed_destination,
        )?;
        return Ok(Some(WorktreeProviderResolution {
            provider_id,
            worktree,
        }));
    }
    Ok(None)
}

pub fn resolve_worktree_provider_from_config(
    handle: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderResolution> {
    resolve_worktree_provider_with_policy_from_config(handle, config, false, None, None)
}

/// Resolve a workspace only from providers explicitly authorized for apply operations.
pub fn resolve_apply_enabled_worktree_provider_from_config(
    handle: &str,
    config: &HomeboyConfig,
    gate_feedback_baseline: Option<&serde_json::Value>,
) -> Result<WorktreeProviderResolution> {
    resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
        handle,
        config,
        gate_feedback_baseline,
        None,
    )
}

/// Whether a provider path names a remote location rather than a filesystem
/// checkout. This is deliberately syntactic so callers never probe a remote
/// location through the local filesystem before its provider materializes it.
pub fn worktree_provider_path_requires_materialization(path: &str) -> bool {
    path.split_once("://")
        .is_some_and(|(scheme, _)| !scheme.is_empty() && scheme != "file")
}

/// Materialize an already-registered provider workspace through that provider's
/// configured mutation capability. The postcondition must be a filesystem path;
/// returning another remote location leaves an actionable, bounded failure
/// rather than letting a later filesystem operation block indefinitely.
pub fn materialize_apply_enabled_worktree_provider_identity_from_config(
    identity: &WorktreeProviderExactIdentity,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderExactIdentity> {
    if !worktree_provider_path_requires_materialization(&identity.path) {
        return Ok(identity.clone());
    }
    let handle = &identity.handle;
    let provider_id = &identity.provider_id;
    let provider = config
        .worktree_providers
        .get(provider_id)
        .expect("resolved provider is configured");
    let command = provider.commands.ensure.as_ref().ok_or_else(|| {
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            format!("registered remote worktree `{handle}` requires materialization, but provider `{provider_id}` does not configure commands.ensure"),
            Some(handle.to_string()),
            Some(vec![format!("Configure worktree_providers.{provider_id}.commands.ensure with the provider-native command that materializes `{handle}`.")]),
        );
        error.details["worktree_provider_materialization"] = Value::String("unavailable".to_string());
        error.details["worktree_provider_id"] = Value::String(provider_id.to_string());
        error
    })?;
    let command = command
        .iter()
        .map(|argument| argument.replace("{handle}", handle))
        .collect::<Vec<_>>();
    let remediation = render_provider_command(&command);
    run_provider_ensure_command(provider_id, provider, &command)?;
    let identity = resolve_apply_enabled_worktree_provider_identity_by_id_from_config(
        handle,
        provider_id,
        config,
    )?;
    if worktree_provider_path_requires_materialization(&identity.path) {
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            format!("provider `{provider_id}` reported registered remote worktree `{handle}` after its materialization command completed"),
            Some(handle.to_string()),
            Some(vec![format!("Run the provider-native materialization command: {remediation}")]),
        );
        error.details["worktree_provider_materialization"] =
            Value::String("incomplete".to_string());
        error.details["worktree_provider_id"] = Value::String(provider_id.to_string());
        error.details["worktree_provider_remediation"] = Value::String(remediation);
        return Err(error);
    }
    Ok(identity)
}

/// Resolve a workspace through one previously selected apply-enabled provider.
/// Durable callers use this when the provider identity is part of their recipe.
pub fn resolve_apply_enabled_worktree_provider_by_id_from_config(
    handle: &str,
    provider_id: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderResolution> {
    let resolution = resolve_apply_enabled_worktree_provider_by_id_unchecked_from_config(
        handle,
        provider_id,
        config,
    )?;
    validate_provider_handle(provider_id, &resolution.worktree, None, None)?;
    Ok(resolution)
}

/// Resolve a workspace through one persisted provider without admitting its
/// safety state. Exact-identity and safety attestation callers need this
/// observation boundary so Cook can attribute an owned promoted candidate
/// before the normal dirty-worktree policy decides admission.
fn resolve_apply_enabled_worktree_provider_by_id_unchecked_from_config(
    handle: &str,
    provider_id: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderResolution> {
    let provider = config.worktree_providers.get(provider_id).ok_or_else(|| {
        Error::validation_invalid_argument(
            "worktree_provider",
            "timed-out worktree provider is no longer configured",
            Some(provider_id.to_string()),
            None,
        )
    })?;
    if !provider.enabled || !provider.apply_enabled {
        return Err(Error::validation_invalid_argument(
            "worktree_provider",
            "timed-out worktree provider is no longer enabled and apply-enabled",
            Some(provider_id.to_string()),
            None,
        ));
    }
    let worktrees = if let Some(command) = provider.commands.resolve.as_ref() {
        run_provider_resolve_command(provider_id, provider, command, handle)?
    } else if let Some(command) = provider.commands.list.as_ref() {
        run_provider_list_command(provider_id, provider, command)?
    } else {
        return Err(Error::validation_invalid_argument(
            "worktree_provider",
            "timed-out worktree provider has no resolve or list command",
            Some(provider_id.to_string()),
            None,
        ));
    };
    let worktree = worktrees.into_iter().find(|item| item.handle == handle).ok_or_else(|| {
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            format!("worktree handle `{handle}` was not returned by timed-out provider `{provider_id}`"),
            Some(handle.to_string()),
            None,
        );
        error.details["worktree_provider_lookup"] = Value::String("not_found".to_string());
        let command = provider
            .commands
            .resolve
            .as_ref()
            .or(provider.commands.list.as_ref())
            .expect("provider resolve or list command was selected");
        let operation = if provider.commands.resolve.is_some() {
            "resolve"
        } else {
            "list"
        };
        annotate_provider_lookup_error(&mut error, provider_id, command, operation, "not_found");
        error
    })?;
    Ok(WorktreeProviderResolution {
        provider_id: provider_id.to_string(),
        worktree,
    })
}

/// Resolve exact identity then obtain current safety evidence. Providers that
/// have not adopted the split commands continue through the established
/// combined resolver, which is deliberately retained as a compatibility adapter.
pub fn resolve_apply_enabled_worktree_provider_split_from_config(
    handle: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderSplitResolution> {
    let identity = resolve_apply_enabled_worktree_provider_identity_from_config(handle, config)?;
    let safety = attest_apply_enabled_worktree_provider_safety_from_config(&identity, config)?;
    if !safety.fresh || safety.dirty || safety.unpushed || identity.primary {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "worktree provider safety attestation is not safe for mutation",
            Some(handle.to_string()),
            None,
        ));
    }
    Ok(WorktreeProviderSplitResolution { identity, safety })
}

/// Resolve only an exact provider workspace identity. This operation performs
/// no cleanliness or freshness assertion and is safe to persist before safety
/// evidence becomes available.
pub fn resolve_apply_enabled_worktree_provider_identity_from_config(
    handle: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderExactIdentity> {
    let mut provider_ids = config
        .worktree_providers
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    provider_ids.sort();
    for provider_id in provider_ids {
        let provider = &config.worktree_providers[&provider_id];
        if !provider.enabled || !provider.apply_enabled {
            continue;
        }
        match (&provider.commands.resolve_identity, &provider.commands.attest_safety) {
            (Some(command), Some(_)) => {
                if let Some(identity) = run_provider_identity_command(&provider_id, provider, command, handle)? {
                    return Ok(identity);
                }
            }
            (None, None) => continue,
            _ => return Err(Error::validation_invalid_argument("worktree_providers.commands", format!("worktree provider `{provider_id}` must configure both resolve_identity and attest_safety"), Some(provider_id), None)),
        }
    }
    let started = std::time::Instant::now();
    let resolution = resolve_apply_enabled_worktree_provider_from_config(handle, config, None)?;
    let elapsed_ms = started.elapsed().as_millis();
    let token = compatibility_identity_token(&resolution);
    Ok(WorktreeProviderExactIdentity {
        schema: "homeboy/worktree-provider-identity/v1".to_string(),
        provider_id: resolution.provider_id.clone(),
        token: token.clone(),
        handle: resolution.worktree.handle.clone(),
        path: resolution.worktree.path.clone(),
        branch: resolution.worktree.branch.clone(),
        primary: resolution.worktree.safety.primary,
        latency_ms: elapsed_ms,
        budget_ms: config.worktree_providers[&resolution.provider_id].lookup_timeout_ms as u128,
    })
}

/// Re-resolve an exact identity only through the provider persisted by Cook.
/// This prevents provider ordering or unrelated configuration changes from
/// changing a durable lookup's authority during continuation.
pub fn resolve_apply_enabled_worktree_provider_identity_by_id_from_config(
    handle: &str,
    provider_id: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderExactIdentity> {
    let provider = config.worktree_providers.get(provider_id).ok_or_else(|| {
        Error::validation_invalid_argument(
            "worktree_provider",
            "persisted worktree provider is no longer configured",
            Some(provider_id.to_string()),
            None,
        )
    })?;
    if !provider.enabled || !provider.apply_enabled {
        return Err(Error::validation_invalid_argument(
            "worktree_provider",
            "persisted worktree provider is no longer enabled and apply-enabled",
            Some(provider_id.to_string()),
            None,
        ));
    }
    if let (Some(command), Some(_)) = (
        &provider.commands.resolve_identity,
        &provider.commands.attest_safety,
    ) {
        let identity = run_provider_identity_command(provider_id, provider, command, handle)?
            .ok_or_else(|| {
                let mut error = Error::validation_invalid_argument(
                    "to_worktree",
                    format!("worktree handle `{handle}` was not returned by persisted provider `{provider_id}`"),
                    Some(handle.to_string()),
                    None,
                );
                error.details["worktree_provider_lookup"] = Value::String("not_found".to_string());
                error.details["worktree_provider_id"] = Value::String(provider_id.to_string());
                error
            })?;
        if identity.handle != handle {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                "worktree provider identity did not resolve the declared exact handle",
                Some(handle.to_string()),
                None,
            ));
        }
        return Ok(identity);
    }
    if provider.commands.resolve_identity.is_some() || provider.commands.attest_safety.is_some() {
        return Err(Error::validation_invalid_argument("worktree_providers.commands", format!("worktree provider `{provider_id}` must configure both resolve_identity and attest_safety"), Some(provider_id.to_string()), None));
    }
    let started = std::time::Instant::now();
    let resolution = resolve_apply_enabled_worktree_provider_by_id_unchecked_from_config(
        handle,
        provider_id,
        config,
    )?;
    let elapsed_ms = started.elapsed().as_millis();
    let token = compatibility_identity_token(&resolution);
    Ok(WorktreeProviderExactIdentity {
        schema: "homeboy/worktree-provider-identity/v1".to_string(),
        provider_id: resolution.provider_id.clone(),
        token,
        handle: resolution.worktree.handle.clone(),
        path: resolution.worktree.path.clone(),
        branch: resolution.worktree.branch.clone(),
        primary: resolution.worktree.safety.primary,
        latency_ms: elapsed_ms,
        budget_ms: provider.lookup_timeout_ms as u128,
    })
}

/// Attest safety for a previously persisted exact identity. A versioned
/// provider receives only its opaque token; the compatibility adapter rechecks
/// the same exact handle and rejects any identity drift.
pub fn attest_apply_enabled_worktree_provider_safety_from_config(
    identity: &WorktreeProviderExactIdentity,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderSafetyAttestation> {
    let provider = config
        .worktree_providers
        .get(&identity.provider_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "worktree_provider",
                "identity provider is no longer configured",
                Some(identity.provider_id.clone()),
                None,
            )
        })?;
    if !provider.enabled || !provider.apply_enabled {
        return Err(Error::validation_invalid_argument(
            "worktree_provider",
            "identity provider is no longer apply-enabled",
            Some(identity.provider_id.clone()),
            None,
        ));
    }
    if let Some(command) = &provider.commands.attest_safety {
        let safety =
            run_provider_safety_command(&identity.provider_id, provider, command, &identity.token)?;
        if safety.identity_token != identity.token {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                "worktree provider safety evidence is bound to a different exact identity",
                Some(identity.handle.clone()),
                None,
            ));
        }
        return Ok(safety);
    }
    let started = std::time::Instant::now();
    let resolution = resolve_apply_enabled_worktree_provider_by_id_unchecked_from_config(
        &identity.handle,
        &identity.provider_id,
        config,
    )?;
    if resolution.provider_id != identity.provider_id
        || compatibility_identity_token(&resolution) != identity.token
    {
        return Err(Error::validation_invalid_argument("to_worktree", "combined worktree provider safety evidence no longer matches the persisted exact identity", Some(identity.handle.clone()), None));
    }
    Ok(WorktreeProviderSafetyAttestation {
        schema: "homeboy/worktree-provider-safety/v1".to_string(),
        identity_token: identity.token.clone(),
        observed_at: chrono::Utc::now().to_rfc3339(),
        dirty: resolution.worktree.safety.dirty,
        unpushed: resolution.worktree.safety.unpushed,
        fresh: true,
        latency_ms: started.elapsed().as_millis(),
        budget_ms: provider.lookup_timeout_ms as u128,
    })
}

fn compatibility_identity_token(resolution: &WorktreeProviderResolution) -> String {
    let mut digest = Sha256::new();
    digest.update(resolution.provider_id.as_bytes());
    digest.update([0]);
    digest.update(resolution.worktree.handle.as_bytes());
    digest.update([0]);
    digest.update(resolution.worktree.path.as_bytes());
    digest.update([0]);
    digest.update(resolution.worktree.branch.as_bytes());
    format!("compat-v1:{:x}", digest.finalize())
}
/// Find the sole apply-enabled provider worktree owned by a tracker URL.
/// Providers must map `task_url` to participate, preserving existing provider
/// configurations that only support handle-based lookup.
pub fn find_apply_enabled_worktree_provider_by_task_url_from_config(
    task_url: &str,
    config: &HomeboyConfig,
) -> Result<Option<WorktreeProviderResolution>> {
    let mut matches = Vec::new();
    for (provider_id, provider) in &config.worktree_providers {
        if !provider.enabled
            || !provider.apply_enabled
            || provider
                .list_result_mapping
                .as_ref()
                .and_then(|mapping| mapping.task_url.as_ref())
                .is_none()
        {
            continue;
        }
        let Some(command) = provider.commands.list.as_ref() else {
            continue;
        };
        for worktree in run_provider_list_command(provider_id, provider, command)? {
            if worktree.task_url.as_deref() == Some(task_url) {
                validate_provider_handle(provider_id, &worktree, None, None)?;
                matches.push(WorktreeProviderResolution {
                    provider_id: provider_id.clone(),
                    worktree,
                });
            }
        }
    }
    matches.sort_by(|left, right| left.worktree.handle.cmp(&right.worktree.handle));
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => Err(Error::validation_invalid_argument(
            "task_url",
            format!(
                "multiple active apply-enabled worktrees are owned by `{task_url}`: {}",
                matches
                    .iter()
                    .map(|resolution| resolution.worktree.handle.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Some(task_url.to_string()),
            None,
        )),
    }
}

/// A clean immutable candidate may be its own destination before Homeboy's
/// finalizer pushes it. The exception remains bound to this exact checkout and
/// commit; every other destination safety requirement still applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedUnpushedWorktree {
    pub path: std::path::PathBuf,
    pub head: String,
}

pub fn resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
    handle: &str,
    config: &HomeboyConfig,
    gate_feedback_baseline: Option<&serde_json::Value>,
    trusted_unpushed_destination: Option<&TrustedUnpushedWorktree>,
) -> Result<WorktreeProviderResolution> {
    resolve_worktree_provider_with_policy_from_config(
        handle,
        config,
        true,
        gate_feedback_baseline,
        trusted_unpushed_destination,
    )
}

/// Resolve a managed destination, creating it through an apply-enabled command
/// provider only when all branch and task intent is explicit.
pub fn provision_apply_enabled_worktree_provider_from_config(
    intent: &WorktreeProviderCreateIntent,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderProvision> {
    provision_apply_enabled_worktree_provider_from_config_with_lifecycle(intent, None, config)
}

/// Determine the provider that will own an ensure before the external command
/// runs. Callers that persist lifecycle ownership use this to close the
/// pre-ensure crash window.
pub fn select_apply_enabled_worktree_provider_from_config(
    intent: &WorktreeProviderCreateIntent,
    config: &HomeboyConfig,
) -> Result<String> {
    match resolve_apply_enabled_worktree_provider_from_config(&intent.handle, config, None) {
        Ok(resolution) => {
            validate_workspace_creation_provider(&resolution.provider_id, config)?;
            return Ok(resolution.provider_id);
        }
        Err(error)
            if error
                .details
                .get("worktree_provider_lookup")
                .and_then(Value::as_str)
                == Some("not_found") => {}
        Err(error) => return Err(error),
    }
    let mut providers = Vec::new();
    let mut capability_errors = Vec::new();
    for (id, provider) in &config.worktree_providers {
        if provider.enabled && provider.apply_enabled {
            match validate_workspace_creation_provider(id, config) {
                Ok(()) => providers.push(id.clone()),
                Err(error) => capability_errors.push(error),
            }
        }
    }
    providers.sort();
    match providers.as_slice() {
        [provider] => Ok(provider.clone()),
        [] if capability_errors.len() == 1 => Err(capability_errors.pop().expect("one capability error")),
        [] => Err(Error::validation_invalid_argument("to_worktree", format!("worktree handle `{}` is missing and no enabled apply-enabled provider configures commands.ensure, so Homeboy cannot create it", intent.handle), Some(intent.handle.clone()), Some(missing_ensure_provider_remediation(intent)))),
        _ => Err(Error::validation_invalid_argument("to_worktree", format!("worktree handle `{}` is missing and multiple providers can ensure it: {}", intent.handle, providers.join(", ")), Some(intent.handle.clone()), Some(ambiguous_ensure_provider_remediation(&providers.iter().map(String::as_str).collect::<Vec<_>>())))),
    }
}

/// Resolve an existing provider workspace or ask its configured read-only
/// `plan` command to project the exact workspace that `ensure` would create.
/// A provider without that command is intentionally not guessed from native
/// worktree naming, because provider path policy is provider-owned.
pub fn plan_apply_enabled_worktree_provider_from_config(
    intent: &WorktreeProviderCreateIntent,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderCreatePlan> {
    plan_apply_enabled_worktree_provider_from_config_with_id(intent, None, config)
}

/// Plan a purpose-owned workspace with the same creation-capable provider
/// selection that execution uses, without running its ensure command.
pub fn plan_apply_enabled_worktree_provider_with_lifecycle_from_config(
    intent: &WorktreeProviderCreateIntent,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderCreatePlan> {
    let provider_id = select_apply_enabled_worktree_provider_from_config(intent, config)?;
    plan_apply_enabled_worktree_provider_from_config_with_id(intent, Some(provider_id), config)
}

fn plan_apply_enabled_worktree_provider_from_config_with_id(
    intent: &WorktreeProviderCreateIntent,
    selected_provider_id: Option<String>,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderCreatePlan> {
    match resolve_apply_enabled_worktree_provider_from_config(&intent.handle, config, None) {
        Ok(resolution) => {
            if selected_provider_id.as_deref() != Some(resolution.provider_id.as_str())
                && selected_provider_id.is_some()
            {
                return Err(Error::validation_invalid_argument(
                    "worktree_provider",
                    "provider selection changed before lifecycle workspace planning",
                    Some(resolution.provider_id.clone()),
                    None,
                ));
            }
            return Ok(WorktreeProviderCreatePlan::Existing(resolution));
        }
        Err(error)
            if error
                .details
                .get("worktree_provider_lookup")
                .and_then(Value::as_str)
                == Some("not_found") => {}
        Err(error) => return Err(error),
    }
    let provider_id = match selected_provider_id {
        Some(provider_id) => provider_id,
        None => {
            let mut provider_ids = config
                .worktree_providers
                .iter()
                .filter_map(|(id, provider)| {
                    (provider.enabled
                        && provider.apply_enabled
                        && provider.commands.ensure.is_some())
                    .then_some(id.clone())
                })
                .collect::<Vec<_>>();
            provider_ids.sort();
            match provider_ids.as_slice() {
                [provider_id] => provider_id.clone(),
                [] => return Err(Error::validation_invalid_argument(
                    "to_worktree",
                    format!("worktree handle `{}` is missing and no enabled apply-enabled provider configures commands.ensure", intent.handle),
                    Some(intent.handle.clone()),
                    None,
                )),
                _ => return Err(Error::validation_invalid_argument(
                    "to_worktree",
                    format!("worktree handle `{}` is missing and multiple providers can ensure it: {}", intent.handle, provider_ids.join(", ")),
                    Some(intent.handle.clone()),
                    None,
                )),
            }
        }
    };
    let provider = config
        .worktree_providers
        .get(&provider_id)
        .expect("selected provider is configured");
    let Some(command) = provider.commands.plan.as_ref() else {
        let mut error = Error::validation_invalid_argument(
            "worktree_providers.commands.plan",
            format!(
                "worktree provider `{provider_id}` can ensure `{}` but cannot non-mutatingly plan its destination",
                intent.handle
            ),
            Some(provider_id.clone()),
            Some(vec![format!(
                "Configure worktree_providers.{provider_id}.commands.plan with the same intent placeholders as commands.ensure."
            )]),
        );
        error.details["worktree_provider_planning"] = Value::String("unsupported".to_string());
        error.details["worktree_provider_id"] = Value::String(provider_id);
        error.details["handle"] = Value::String(intent.handle.clone());
        return Err(error);
    };
    let command = expand_ensure_command(command, intent, &provision_idempotency_key(intent));
    let worktrees = run_provider_lookup_command(
        &provider_id,
        provider,
        &command,
        "plan",
        &provider.commands.resolve_not_found_exit_codes,
    )?;
    let worktree = worktrees
        .into_iter()
        .find(|worktree| worktree.handle == intent.handle)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "worktree_providers.commands.plan",
                format!(
                    "worktree provider `{provider_id}` plan did not return requested handle `{}`",
                    intent.handle
                ),
                Some(provider_id.clone()),
                None,
            )
        })?;
    if worktree.path.trim().is_empty() || worktree.branch != intent.head || worktree.safety.primary
    {
        return Err(Error::validation_invalid_argument(
            "worktree_providers.commands.plan",
            format!(
                "worktree provider `{provider_id}` returned an incomplete or unsafe planned destination for `{}`",
                intent.handle
            ),
            Some(provider_id),
            None,
        ));
    }
    Ok(WorktreeProviderCreatePlan::WouldCreate(
        WorktreeProviderResolution {
            provider_id,
            worktree,
        },
    ))
}

/// Auto-creation is not a default capability: it needs an operator-configured
/// provider. Naming the config shape does not tell a caller what to run, so the
/// remediation leads with the command that creates the destination now and
/// follows with the command that enables auto-creation for later runs.
fn missing_ensure_provider_remediation(intent: &WorktreeProviderCreateIntent) -> Vec<String> {
    let mut actions = vec![
        format!(
            "Create it now with: homeboy worktree create {} --branch {} --from {} --task-url {}",
            intent.repo, intent.head, intent.base, intent.task_url
        ),
        format!(
            "Then rerun with: --to-worktree {}",
            crate::worktree::handle_for_branch(&intent.repo, &intent.head)
        ),
        "Or enable auto-creation for later runs with: homeboy config set \
         /worktree_providers/<provider-id> \
         '{\"enabled\":true,\"apply_enabled\":true,\"commands\":{\"ensure\":[\"<executable>\",\"<argument>\"]}}'"
            .to_string(),
    ];
    if let Some(expected) = normalized_branch_handle(intent) {
        actions.insert(
            0,
            format!(
                "Handles slugify the branch, so `--head {}` names `{expected}`, not `{}`",
                intent.head, intent.handle
            ),
        );
    }
    actions
}

/// The handle `--head` actually slugifies to, when the caller passed a different
/// one. A caller who guesses the slug wrong otherwise reads a "missing handle"
/// error about a handle that was never going to exist.
fn normalized_branch_handle(intent: &WorktreeProviderCreateIntent) -> Option<String> {
    let expected = crate::worktree::handle_for_branch(&intent.repo, &intent.head);
    (expected != intent.handle).then_some(expected)
}

/// Homeboy will not pick between equally eligible providers, so the remediation
/// is the command that removes the ambiguity rather than a restatement of it.
fn ambiguous_ensure_provider_remediation(providers: &[&str]) -> Vec<String> {
    providers
        .iter()
        .map(|id| {
            format!("Disable all but one with: homeboy config set /worktree_providers/{id}/enabled false")
        })
        .collect()
}

/// Creation needs a provider mutation and a provider-backed postcondition
/// lookup. Terminal finalization is optional: providers that do not expose it
/// remain authoritative for their ensure/resolve lifecycle.
fn validate_workspace_creation_provider(provider_id: &str, config: &HomeboyConfig) -> Result<()> {
    let provider = config.worktree_providers.get(provider_id).ok_or_else(|| {
        Error::validation_invalid_argument(
            "worktree_provider",
            "selected provider is no longer configured",
            Some(provider_id.to_string()),
            None,
        )
    })?;
    let mut selected_capabilities = Vec::new();
    if provider.enabled {
        selected_capabilities.push("enabled");
    }
    if provider.apply_enabled {
        selected_capabilities.push("apply_enabled");
    }
    if provider.commands.ensure.is_some() {
        selected_capabilities.push("ensure");
    }
    if provider.commands.resolve.is_some() {
        selected_capabilities.push("resolve");
    }
    if provider.commands.list.is_some() {
        selected_capabilities.push("list");
    }
    if worktree_provider_lifecycle_finalizer_argv_from_config(provider_id, config)?.is_some() {
        selected_capabilities.push("finalize");
    }
    let mut missing_required_capabilities = Vec::new();
    if !provider.enabled {
        missing_required_capabilities.push("enabled");
    }
    if !provider.apply_enabled {
        missing_required_capabilities.push("apply_enabled");
    }
    if provider.commands.ensure.is_none() {
        missing_required_capabilities.push("ensure");
    }
    if provider.commands.resolve.is_none() && provider.commands.list.is_none() {
        missing_required_capabilities.push("resolve_or_list");
    }
    if missing_required_capabilities.is_empty() {
        return Ok(());
    }
    let mut error = Error::validation_invalid_argument(
        "worktree_provider",
        format!(
            "worktree provider `{provider_id}` cannot create and resolve a fanout workspace; missing required capabilities: {}",
            missing_required_capabilities.join(", ")
        ),
        Some(provider_id.to_string()),
        None,
    );
    error.details["worktree_provider_id"] = Value::String(provider_id.to_string());
    error.details["worktree_provider_selected_capabilities"] =
        serde_json::json!(selected_capabilities);
    error.details["worktree_provider_missing_required_capabilities"] =
        serde_json::json!(missing_required_capabilities);
    Err(error)
}

fn provision_apply_enabled_worktree_provider_from_config_with_lifecycle(
    intent: &WorktreeProviderCreateIntent,
    lifecycle: Option<&WorktreeProviderLifecycleIntent>,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderProvision> {
    // Purpose-owned callers select once using the creation capability contract.
    // Keep that exact identity through ensure and postcondition lookup.
    let lifecycle_provider = lifecycle
        .map(|_| select_apply_enabled_worktree_provider_from_config(intent, config))
        .transpose()?;
    match resolve_apply_enabled_worktree_provider_from_config(&intent.handle, config, None) {
        Ok(resolution) => {
            if let Some(lifecycle) = lifecycle {
                if lifecycle_provider.as_deref() != Some(&resolution.provider_id) {
                    return Err(Error::validation_invalid_argument(
                        "worktree_provider",
                        "provider selection changed before lifecycle workspace adoption",
                        Some(resolution.provider_id.clone()),
                        None,
                    ));
                }
                let provider = config
                    .worktree_providers
                    .get(&resolution.provider_id)
                    .expect("resolved provider is configured");
                let command = provider.commands.ensure.as_ref().ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "worktree_providers.commands.ensure",
                        "purpose-owned workspace requires an ensure command",
                        Some(resolution.provider_id.clone()),
                        None,
                    )
                })?;
                run_provider_ensure_command(
                    &resolution.provider_id,
                    provider,
                    &expand_lifecycle_ensure_command(
                        command,
                        intent,
                        lifecycle,
                        &provision_idempotency_key(intent),
                    ),
                )?;
            }
            return Ok(WorktreeProviderProvision {
                resolution,
                action: "adopted",
                idempotency_key: provision_idempotency_key(intent),
            });
        }
        Err(error)
            if error
                .details
                .get("worktree_provider_lookup")
                .and_then(Value::as_str)
                == Some("not_found") => {}
        Err(error) => return Err(error),
    }

    let mut providers = match lifecycle_provider.as_deref() {
        Some(provider_id) => vec![(
            provider_id,
            config
                .worktree_providers
                .get(provider_id)
                .expect("selected provider is configured"),
        )],
        None => config
            .worktree_providers
            .iter()
            .filter_map(|(id, provider)| {
                provider
                    .enabled
                    .then_some((id.as_str(), provider))
                    .filter(|(_, provider)| provider.commands.ensure.is_some())
            })
            .collect::<Vec<_>>(),
    };
    providers.sort_by_key(|(id, _)| *id);
    if providers.len() > 1 {
        let ids = providers.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree handle `{}` is missing and multiple providers can ensure it: {}",
                intent.handle,
                ids.join(", ")
            ),
            Some(intent.handle.clone()),
            Some(ambiguous_ensure_provider_remediation(&ids)),
        ));
    }
    let Some((provider_id, provider)) = providers.first().copied() else {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree handle `{}` is missing and no enabled worktree provider configures commands.ensure, so Homeboy cannot create it",
                intent.handle
            ),
            Some(intent.handle.clone()),
            Some(missing_ensure_provider_remediation(intent)),
        ));
    };
    let idempotency_key = provision_idempotency_key(intent);
    let command = if let Some(lifecycle) = lifecycle {
        expand_lifecycle_ensure_command(
            provider
                .commands
                .ensure
                .as_ref()
                .expect("filtered ensure command"),
            intent,
            lifecycle,
            &idempotency_key,
        )
    } else {
        expand_ensure_command(
            provider
                .commands
                .ensure
                .as_ref()
                .expect("filtered ensure command"),
            intent,
            &idempotency_key,
        )
    };
    let rendered_command = render_provider_command(&command);
    if !provider.apply_enabled {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree handle `{}` is missing and provider `{provider_id}` ensure is disabled",
                intent.handle
            ),
            Some(intent.handle.clone()),
            Some(vec![format!("Create it with: {rendered_command}")]),
        ));
    }
    run_provider_ensure_command(provider_id, provider, &command)?;
    let resolution =
        resolve_apply_enabled_worktree_provider_from_config(&intent.handle, config, None)
            .map_err(mark_bootstrap_postcondition_failure)?;
    if lifecycle_provider.as_deref() != Some(resolution.provider_id.as_str())
        && lifecycle_provider.is_some()
    {
        return Err(Error::validation_invalid_argument(
            "worktree_provider",
            "provider postcondition resolved a different provider than the lifecycle owner",
            Some(resolution.provider_id.clone()),
            None,
        ));
    }
    Ok(WorktreeProviderProvision {
        resolution,
        action: "ensured",
        idempotency_key,
    })
}

/// Provision a workspace with generic purpose/owner lifecycle intent. The
/// existing provision contract remains available for callers that do not own a
/// terminal lifecycle.
pub fn provision_apply_enabled_worktree_provider_with_lifecycle_from_config(
    intent: &WorktreeProviderCreateIntent,
    lifecycle: &WorktreeProviderLifecycleIntent,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderProvision> {
    provision_apply_enabled_worktree_provider_from_config_with_lifecycle(
        intent,
        Some(lifecycle),
        config,
    )
}

/// Finalize a purpose-owned workspace once its owner has reached a terminal
/// disposition. The command is argv-only and receives no shell-expanded input.
pub fn finalize_apply_enabled_worktree_provider_from_config(
    resolution: &WorktreeProviderResolution,
    lifecycle: &WorktreeProviderLifecycleIntent,
    disposition: WorktreeProviderTerminalDisposition,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderFinalization> {
    let provider = config
        .worktree_providers
        .get(&resolution.provider_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "worktree_provider",
                "resolved provider is no longer configured",
                Some(resolution.provider_id.clone()),
                None,
            )
        })?;
    if !provider.enabled || !provider.apply_enabled {
        return Err(Error::validation_invalid_argument(
            "worktree_provider",
            "purpose-owned workspace provider finalization is not apply-enabled",
            Some(resolution.provider_id.clone()),
            None,
        ));
    }
    let command =
        worktree_provider_lifecycle_finalizer_argv_from_config(&resolution.provider_id, config)?
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "settings.worktree_provider_lifecycle",
                    "purpose-owned workspace provider must configure a lifecycle finalize command",
                    Some(resolution.provider_id.clone()),
                    None,
                )
            })?;
    let finalization_idempotency_key = worktree_provider_finalization_idempotency_key(lifecycle);
    let command = command
        .iter()
        .map(|argument| {
            argument
                .replace("{handle}", &resolution.worktree.handle)
                .replace("{purpose}", &lifecycle.purpose)
                .replace("{owner_run_ref}", &lifecycle.owner_run_ref)
                .replace("{idempotency_key}", &finalization_idempotency_key)
                .replace("{cleanup_policy}", lifecycle.cleanup_policy.as_str())
                .replace("{disposition}", disposition.as_str())
                .replace("{owner_outcome}", disposition.owner_outcome())
                .replace("{lifecycle_state}", disposition.lifecycle_state())
        })
        .collect::<Vec<_>>();
    run_provider_mutation_command(&resolution.provider_id, provider, &command, "finalize")?;
    Ok(WorktreeProviderFinalization {
        provider_id: resolution.provider_id.clone(),
        handle: resolution.worktree.handle.clone(),
        disposition,
        owner_outcome: disposition.owner_outcome().to_string(),
        lifecycle_state: disposition.lifecycle_state().to_string(),
        inspection_path: resolution.worktree.path.clone(),
    })
}

pub fn worktree_provider_idempotency_key(intent: &WorktreeProviderCreateIntent) -> String {
    format!(
        "{}:{}:{}:{}",
        intent.handle, intent.repo, intent.base, intent.head
    )
}

/// Render the configured workspace owner's ensure command for an explicit
/// lifecycle request. Callers can expose this as a repair action without
/// assuming that Homeboy owns the workspace implementation.
pub fn worktree_provider_lifecycle_ensure_argv_from_config(
    intent: &WorktreeProviderCreateIntent,
    lifecycle: &WorktreeProviderLifecycleIntent,
    config: &HomeboyConfig,
) -> Result<Vec<String>> {
    let provider_id = select_apply_enabled_worktree_provider_from_config(intent, config)?;
    let provider = config
        .worktree_providers
        .get(&provider_id)
        .expect("selected provider is configured");
    let command = provider
        .commands
        .ensure
        .as_ref()
        .expect("selected lifecycle provider configures ensure");
    Ok(expand_lifecycle_ensure_command(
        command,
        intent,
        lifecycle,
        &provision_idempotency_key(intent),
    ))
}

/// Stable finalization key. A stale lease may cause multiple invocations, so
/// providers must use this key to deduplicate their logical terminal effect.
pub fn worktree_provider_finalization_idempotency_key(
    lifecycle: &WorktreeProviderLifecycleIntent,
) -> String {
    format!("finalize:{}", lifecycle.owner_run_ref)
}

fn provision_idempotency_key(intent: &WorktreeProviderCreateIntent) -> String {
    worktree_provider_idempotency_key(intent)
}

fn expand_ensure_command(
    command: &[String],
    intent: &WorktreeProviderCreateIntent,
    idempotency_key: &str,
) -> Vec<String> {
    command
        .iter()
        .map(|argument| {
            argument
                .replace("{handle}", &intent.handle)
                .replace("{repo}", &intent.repo)
                .replace("{base}", &intent.base)
                .replace("{head}", &intent.head)
                .replace("{task_url}", &intent.task_url)
                .replace("{idempotency_key}", idempotency_key)
        })
        .collect()
}

fn expand_lifecycle_ensure_command(
    command: &[String],
    intent: &WorktreeProviderCreateIntent,
    lifecycle: &WorktreeProviderLifecycleIntent,
    idempotency_key: &str,
) -> Vec<String> {
    expand_ensure_command(command, intent, idempotency_key)
        .into_iter()
        .map(|argument| {
            argument
                .replace("{purpose}", &lifecycle.purpose)
                .replace("{owner_run_ref}", &lifecycle.owner_run_ref)
                .replace("{cleanup_policy}", lifecycle.cleanup_policy.as_str())
        })
        .collect()
}

fn render_provider_command(command: &[String]) -> String {
    command
        .iter()
        .map(|argument| format!("'{}'", argument.replace('\'', "'\\\"'\\\"'")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn run_provider_ensure_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
) -> Result<()> {
    run_provider_mutation_command(provider_id, provider, command, "ensure").map(|_| ())
}

fn run_provider_mutation_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
    operation: &str,
) -> Result<Vec<u8>> {
    if let Some(argument) = command.iter().find(|argument| {
        argument.split('{').skip(1).any(|tail| {
            tail.chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
                && tail.contains('}')
        })
    }) {
        return Err(provider_lookup_error(
            provider_id,
            command,
            operation,
            "command",
            "worktree_providers.commands",
            format!(
                "worktree provider `{provider_id}` command contains an unresolved placeholder: {argument}"
            ),
            false,
        ));
    }
    let timeout = provider_mutation_timeout(provider)?;
    let output_limit = provider_lookup_output_limit(provider)?;
    let supervised = run_bounded_provider_lookup_command(
        provider_id,
        command,
        operation,
        timeout,
        output_limit,
    )?;
    let elapsed_ms = supervised.elapsed_ms;
    let output = supervised.output;
    if output.termination != crate::engine::command::SupervisedCommandTermination::Completed {
        let classification = match output.termination {
            crate::engine::command::SupervisedCommandTermination::Cancelled => "cancelled",
            crate::engine::command::SupervisedCommandTermination::TimedOut
            | crate::engine::command::SupervisedCommandTermination::NoProgress => "timeout",
            crate::engine::command::SupervisedCommandTermination::Completed => unreachable!(),
        };
        return Err(provider_lookup_error(
            provider_id,
            command,
                operation,
            classification,
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` {operation} command {classification} after {elapsed_ms} ms (configured provider budget: {} ms)",
                timeout.as_millis()
            ),
            classification == "timeout" || classification == "cancelled",
        ));
    }
    let output = output.output;
    if output.capture.stdout.truncated || output.capture.stderr.truncated {
        return Err(provider_lookup_error(
            provider_id,
            command,
            operation,
            "malformed",
            "to_worktree",
            format!("worktree provider `{provider_id}` {operation} command output exceeded configured lookup_output_limit_bytes"),
            false,
        ));
    }
    let output = output.into_output();
    if output.status.success() {
        return Ok(output.stdout);
    }
    let mut error = Error::validation_invalid_argument_with_evidence(
        "to_worktree",
        format!(
            "worktree provider `{provider_id}` {operation} command failed with {}",
            output
                .status
                .code()
                .map(|code| format!("exit code {code}"))
                .unwrap_or_else(|| "a signal".to_string())
        ),
        Some(provider_id.to_string()),
        None,
        Some(provider_command_evidence(command, &output)),
    );
    annotate_provider_lookup_error(&mut error, provider_id, command, operation, "command");
    Err(error)
}

fn resolve_worktree_provider_with_policy_from_config(
    handle: &str,
    config: &HomeboyConfig,
    require_apply_enabled: bool,
    gate_feedback_baseline: Option<&serde_json::Value>,
    trusted_unpushed_destination: Option<&TrustedUnpushedWorktree>,
) -> Result<WorktreeProviderResolution> {
    let mut provider_ids = config
        .worktree_providers
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    provider_ids.sort();
    let mut attempted = Vec::new();
    let mut not_apply_enabled = Vec::new();

    for provider_id in provider_ids.iter().cloned() {
        let provider = &config.worktree_providers[&provider_id];
        if !provider.enabled {
            continue;
        }
        if require_apply_enabled && !provider.apply_enabled {
            not_apply_enabled.push(provider_id);
            continue;
        }
        if let Some(command) = provider.commands.resolve.as_ref() {
            attempted.push(provider_id.clone());
            let worktrees = run_provider_resolve_command(&provider_id, provider, command, handle)?;
            if let Some(worktree) = worktrees.into_iter().find(|item| item.handle == handle) {
                validate_provider_handle(
                    &provider_id,
                    &worktree,
                    gate_feedback_baseline,
                    trusted_unpushed_destination,
                )?;
                return Ok(WorktreeProviderResolution {
                    provider_id,
                    worktree,
                });
            }
            continue;
        }
        let Some(command) = provider.commands.list.as_ref() else {
            continue;
        };
        attempted.push(provider_id.clone());
        let worktrees = run_provider_list_command(&provider_id, provider, command)?;
        if let Some(worktree) = worktrees.into_iter().find(|item| item.handle == handle) {
            validate_provider_handle(
                &provider_id,
                &worktree,
                gate_feedback_baseline,
                trusted_unpushed_destination,
            )?;
            return Ok(WorktreeProviderResolution {
                provider_id,
                worktree,
            });
        }
    }

    let configured = if provider_ids.is_empty() {
        "no worktree providers are configured".to_string()
    } else {
        format!(
            "configured provider(s): {}; checked provider(s): {}{}",
            provider_ids.join(", "),
            if attempted.is_empty() {
                "none with an enabled resolve or list command".to_string()
            } else {
                attempted.join(", ")
            },
            if not_apply_enabled.is_empty() {
                String::new()
            } else {
                format!(
                    "; not apply-enabled provider(s): {}",
                    not_apply_enabled.join(", ")
                )
            },
        )
    };
    let mut error = Error::validation_invalid_argument(
        "to_worktree",
        format!(
            "worktree handle `{handle}` is not a Homeboy task worktree and was not returned by a configured worktree provider ({configured})"
        ),
        Some(handle.to_string()),
        Some(vec![
            "Create the destination through its workspace provider, or use an existing Homeboy task worktree handle.".to_string(),
            if require_apply_enabled {
                "Configure an enabled, apply-enabled worktree provider commands.list command that returns typed worktree path, branch, and safety metadata.".to_string()
            } else {
                "Configure an enabled worktree provider commands.list command that returns typed worktree path, branch, and safety metadata.".to_string()
            },
        ]),
    );
    error.details["worktree_provider_lookup"] = Value::String("not_found".to_string());
    error.details["worktree_provider_call_classification"] = Value::String("not_found".to_string());
    error.details["worktree_provider_phase"] =
        Value::String("worktree_provider_lookup".to_string());
    Err(error)
}

fn run_provider_resolve_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
    handle: &str,
) -> Result<Vec<WorktreeProviderHandle>> {
    let command = command
        .iter()
        .map(|argument| argument.replace("{handle}", handle))
        .collect::<Vec<_>>();
    run_provider_lookup_command(
        provider_id,
        provider,
        &command,
        "resolve",
        &provider.commands.resolve_not_found_exit_codes,
    )
}

fn run_provider_identity_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
    handle: &str,
) -> Result<Option<WorktreeProviderExactIdentity>> {
    let command = command
        .iter()
        .map(|argument| argument.replace("{handle}", handle))
        .collect::<Vec<_>>();
    let (payload, latency_ms, budget_ms) =
        run_provider_split_command(provider_id, provider, &command, "resolve_identity")?;
    if split_identity_not_owned(&payload) {
        return Ok(None);
    }
    let identity: WorktreeProviderExactIdentity = serde_json::from_value(payload).map_err(|error| {
        provider_lookup_error(
            provider_id,
            &command,
            "resolve_identity",
            "malformed",
            "worktree_providers.commands.resolve_identity",
            format!("worktree provider `{provider_id}` returned an invalid identity envelope: {error}"),
            false,
        )
    })?;
    if identity.schema != "homeboy/worktree-provider-identity/v1"
        || identity.provider_id != provider_id
        || identity.token.trim().is_empty()
    {
        return Err(provider_lookup_error(
            provider_id,
            &command,
            "resolve_identity",
            "malformed",
            "worktree_providers.commands.resolve_identity",
            format!("worktree provider `{provider_id}` returned an unsupported or incomplete identity envelope"),
            false,
        ));
    }
    validate_split_identity(provider_id, handle, &identity).map_err(|mut error| {
        annotate_provider_lookup_error(
            &mut error,
            provider_id,
            &command,
            "resolve_identity",
            "malformed",
        );
        error
    })?;
    Ok(Some(WorktreeProviderExactIdentity {
        latency_ms,
        budget_ms,
        ..identity
    }))
}

/// Split identity output is provider input, not an authority bypass. Prove the
/// exact requested handle names the claimed canonical linked checkout before it
/// can be selected or persisted.
fn validate_split_identity(
    provider_id: &str,
    requested_handle: &str,
    identity: &WorktreeProviderExactIdentity,
) -> Result<()> {
    if identity.handle != requested_handle {
        return Err(Error::validation_invalid_argument(
            "worktree_providers.commands.resolve_identity",
            format!(
                "worktree provider `{provider_id}` resolved requested handle `{requested_handle}` as different handle `{}`",
                identity.handle
            ),
            Some(provider_id.to_string()),
            None,
        ));
    }
    if identity.branch.trim().is_empty() || identity.primary {
        return Err(Error::validation_invalid_argument(
            "worktree_providers.commands.resolve_identity",
            format!("worktree provider `{provider_id}` returned unsafe linked-worktree metadata for `{requested_handle}`"),
            Some(provider_id.to_string()),
            None,
        ));
    }
    let path = std::path::Path::new(&identity.path);
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        Error::validation_invalid_argument(
            "worktree_providers.commands.resolve_identity",
            format!("worktree provider `{provider_id}` returned an unresolvable checkout for `{requested_handle}`: {error}"),
            Some(provider_id.to_string()),
            None,
        )
    })?;
    if canonical != path {
        return Err(Error::validation_invalid_argument(
            "worktree_providers.commands.resolve_identity",
            format!("worktree provider `{provider_id}` returned a non-canonical checkout path for `{requested_handle}`"),
            Some(provider_id.to_string()),
            None,
        ));
    }
    validate_task_worktree_root(&canonical, requested_handle)?;
    if crate::git::current_branch(&canonical).as_deref() != Some(identity.branch.as_str()) {
        return Err(Error::validation_invalid_argument(
            "worktree_providers.commands.resolve_identity",
            format!("worktree provider `{provider_id}` branch metadata for `{requested_handle}` does not match the checkout branch"),
            Some(provider_id.to_string()),
            None,
        ));
    }
    Ok(())
}

/// Versioned exact resolvers may explicitly decline a handle. This is distinct
/// from malformed output: callers continue deterministic provider probing only
/// for these typed ownership outcomes.
fn split_identity_not_owned(payload: &Value) -> bool {
    matches!(
        payload.get("status").and_then(Value::as_str),
        Some("not_found" | "not_owned")
    ) || matches!(
        payload.get("ownership").and_then(Value::as_str),
        Some("not_owned")
    )
}

fn run_provider_safety_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
    token: &str,
) -> Result<WorktreeProviderSafetyAttestation> {
    let command = command
        .iter()
        .map(|argument| argument.replace("{identity}", token))
        .collect::<Vec<_>>();
    let (payload, latency_ms, budget_ms) =
        run_provider_split_command(provider_id, provider, &command, "attest_safety")?;
    let safety: WorktreeProviderSafetyAttestation = serde_json::from_value(payload).map_err(|error| {
        provider_lookup_error(
            provider_id,
            &command,
            "attest_safety",
            "malformed",
            "worktree_providers.commands.attest_safety",
            format!("worktree provider `{provider_id}` returned an invalid safety envelope: {error}"),
            false,
        )
    })?;
    if safety.schema != "homeboy/worktree-provider-safety/v1"
        || safety.identity_token.trim().is_empty()
        || safety.observed_at.trim().is_empty()
    {
        return Err(provider_lookup_error(
            provider_id,
            &command,
            "attest_safety",
            "malformed",
            "worktree_providers.commands.attest_safety",
            format!("worktree provider `{provider_id}` returned an unsupported or incomplete safety envelope"),
            false,
        ));
    }
    Ok(WorktreeProviderSafetyAttestation {
        latency_ms,
        budget_ms,
        ..safety
    })
}

fn run_provider_split_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
    operation: &str,
) -> Result<(Value, u128, u128)> {
    let timeout = provider_lookup_timeout(provider)?;
    let output_limit = provider_lookup_output_limit(provider)?;
    let supervised = run_bounded_provider_lookup_command(
        provider_id,
        command,
        operation,
        timeout,
        output_limit,
    )?;
    let elapsed_ms = supervised.elapsed_ms;
    let output = supervised.output;
    if output.termination != crate::engine::command::SupervisedCommandTermination::Completed {
        let cancelled =
            output.termination == crate::engine::command::SupervisedCommandTermination::Cancelled;
        let outcome = if cancelled { "cancelled" } else { "timed out" };
        let mut error = Error::validation_invalid_argument("to_worktree", format!("worktree provider `{provider_id}` {operation} command {outcome} after {elapsed_ms} ms (configured safety/identity budget: {} ms)", timeout.as_millis()), Some(provider_id.to_string()), None);
        error.retryable = Some(true);
        // Preserve the established deferred-Cook contract so split resolver
        // timeouts create the same durable lookup_pending state as legacy ones.
        error.details["worktree_provider_lookup"] =
            Value::String(if cancelled { "cancelled" } else { "timed_out" }.to_string());
        error.details["worktree_provider_id"] = Value::String(provider_id.to_string());
        error.details["worktree_provider_operation"] = Value::String(operation.to_string());
        error.details["worktree_provider_split_operation"] = Value::String(operation.to_string());
        error.details["worktree_provider_split"] =
            Value::String(if cancelled { "cancelled" } else { "timed_out" }.to_string());
        error.details["latency_ms"] = Value::from(elapsed_ms as u64);
        error.details["budget_ms"] = Value::from(timeout.as_millis() as u64);
        annotate_provider_lookup_error(
            &mut error,
            provider_id,
            command,
            operation,
            if cancelled { "cancelled" } else { "timeout" },
        );
        return Err(error);
    }
    let output = output.output;
    if output.capture.stdout.truncated {
        return Err(provider_lookup_error(
            provider_id,
            command,
            operation,
            "malformed",
            "to_worktree",
            format!("worktree provider `{provider_id}` {operation} command output exceeded configured lookup_output_limit_bytes"),
            false,
        ));
    }
    let output = output.into_output();
    if !output.status.success() {
        let mut error = Error::validation_invalid_argument_with_evidence(
            "to_worktree",
            format!("worktree provider `{provider_id}` {operation} command failed"),
            Some(provider_id.to_string()),
            None,
            Some(provider_command_evidence(command, &output)),
        );
        annotate_provider_lookup_error(&mut error, provider_id, command, operation, "command");
        return Err(error);
    }
    let payload = serde_json::from_slice(&output.stdout).map_err(|error| {
        provider_lookup_error(
            provider_id,
            command,
            operation,
            "malformed",
            "to_worktree",
            format!("worktree provider `{provider_id}` {operation} command returned invalid JSON: {error}"),
            false,
        )
    })?;
    Ok((payload, elapsed_ms, timeout.as_millis()))
}

fn run_provider_resolve_path_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
    path: &std::path::Path,
) -> Result<Vec<WorktreeProviderHandle>> {
    let path = path.display().to_string();
    let command = command
        .iter()
        .map(|argument| argument.replace("{path}", &path))
        .collect::<Vec<_>>();
    retry_resolve_path_timeouts(provider, &command, || {
        run_provider_lookup_command(
            provider_id,
            provider,
            &command,
            "resolve_path",
            &provider.commands.resolve_not_found_exit_codes,
        )
    })
}

fn retry_resolve_path_timeouts(
    provider: &WorktreeProviderConfig,
    command: &[String],
    mut run: impl FnMut() -> Result<Vec<WorktreeProviderHandle>>,
) -> Result<Vec<WorktreeProviderHandle>> {
    const RESOLVE_PATH_ATTEMPTS: u64 = 2;
    let started = std::time::Instant::now();
    let mut timed_out_attempts = Vec::with_capacity(RESOLVE_PATH_ATTEMPTS as usize);

    for attempt in 1..=RESOLVE_PATH_ATTEMPTS {
        match run() {
            Ok(worktrees) => return Ok(worktrees),
            Err(mut error)
                if error.details["worktree_provider_lookup"] == "timed_out"
                    && error.details["provider_lookup_timeout_kind"] == "supervision" =>
            {
                timed_out_attempts.push(serde_json::json!({
                    "attempt": attempt,
                    "elapsed_ms": error.details["latency_ms"],
                    "timed_out": true,
                }));
                if attempt < RESOLVE_PATH_ATTEMPTS {
                    continue;
                }
                let timeout_ms = provider_lookup_timeout(provider)?.as_millis() as u64;
                error.details["worktree_provider_resolve_path_attempts"] = serde_json::json!({
                    "attempt_count": RESOLVE_PATH_ATTEMPTS,
                    "configured_execution_budget_ms": timeout_ms
                        .saturating_mul(RESOLVE_PATH_ATTEMPTS),
                    "observed_total_elapsed_ms": u64::try_from(started.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                    "attempts": timed_out_attempts,
                    "replay_command": crate::redaction::redact_argv_shell_display(command),
                });
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("resolve_path retries return on success or final failure")
}

fn targeted_path_result(
    provider_id: &str,
    worktrees: Vec<WorktreeProviderHandle>,
    requested: &std::path::Path,
) -> Result<Option<WorktreeProviderHandle>> {
    if worktrees.is_empty() {
        return Ok(None);
    }
    if let Some(worktree) = worktrees
        .into_iter()
        .find(|worktree| std::fs::canonicalize(&worktree.path).ok().as_deref() == Some(requested))
    {
        return Ok(Some(worktree));
    }
    Err(Error::validation_invalid_argument(
        "to_worktree",
        format!(
            "worktree provider `{provider_id}` resolve_path command did not return the requested canonical path {}",
            requested.display()
        ),
        Some(provider_id.to_string()),
        None,
    ))
}

fn run_provider_list_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
) -> Result<Vec<WorktreeProviderHandle>> {
    run_provider_lookup_command(provider_id, provider, command, "list", &[])
}

fn run_provider_lookup_command(
    provider_id: &str,
    provider: &WorktreeProviderConfig,
    command: &[String],
    operation: &str,
    not_found_exit_codes: &[i32],
) -> Result<Vec<WorktreeProviderHandle>> {
    let timeout = provider_lookup_timeout(provider)?;
    let output_limit = provider_lookup_output_limit(provider)?;
    let supervised = run_bounded_provider_lookup_command(
        provider_id,
        command,
        operation,
        timeout,
        output_limit,
    )?;
    let elapsed_ms = supervised.elapsed_ms;
    let output = supervised.output;
    if output.termination != crate::engine::command::SupervisedCommandTermination::Completed {
        let cancelled =
            output.termination == crate::engine::command::SupervisedCommandTermination::Cancelled;
        let outcome = if cancelled { "cancelled" } else { "timed out" };
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` {operation} command {outcome} after {elapsed_ms} ms (configured lookup_timeout_ms: {})",
                timeout.as_millis()
            ),
            Some(provider_id.to_string()),
            Some(vec![
                "Refresh or repair the configured workspace provider, then retry the operation."
                    .to_string(),
            ]),
        );
        // A bounded read-only probe cannot establish that an exact handle is
        // invalid. Preserve that distinction for durable callers instead of
        // turning provider latency into an input error.
        error.retryable = Some(true);
        error.details["worktree_provider_lookup"] =
            Value::String(if cancelled { "cancelled" } else { "timed_out" }.to_string());
        error.details["worktree_provider_id"] = Value::String(provider_id.to_string());
        error.details["worktree_provider_operation"] = Value::String(operation.to_string());
        error.details["lookup_timeout_ms"] = Value::from(timeout.as_millis() as u64);
        error.details["latency_ms"] = Value::from(elapsed_ms as u64);
        error.details["provider_lookup_timeout_kind"] = Value::String(
            if cancelled {
                "cancellation"
            } else {
                "supervision"
            }
            .to_string(),
        );
        annotate_provider_lookup_error(
            &mut error,
            provider_id,
            command,
            operation,
            if cancelled { "cancelled" } else { "timeout" },
        );
        return Err(error);
    }
    let output = output.output;
    if output.capture.stdout.truncated {
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` {operation} command output exceeded configured lookup_output_limit_bytes: {output_limit} (received {} bytes)",
                output.capture.stdout.bytes_seen
            ),
            Some(provider_id.to_string()),
            Some(vec![
                "Increase the provider lookup_output_limit_bytes within its configured bound, then retry the operation.".to_string(),
            ]),
        );
        annotate_provider_lookup_error(&mut error, provider_id, command, operation, "malformed");
        return Err(error);
    }
    let output = output.into_output();
    if !output.status.success() {
        if output
            .status
            .code()
            .is_some_and(|code| not_found_exit_codes.contains(&code))
        {
            return Ok(Vec::new());
        }
        let mut error = Error::validation_invalid_argument_with_evidence(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` {operation} command failed with {}",
                output
                    .status
                    .code()
                    .map(|code| format!("exit code {code}"))
                    .unwrap_or_else(|| "a signal".to_string())
            ),
            Some(provider_id.to_string()),
            None,
            Some(provider_command_evidence(command, &output)),
        );
        annotate_provider_lookup_error(&mut error, provider_id, command, operation, "command");
        return Err(error);
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        provider_lookup_error(
            provider_id,
            command,
            operation,
            "malformed",
            "to_worktree",
            format!("worktree provider `{provider_id}` {operation} command returned invalid JSON: {error}"),
            false,
        )
    })?;
    if provider_declared_lookup_not_found(&value) {
        return Ok(Vec::new());
    }
    if let Some(attribution) = provider_failed_lookup_timeout_attribution(&value) {
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` {operation} command completed with a retryable git command timeout"
            ),
            Some(provider_id.to_string()),
            Some(vec![
                "Refresh or repair the configured workspace provider, then retry the operation."
                    .to_string(),
            ]),
        );
        error.retryable = Some(true);
        error.details["worktree_provider_lookup"] = Value::String("timed_out".to_string());
        error.details["worktree_provider_id"] = Value::String(provider_id.to_string());
        error.details["worktree_provider_operation"] = Value::String(operation.to_string());
        // Provider output can contain credentials and arbitrary diagnostics.
        // Persist only this fixed, redacted timeout classification.
        error.details["provider_timeout_attribution"] =
            serde_json::to_value(attribution).expect("fixed timeout attribution serializes");
        annotate_provider_lookup_error(&mut error, provider_id, command, operation, "timeout");
        return Err(error);
    }
    let mapping = provider.list_result_mapping.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "worktree_providers.list_result_mapping",
            format!(
                "worktree provider `{provider_id}` must configure an explicit list_result_mapping"
            ),
            Some(provider_id.to_string()),
            None,
        )
    })?;
    map_provider_list_result(provider_id, mapping, &value).map_err(|mut error| {
        annotate_provider_lookup_error(&mut error, provider_id, command, operation, "malformed");
        error
    })
}

/// Command providers may report an absent handle in a successful typed response.
/// This is a lookup result, unlike a failed command or a malformed response, so
/// callers can converge it through their configured ensure lifecycle.
fn provider_declared_lookup_not_found(value: &Value) -> bool {
    matches!(
        value.get("status").and_then(Value::as_str),
        Some("not_found")
    ) || matches!(
        value.get("code").and_then(Value::as_str),
        Some("worktree_not_found")
    ) || matches!(
        value.pointer("/error/code").and_then(Value::as_str),
        Some("worktree_not_found")
    )
}

#[derive(Serialize)]
struct ProviderLookupTimeoutAttribution {
    error_code: &'static str,
}

/// A command can finish successfully while its provider reports a failed lookup.
/// Treat only this explicit failure envelope as retryable; payload metadata is
/// provider-owned data rather than a causal error signal.
fn provider_failed_lookup_timeout_attribution(
    value: &Value,
) -> Option<ProviderLookupTimeoutAttribution> {
    let failed = matches!(
        value.get("status").and_then(Value::as_str),
        Some("failed" | "error")
    );
    let timed_out =
        value.pointer("/error/code").and_then(Value::as_str) == Some("git_command_timeout");
    (failed && timed_out).then_some(ProviderLookupTimeoutAttribution {
        error_code: "git_command_timeout",
    })
}

fn provider_lookup_timeout(provider: &WorktreeProviderConfig) -> Result<Duration> {
    defaults::validate_worktree_provider_lookup_timeout_ms(provider.lookup_timeout_ms).map_err(
        |message| {
            Error::validation_invalid_argument(
                "worktree_providers.lookup_timeout_ms",
                message,
                Some(provider.lookup_timeout_ms.to_string()),
                None,
            )
        },
    )?;
    Ok(Duration::from_millis(provider.lookup_timeout_ms))
}

fn provider_mutation_timeout(provider: &WorktreeProviderConfig) -> Result<Duration> {
    defaults::validate_worktree_provider_mutation_timeout_ms(provider.mutation_timeout_ms)
        .map_err(|message| {
            Error::validation_invalid_argument(
                "worktree_providers.mutation_timeout_ms",
                message,
                Some(provider.mutation_timeout_ms.to_string()),
                None,
            )
        })?;
    Ok(Duration::from_millis(provider.mutation_timeout_ms))
}

fn provider_lookup_output_limit(provider: &WorktreeProviderConfig) -> Result<usize> {
    defaults::validate_worktree_provider_lookup_output_limit_bytes(
        provider.lookup_output_limit_bytes,
    )
    .map_err(|message| {
        Error::validation_invalid_argument(
            "worktree_providers.lookup_output_limit_bytes",
            message,
            Some(provider.lookup_output_limit_bytes.to_string()),
            None,
        )
    })?;
    Ok(provider.lookup_output_limit_bytes)
}

struct BoundedProviderLookupCommand {
    output: crate::engine::command::SupervisedCommandOutput,
    elapsed_ms: u128,
}

/// Run one provider command in its own process group with its operation-specific
/// configured bound. This prevents an external provider from leaving Cook
/// waiting silently or retaining descendants on timeout.
fn run_bounded_provider_lookup_command(
    provider_id: &str,
    command: &[String],
    operation: &str,
    timeout: Duration,
    output_limit: usize,
) -> Result<BoundedProviderLookupCommand> {
    let (program, args) = command
        .split_first()
        .filter(|(program, _)| !program.trim().is_empty())
        .ok_or_else(|| {
            provider_lookup_error(
                provider_id,
                command,
                operation,
                "command",
                &format!("worktree_providers.commands.{operation}"),
                format!("worktree provider `{provider_id}` {operation} command must include an executable"),
                false,
            )
        })?;
    let mut process = Command::new(program);
    process
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::engine::command::isolate_process_tree(&mut process);
    let mut child = process.spawn().map_err(|error| {
        provider_lookup_error(
            provider_id,
            command,
            operation,
            "command",
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` {operation} command could not start: {error}"
            ),
            false,
        )
    })?;
    let started = std::time::Instant::now();
    let output = crate::engine::command::wait_with_bounded_output_supervised(
        &mut child,
        output_limit,
        timeout,
        PROVIDER_LOOKUP_HEARTBEAT,
        || {
            PROVIDER_COMMAND_CONTROL.with(|active| {
                active
                    .borrow()
                    .as_ref()
                    .is_some_and(WorktreeProviderCommandControl::is_cancelled)
            })
        },
        |_, _| Ok(()),
    )
    .map_err(|error| {
        provider_lookup_error(
            provider_id,
            command,
            operation,
            "command",
            "to_worktree",
            format!("worktree provider `{provider_id}` {operation} command could not be supervised: {error}"),
            false,
        )
    })?;
    Ok(BoundedProviderLookupCommand {
        output,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn provider_lookup_error(
    provider_id: &str,
    command: &[String],
    operation: &str,
    classification: &str,
    field: &str,
    message: String,
    retryable: bool,
) -> Error {
    let mut error =
        Error::validation_invalid_argument(field, message, Some(provider_id.to_string()), None);
    error.retryable = retryable.then_some(true);
    annotate_provider_lookup_error(&mut error, provider_id, command, operation, classification);
    error
}

fn annotate_provider_lookup_error(
    error: &mut Error,
    provider_id: &str,
    command: &[String],
    operation: &str,
    classification: &str,
) {
    error.details["worktree_provider_id"] = Value::String(provider_id.to_string());
    error.details["worktree_provider_operation"] = Value::String(operation.to_string());
    error.details["worktree_provider_call_classification"] =
        Value::String(classification.to_string());
    error.details["worktree_provider_phase"] =
        Value::String(format!("worktree_provider_{operation}"));
    error.details["worktree_provider_replay_command"] =
        Value::String(crate::redaction::redact_argv_shell_display(command));
}

/// Extract the causal command facts needed by compact operator projections.
/// Full command evidence remains attached to the durable error details.
pub fn compact_provider_failure_details(details: &Value) -> Option<Value> {
    const STDERR_EXCERPT_LIMIT: usize = 512;

    let operation = details.get("worktree_provider_operation")?.as_str()?;
    let evidence = details.get("command_evidence")?;
    let stderr = evidence.get("stderr")?.as_str()?;
    let stderr_excerpt: String = stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(stderr)
        .chars()
        .take(STDERR_EXCERPT_LIMIT)
        .collect();
    if stderr_excerpt.trim().is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "operation": operation,
        "exit_code": evidence.get("exit_code"),
        "replay_command": details.get("worktree_provider_replay_command").or_else(|| evidence.get("command")),
        "stderr_excerpt": stderr_excerpt,
    }))
}

fn provider_command_evidence(command: &[String], output: &std::process::Output) -> CommandEvidence {
    let (stdout, stdout_truncated) = bounded_provider_output(&output.stdout);
    let (stderr, stderr_truncated) = bounded_provider_output(&output.stderr);
    CommandEvidence {
        command: crate::redaction::redact_argv_shell_display(command),
        cwd: None,
        location: Some("local".to_string()),
        exit_code: output.status.code().unwrap_or(-1),
        stdout: crate::redaction::redact_string(&stdout),
        stderr: crate::redaction::redact_string(&stderr),
        truncated: stdout_truncated || stderr_truncated,
    }
}

fn bounded_provider_output(output: &[u8]) -> (String, bool) {
    const MAX_OUTPUT_CHARS: usize = 8_192;

    let output = String::from_utf8_lossy(output);
    let truncated = output.chars().count() > MAX_OUTPUT_CHARS;
    (output.chars().take(MAX_OUTPUT_CHARS).collect(), truncated)
}

#[cfg(test)]
mod compact_provider_failure_details_tests {
    use super::compact_provider_failure_details;
    use serde_json::json;

    #[test]
    fn projects_short_stderr_for_ensure_resolve_and_plan_failures() {
        for operation in ["ensure", "resolve", "plan"] {
            let details = json!({
                "worktree_provider_operation": operation,
                "worktree_provider_replay_command": format!("fixture-provider {operation}"),
                "command_evidence": {
                    "command": format!("fixture-provider {operation}"),
                    "exit_code": 17,
                    "stderr": "Error: actionable provider failure\nsecondary context",
                },
            });

            assert_eq!(
                compact_provider_failure_details(&details),
                Some(json!({
                    "operation": operation,
                    "exit_code": 17,
                    "replay_command": format!("fixture-provider {operation}"),
                    "stderr_excerpt": "Error: actionable provider failure",
                }))
            );
        }
    }

    #[test]
    fn bounds_large_stderr_without_changing_durable_evidence() {
        let stderr = format!("Error: {}", "x".repeat(2_048));
        let details = json!({
            "worktree_provider_operation": "ensure",
            "worktree_provider_replay_command": "fixture-provider ensure",
            "command_evidence": {
                "command": "fixture-provider ensure",
                "exit_code": 1,
                "stderr": stderr,
            },
        });

        let projection = compact_provider_failure_details(&details).expect("compact evidence");
        assert_eq!(
            projection["stderr_excerpt"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            512
        );
        assert_eq!(
            details["command_evidence"]["stderr"].as_str(),
            Some(stderr.as_str())
        );
    }
}

fn map_provider_list_result(
    provider_id: &str,
    mapping: &WorktreeProviderListResultMapping,
    value: &Value,
) -> Result<Vec<WorktreeProviderHandle>> {
    let items = required_jsonpath_value(provider_id, "items", &mapping.items, value)?;
    let items = items.as_array().ok_or_else(|| {
        mapping_error(
            provider_id,
            "items",
            &mapping.items,
            "must resolve to an array",
        )
    })?;

    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            Ok(WorktreeProviderHandle {
                handle: required_string(provider_id, index, "handle", &mapping.handle, item)?,
                path: required_string(provider_id, index, "path", &mapping.path, item)?,
                branch: required_string(provider_id, index, "branch", &mapping.branch, item)?,
                task_url: mapping
                    .task_url
                    .as_deref()
                    .map(|path| optional_string(provider_id, index, "task_url", path, item))
                    .transpose()?
                    .flatten(),
                // Safety flags are advisory hints a provider raises to BLOCK an
                // unsafe destination (dirty/unpushed/primary => refuse). A
                // provider that does not report one is making no claim of
                // unsafety, so an absent value defaults to `false` (permissive)
                // rather than failing the whole cook closed — the DMC worktree
                // provider legitimately omits `safety.dirty` (#7886). A value
                // that IS present but not a boolean is still a contract error.
                safety: WorktreeProviderHandleSafety {
                    dirty: optional_bool(provider_id, index, "dirty", &mapping.dirty, item)?,
                    unpushed: optional_bool(
                        provider_id,
                        index,
                        "unpushed",
                        &mapping.unpushed,
                        item,
                    )?,
                    primary: optional_bool(provider_id, index, "primary", &mapping.primary, item)?,
                },
            })
        })
        .collect()
}

fn required_string(
    provider_id: &str,
    index: usize,
    field: &str,
    path: &str,
    item: &Value,
) -> Result<String> {
    required_jsonpath_value(provider_id, &format!("items[{index}].{field}"), path, item)?
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| mapping_error(provider_id, field, path, "must resolve to a string"))
}

/// Task ownership is optional per row even when a provider supports exposing
/// it. A provider list can legitimately include unmanaged worktrees.
fn optional_string(
    provider_id: &str,
    index: usize,
    field: &str,
    path: &str,
    item: &Value,
) -> Result<Option<String>> {
    match optional_jsonpath_value(provider_id, &format!("items[{index}].{field}"), path, item)? {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| {
                mapping_error(provider_id, field, path, "must resolve to a string or null")
            }),
    }
}

/// Resolve an advisory boolean safety flag. An absent value defaults to `false`
/// (the provider makes no claim of unsafety), so a provider that omits the field
/// does not block the cook (#7886). A value that resolves but is not a boolean
/// remains a contract error.
fn optional_bool(
    provider_id: &str,
    index: usize,
    field: &str,
    path: &str,
    item: &Value,
) -> Result<bool> {
    match optional_jsonpath_value(provider_id, &format!("items[{index}].{field}"), path, item)? {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| mapping_error(provider_id, field, path, "must resolve to a boolean")),
    }
}

fn required_jsonpath_value<'a>(
    provider_id: &str,
    field: &str,
    expression: &str,
    value: &'a Value,
) -> Result<&'a Value> {
    optional_jsonpath_value(provider_id, field, expression, value)?
        .ok_or_else(|| mapping_error(provider_id, field, expression, "did not resolve a value"))
}

/// Resolve a JSONPath to at most one value: `Ok(None)` when it resolves nothing,
/// `Ok(Some(value))` for exactly one, and an error only for invalid JSONPath or
/// an ambiguous multi-match.
fn optional_jsonpath_value<'a>(
    provider_id: &str,
    field: &str,
    expression: &str,
    value: &'a Value,
) -> Result<Option<&'a Value>> {
    let path = serde_json_path::JsonPath::parse(expression).map_err(|error| {
        mapping_error(
            provider_id,
            field,
            expression,
            &format!("is not valid JSONPath: {error}"),
        )
    })?;
    let matches = path.query(value).all();
    match matches.as_slice() {
        [value] => Ok(Some(*value)),
        [] => Ok(None),
        _ => Err(mapping_error(
            provider_id,
            field,
            expression,
            "resolved more than one value",
        )),
    }
}

fn mapping_error(provider_id: &str, field: &str, path: &str, detail: &str) -> Error {
    Error::validation_invalid_argument(
        "worktree_providers.list_result_mapping",
        format!("worktree provider `{provider_id}` mapping `{field}` ({path}) {detail}"),
        Some(provider_id.to_string()),
        None,
    )
}

fn validate_provider_handle(
    provider_id: &str,
    worktree: &WorktreeProviderHandle,
    gate_feedback_baseline: Option<&serde_json::Value>,
    trusted_unpushed_destination: Option<&TrustedUnpushedWorktree>,
) -> Result<()> {
    let remote = worktree_provider_path_requires_materialization(&worktree.path);
    let path = std::path::PathBuf::from(&worktree.path);
    if !remote && !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` resolved `{}` to a missing directory {}",
                worktree.handle,
                path.display()
            ),
            Some(worktree.handle.clone()),
            None,
        ));
    }
    if worktree.branch.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` resolved `{}` without a branch",
                worktree.handle
            ),
            Some(worktree.handle.clone()),
            None,
        ));
    }
    let baseline_verification = if worktree.safety.dirty {
        match gate_feedback_baseline {
            Some(baseline) => {
                match crate::gate_feedback_baseline::validate_gate_feedback_candidate_baseline(
                    &path, baseline,
                ) {
                    Ok(_) => BaselineVerification::Matches,
                    Err(error) => BaselineVerification::Diverges(error.message),
                }
            }
            None => BaselineVerification::Absent,
        }
    } else {
        BaselineVerification::NotRequired
    };
    let verified_gate_feedback_baseline = baseline_verification == BaselineVerification::Matches;
    let blocked = [
        (
            worktree.safety.dirty && !verified_gate_feedback_baseline,
            "dirty",
        ),
        (
            worktree.safety.unpushed
                && !trusted_unpushed_destination_matches(&path, trusted_unpushed_destination),
            "unpushed",
        ),
        (worktree.safety.primary, "primary"),
    ]
    .into_iter()
    .filter_map(|(blocked, name)| blocked.then_some(name))
    .collect::<Vec<_>>();
    if !blocked.is_empty() {
        let mut error = Error::validation_invalid_argument(
            "to_worktree",
            dirty_worktree_message(
                provider_id,
                worktree,
                &blocked,
                &baseline_verification,
                &path,
            ),
            Some(worktree.handle.clone()),
            Some(vec![dirty_worktree_recovery(&path)]),
        );
        if worktree.safety.dirty {
            error.details["workspace"] =
                dirty_worktree_details(provider_id, worktree, &baseline_verification, &path);
        } else if blocked == ["unpushed"] {
            error.details["workspace"] = serde_json::json!({
                "classification": "workspace.untrusted_unpushed",
                "reason": "exact_clean_checkout_and_head_not_trusted",
                "owning_layer": "worktree_provider",
                "provider_id": provider_id,
                "resolution": {
                    "handle": worktree.handle,
                    "path": worktree.path,
                    "branch": worktree.branch,
                    "primary": worktree.safety.primary,
                    "safety": worktree.safety,
                },
                "inspect_command": git_status_command(&path),
            });
        }
        return Err(error);
    }
    if remote {
        return Ok(());
    }
    if crate::git::current_branch(&path).as_deref() != Some(worktree.branch.as_str()) {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!("worktree provider `{provider_id}` branch metadata for `{}` does not match the checkout branch", worktree.handle),
            Some(worktree.handle.clone()),
            Some(vec![format!("Provider reported branch `{}`; refresh provider metadata and retry.", worktree.branch)]),
        ));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum BaselineVerification {
    NotRequired,
    Absent,
    Matches,
    Diverges(String),
}

fn dirty_worktree_message(
    provider_id: &str,
    worktree: &WorktreeProviderHandle,
    blocked: &[&str],
    baseline: &BaselineVerification,
    path: &std::path::Path,
) -> String {
    let baseline_detail = match baseline {
        BaselineVerification::Absent => String::new(),
        BaselineVerification::Diverges(_) => {
            "; promoted candidate baseline could not be verified against the current changes"
                .to_string()
        }
        BaselineVerification::NotRequired | BaselineVerification::Matches => String::new(),
    };
    let changed_paths = changed_paths(path);
    let evidence = if changed_paths.is_empty() {
        format!("; inspect with {}", git_status_command(path))
    } else {
        format!("; changed paths: {}", changed_paths.join(", "))
    };
    format!(
        "worktree provider `{provider_id}` resolved `{}` at {} on branch `{}` but marked it {}; workspace.resolved_but_dirty{baseline_detail}{evidence}; refusing to cook into an unsafe destination",
        worktree.handle,
        path.display(),
        worktree.branch,
        blocked.join(", "),
    )
}

fn dirty_worktree_details(
    provider_id: &str,
    worktree: &WorktreeProviderHandle,
    baseline: &BaselineVerification,
    path: &std::path::Path,
) -> Value {
    let (reason, baseline_error) = match baseline {
        BaselineVerification::Absent => ("unattributed_drift", None),
        BaselineVerification::Diverges(error) => ("divergent_user_edits", Some(error.clone())),
        // A matching promoted candidate can still be unsafe for another reason,
        // such as an unpushed commit or a primary checkout.
        BaselineVerification::Matches => ("verified_promoted_candidate", None),
        BaselineVerification::NotRequired => unreachable!(),
    };
    let mut details = serde_json::json!({
        "classification": "workspace.resolved_but_dirty",
        "reason": reason,
        "owning_layer": "worktree_provider",
        "provider_id": provider_id,
        "resolution": {
            "handle": worktree.handle,
            "path": worktree.path,
            "branch": worktree.branch,
            "primary": worktree.safety.primary,
            "safety": worktree.safety,
        },
        "changed_paths": changed_paths(path),
        "inspect_command": git_status_command(path),
        "recovery_action": dirty_worktree_recovery(path),
    });
    if let Some(error) = baseline_error {
        details["baseline_verification_error"] = Value::String(error);
    }
    details
}

fn changed_paths(path: &std::path::Path) -> Vec<String> {
    let Ok(output) = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .current_dir(path)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.get(3..))
        .map(str::to_string)
        .take(20)
        .collect()
}

fn git_status_command(path: &std::path::Path) -> String {
    render_provider_command(&[
        "git".to_string(),
        "-C".to_string(),
        path.display().to_string(),
        "status".to_string(),
        "--short".to_string(),
        "--untracked-files=all".to_string(),
    ])
}

fn dirty_worktree_recovery(path: &std::path::Path) -> String {
    format!(
        "Inspect the provider-owned checkout with {}; restore or commit only changes authorized by its owning workflow, then retry Cook.",
        git_status_command(path)
    )
}

fn mark_bootstrap_postcondition_failure(mut error: Error) -> Error {
    let Some(workspace) = error.details.get_mut("workspace") else {
        return error;
    };
    if workspace["classification"] != "workspace.resolved_but_dirty" {
        return error;
    }
    workspace["reason"] = Value::String("fresh_bootstrap_drift".to_string());
    workspace["owning_layer"] = Value::String("worktree_provider_bootstrap".to_string());
    let recovery = workspace["recovery_action"].as_str().map(|action| {
        format!("Provider bootstrap reported success with tracked changes. {action}")
    });
    if let Some(recovery) = recovery {
        workspace["recovery_action"] = Value::String(recovery);
    }
    error.message = format!(
        "{}; provider bootstrap postcondition failed: ensure must leave a clean tracked checkout",
        error.message
    );
    error
}

/// Verify that a provider target is a linked Git worktree rooted at the exact
/// path it returned. A primary checkout has a `.git` directory, whereas a
/// linked worktree has a `.git` file; this filesystem proof prevents stale or
/// misleading provider safety metadata from redirecting a Cook into a primary.
pub fn validate_task_worktree_root(path: &std::path::Path, handle: &str) -> Result<()> {
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!("task worktree `{handle}` cannot be canonicalized: {error}"),
            Some(handle.to_string()),
            None,
        )
    })?;
    let git_root =
        crate::git::repo_root(&canonical).and_then(|root| std::fs::canonicalize(root).ok());
    if git_root.as_deref() != Some(canonical.as_path()) {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!("task worktree `{handle}` does not resolve to its exact Git root"),
            Some(handle.to_string()),
            Some(vec!["Refresh the workspace provider and select the declared task worktree, not a parent, child, or path alias.".to_string()]),
        ));
    }
    let git_metadata = std::fs::symlink_metadata(canonical.join(".git")).map_err(|error| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!("task worktree `{handle}` is missing linked-worktree Git metadata: {error}"),
            Some(handle.to_string()),
            Some(vec![
                "Create the destination through the configured worktree provider, then retry Cook."
                    .to_string(),
            ]),
        )
    })?;
    if !git_metadata.file_type().is_file() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!("task worktree `{handle}` is a primary or non-linked checkout; refusing provider execution"),
            Some(handle.to_string()),
            Some(vec!["Create or select the declared linked task worktree through the configured worktree provider, then retry Cook.".to_string()]),
        ));
    }
    Ok(())
}

/// Verify a resolved linked worktree belongs to Cook's durable repository
/// expectation without exposing provider-owned remote URLs in failures.
pub fn validate_task_worktree_repository_identity(
    path: &std::path::Path,
    expected_remote: Option<&str>,
    expected_repository_name: Option<&str>,
) -> Result<()> {
    if expected_remote.is_none() && expected_repository_name.is_none() {
        return Ok(());
    }
    let Some(git_root) = crate::git::repo_root(path) else {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "Cook destination is not a Git checkout bound to the resolved repository identity",
            Some(path.display().to_string()),
            None,
        ));
    };
    let identities = crate::git::output_optional(&git_root, &["remote"])
        .unwrap_or_default()
        .lines()
        .filter_map(|remote| crate::git::remote_url(&git_root, remote))
        .filter_map(|remote| canonical_remote_identity(&remote))
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(expected) = expected_remote {
        if identities.len() == 1 && identities.contains(expected) {
            return Ok(());
        }
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!("Cook destination repository identity does not match resolved `{expected}`"),
            Some(path.display().to_string()),
            None,
        ));
    }
    let expected = expected_repository_name.expect("repository expectation exists");
    if identities.len() == 1
        && identities.iter().any(|identity| {
            identity
                .strip_prefix("git://")
                .and_then(|identity| identity.rsplit('/').next())
                == Some(expected)
        })
    {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "to_worktree",
        "Cook destination repository does not match the requested Cook repository",
        Some(path.display().to_string()),
        None,
    ))
}

fn canonical_remote_identity(remote_url: &str) -> Option<String> {
    let remote_url = remote_url.trim();
    let (host, path) = if let Some((_, rest)) = remote_url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        (authority.rsplit('@').next()?, path)
    } else {
        let (authority, path) = remote_url.split_once(':')?;
        (authority.rsplit('@').next()?, path)
    };
    let path = path.trim_matches('/').trim_end_matches(".git");
    (!host.is_empty()
        && path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count()
            >= 2)
        .then(|| {
            format!(
                "git://{}/{}",
                host.to_ascii_lowercase(),
                path.to_ascii_lowercase()
            )
        })
}

fn trusted_unpushed_destination_matches(
    path: &std::path::Path,
    trusted: Option<&TrustedUnpushedWorktree>,
) -> bool {
    let Some(trusted) = trusted else {
        return false;
    };
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    let Ok(trusted_path) = std::fs::canonicalize(&trusted.path) else {
        return false;
    };
    path == trusted_path
        && changed_paths(&path).is_empty()
        && crate::git::run_git(
            &path,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            "verify trusted unpushed worktree HEAD",
        )
        .ok()
        .is_some_and(|head| head.trim() == trusted.head)
}

pub fn cleanup_worktree_providers_from_config(
    options: WorktreeProviderCleanupOptions,
    config: HomeboyConfig,
) -> Result<WorktreeProviderCleanupOutput> {
    validate_selection(&options)?;

    let mode = if options.apply {
        WorktreeProviderCleanupMode::Apply
    } else {
        WorktreeProviderCleanupMode::Preview
    };

    let providers = selected_providers(&options, &config)?
        .into_iter()
        .map(|(id, provider)| (id, provider.clone()))
        .collect::<Vec<_>>();
    let mut results = std::thread::scope(|scope| {
        let mut tasks = Vec::new();
        for (provider_id, provider_config) in providers {
            let mode = mode.clone();
            tasks.push(scope.spawn(move || {
                run_provider_cleanup(&provider_id, &provider_config, mode, options.timeout)
            }));
        }
        tasks
            .into_iter()
            .map(|task| task.join().expect("provider cleanup worker must not panic"))
            .collect::<Vec<_>>()
    });
    results.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));

    let success_count = results.iter().filter(|row| row.success).count();
    let failure_count = results.len().saturating_sub(success_count);

    Ok(WorktreeProviderCleanupOutput {
        command: "cleanup.worktrees",
        mode,
        provider_count: results.len(),
        success_count,
        failure_count,
        inventory_completeness: if results.iter().all(|row| {
            row.inventory_completeness == WorktreeProviderInventoryCompleteness::Complete
        }) {
            WorktreeProviderInventoryCompleteness::Complete
        } else {
            WorktreeProviderInventoryCompleteness::Partial
        },
        providers: results,
    })
}

fn validate_selection(options: &WorktreeProviderCleanupOptions) -> Result<()> {
    if options.all_providers && !options.provider.is_empty() {
        return Err(Error::validation_invalid_argument(
            "provider",
            "--provider cannot be combined with --all-providers",
            None,
            None,
        ));
    }
    if !options.all_providers && options.provider.is_empty() {
        return Err(Error::validation_missing_argument(vec![
            "--provider <id> or --all-providers".to_string(),
        ]));
    }
    Ok(())
}

fn selected_providers<'a>(
    options: &WorktreeProviderCleanupOptions,
    config: &'a HomeboyConfig,
) -> Result<Vec<(String, &'a WorktreeProviderConfig)>> {
    if options.all_providers {
        let sorted: BTreeMap<_, _> = config.worktree_providers.iter().collect();
        return Ok(sorted
            .into_iter()
            .filter_map(|(id, provider)| provider.enabled.then_some((id.clone(), provider)))
            .collect());
    }

    let mut providers = Vec::new();
    for provider_id in &options.provider {
        let Some(provider_config) = config.worktree_providers.get(provider_id) else {
            return Err(Error::validation_invalid_argument(
                "provider",
                format!("unknown worktree provider '{provider_id}'"),
                Some(provider_id.clone()),
                Some(config.worktree_providers.keys().cloned().collect()),
            ));
        };
        providers.push((provider_id.clone(), provider_config));
    }
    Ok(providers)
}

fn run_provider_cleanup(
    provider_id: &str,
    provider_config: &WorktreeProviderConfig,
    mode: WorktreeProviderCleanupMode,
    timeout: Option<Duration>,
) -> WorktreeProviderCleanupResult {
    let timeout = match provider_cleanup_timeout(provider_config, &mode, timeout) {
        Ok(timeout) => timeout,
        Err(error) => return provider_failure(provider_id, mode, None, &error),
    };
    if !provider_config.enabled {
        return provider_failure(provider_id, mode, Some(timeout), "provider is disabled");
    }

    match provider_config.kind {
        WorktreeProviderKind::Command => {
            run_command_provider_cleanup(provider_id, provider_config, mode, timeout)
        }
    }
}

fn provider_cleanup_timeout(
    provider: &WorktreeProviderConfig,
    mode: &WorktreeProviderCleanupMode,
    aggregate_cap: Option<Duration>,
) -> std::result::Result<Duration, String> {
    let configured_timeout_ms = match mode {
        WorktreeProviderCleanupMode::Preview => provider.commands.cleanup_preview_timeout_ms,
        WorktreeProviderCleanupMode::Apply => provider.commands.cleanup_apply_timeout_ms,
    };
    defaults::validate_worktree_provider_cleanup_timeout_ms(configured_timeout_ms)?;
    let configured = Duration::from_millis(configured_timeout_ms);
    Ok(aggregate_cap.map_or(configured, |cap| cap.min(configured)))
}

fn run_command_provider_cleanup(
    provider_id: &str,
    provider_config: &WorktreeProviderConfig,
    mode: WorktreeProviderCleanupMode,
    timeout: Duration,
) -> WorktreeProviderCleanupResult {
    run_command_provider_cleanup_with_liveness(
        provider_id,
        provider_config,
        mode,
        timeout,
        PROVIDER_CLEANUP_HEARTBEAT,
    )
}

fn run_command_provider_cleanup_with_liveness(
    provider_id: &str,
    provider_config: &WorktreeProviderConfig,
    mode: WorktreeProviderCleanupMode,
    timeout: Duration,
    heartbeat_interval: Duration,
) -> WorktreeProviderCleanupResult {
    if mode == WorktreeProviderCleanupMode::Apply && !provider_config.apply_enabled {
        return provider_failure(
            provider_id,
            mode,
            Some(timeout),
            "provider apply is not enabled",
        );
    }

    let command = match mode {
        WorktreeProviderCleanupMode::Preview => &provider_config.commands.cleanup_preview,
        WorktreeProviderCleanupMode::Apply => &provider_config.commands.cleanup_apply,
    };

    let Some(command) = command.as_ref() else {
        return provider_failure(
            provider_id,
            mode,
            Some(timeout),
            "provider cleanup command is not configured",
        );
    };
    if command.is_empty() || command[0].trim().is_empty() {
        return provider_failure(
            provider_id,
            mode,
            Some(timeout),
            "provider command argv must include an executable",
        );
    }

    let mut process = Command::new(&command[0]);
    process
        .args(&command[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::engine::command::isolate_process_tree(&mut process);
    eprintln!(
        "[cleanup.worktrees provider={provider_id} phase={}] starting",
        mode_phase(&mode)
    );
    match process.spawn() {
        Ok(mut child) => {
            let started = std::time::Instant::now();
            let mut heartbeats = 0;
            let wait_result = crate::engine::command::wait_with_bounded_output_supervised(
                &mut child,
                PROVIDER_CLEANUP_OUTPUT_LIMIT,
                timeout,
                heartbeat_interval,
                || false,
                |elapsed, tail| {
                    heartbeats += 1;
                    eprintln!(
                        "[cleanup.worktrees provider={provider_id} phase={} elapsed_ms={} remaining_ms={} heartbeat={heartbeats}] {}",
                        mode_phase(&mode),
                        elapsed.as_millis(),
                        timeout.saturating_sub(elapsed).as_millis(),
                        tail.lines().last().unwrap_or("waiting for provider output"),
                    );
                    Ok(())
                },
            );
            let elapsed_ms = started.elapsed().as_millis();
            let (output_status, outcome, stdout, stderr) = match wait_result {
                Ok(output) => {
                    let outcome = match output.termination {
                        crate::engine::command::SupervisedCommandTermination::Completed
                            if output.output.status.success() =>
                        {
                            WorktreeProviderCleanupOutcome::Completed
                        }
                        crate::engine::command::SupervisedCommandTermination::Completed => {
                            WorktreeProviderCleanupOutcome::Failed
                        }
                        crate::engine::command::SupervisedCommandTermination::TimedOut => {
                            WorktreeProviderCleanupOutcome::TimedOut
                        }
                        crate::engine::command::SupervisedCommandTermination::NoProgress => {
                            WorktreeProviderCleanupOutcome::TimedOut
                        }
                        crate::engine::command::SupervisedCommandTermination::Cancelled => {
                            WorktreeProviderCleanupOutcome::Cancelled
                        }
                    };
                    (
                        Some(output.output.status),
                        outcome,
                        String::from_utf8_lossy(&output.output.stdout).to_string(),
                        String::from_utf8_lossy(&output.output.stderr).to_string(),
                    )
                }
                Err(err) => {
                    return provider_failure_with_details(
                        provider_id,
                        mode,
                        Some(command.clone()),
                        format!("failed to supervise provider command: {err}"),
                        elapsed_ms,
                        heartbeats,
                        Some(timeout),
                    );
                }
            };
            let parsed_payload = parse_json_stdout(&stdout);
            let phase = provider_phase(&parsed_payload, &mode);
            let last_progress = provider_last_progress(&parsed_payload)
                .or_else(|| last_non_empty_line(&stdout))
                .or_else(|| last_non_empty_line(&stderr));
            let run_refs = provider_run_refs(&parsed_payload);
            let follow_up_command = provider_follow_up_command(&run_refs);

            let status = output_status.and_then(|status| status.code());
            WorktreeProviderCleanupResult {
                provider_id: provider_id.to_string(),
                success: outcome == WorktreeProviderCleanupOutcome::Completed,
                inventory_completeness: if outcome == WorktreeProviderCleanupOutcome::Completed {
                    WorktreeProviderInventoryCompleteness::Complete
                } else {
                    WorktreeProviderInventoryCompleteness::Partial
                },
                outcome: outcome.clone(),
                elapsed_ms,
                heartbeat_count: heartbeats,
                timeout_ms: timeout.as_millis(),
                mode,
                command_run: Some(command.clone()),
                status,
                stdout,
                stderr,
                parsed_payload,
                phase,
                last_progress,
                run_refs,
                follow_up_command,
                error: (outcome != WorktreeProviderCleanupOutcome::Completed).then(
                    || match outcome {
                        WorktreeProviderCleanupOutcome::TimedOut => {
                            format!("provider timed out after {} ms", timeout.as_millis())
                        }
                        WorktreeProviderCleanupOutcome::Cancelled => {
                            "provider command was cancelled".to_string()
                        }
                        _ => "provider command failed".to_string(),
                    },
                ),
            }
        }
        Err(err) => provider_failure_with_details(
            provider_id,
            mode,
            Some(command.clone()),
            format!("failed to execute provider command: {err}"),
            0,
            0,
            Some(timeout),
        ),
    }
}

fn last_non_empty_line(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
}

fn provider_failure(
    provider_id: &str,
    mode: WorktreeProviderCleanupMode,
    timeout: Option<Duration>,
    error: &str,
) -> WorktreeProviderCleanupResult {
    provider_failure_with_details(provider_id, mode, None, error.to_string(), 0, 0, timeout)
}

fn provider_failure_with_details(
    provider_id: &str,
    mode: WorktreeProviderCleanupMode,
    command_run: Option<Vec<String>>,
    error: String,
    elapsed_ms: u128,
    heartbeat_count: usize,
    timeout: Option<Duration>,
) -> WorktreeProviderCleanupResult {
    let phase = Some(mode_phase(&mode).to_string());
    WorktreeProviderCleanupResult {
        provider_id: provider_id.to_string(),
        success: false,
        outcome: WorktreeProviderCleanupOutcome::Failed,
        inventory_completeness: WorktreeProviderInventoryCompleteness::Partial,
        elapsed_ms,
        heartbeat_count,
        timeout_ms: timeout.map_or(0, |timeout| timeout.as_millis()),
        mode,
        command_run,
        status: None,
        stdout: String::new(),
        stderr: String::new(),
        parsed_payload: None,
        phase,
        last_progress: None,
        run_refs: Vec::new(),
        follow_up_command: None,
        error: Some(error),
    }
}

fn parse_json_stdout(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok().or_else(|| {
        trimmed
            .lines()
            .rev()
            .map(str::trim)
            .find_map(|line| serde_json::from_str(line).ok())
    })
}

fn provider_phase(payload: &Option<Value>, mode: &WorktreeProviderCleanupMode) -> Option<String> {
    payload
        .as_ref()
        .and_then(|payload| first_string_for_keys(payload, &["phase", "state", "status"]))
        .or_else(|| Some(mode_phase(mode).to_string()))
}

fn provider_last_progress(payload: &Option<Value>) -> Option<String> {
    payload.as_ref().and_then(|payload| {
        first_string_for_keys(
            payload,
            &[
                "last_progress",
                "progress",
                "message",
                "summary",
                "last_observed_progress",
            ],
        )
    })
}

fn provider_run_refs(payload: &Option<Value>) -> Vec<WorktreeProviderRunRef> {
    let Some(payload) = payload else {
        return Vec::new();
    };
    let mut run_ids = Vec::new();
    let mut status_commands = Vec::new();
    collect_strings_for_keys(
        payload,
        &["run_id", "runId", "durable_run_id"],
        &mut run_ids,
    );
    collect_strings_for_keys(
        payload,
        &["status_command", "statusCommand", "status_cmd"],
        &mut status_commands,
    );
    collect_status_commands_from_arrays(payload, &mut status_commands);

    let len = run_ids.len().max(status_commands.len());
    (0..len)
        .map(|index| WorktreeProviderRunRef {
            run_id: run_ids.get(index).cloned(),
            status_command: status_commands.get(index).cloned(),
        })
        .collect()
}

fn provider_follow_up_command(refs: &[WorktreeProviderRunRef]) -> Option<String> {
    refs.iter().find_map(|row| row.status_command.clone())
}

fn mode_phase(mode: &WorktreeProviderCleanupMode) -> &'static str {
    match mode {
        WorktreeProviderCleanupMode::Preview => "preview",
        WorktreeProviderCleanupMode::Apply => "apply",
    }
}

fn first_string_for_keys(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key).and_then(Value::as_str) {
                    return Some(value.to_string());
                }
            }
            map.values()
                .find_map(|value| first_string_for_keys(value, keys))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| first_string_for_keys(value, keys)),
        _ => None,
    }
}

fn collect_strings_for_keys(value: &Value, keys: &[&str], out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key).and_then(Value::as_str) {
                    if !out.contains(&value.to_string()) {
                        out.push(value.to_string());
                    }
                }
            }
            for value in map.values() {
                collect_strings_for_keys(value, keys, out);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_strings_for_keys(value, keys, out);
            }
        }
        _ => {}
    }
}

fn collect_status_commands_from_arrays(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for key in [
                "status_commands",
                "statusCommands",
                "next_commands",
                "nextCommands",
            ] {
                if let Some(values) = map.get(key).and_then(Value::as_array) {
                    for value in values {
                        if let Some(command) = value.as_str().or_else(|| {
                            value
                                .get("command")
                                .and_then(Value::as_str)
                                .or_else(|| value.get("status_command").and_then(Value::as_str))
                        }) {
                            if !out.contains(&command.to_string()) {
                                out.push(command.to_string());
                            }
                        }
                    }
                }
            }
            for value in map.values() {
                collect_status_commands_from_arrays(value, out);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_status_commands_from_arrays(value, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::defaults::WorktreeProviderCommands;

    #[test]
    fn deserializes_worktree_provider_config() {
        let config: HomeboyConfig = serde_json::from_value(json!({
            "worktree_providers": {
                "fixture": {
                    "enabled": true,
                    "kind": "command",
                    "apply_enabled": true,
                    "commands": {
                        "cleanup_preview": ["fixture-bin", "preview"],
                        "cleanup_apply": ["fixture-bin", "apply"],
                        "artifacts_preview": ["fixture-bin", "artifacts-preview"]
                    },
                    "list_result_mapping": {
                        "items": "$.result.items",
                        "handle": "$.id",
                        "path": "$.checkout.path",
                        "branch": "$.checkout.branch",
                        "dirty": "$.safety.dirty",
                        "unpushed": "$.safety.unpushed",
                        "primary": "$.safety.primary"
                    }
                }
            }
        }))
        .expect("config deserializes");

        let provider = config.worktree_providers.get("fixture").expect("provider");
        assert!(provider.enabled);
        assert_eq!(provider.kind, WorktreeProviderKind::Command);
        assert!(provider.apply_enabled);
        assert_eq!(
            provider.commands.cleanup_preview.as_ref().expect("command"),
            &vec!["fixture-bin".to_string(), "preview".to_string()]
        );
        assert_eq!(
            provider
                .list_result_mapping
                .as_ref()
                .expect("list result mapping")
                .items,
            "$.result.items"
        );
    }

    #[test]
    fn lifecycle_ensure_argv_uses_the_registered_workspace_owner() {
        let mut config = HomeboyConfig::default();
        config.worktree_providers.insert(
            "managed".to_string(),
            WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec!["false".to_string()]),
                    resolve_not_found_exit_codes: vec![1],
                    ensure: Some(vec![
                        "workspace-owner".to_string(),
                        "worktree".to_string(),
                        "add".to_string(),
                        "{repo}".to_string(),
                        "{head}".to_string(),
                        "--from={base}".to_string(),
                        "--task-url={task_url}".to_string(),
                        "--owner-run-ref={owner_run_ref}".to_string(),
                    ]),
                    ..WorktreeProviderCommands::default()
                },
                list_result_mapping: None,
            },
        );
        config.settings.insert(
            WORKTREE_PROVIDER_LIFECYCLE_SETTINGS_KEY.to_string(),
            json!({ "managed": { "finalize": ["workspace-owner", "finalize"] } }),
        );
        let intent = WorktreeProviderCreateIntent {
            handle: "blocks-engine@fix-12252".to_string(),
            repo: "blocks-engine".to_string(),
            base: "origin/trunk".to_string(),
            head: "fix/12252".to_string(),
            task_url: "https://example.test/issues/12252".to_string(),
        };
        let lifecycle = WorktreeProviderLifecycleIntent {
            purpose: "agent_task_cook".to_string(),
            owner_run_ref: "fanout-12252".to_string(),
            cleanup_policy: WorktreeProviderCleanupPolicy::RemoveOnSuccess,
        };

        let argv =
            worktree_provider_lifecycle_ensure_argv_from_config(&intent, &lifecycle, &config)
                .expect("registered owner repair argv");

        assert_eq!(
            argv,
            vec![
                "workspace-owner",
                "worktree",
                "add",
                "blocks-engine",
                "fix/12252",
                "--from=origin/trunk",
                "--task-url=https://example.test/issues/12252",
                "--owner-run-ref=fanout-12252",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn provider_create_plan_projects_absent_existing_and_unsupported_destinations_without_ensure() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let existing = temp.path().join("existing");
        let marker = temp.path().join("ensure-called");
        let script = temp.path().join("provider");
        Command::new("git")
            .args([
                "init",
                "-q",
                "-b",
                "fix/existing",
                existing.to_str().unwrap(),
            ])
            .status()
            .expect("initialize existing workspace");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$1\" in\nresolve) if [ \"$2\" = \"fixture@existing\" ]; then printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@existing\",\"path\":\"{}\",\"branch\":\"fix/existing\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'; else printf '%s\\n' '{{\"worktrees\":[]}}'; fi ;;\nplan) printf '%s\\n' \"{{\\\"worktrees\\\":[{{\\\"handle\\\":\\\"$2\\\",\\\"path\\\":\\\"/provider/planned/$2\\\",\\\"branch\\\":\\\"$5\\\",\\\"safety\\\":{{\\\"dirty\\\":false,\\\"unpushed\\\":false,\\\"primary\\\":false}}}}]}}\" ;;\nensure) touch '{}' ;;\nesac\n",
                existing.display(),
                marker.display(),
            ),
        )
        .expect("write provider");
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("executable");
        let mut config = config_with_provider(WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: true,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands {
                resolve: Some(vec![
                    script.display().to_string(),
                    "resolve".to_string(),
                    "{handle}".to_string(),
                ]),
                plan: Some(vec![
                    script.display().to_string(),
                    "plan".to_string(),
                    "{handle}".to_string(),
                    "{repo}".to_string(),
                    "{base}".to_string(),
                    "{head}".to_string(),
                    "{task_url}".to_string(),
                    "{idempotency_key}".to_string(),
                ]),
                ensure: Some(vec![script.display().to_string(), "ensure".to_string()]),
                ..Default::default()
            },
            list_result_mapping: Some(worktrees_mapping()),
        });
        let intent = |handle: &str, head: &str| WorktreeProviderCreateIntent {
            handle: handle.to_string(),
            repo: "fixture".to_string(),
            base: "main".to_string(),
            head: head.to_string(),
            task_url: "https://example.test/issues/1".to_string(),
        };

        let existing = plan_apply_enabled_worktree_provider_from_config(
            &intent("fixture@existing", "fix/existing"),
            &config,
        )
        .expect("existing plan");
        assert!(matches!(existing, WorktreeProviderCreatePlan::Existing(_)));
        let absent = plan_apply_enabled_worktree_provider_from_config(
            &intent("fixture@fix-new", "fix/new"),
            &config,
        )
        .expect("absent plan");
        let WorktreeProviderCreatePlan::WouldCreate(absent) = absent else {
            panic!("would create")
        };
        assert_eq!(absent.worktree.path, "/provider/planned/fixture@fix-new");
        assert!(!marker.exists(), "planning must not invoke ensure");

        config
            .worktree_providers
            .get_mut("fixture")
            .unwrap()
            .commands
            .plan = None;
        let error = plan_apply_enabled_worktree_provider_from_config(
            &intent("fixture@fix-unsupported", "fix/unsupported"),
            &config,
        )
        .expect_err("unsupported planning");
        assert_eq!(error.details["worktree_provider_planning"], "unsupported");
        assert!(
            !marker.exists(),
            "unsupported planning must not invoke ensure"
        );
    }

    #[test]
    fn lifecycle_commands_resolve_all_owner_and_terminal_placeholders() {
        let intent = WorktreeProviderCreateIntent {
            handle: "fixture@release".to_string(),
            repo: "https://example.invalid/repo.git".to_string(),
            base: "main".to_string(),
            head: "0123456789abcdef".to_string(),
            task_url: "release/run-1".to_string(),
        };
        let lifecycle = WorktreeProviderLifecycleIntent {
            purpose: "release_staging".to_string(),
            owner_run_ref: "release/run-1".to_string(),
            cleanup_policy: WorktreeProviderCleanupPolicy::RemoveOnSuccess,
        };
        let ensure = expand_lifecycle_ensure_command(
            &[
                "provider".to_string(),
                "{handle}".to_string(),
                "{purpose}".to_string(),
                "{owner_run_ref}".to_string(),
                "{cleanup_policy}".to_string(),
            ],
            &intent,
            &lifecycle,
            "fixture-key",
        );
        assert_eq!(
            ensure,
            [
                "provider",
                "fixture@release",
                "release_staging",
                "release/run-1",
                "remove_on_success"
            ]
        );

        let finalization_key = worktree_provider_finalization_idempotency_key(&lifecycle);
        let finalize = [
            "{owner_outcome}",
            "{lifecycle_state}",
            "{disposition}",
            "{idempotency_key}",
        ]
        .iter()
        .map(|argument| {
            argument
                .replace(
                    "{owner_outcome}",
                    WorktreeProviderTerminalDisposition::Succeeded.owner_outcome(),
                )
                .replace("{idempotency_key}", &finalization_key)
                .replace(
                    "{lifecycle_state}",
                    WorktreeProviderTerminalDisposition::Succeeded.lifecycle_state(),
                )
                .replace(
                    "{disposition}",
                    WorktreeProviderTerminalDisposition::Succeeded.as_str(),
                )
        })
        .collect::<Vec<_>>();
        assert_eq!(
            finalize,
            [
                "success",
                "completed",
                "succeeded",
                "finalize:release/run-1"
            ]
        );
    }

    #[test]
    fn workspace_creation_selection_reports_missing_lookup_capability_before_ensure_can_run() {
        let marker = tempfile::NamedTempFile::new().expect("marker");
        std::fs::remove_file(marker.path()).expect("remove marker");
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: true,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands {
                ensure: Some(vec![fake_provider_script_body(&format!(
                    "touch '{}'",
                    marker.path().display()
                ))]),
                ..Default::default()
            },
            list_result_mapping: Some(worktrees_mapping()),
        };
        let error = select_apply_enabled_worktree_provider_from_config(
            &WorktreeProviderCreateIntent {
                handle: "missing".to_string(),
                repo: "repo".to_string(),
                base: "main".to_string(),
                head: "abc".to_string(),
                task_url: "task".to_string(),
            },
            &config_with_provider(provider),
        )
        .expect_err("missing lookup capability must reject workspace creation");
        assert_eq!(error.details["worktree_provider_id"], "fixture");
        assert_eq!(
            error.details["worktree_provider_missing_required_capabilities"],
            serde_json::json!(["resolve_or_list"])
        );
        assert!(!marker.path().exists(), "ensure command must not run");
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_provision_uses_the_selected_eligible_provider() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let selected_effect = temp.path().join("selected-effect");
        let competing_effect = temp.path().join("competing-effect");
        let selected = temp.path().join("selected-provider");
        let competing = temp.path().join("competing-provider");
        fs::write(
            &selected,
            format!(
                "#!/bin/sh\nif [ \"$1\" = resolve ]; then\n  if [ -f '{}' ]; then printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"homeboy@fix-12124\",\"path\":\"{}\",\"branch\":\"fix/12124\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'; else exit 1; fi\nelse\n  touch '{}'\n  git init -q -b fix/12124 '{}'\nfi\n",
                selected_effect.display(),
                workspace.display(),
                selected_effect.display(),
                workspace.display(),
            ),
        )
        .expect("write selected provider");
        fs::write(
            &competing,
            format!(
                "#!/bin/sh\nif [ \"$1\" = resolve ]; then exit 1; fi\ntouch '{}'\n",
                competing_effect.display(),
            ),
        )
        .expect("write competing provider");
        for script in [&selected, &competing] {
            let mut permissions = fs::metadata(script).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(script, permissions).expect("executable");
        }
        let provider = |script: &std::path::Path, apply_enabled| WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands {
                resolve: Some(vec![script.display().to_string(), "resolve".to_string()]),
                resolve_not_found_exit_codes: vec![1],
                ensure: Some(vec![script.display().to_string(), "ensure".to_string()]),
                ..Default::default()
            },
            list_result_mapping: Some(worktrees_mapping()),
        };
        let mut config = HomeboyConfig::default();
        config
            .worktree_providers
            .insert("selected".to_string(), provider(&selected, true));
        config
            .worktree_providers
            .insert("competing".to_string(), provider(&competing, false));
        let intent = WorktreeProviderCreateIntent {
            handle: "homeboy@fix-12124".to_string(),
            repo: "homeboy".to_string(),
            base: "main".to_string(),
            head: "fix/12124".to_string(),
            task_url: "https://github.com/Extra-Chill/homeboy/issues/12124".to_string(),
        };
        let lifecycle = WorktreeProviderLifecycleIntent {
            purpose: "agent_task_cook".to_string(),
            owner_run_ref: "cook-12124".to_string(),
            cleanup_policy: WorktreeProviderCleanupPolicy::RemoveOnSuccess,
        };

        let provision = provision_apply_enabled_worktree_provider_with_lifecycle_from_config(
            &intent, &lifecycle, &config,
        )
        .expect("selected provider provisions lifecycle worktree");

        assert_eq!(provision.resolution.provider_id, "selected");
        assert!(selected_effect.exists(), "selected ensure ran");
        assert!(
            !competing_effect.exists(),
            "ineligible provider was not invoked"
        );
    }

    #[cfg(unix)]
    #[test]
    fn finalization_reuses_a_stable_key_and_provider_deduplicates_the_effect() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let effects = temp.path().join("effects");
        let script = temp.path().join("provider");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nkey=\"${{10}}\"\nif [ ! -f '{0}' ] || ! grep -Fqx \"$key\" '{0}'; then printf '%s\\n' \"$key\" >> '{0}'; fi\n",
                effects.display()
            ),
        )
        .expect("write provider");
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("executable");

        let mut config = HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                commands: WorktreeProviderCommands::default(),
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                list_result_mapping: None,
            },
        );
        config.settings.insert(
            WORKTREE_PROVIDER_LIFECYCLE_SETTINGS_KEY.to_string(),
            serde_json::json!({ "fixture": { "finalize": [script.display().to_string(), "finalize", "{handle}", "{purpose}", "{owner_run_ref}", "{cleanup_policy}", "{disposition}", "{owner_outcome}", "{lifecycle_state}", "{idempotency_key}"] } }),
        );
        let resolution = WorktreeProviderResolution {
            provider_id: "fixture".to_string(),
            worktree: WorktreeProviderHandle {
                handle: "fixture@release".to_string(),
                path: temp.path().display().to_string(),
                branch: "main".to_string(),
                task_url: None,
                safety: WorktreeProviderHandleSafety {
                    dirty: false,
                    unpushed: false,
                    primary: false,
                },
            },
        };
        let lifecycle = WorktreeProviderLifecycleIntent {
            purpose: "release_staging".to_string(),
            owner_run_ref: "release/stable-owner".to_string(),
            cleanup_policy: WorktreeProviderCleanupPolicy::RemoveOnSuccess,
        };

        for _ in 0..2 {
            finalize_apply_enabled_worktree_provider_from_config(
                &resolution,
                &lifecycle,
                WorktreeProviderTerminalDisposition::Succeeded,
                &config,
            )
            .expect("provider finalization");
        }

        assert_eq!(
            fs::read_to_string(effects)
                .expect("provider effects")
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn rejects_primary_checkout_even_when_provider_metadata_is_stale() {
        let primary = tempfile::tempdir().expect("primary checkout");
        let output = Command::new("git")
            .args(["init"])
            .current_dir(primary.path())
            .output()
            .expect("initialize primary checkout");
        assert!(output.status.success());

        let error = validate_task_worktree_root(primary.path(), "repo@task")
            .expect_err("primary checkout must never become a provider workspace");
        assert!(error.message.contains("primary or non-linked checkout"));
        assert!(primary.path().join(".git").is_dir());
    }

    #[test]
    fn accepts_direct_linked_task_worktree_path() {
        let primary = tempfile::tempdir().expect("primary checkout");
        let task = tempfile::tempdir().expect("task parent");
        let output = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(primary.path())
            .output()
            .expect("initialize primary checkout");
        assert!(output.status.success());
        std::fs::write(primary.path().join("README"), "base\n").expect("write base");
        for args in [
            ["add", "README"].as_slice(),
            [
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "base",
            ]
            .as_slice(),
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(primary.path())
                .status()
                .expect("git")
                .success());
        }
        let task_path = task.path().join("repo@task");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "task",
                task_path.to_str().expect("utf8")
            ])
            .current_dir(primary.path())
            .status()
            .expect("create linked task worktree")
            .success());
        validate_task_worktree_root(&task_path, task_path.to_str().expect("utf8"))
            .expect("direct linked task worktree is valid");
    }

    /// Auto-creation reads as a default capability of `--to-worktree`, so the
    /// refusal has to say that it is configuration-gated and hand back commands
    /// the caller can run — not a description of a JSON shape.
    #[test]
    fn provision_without_an_ensure_provider_reports_runnable_next_steps() {
        let config = HomeboyConfig::default();

        let error = provision_apply_enabled_worktree_provider_from_config(
            &WorktreeProviderCreateIntent {
                handle: "homeboy@11168-wire-compiler-warning-provider".to_string(),
                repo: "homeboy".to_string(),
                base: "main".to_string(),
                head: "fix/11168-wire-compiler-warning-provider".to_string(),
                task_url: "https://github.com/Extra-Chill/homeboy/issues/11168".to_string(),
            },
            &config,
        )
        .expect_err("no configured provider can create the destination");

        assert!(
            error
                .message
                .contains("no enabled worktree provider configures commands.ensure"),
            "{}",
            error.message
        );
        let tried = error.details["tried"]
            .as_array()
            .expect("remediation")
            .iter()
            .map(|value| value.as_str().expect("remediation string").to_string())
            .collect::<Vec<_>>();
        assert!(
            tried.iter().any(|action| action.contains(
                "homeboy worktree create homeboy --branch fix/11168-wire-compiler-warning-provider --from main --task-url https://github.com/Extra-Chill/homeboy/issues/11168"
            )),
            "{tried:?}"
        );
        assert!(
            tried
                .iter()
                .any(|action| action.contains("homeboy config set /worktree_providers/")),
            "{tried:?}"
        );
    }

    /// The handle is the branch slugified, so a caller who guessed it otherwise
    /// reads an error about a handle that was never going to exist.
    #[test]
    fn provision_names_the_handle_the_branch_actually_slugifies_to() {
        let config = HomeboyConfig::default();

        let error = provision_apply_enabled_worktree_provider_from_config(
            &WorktreeProviderCreateIntent {
                handle: "homeboy@11168-wire-compiler-warning-provider".to_string(),
                repo: "homeboy".to_string(),
                base: "main".to_string(),
                head: "fix/11168-wire-compiler-warning-provider".to_string(),
                task_url: "https://github.com/Extra-Chill/homeboy/issues/11168".to_string(),
            },
            &config,
        )
        .expect_err("no configured provider can create the destination");

        let tried = error.details["tried"]
            .as_array()
            .expect("remediation")
            .iter()
            .map(|value| value.as_str().expect("remediation string").to_string())
            .collect::<Vec<_>>();
        assert!(
            tried
                .iter()
                .any(|action| action.contains("homeboy@fix-11168-wire-compiler-warning-provider")),
            "{tried:?}"
        );
    }

    /// A caller who already passed the slugified handle must not be told its own
    /// handle is wrong.
    #[test]
    fn provision_does_not_claim_a_mismatch_when_the_handle_already_matches() {
        let config = HomeboyConfig::default();

        let error = provision_apply_enabled_worktree_provider_from_config(
            &WorktreeProviderCreateIntent {
                handle: "homeboy@fix-11168".to_string(),
                repo: "homeboy".to_string(),
                base: "main".to_string(),
                head: "fix/11168".to_string(),
                task_url: "https://github.com/Extra-Chill/homeboy/issues/11168".to_string(),
            },
            &config,
        )
        .expect_err("no configured provider can create the destination");

        let tried = error.details["tried"]
            .as_array()
            .expect("remediation")
            .iter()
            .map(|value| value.as_str().expect("remediation string").to_string())
            .collect::<Vec<_>>();
        assert!(
            !tried
                .iter()
                .any(|action| action.contains("Handles slugify the branch")),
            "{tried:?}"
        );
        assert!(
            tried
                .iter()
                .any(|action| action.contains("homeboy worktree create homeboy")),
            "{tried:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn provision_creates_missing_destination_from_explicit_generic_intent() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("destination");
        let state = temp.path().join("state");
        let script = temp.path().join("provider");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$1\" = resolve ]; then\n  if [ -f '{}' ]; then\n    printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"homeboy@fix-9908\",\"path\":\"{}\",\"branch\":\"fix/9908\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n  else\n    exit 1\n  fi\nelse\n  sleep 2\n  git init -b fix/9908 '{}' >/dev/null\n  printf '%s|%s|%s|%s|%s|%s' \"$2\" \"$3\" \"$4\" \"$5\" \"$6\" \"$7\" > '{}'\nfi\n",
                state.display(),
                workspace.display(),
                workspace.display(),
                state.display(),
            ),
        )
        .expect("write provider");
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("executable");
        let mut config = HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![
                        script.display().to_string(),
                        "resolve".to_string(),
                        "{handle}".to_string(),
                    ]),
                    resolve_not_found_exit_codes: vec![1],
                    ensure: Some(vec![
                        script.display().to_string(),
                        "create".to_string(),
                        "{handle}".to_string(),
                        "{repo}".to_string(),
                        "{base}".to_string(),
                        "{head}".to_string(),
                        "{task_url}".to_string(),
                        "{idempotency_key}".to_string(),
                    ]),
                    ..Default::default()
                },
                list_result_mapping: Some(WorktreeProviderListResultMapping {
                    items: "$.worktrees".to_string(),
                    handle: "$.handle".to_string(),
                    path: "$.path".to_string(),
                    branch: "$.branch".to_string(),
                    dirty: "$.safety.dirty".to_string(),
                    unpushed: "$.safety.unpushed".to_string(),
                    primary: "$.safety.primary".to_string(),
                    task_url: None,
                }),
            },
        );

        let resolution = provision_apply_enabled_worktree_provider_from_config(
            &WorktreeProviderCreateIntent {
                handle: "homeboy@fix-9908".to_string(),
                repo: "homeboy".to_string(),
                base: "main".to_string(),
                head: "fix/9908".to_string(),
                task_url: "https://github.com/Extra-Chill/homeboy/issues/9908".to_string(),
            },
            &config,
        )
        .expect("provider creates then resolves destination");

        assert_eq!(resolution.action, "ensured");
        assert_eq!(resolution.resolution.provider_id, "fixture");
        assert_eq!(resolution.resolution.worktree.handle, "homeboy@fix-9908");
        assert_eq!(
            fs::read_to_string(state).expect("creation intent"),
            "homeboy@fix-9908|homeboy|main|fix/9908|https://github.com/Extra-Chill/homeboy/issues/9908|homeboy@fix-9908:homeboy:main:fix/9908"
        );
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_identical_provisioning_reuses_one_destination_and_idempotency_key() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, Barrier};

        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("destination");
        let lock = temp.path().join("ensure-lock");
        let keys = temp.path().join("keys");
        let script = temp.path().join("provider");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$1\" = resolve ]; then\n  if [ -d '{}' ]; then\n    printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"homeboy@fix-9908\",\"path\":\"{}\",\"branch\":\"fix/9908\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n  else\n    printf '%s\\n' '{{\"worktrees\":[]}}'\n  fi\nelse\n  printf '%s\\n' \"$7\" >> '{}'\n  if mkdir '{}' 2>/dev/null; then\n    git init -b fix/9908 '{}' >/dev/null\n    rmdir '{}'\n  else\n    while [ -d '{}' ]; do sleep 0.01; done\n  fi\nfi\n",
                workspace.display(),
                workspace.display(),
                keys.display(),
                lock.display(),
                workspace.display(),
                lock.display(),
                lock.display(),
            ),
        )
        .expect("write provider");
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("executable");

        let mut config = HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![
                        script.display().to_string(),
                        "resolve".to_string(),
                        "{handle}".to_string(),
                    ]),
                    ensure: Some(vec![
                        script.display().to_string(),
                        "ensure".to_string(),
                        "{handle}".to_string(),
                        "{repo}".to_string(),
                        "{base}".to_string(),
                        "{head}".to_string(),
                        "{task_url}".to_string(),
                        "{idempotency_key}".to_string(),
                    ]),
                    ..Default::default()
                },
                list_result_mapping: Some(WorktreeProviderListResultMapping {
                    items: "$.worktrees".to_string(),
                    handle: "$.handle".to_string(),
                    path: "$.path".to_string(),
                    branch: "$.branch".to_string(),
                    dirty: "$.safety.dirty".to_string(),
                    unpushed: "$.safety.unpushed".to_string(),
                    primary: "$.safety.primary".to_string(),
                    task_url: None,
                }),
            },
        );
        let intent = WorktreeProviderCreateIntent {
            handle: "homeboy@fix-9908".to_string(),
            repo: "homeboy".to_string(),
            base: "main".to_string(),
            head: "fix/9908".to_string(),
            task_url: "https://github.com/Extra-Chill/homeboy/issues/9908".to_string(),
        };
        let barrier = Arc::new(Barrier::new(2));
        let mut joins = Vec::new();
        for _ in 0..2 {
            let config = config.clone();
            let intent = intent.clone();
            let barrier = barrier.clone();
            joins.push(std::thread::spawn(move || {
                barrier.wait();
                provision_apply_enabled_worktree_provider_from_config(&intent, &config)
                    .expect("concurrent ensure converges")
            }));
        }
        let provisions = joins
            .into_iter()
            .map(|join| join.join().expect("thread"))
            .collect::<Vec<_>>();

        assert!(workspace.is_dir());
        assert!(
            provisions
                .iter()
                .all(|provision| provision.resolution.worktree.path
                    == workspace.display().to_string())
        );
        assert!(
            provisions
                .iter()
                .all(|provision| provision.idempotency_key
                    == "homeboy@fix-9908:homeboy:main:fix/9908")
        );
        let keys = fs::read_to_string(keys).expect("ensure keys");
        assert!(keys
            .lines()
            .all(|key| key == "homeboy@fix-9908:homeboy:main:fix/9908"));
    }

    #[cfg(unix)]
    #[test]
    fn lookup_auth_and_malformed_failures_do_not_run_ensure() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let ensured = temp.path().join("ensured");
        let script = temp.path().join("provider");
        fs::write(&script, format!("#!/bin/sh\nif [ \"$1\" = resolve ]; then\n  case \"$2\" in auth) exit 77 ;; malformed) printf '{{' ;; esac\n  exit 0\nfi\ntouch '{}'\n", ensured.display())).expect("write provider");
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("executable");
        let mut config = HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![
                        script.display().to_string(),
                        "resolve".to_string(),
                        "{handle}".to_string(),
                    ]),
                    ensure: Some(vec![script.display().to_string(), "ensure".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            },
        );
        for (handle, classification) in [("auth", "command"), ("malformed", "malformed")] {
            let error = provision_apply_enabled_worktree_provider_from_config(
                &WorktreeProviderCreateIntent {
                    handle: handle.to_string(),
                    repo: "homeboy".to_string(),
                    base: "main".to_string(),
                    head: "fix/9908".to_string(),
                    task_url: "https://example.test/9908".to_string(),
                },
                &config,
            )
            .expect_err("lookup failure is not absence");
            assert!(error.message.contains("provider `fixture` resolve command"));
            assert_eq!(
                error.details["worktree_provider_call_classification"],
                classification
            );
            assert_eq!(error.details["worktree_provider_id"], "fixture");
            assert!(error.details["worktree_provider_replay_command"]
                .as_str()
                .expect("replay command")
                .contains(script.to_string_lossy().as_ref()));
        }
        assert!(
            !ensured.exists(),
            "ensure must never follow an auth or malformed lookup failure"
        );
    }

    #[test]
    fn dry_run_executes_preview_command_and_parses_json() {
        let script = fake_provider_script();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: vec!["fixture".to_string()],
                all_providers: false,
                apply: false,
                timeout: None,
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    cleanup_preview: Some(vec![script, "preview".to_string()]),
                    cleanup_apply: None,
                    ..Default::default()
                },
                list_result_mapping: None,
            }),
        )
        .expect("cleanup succeeds");

        assert_eq!(output.mode, WorktreeProviderCleanupMode::Preview);
        assert_eq!(output.success_count, 1);
        assert_eq!(output.failure_count, 0);
        assert_eq!(output.providers[0].status, Some(0));
        assert_eq!(
            output.providers[0].parsed_payload,
            Some(json!({ "mode": "preview" }))
        );
        assert_eq!(output.providers[0].timeout_ms, 30_000);
    }

    #[test]
    fn cleanup_budget_selects_configured_values_and_honors_aggregate_caps() {
        let provider = WorktreeProviderConfig {
            commands: WorktreeProviderCommands {
                cleanup_preview_timeout_ms: 45_000,
                cleanup_apply_timeout_ms: 90_000,
                ..Default::default()
            },
            ..default_command_provider()
        };

        assert_eq!(
            provider_cleanup_timeout(&provider, &WorktreeProviderCleanupMode::Preview, None)
                .expect("configured preview budget")
                .as_millis(),
            45_000
        );
        assert_eq!(
            provider_cleanup_timeout(
                &provider,
                &WorktreeProviderCleanupMode::Preview,
                Some(Duration::from_secs(20)),
            )
            .expect("capped preview budget")
            .as_millis(),
            20_000
        );
        assert_eq!(
            provider_cleanup_timeout(
                &provider,
                &WorktreeProviderCleanupMode::Apply,
                Some(Duration::from_secs(120)),
            )
            .expect("uncapped apply budget")
            .as_millis(),
            90_000
        );
    }

    #[test]
    fn resolved_cleanup_budget_is_reported_for_early_failures() {
        let cases = [
            (
                "disabled",
                WorktreeProviderCleanupMode::Preview,
                WorktreeProviderConfig {
                    enabled: false,
                    ..default_command_provider()
                },
            ),
            (
                "apply-disabled",
                WorktreeProviderCleanupMode::Apply,
                WorktreeProviderConfig {
                    apply_enabled: false,
                    commands: WorktreeProviderCommands {
                        cleanup_apply: Some(vec![fake_provider_script()]),
                        ..Default::default()
                    },
                    ..default_command_provider()
                },
            ),
            (
                "missing-command",
                WorktreeProviderCleanupMode::Preview,
                default_command_provider(),
            ),
            (
                "invalid-argv",
                WorktreeProviderCleanupMode::Preview,
                WorktreeProviderConfig {
                    commands: WorktreeProviderCommands {
                        cleanup_preview: Some(vec![String::new()]),
                        ..Default::default()
                    },
                    ..default_command_provider()
                },
            ),
            (
                "spawn",
                WorktreeProviderCleanupMode::Preview,
                WorktreeProviderConfig {
                    commands: WorktreeProviderCommands {
                        cleanup_preview: Some(vec!["/definitely/missing/provider".to_string()]),
                        ..Default::default()
                    },
                    ..default_command_provider()
                },
            ),
        ];

        for (name, mode, provider) in cases {
            let result =
                run_provider_cleanup(name, &provider, mode, Some(Duration::from_millis(750)));
            assert_eq!(result.timeout_ms, 750, "{name}");
            assert_eq!(
                result.outcome,
                WorktreeProviderCleanupOutcome::Failed,
                "{name}"
            );
        }
    }

    #[test]
    fn configured_preview_timeout_allows_a_slow_provider() {
        let dir = unique_fixture_script_dir();
        let started = dir.join("started");
        let release = dir.join("release");
        let script = waiting_provider_script(&started, &release, "preview");
        let cleanup = std::thread::spawn(move || {
            cleanup_worktree_providers_from_config(
                WorktreeProviderCleanupOptions {
                    provider: vec!["fixture".to_string()],
                    all_providers: false,
                    apply: false,
                    timeout: None,
                },
                config_with_provider(WorktreeProviderConfig {
                    enabled: true,
                    kind: WorktreeProviderKind::Command,
                    apply_enabled: false,
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    commands: WorktreeProviderCommands {
                        cleanup_preview: Some(vec![script]),
                        cleanup_preview_timeout_ms: 2_000,
                        ..Default::default()
                    },
                    list_result_mapping: None,
                }),
            )
        });
        wait_for_path(&started);
        fs::write(&release, "release").expect("release provider");
        let output = cleanup
            .join()
            .expect("cleanup thread")
            .expect("configured timeout allows provider response");

        assert_eq!(
            output.providers[0].outcome,
            WorktreeProviderCleanupOutcome::Completed
        );
        assert_eq!(output.providers[0].timeout_ms, 2_000);
    }

    #[test]
    fn preview_and_apply_use_distinct_configured_budgets() {
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: true,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands {
                cleanup_preview_timeout_ms: 25,
                cleanup_apply_timeout_ms: 2_000,
                ..Default::default()
            },
            list_result_mapping: None,
        };

        assert_eq!(
            provider_cleanup_timeout(&provider, &WorktreeProviderCleanupMode::Preview, None)
                .expect("preview budget")
                .as_millis(),
            25
        );
        assert_eq!(
            provider_cleanup_timeout(&provider, &WorktreeProviderCleanupMode::Apply, None)
                .expect("apply budget")
                .as_millis(),
            2_000
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_timeout_terminates_the_provider_process_tree() {
        let dir = unique_fixture_script_dir();
        let marker = dir.join("orphaned-child");
        let child_started = dir.join("child-started");
        let permit_timeout = dir.join("permit-timeout");
        let release = dir.join("release");
        let script = fake_provider_script_body(&format!(
            "(touch '{}'; while [ ! -f '{}' ]; do sleep 0.01; done; touch '{}') &\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nwhile :; do sleep 1; done\n",
            child_started.display(),
            release.display(),
            marker.display(),
            permit_timeout.display(),
        ));
        let cleanup = std::thread::spawn(move || {
            cleanup_worktree_providers_from_config(
                WorktreeProviderCleanupOptions {
                    provider: vec!["fixture".to_string()],
                    all_providers: false,
                    apply: false,
                    timeout: None,
                },
                config_with_provider(WorktreeProviderConfig {
                    enabled: true,
                    kind: WorktreeProviderKind::Command,
                    apply_enabled: false,
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    commands: WorktreeProviderCommands {
                        cleanup_preview: Some(vec![script]),
                        cleanup_preview_timeout_ms: 2_000,
                        ..Default::default()
                    },
                    list_result_mapping: None,
                }),
            )
        });
        wait_for_path(&child_started);
        fs::write(&permit_timeout, "permit timeout").expect("permit timeout path");
        let output = cleanup
            .join()
            .expect("cleanup thread")
            .expect("timeout returns cleanup evidence");

        assert_eq!(
            output.providers[0].outcome,
            WorktreeProviderCleanupOutcome::TimedOut
        );
        fs::write(&release, "release").expect("release orphaned child");
        assert_path_remains_absent(&marker);
    }

    #[test]
    fn cleanup_provider_liveness_is_bounded_and_reports_partial_inventory() {
        let hanging = fake_provider_script_body("sleep 60\n");
        let failing = fake_provider_script_body("printf 'provider failed\\n' >&2\nexit 23\n");
        let provider = |script: String| WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands {
                cleanup_preview: Some(vec![script]),
                ..Default::default()
            },
            list_result_mapping: None,
        };

        let mut config = HomeboyConfig::default();
        config
            .worktree_providers
            .insert("hanging".to_string(), provider(hanging));
        config
            .worktree_providers
            .insert("failing".to_string(), provider(failing));
        let started = std::time::Instant::now();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: Vec::new(),
                all_providers: true,
                apply: false,
                timeout: Some(Duration::from_millis(1_500)),
            },
            config,
        )
        .expect("bounded cleanup completes");

        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            output.inventory_completeness,
            WorktreeProviderInventoryCompleteness::Partial
        );
        let hanging = output
            .providers
            .iter()
            .find(|row| row.provider_id == "hanging")
            .expect("hanging result");
        assert_eq!(hanging.outcome, WorktreeProviderCleanupOutcome::TimedOut);
        assert_eq!(
            hanging.inventory_completeness,
            WorktreeProviderInventoryCompleteness::Partial
        );
        let failing = output
            .providers
            .iter()
            .find(|row| row.provider_id == "failing")
            .expect("failing result");
        assert_eq!(failing.outcome, WorktreeProviderCleanupOutcome::Failed);
    }

    #[test]
    fn apply_refuses_when_provider_apply_is_disabled() {
        let script = fake_provider_script();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: vec!["fixture".to_string()],
                all_providers: false,
                apply: true,
                timeout: None,
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    cleanup_apply: Some(vec![script, "apply".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            }),
        )
        .expect("cleanup reports refusal");

        assert_eq!(output.success_count, 0);
        assert_eq!(output.failure_count, 1);
        assert_eq!(output.providers[0].command_run, None);
        assert_eq!(
            output.providers[0].error.as_deref(),
            Some("provider apply is not enabled")
        );
    }

    #[test]
    fn apply_executes_apply_command_when_enabled() {
        let script = fake_provider_script();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: vec!["fixture".to_string()],
                all_providers: false,
                apply: true,
                timeout: None,
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    cleanup_apply: Some(vec![script, "apply".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            }),
        )
        .expect("cleanup succeeds");

        assert_eq!(output.mode, WorktreeProviderCleanupMode::Apply);
        assert_eq!(output.success_count, 1);
        assert_eq!(
            output.providers[0].parsed_payload,
            Some(json!({ "mode": "apply" }))
        );
    }

    #[test]
    fn resolves_a_clean_provider_managed_handle_with_targeted_command() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let script = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }]
        }));
        let handle = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![script, "{handle}".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect("provider handle resolves");

        assert_eq!(handle.path, workspace.path().display().to_string());
        assert_eq!(handle.branch, "cook-target");
    }

    #[test]
    fn path_resolution_rejects_a_provider_declared_primary_before_work_starts() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "main");
        let script = fake_list_provider_script(json!({ "worktrees": [{
            "handle": "fixture",
            "path": workspace.path(),
            "branch": "main",
            "safety": { "dirty": false, "unpushed": false, "primary": true }
        }]}));

        let error = resolve_worktree_provider_path_from_config(
            workspace.path(),
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    list: Some(vec![script]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect_err("provider primary must fail before rig work starts");

        assert!(error.message.contains("primary"));
        assert_eq!(error.details["worktree_provider_lookup"], Value::Null);
    }

    #[test]
    fn path_resolution_falls_back_to_list_when_resolve_path_is_absent() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "fix/10251");
        let script = fake_list_provider_script(json!({ "worktrees": [{
            "handle": "fixture@fix-10251",
            "path": workspace.path(),
            "branch": "fix/10251",
            "safety": { "dirty": false, "unpushed": false, "primary": false }
        }]}));

        let resolution = resolve_worktree_provider_path_from_config(
            workspace.path(),
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    list: Some(vec![script]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect("provider lookup")
        .expect("exact path is provider managed");

        assert_eq!(resolution.provider_id, "fixture");
        assert_eq!(resolution.worktree.handle, "fixture@fix-10251");
        assert_eq!(
            resolution.worktree.path,
            workspace.path().display().to_string()
        );
    }

    #[test]
    fn path_resolution_prefers_resolve_path_and_expands_the_canonical_path() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let requested = std::fs::canonicalize(workspace.path()).expect("canonical workspace");
        let script = fake_provider_script_body(&format!(
            "if [ \"$1\" != \"{}\" ]; then exit 44; fi\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@cook-target\",\"path\":\"{}\",\"branch\":\"cook-target\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
            requested.display(),
            requested.display(),
        ));
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands {
                resolve_path: Some(vec![script, "{path}".to_string()]),
                ..Default::default()
            },
            list_result_mapping: Some(worktrees_mapping()),
        };

        let resolution = resolve_worktree_provider_path_from_config(
            workspace.path(),
            &config_with_provider(provider),
        )
        .expect("path lookup succeeds")
        .expect("provider owns requested path");
        assert_eq!(resolution.worktree.path, requested.display().to_string());
    }

    #[test]
    fn path_resolution_resolve_path_not_found_does_not_fall_back_to_list() {
        let workspace = tempfile::tempdir().expect("workspace");
        let marker = workspace.path().join("list-invoked");
        let list =
            fake_list_provider_script_with_marker(serde_json::json!({ "worktrees": [] }), &marker);
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands {
                resolve_path: Some(vec![
                    fake_provider_script_body("exit 42\n"),
                    "{path}".to_string(),
                ]),
                resolve_not_found_exit_codes: vec![42],
                list: Some(vec![list]),
                ..Default::default()
            },
            list_result_mapping: Some(worktrees_mapping()),
        };

        assert!(resolve_worktree_provider_path_from_config(
            workspace.path(),
            &config_with_provider(provider)
        )
        .expect("not found is not an error")
        .is_none());
        assert!(!marker.exists(), "list fallback must not run");
    }

    #[test]
    fn path_resolution_resolve_path_rejects_malformed_or_mismatched_results() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let other = tempfile::tempdir().expect("other workspace");
        git_init(other.path(), "cook-target");
        let mismatched = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@other",
                "path": other.path(),
                "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }]
        }));
        for (script, expected) in [
            (
                fake_provider_script_body("printf '{'\n"),
                "returned invalid JSON",
            ),
            (mismatched, "did not return the requested canonical path"),
        ] {
            let provider = WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    resolve_path: Some(vec![script, "{path}".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            };
            let error = resolve_worktree_provider_path_from_config(
                workspace.path(),
                &config_with_provider(provider),
            )
            .expect_err("invalid targeted path result must fail closed");
            assert!(error.message.contains(expected), "{}", error.message);
        }
    }

    #[test]
    fn path_resolution_retries_a_transient_resolve_path_timeout() {
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            lookup_timeout_ms: 25,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands::default(),
            list_result_mapping: Some(worktrees_mapping()),
        };
        let mut attempts = 0;
        let result = retry_resolve_path_timeouts(&provider, &["fixture".to_string()], || {
            attempts += 1;
            if attempts == 2 {
                return Ok(Vec::new());
            }
            let mut error =
                Error::validation_invalid_argument("to_worktree", "timeout", None, None);
            error.details["worktree_provider_lookup"] = Value::String("timed_out".to_string());
            error.details["provider_lookup_timeout_kind"] =
                Value::String("supervision".to_string());
            error.details["latency_ms"] = Value::from(25);
            Err(error)
        });

        assert!(result.is_ok());
        assert_eq!(attempts, 2);
    }

    #[test]
    fn path_resolution_retries_after_terminating_the_timed_out_provider_process() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let requested = std::fs::canonicalize(workspace.path()).expect("canonical workspace");
        let first_attempt = tempfile::NamedTempFile::new().expect("first attempt marker");
        std::fs::remove_file(first_attempt.path()).expect("remove first attempt marker");
        let active = tempfile::NamedTempFile::new().expect("active marker");
        std::fs::remove_file(active.path()).expect("remove active marker");
        let script = fake_provider_script_body(&format!(
            "if mkdir '{first_attempt}'; then trap 'rm -f \"{active}\"; exit 0' TERM EXIT\n  touch '{active}'\n  sleep 8\n  exit 0\nfi\nif [ -e '{active}' ]; then exit 99; fi\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@cook-target\",\"path\":\"{path}\",\"branch\":\"cook-target\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
            first_attempt = first_attempt.path().display(),
            active = active.path().display(),
            path = requested.display(),
        ));
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            lookup_timeout_ms: 4_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands {
                resolve_path: Some(vec![script, "{path}".to_string()]),
                ..Default::default()
            },
            list_result_mapping: Some(worktrees_mapping()),
        };

        let resolution = resolve_worktree_provider_path_from_config(
            workspace.path(),
            &config_with_provider(provider),
        )
        .expect("second invocation succeeds after first process terminates")
        .expect("provider owns requested path");

        assert_eq!(resolution.worktree.path, requested.display().to_string());
        assert!(!active.path().exists(), "timed-out provider was reaped");
    }

    #[test]
    fn path_resolution_reports_exhausted_resolve_path_timeout_evidence() {
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            lookup_timeout_ms: 25,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands::default(),
            list_result_mapping: Some(worktrees_mapping()),
        };
        let command = vec!["fixture".to_string(), "--token=secret".to_string()];
        let mut attempts = 0;
        let error = retry_resolve_path_timeouts(&provider, &command, || {
            attempts += 1;
            let mut error =
                Error::validation_invalid_argument("to_worktree", "timeout", None, None);
            error.details["worktree_provider_lookup"] = Value::String("timed_out".to_string());
            error.details["provider_lookup_timeout_kind"] =
                Value::String("supervision".to_string());
            error.details["latency_ms"] = Value::from(25);
            error.retryable = Some(true);
            Err(error)
        })
        .expect_err("both bounded attempts time out");

        let evidence = &error.details["worktree_provider_resolve_path_attempts"];
        assert_eq!(error.retryable, Some(true));
        assert_eq!(evidence["attempt_count"], 2);
        assert_eq!(attempts, 2);
        assert_eq!(evidence["configured_execution_budget_ms"], 50);
        assert!(evidence["observed_total_elapsed_ms"].as_u64().is_some());
        assert_eq!(evidence["attempts"].as_array().expect("attempts").len(), 2);
        assert!(evidence["attempts"][0]["elapsed_ms"].as_u64().is_some());
        let replay = evidence["replay_command"].as_str().expect("replay command");
        assert!(replay.contains("[REDACTED]"));
        assert!(!replay.contains("secret"));
    }

    #[test]
    fn path_resolution_reports_provider_timeout() {
        let workspace = tempfile::tempdir().expect("workspace");
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(
            &script,
            "#!/bin/sh\nsleep 0.2\nprintf '{\"worktrees\":[]}'\n",
        )
        .expect("write provider");
        make_executable(&script);

        let error = resolve_worktree_provider_path_from_config(
            workspace.path(),
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 25,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    list: Some(vec![script.to_string_lossy().to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect_err("a hung provider must be bounded");

        assert!(error.message.contains("timed out"));
        assert!(error.message.contains("configured lookup_timeout_ms: 25"));
        assert_eq!(error.retryable, Some(true));
        assert_eq!(error.details["worktree_provider_lookup"], "timed_out");
        assert_eq!(error.details["worktree_provider_operation"], "list");
        assert_eq!(
            error.details["worktree_provider_call_classification"],
            "timeout"
        );
        assert!(error.details["worktree_provider_replay_command"]
            .as_str()
            .expect("replay command")
            .contains(script.to_string_lossy().as_ref()));
    }

    #[test]
    fn failed_provider_timeout_envelope_is_retryable_and_redacted() {
        let script = fake_list_provider_script(json!({
            "status": "failed",
            "error": {
                "code": "git_command_timeout",
                "access_token": "provider-secret-must-not-persist"
            }
        }));
        let error = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![
                        script,
                        "{handle}".to_string(),
                        "token=secret".to_string(),
                    ]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect_err("explicit failed timeout remains retryable");

        assert_eq!(error.retryable, Some(true));
        assert_eq!(error.details["worktree_provider_lookup"], "timed_out");
        assert_eq!(error.details["worktree_provider_operation"], "resolve");
        assert_eq!(
            error.details["worktree_provider_call_classification"],
            "timeout"
        );
        assert_eq!(
            error.details["worktree_provider_phase"],
            "worktree_provider_resolve"
        );
        assert!(error.details["worktree_provider_replay_command"]
            .as_str()
            .expect("replay command")
            .contains("[REDACTED]"));
        assert_eq!(
            error.details["provider_timeout_attribution"],
            json!({ "error_code": "git_command_timeout" })
        );
        assert!(!error
            .details
            .to_string()
            .contains("provider-secret-must-not-persist"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_uses_the_configured_provider_bound_and_typed_evidence() {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(&script, "#!/bin/sh\nsleep 1\n").expect("write provider");
        make_executable(&script);
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: true,
            lookup_timeout_ms: 1,
            mutation_timeout_ms: 25,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands::default(),
            list_result_mapping: None,
        };

        let error = run_provider_ensure_command(
            "fixture",
            &provider,
            &[
                script.to_string_lossy().to_string(),
                "token=secret".to_string(),
            ],
        )
        .expect_err("ensure is supervised by the configured provider budget");

        assert_eq!(error.retryable, Some(true));
        assert_eq!(error.details["worktree_provider_operation"], "ensure");
        assert_eq!(
            error.details["worktree_provider_call_classification"],
            "timeout"
        );
        assert_eq!(
            error.details["worktree_provider_phase"],
            "worktree_provider_ensure"
        );
        assert!(error.details["worktree_provider_replay_command"]
            .as_str()
            .expect("replay command")
            .contains("[REDACTED]"));
    }

    #[cfg(unix)]
    #[test]
    fn provider_mutation_evidence_redacts_command_and_output_secrets() {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'stdout_token=provider-secret-must-not-persist\\n'\nprintf 'stderr_token=provider-secret-must-not-persist\\n' >&2\nexit 1\n",
        )
        .expect("write provider");
        make_executable(&script);
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: true,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands::default(),
            list_result_mapping: None,
        };

        let error = run_provider_mutation_command(
            "fixture",
            &provider,
            &[
                script.to_string_lossy().to_string(),
                "token=provider-secret-must-not-persist".to_string(),
            ],
            "finalize",
        )
        .expect_err("failed provider mutation returns evidence");

        assert_eq!(error.details["worktree_provider_operation"], "finalize");
        assert_eq!(
            error.details["worktree_provider_phase"],
            "worktree_provider_finalize"
        );
        assert!(error.details["command_evidence"]["command"]
            .as_str()
            .expect("command evidence")
            .contains("[REDACTED]"));
        assert!(!error
            .details
            .to_string()
            .contains("provider-secret-must-not-persist"));
    }

    #[cfg(unix)]
    #[test]
    fn malformed_split_provider_results_have_typed_redacted_replay_evidence() {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(&script, "#!/bin/sh\nprintf '%s\\n' '{\"schema\":false}'\n")
            .expect("write provider");
        make_executable(&script);
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: true,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands::default(),
            list_result_mapping: None,
        };

        for (operation, result) in [
            (
                "resolve_identity",
                run_provider_identity_command(
                    "fixture",
                    &provider,
                    &[
                        script.to_string_lossy().to_string(),
                        "token=provider-secret-must-not-persist".to_string(),
                    ],
                    "fixture@target",
                )
                .map(|_| ()),
            ),
            (
                "attest_safety",
                run_provider_safety_command(
                    "fixture",
                    &provider,
                    &[
                        script.to_string_lossy().to_string(),
                        "token=provider-secret-must-not-persist".to_string(),
                    ],
                    "provider-secret-must-not-persist",
                )
                .map(|_| ()),
            ),
        ] {
            let error = result.expect_err("malformed split response is rejected");
            assert_eq!(error.details["worktree_provider_operation"], operation);
            assert_eq!(
                error.details["worktree_provider_call_classification"],
                "malformed"
            );
            assert!(error.details["worktree_provider_replay_command"]
                .as_str()
                .expect("replay command")
                .contains("[REDACTED]"));
            assert!(!error
                .details
                .to_string()
                .contains("provider-secret-must-not-persist"));
        }
    }

    #[test]
    fn path_resolution_does_not_retry_a_provider_reported_timeout() {
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            lookup_timeout_ms: 25,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands::default(),
            list_result_mapping: Some(worktrees_mapping()),
        };
        let mut attempts = 0;
        let error = retry_resolve_path_timeouts(&provider, &["fixture".to_string()], || {
            attempts += 1;
            let mut error =
                Error::validation_invalid_argument("to_worktree", "timeout", None, None);
            error.details["worktree_provider_lookup"] = Value::String("timed_out".to_string());
            error.details["provider_timeout_attribution"] =
                json!({ "error_code": "git_command_timeout" });
            error.retryable = Some(true);
            Err(error)
        })
        .expect_err("provider-reported timeout does not retry");

        assert_eq!(
            error.details["provider_timeout_attribution"],
            json!({ "error_code": "git_command_timeout" })
        );
        assert_eq!(
            error.details["worktree_provider_resolve_path_attempts"],
            Value::Null
        );
        assert_eq!(attempts, 1);
    }

    #[test]
    fn successful_payload_timeout_metadata_does_not_classify_a_timeout() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let script = fake_list_provider_script(json!({
            "status": "completed",
            "metadata": { "error": { "code": "git_command_timeout" } },
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }]
        }));
        let resolved = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![script, "{handle}".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect("successful payload metadata is not an error envelope");

        assert_eq!(resolved.handle, "fixture@cook-target");
    }

    #[test]
    fn configured_lookup_timeout_allows_a_slow_provider_response() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let script = fake_provider_script_body(&format!(
            "sleep 0.1\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@cook-target\",\"path\":\"{}\",\"branch\":\"cook-target\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
            workspace.path().display()
        ));
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            lookup_timeout_ms: 5_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands {
                resolve: Some(vec![script, "{handle}".to_string()]),
                ..Default::default()
            },
            list_result_mapping: Some(worktrees_mapping()),
        };

        let resolved = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(provider),
        )
        .expect("configured timeout allows provider response");
        assert_eq!(resolved.path, workspace.path().display().to_string());
    }

    #[test]
    fn configured_lookup_output_limit_accepts_large_provider_json() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let script = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }],
            "padding": "x".repeat(70 * 1024),
        }));
        let provider = WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 128 * 1024,
            commands: WorktreeProviderCommands {
                resolve: Some(vec![script, "{handle}".to_string()]),
                ..Default::default()
            },
            list_result_mapping: Some(worktrees_mapping()),
        };

        let resolved = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(provider),
        )
        .expect("configured output limit retains complete provider JSON");
        assert_eq!(resolved.path, workspace.path().display().to_string());
    }

    #[test]
    fn resolution_retains_the_selected_provider_identity() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let script = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }]
        }));

        let resolution = resolve_worktree_provider_from_config(
            "fixture@cook-target",
            &config_with_provider(list_provider(script, worktrees_mapping())),
        )
        .expect("provider handle resolves");

        assert_eq!(resolution.provider_id, "fixture");
        assert_eq!(
            resolution.worktree.path,
            workspace.path().display().to_string()
        );
    }

    #[test]
    fn unresolved_handle_reports_sorted_configured_provider_ids() {
        let mut config = HomeboyConfig::default();
        for provider_id in ["zeta", "alpha"] {
            config.worktree_providers.insert(
                provider_id.to_string(),
                WorktreeProviderConfig {
                    enabled: false,
                    kind: WorktreeProviderKind::Command,
                    apply_enabled: false,
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    commands: WorktreeProviderCommands::default(),
                    list_result_mapping: None,
                },
            );
        }

        let error = resolve_worktree_provider_from_config("fixture@missing", &config)
            .expect_err("missing handle fails");

        assert!(
            error
                .message
                .contains("configured provider(s): alpha, zeta"),
            "{}",
            error.message
        );
    }

    #[test]
    fn resolve_does_not_invoke_list_when_both_commands_are_configured() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let marker = workspace.path().join("list-invoked");
        let resolve = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }]
        }));
        let list =
            fake_list_provider_script_with_marker(serde_json::json!({ "worktrees": [] }), &marker);

        let handle = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![resolve, "{handle}".to_string()]),
                    list: Some(vec![list]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect("targeted resolve succeeds");

        assert_eq!(handle.handle, "fixture@cook-target");
        assert!(!marker.exists(), "list command must not be invoked");
    }

    #[test]
    fn list_is_the_compatibility_fallback_when_resolve_is_unavailable() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let marker = workspace.path().join("list-invoked");
        let list = fake_list_provider_script_with_marker(
            serde_json::json!({
                "worktrees": [{
                    "handle": "fixture@cook-target",
                    "path": workspace.path(),
                    "branch": "cook-target",
                    "safety": { "dirty": false, "unpushed": false, "primary": false }
                }]
            }),
            &marker,
        );

        resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(list_provider(list, worktrees_mapping())),
        )
        .expect("list fallback succeeds");

        assert!(marker.exists(), "list fallback must be invoked");
    }

    #[test]
    fn resolve_failure_preserves_command_classification() {
        let script = fake_failing_provider_script();
        let err = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![script, "{handle}".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect_err("failed resolve must be reported");

        assert!(err
            .message
            .contains("resolve command failed with exit code 23"));
        assert!(!err.message.contains("invalid JSON"));
        assert_eq!(err.details["command_evidence"]["exit_code"], 23);
        assert_eq!(
            err.details["command_evidence"]["stderr"],
            "provider failed\n"
        );
    }

    #[test]
    fn rejects_provider_handles_with_unsafe_safety_metadata() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        fs::write(workspace.path().join("bootstrap.txt"), "drift\n").expect("write drift");
        let script = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": true, "unpushed": false, "primary": false }
            }]
        }));
        let err = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    list: Some(vec![script]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            }),
        )
        .expect_err("dirty provider handle must be rejected");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("dirty"));
        assert!(err.message.contains("workspace.resolved_but_dirty"));
        assert_eq!(
            err.details["workspace"]["classification"],
            "workspace.resolved_but_dirty"
        );
        assert_eq!(err.details["workspace"]["provider_id"], "fixture");
        assert_eq!(
            err.details["workspace"]["resolution"]["handle"],
            "fixture@cook-target"
        );
        assert_eq!(
            err.details["workspace"]["resolution"]["branch"],
            "cook-target"
        );
        assert_eq!(err.details["workspace"]["reason"], "unattributed_drift");
        assert_eq!(
            err.details["workspace"]["changed_paths"],
            serde_json::json!(["bootstrap.txt"])
        );
        assert!(err.details["workspace"]["inspect_command"]
            .as_str()
            .is_some_and(|command| command.contains("git") && command.contains("status")));
    }

    #[test]
    fn matching_promoted_baseline_is_allowed_and_divergent_edits_remain_refused() {
        struct FixtureBaselineProvider;

        impl crate::gate_feedback_baseline::GateFeedbackBaselineProvider for FixtureBaselineProvider {
            fn validate_gate_feedback_candidate_baseline(
                &self,
                _root: &std::path::Path,
                baseline: &Value,
            ) -> Result<String> {
                if baseline["matches"] == true {
                    Ok("matching fixture candidate".to_string())
                } else {
                    Err(Error::validation_invalid_argument(
                        "gate_feedback_candidate_baseline",
                        "fixture baseline differs from the current worktree",
                        None,
                        None,
                    ))
                }
            }
        }

        crate::gate_feedback_baseline::register_gate_feedback_baseline_provider(Box::new(
            FixtureBaselineProvider,
        ));
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        fs::write(workspace.path().join("candidate.txt"), "candidate\n").expect("write candidate");
        let worktree = WorktreeProviderHandle {
            handle: "fixture@cook-target".to_string(),
            path: workspace.path().display().to_string(),
            branch: "cook-target".to_string(),
            task_url: None,
            safety: WorktreeProviderHandleSafety {
                dirty: true,
                unpushed: false,
                primary: false,
            },
        };

        validate_provider_handle(
            "fixture",
            &worktree,
            Some(&serde_json::json!({ "matches": true })),
            None,
        )
        .expect("matching promoted baseline is reusable");

        let unpushed_error = validate_provider_handle(
            "fixture",
            &WorktreeProviderHandle {
                safety: WorktreeProviderHandleSafety {
                    dirty: true,
                    unpushed: true,
                    primary: false,
                },
                ..worktree.clone()
            },
            Some(&serde_json::json!({ "matches": true })),
            None,
        )
        .expect_err("an unpushed promoted candidate must remain refused");
        assert!(unpushed_error.message.contains("unpushed"));
        assert_eq!(
            unpushed_error.details["workspace"]["classification"],
            "workspace.resolved_but_dirty"
        );
        assert_eq!(
            unpushed_error.details["workspace"]["reason"],
            "verified_promoted_candidate"
        );

        let error = validate_provider_handle(
            "fixture",
            &worktree,
            Some(&serde_json::json!({ "matches": false })),
            None,
        )
        .expect_err("divergent edits must remain fail-closed");
        assert_eq!(error.details["workspace"]["reason"], "divergent_user_edits");
        assert!(error
            .message
            .contains("promoted candidate baseline could not be verified"));
    }

    #[test]
    fn bootstrap_success_with_tracked_drift_is_a_typed_postcondition_failure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("destination");
        let state = temp.path().join("state");
        let script = temp.path().join("provider");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$1\" = resolve ]; then\n  if [ -f '{}' ]; then\n    printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"homeboy@fix-9908\",\"path\":\"{}\",\"branch\":\"fix/9908\",\"safety\":{{\"dirty\":true,\"unpushed\":false,\"primary\":false}}}}]}}'\n  else\n    printf '%s\\n' '{{\"worktrees\":[]}}'\n  fi\nelse\n  git init -b fix/9908 '{}' >/dev/null && git -C '{}' config user.email test@example.com && git -C '{}' config user.name Test && printf base > '{}/tracked.txt' && git -C '{}' add tracked.txt && git -C '{}' commit -m base >/dev/null && printf drift >> '{}/tracked.txt' && touch '{}'\nfi\n",
                state.display(),
                workspace.display(),
                workspace.display(),
                workspace.display(),
                workspace.display(),
                workspace.display(),
                workspace.display(),
                workspace.display(),
                workspace.display(),
                state.display(),
            ),
        )
        .expect("write provider");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).expect("executable");
        }
        let mut config = HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    resolve: Some(vec![
                        script.display().to_string(),
                        "resolve".to_string(),
                        "{handle}".to_string(),
                    ]),
                    ensure: Some(vec![script.display().to_string(), "ensure".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(worktrees_mapping()),
            },
        );

        let error = provision_apply_enabled_worktree_provider_from_config(
            &WorktreeProviderCreateIntent {
                handle: "homeboy@fix-9908".to_string(),
                repo: "homeboy".to_string(),
                base: "main".to_string(),
                head: "fix/9908".to_string(),
                task_url: "https://example.test/9908".to_string(),
            },
            &config,
        )
        .expect_err("ensure cannot report success with tracked drift");

        assert!(error.message.contains("bootstrap postcondition failed"));
        assert_eq!(
            error.details["workspace"]["classification"],
            "workspace.resolved_but_dirty"
        );
        assert_eq!(
            error.details["workspace"]["reason"],
            "fresh_bootstrap_drift"
        );
        assert_eq!(
            error.details["workspace"]["owning_layer"],
            "worktree_provider_bootstrap"
        );
        assert_eq!(
            error.details["workspace"]["changed_paths"],
            serde_json::json!(["tracked.txt"])
        );
    }

    #[test]
    fn maps_differently_nested_provider_envelopes_from_configuration() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let cases = [
            (
                json!({ "result": { "items": [{
                    "id": "fixture@cook-target", "checkout": { "path": workspace.path(), "branch": "cook-target" },
                    "state": { "dirty": false, "unpushed": false, "primary": false }
                }]}}),
                WorktreeProviderListResultMapping {
                    items: "$.result.items".to_string(),
                    handle: "$.id".to_string(),
                    path: "$.checkout.path".to_string(),
                    branch: "$.checkout.branch".to_string(),
                    dirty: "$.state.dirty".to_string(),
                    unpushed: "$.state.unpushed".to_string(),
                    primary: "$.state.primary".to_string(),
                    task_url: None,
                },
            ),
            (
                json!({ "payload": [{
                    "name": "fixture@cook-target", "location": workspace.path(), "ref": "cook-target",
                    "dirty": false, "unpushed": false, "primary": false
                }]}),
                WorktreeProviderListResultMapping {
                    items: "$.payload".to_string(),
                    handle: "$.name".to_string(),
                    path: "$.location".to_string(),
                    branch: "$.ref".to_string(),
                    dirty: "$.dirty".to_string(),
                    unpushed: "$.unpushed".to_string(),
                    primary: "$.primary".to_string(),
                    task_url: None,
                },
            ),
        ];

        for (payload, mapping) in cases {
            let script = fake_list_provider_script(payload);
            let handle = resolve_worktree_provider_handle_from_config(
                "fixture@cook-target",
                &config_with_provider(list_provider(script, mapping)),
            )
            .expect("configured envelope resolves");
            assert_eq!(handle.path, workspace.path().display().to_string());
        }
    }

    #[test]
    fn finds_one_task_owned_worktree_and_rejects_ambiguous_ownership() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "issue-42");
        let task_url = "https://github.com/example/project/issues/42";
        let mapping = WorktreeProviderListResultMapping {
            items: "$.worktrees".to_string(),
            handle: "$.handle".to_string(),
            path: "$.path".to_string(),
            branch: "$.branch".to_string(),
            dirty: "$.safety.dirty".to_string(),
            unpushed: "$.safety.unpushed".to_string(),
            primary: "$.safety.primary".to_string(),
            task_url: Some("$.task_url".to_string()),
        };
        let item = |handle: &str| {
            json!({
                "handle": handle,
                "path": workspace.path(),
                "branch": "issue-42",
                "task_url": task_url,
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            })
        };
        let mut provider = list_provider(
            fake_list_provider_script(
                json!({ "worktrees": [item("project@fix-issue-42-project")] }),
            ),
            mapping.clone(),
        );
        provider.apply_enabled = true;
        let config = config_with_provider(provider);
        let found = find_apply_enabled_worktree_provider_by_task_url_from_config(task_url, &config)
            .expect("task lookup")
            .expect("task worktree");
        assert_eq!(found.worktree.handle, "project@fix-issue-42-project");

        let mapped = map_provider_list_result(
            "fixture",
            &mapping,
            &json!({ "worktrees": [
                { "handle": "project@unowned", "path": workspace.path(), "branch": "issue-42", "task_url": null, "safety": { "dirty": false, "unpushed": false, "primary": false } },
                { "handle": "project@legacy", "path": workspace.path(), "branch": "issue-42", "safety": { "dirty": false, "unpushed": false, "primary": false } }
            ] }),
        )
        .expect("mixed task ownership maps");
        assert!(mapped.iter().all(|worktree| worktree.task_url.is_none()));

        let mut provider = list_provider(
            fake_list_provider_script(
                json!({ "worktrees": [item("project@first"), item("project@second")] }),
            ),
            mapping,
        );
        provider.apply_enabled = true;
        let error = find_apply_enabled_worktree_provider_by_task_url_from_config(
            task_url,
            &config_with_provider(provider),
        )
        .expect_err("duplicate ownership must be explicit");
        assert!(error.message.contains("project@first, project@second"));
    }

    #[test]
    fn rejects_malformed_or_incomplete_provider_mappings() {
        let payload = json!({ "items": [{
            "handle": "fixture@cook-target", "path": "/tmp/fixture", "branch": "cook-target",
            "dirty": false, "unpushed": false, "primary": false
        }] });
        let cases = [
            ("items", "not a jsonpath", "is not valid JSONPath"),
            ("path", "$.missing", "did not resolve a value"),
            ("dirty", "$.handle", "must resolve to a boolean"),
        ];

        for (field, path, expected) in cases {
            let mut mapping = WorktreeProviderListResultMapping {
                items: "$.items".to_string(),
                handle: "$.handle".to_string(),
                path: "$.path".to_string(),
                branch: "$.branch".to_string(),
                dirty: "$.dirty".to_string(),
                unpushed: "$.unpushed".to_string(),
                primary: "$.primary".to_string(),
                task_url: None,
            };
            *match field {
                "items" => &mut mapping.items,
                "path" => &mut mapping.path,
                "dirty" => &mut mapping.dirty,
                _ => unreachable!(),
            } = path.to_string();
            let err = resolve_worktree_provider_handle_from_config(
                "fixture@cook-target",
                &config_with_provider(list_provider(
                    fake_list_provider_script(payload.clone()),
                    mapping,
                )),
            )
            .expect_err("invalid mapping must fail closed");
            assert!(err.message.contains(expected), "{}", err.message);
        }
    }

    #[test]
    fn absent_safety_flags_default_to_permissive_instead_of_failing_the_cook() {
        // The DMC worktree provider omits `safety.dirty` (and can omit the whole
        // `safety` object). A missing advisory safety flag is not a claim of
        // unsafety, so it must default to `false` and not reject the cook
        // pre-dispatch (#7886).
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");

        // Response omits `safety.dirty` (only reports unpushed/primary).
        let partial_safety = json!({ "worktrees": [{
            "handle": "fixture@cook-target", "path": workspace.path(), "branch": "cook-target",
            "safety": { "unpushed": false, "primary": false }
        }]});
        // Response omits the `safety` object entirely.
        let no_safety = json!({ "worktrees": [{
            "handle": "fixture@cook-target", "path": workspace.path(), "branch": "cook-target"
        }]});

        for payload in [partial_safety, no_safety] {
            let handle = resolve_worktree_provider_handle_from_config(
                "fixture@cook-target",
                &config_with_provider(list_provider(
                    fake_list_provider_script(payload),
                    worktrees_mapping(),
                )),
            )
            .expect("absent safety flags resolve without failing the cook");
            assert!(!handle.safety.dirty);
            assert!(!handle.safety.unpushed);
            assert!(!handle.safety.primary);
        }

        // A present-but-non-boolean safety value is still a contract error.
        let wrong_type = json!({ "worktrees": [{
            "handle": "fixture@cook-target", "path": workspace.path(), "branch": "cook-target",
            "safety": { "dirty": "yes", "unpushed": false, "primary": false }
        }]});
        let err = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(list_provider(
                fake_list_provider_script(wrong_type),
                worktrees_mapping(),
            )),
        )
        .expect_err("a non-boolean safety value is still rejected");
        assert!(
            err.message.contains("must resolve to a boolean"),
            "{}",
            err.message
        );
    }

    #[test]
    fn rejects_unsafe_and_mismatched_provider_metadata() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        for (field, value, expected) in [
            ("dirty", json!(true), "dirty"),
            ("unpushed", json!(true), "unpushed"),
            ("primary", json!(true), "primary"),
            ("branch", json!("wrong-branch"), "does not match"),
        ] {
            let mut row = json!({
                "handle": "fixture@cook-target", "path": workspace.path(), "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            });
            if field == "branch" {
                row[field] = value;
            } else {
                row["safety"][field] = value;
            }
            let err = resolve_worktree_provider_handle_from_config(
                "fixture@cook-target",
                &config_with_provider(list_provider(
                    fake_list_provider_script(json!({ "worktrees": [row] })),
                    worktrees_mapping(),
                )),
            )
            .expect_err("unsafe metadata must fail closed");
            assert!(err.message.contains(expected), "{}", err.message);
        }
    }

    #[test]
    fn trusted_immutable_destination_is_the_only_unpushed_apply_exception() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(workspace.path())
                .output()
                .expect("run git");
            assert!(output.status.success());
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["config", "user.email", "agent@example.test"]);
        git(&["config", "user.name", "Agent"]);
        std::fs::write(workspace.path().join("candidate"), "candidate\n").expect("write candidate");
        git(&["add", "candidate"]);
        git(&["commit", "-m", "candidate"]);
        let head = git(&["rev-parse", "HEAD"]);
        let mut config = config_with_provider(list_provider(
            fake_list_provider_script(json!({ "worktrees": [{
                "handle": "fixture@cook-target", "path": workspace.path(), "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": true, "primary": false }
            }]})),
            worktrees_mapping(),
        ));
        config
            .worktree_providers
            .get_mut("fixture")
            .expect("fixture provider")
            .apply_enabled = true;

        let rejected = resolve_apply_enabled_worktree_provider_from_config(
            "fixture@cook-target",
            &config,
            None,
        )
        .expect_err("ordinary unpushed destination remains blocked");
        assert!(rejected.message.contains("unpushed"));

        let resolved =
            resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
                "fixture@cook-target",
                &config,
                None,
                Some(&TrustedUnpushedWorktree {
                    path: workspace.path().to_path_buf(),
                    head: head.clone(),
                }),
            )
            .expect("exact immutable destination is allowed before finalizer push");
        assert_eq!(
            resolved.worktree.path,
            workspace.path().display().to_string()
        );

        std::fs::write(workspace.path().join("uncommitted"), "drift\n")
            .expect("introduce uncommitted drift");
        let dirty =
            resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
                "fixture@cook-target",
                &config,
                None,
                Some(&TrustedUnpushedWorktree {
                    path: workspace.path().to_path_buf(),
                    head: head.clone(),
                }),
            )
            .expect_err("trusted unpushed destination must still be clean");
        assert!(dirty.message.contains("dirty"));
        std::fs::remove_file(workspace.path().join("uncommitted"))
            .expect("remove uncommitted drift");

        let stale =
            resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
                "fixture@cook-target",
                &config,
                None,
                Some(&TrustedUnpushedWorktree {
                    path: workspace.path().to_path_buf(),
                    head: format!("{head}0"),
                }),
            )
            .expect_err("different candidate commit remains blocked");
        assert!(stale.message.contains("unpushed"));
    }

    #[test]
    fn provider_apply_captures_phase_progress_and_durable_refs() {
        let script = fake_provider_script_with_refs();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: vec!["fixture".to_string()],
                all_providers: false,
                apply: true,
                timeout: None,
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: WorktreeProviderCommands {
                    cleanup_apply: Some(vec![script]),
                    ..Default::default()
                },
                list_result_mapping: None,
            }),
        )
        .expect("cleanup succeeds");

        let provider = &output.providers[0];
        assert_eq!(provider.phase.as_deref(), Some("running"));
        assert_eq!(provider.last_progress.as_deref(), Some("removed 10/20"));
        assert_eq!(provider.run_refs.len(), 1);
        assert_eq!(
            provider.run_refs[0].run_id.as_deref(),
            Some("cleanup-run-1")
        );
        assert_eq!(
            provider.run_refs[0].status_command.as_deref(),
            Some("provider status cleanup-run-1")
        );
        assert_eq!(
            provider.follow_up_command.as_deref(),
            Some("provider status cleanup-run-1")
        );
    }

    #[test]
    fn split_identity_survives_a_timed_out_safety_attestation() {
        let (_root, workspace) = linked_workspace("branch");
        let mut provider = default_command_provider();
        provider.lookup_timeout_ms = 25;
        provider.commands.resolve_identity = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("printf '%s' '{{\"schema\":\"homeboy/worktree-provider-identity/v1\",\"provider_id\":\"fixture\",\"token\":\"opaque-identity\",\"handle\":\"fixture@branch\",\"path\":\"{}\",\"branch\":\"branch\",\"primary\":false,\"latency_ms\":0,\"budget_ms\":0}}'", workspace.display()),
        ]);
        provider.commands.attest_safety = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 1; printf '%s' '{}'".to_string(),
        ]);
        let config = config_with_provider(provider);

        let identity =
            resolve_apply_enabled_worktree_provider_identity_from_config("fixture@branch", &config)
                .expect("cheap exact identity is available");
        let error = attest_apply_enabled_worktree_provider_safety_from_config(&identity, &config)
            .expect_err("bounded safety probe times out");

        assert_eq!(identity.token, "opaque-identity");
        assert_eq!(error.details["worktree_provider_split"], "timed_out");
        assert_eq!(
            error.details["worktree_provider_split_operation"],
            "attest_safety"
        );
    }

    #[test]
    fn split_provider_rejects_safety_evidence_for_a_different_identity() {
        let (_root, workspace) = linked_workspace("branch");
        let mut provider = default_command_provider();
        provider.commands.resolve_identity = Some(vec![
            "sh".to_string(), "-c".to_string(),
            format!("printf '%s' '{{\"schema\":\"homeboy/worktree-provider-identity/v1\",\"provider_id\":\"fixture\",\"token\":\"opaque-identity\",\"handle\":\"fixture@branch\",\"path\":\"{}\",\"branch\":\"branch\",\"primary\":false,\"latency_ms\":0,\"budget_ms\":0}}'", workspace.display()),
        ]);
        provider.commands.attest_safety = Some(vec![
            "sh".to_string(), "-c".to_string(),
            "printf '%s' '{\"schema\":\"homeboy/worktree-provider-safety/v1\",\"identity_token\":\"other\",\"observed_at\":\"2026-01-01T00:00:00Z\",\"dirty\":false,\"unpushed\":false,\"fresh\":true,\"latency_ms\":0,\"budget_ms\":0}'".to_string(),
        ]);
        let error = resolve_apply_enabled_worktree_provider_split_from_config(
            "fixture@branch",
            &config_with_provider(provider),
        )
        .expect_err("mismatched evidence is fail-closed");
        assert!(error.message.contains("different exact identity"));
    }

    #[test]
    fn provider_convergence_uses_the_pinned_base_after_safe_attestation() {
        let (root, workspace) = linked_workspace("branch");
        let source = root.path().join("source");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&source)
                .output()
                .expect("run source git command");
            assert!(output.status.success(), "git {args:?} failed");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        // Materialization starts at the initial commit. Pin the first advance,
        // then advance the moving branch again before destination admission.
        git(&["commit", "--allow-empty", "-m", "pinned base"]);
        let pinned_base = git(&["rev-parse", "HEAD"]);
        git(&["commit", "--allow-empty", "-m", "later base"]);
        let later_base = git(&["rev-parse", "HEAD"]);
        let evidence = tempfile::NamedTempFile::new().expect("convergence evidence file");
        let evidence_path = evidence.path().display().to_string();
        let mut provider = default_command_provider();
        provider.commands.resolve_identity = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("printf '%s' '{{\"schema\":\"homeboy/worktree-provider-identity/v1\",\"provider_id\":\"fixture\",\"token\":\"opaque-identity\",\"handle\":\"fixture@branch\",\"path\":\"{}\",\"branch\":\"branch\",\"primary\":false,\"latency_ms\":0,\"budget_ms\":0}}'", workspace.display()),
        ]);
        provider.commands.attest_safety = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf '%s' '{\"schema\":\"homeboy/worktree-provider-safety/v1\",\"identity_token\":\"opaque-identity\",\"observed_at\":\"2026-01-01T00:00:00Z\",\"dirty\":false,\"unpushed\":false,\"fresh\":true,\"latency_ms\":0,\"budget_ms\":0}'".to_string(),
        ]);
        provider.commands.converge = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "git -C '{}' merge --ff-only \"$3\" >/dev/null && printf '%s' \"$3\" > '{evidence_path}' && printf '{{\"schema\":\"homeboy/worktree-provider-convergence/v1\",\"identity_token\":\"%s\",\"base_sha\":\"%s\"}}' \"$2\" \"$3\"",
                workspace.display()
            ),
            "_".to_string(),
            "{handle}".to_string(),
            "{identity}".to_string(),
            "{base}".to_string(),
        ]);

        let convergence = converge_apply_enabled_worktree_provider_to_base_from_config(
            "fixture@branch",
            &pinned_base,
            &config_with_provider(provider),
        )
        .expect("clean managed worktree converges through its provider");

        assert_eq!(convergence.provider_id, "fixture");
        assert_eq!(
            std::fs::read_to_string(evidence.path()).expect("read convergence evidence"),
            pinned_base
        );
        let workspace_head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&workspace)
            .output()
            .expect("read converged workspace HEAD");
        assert!(workspace_head.status.success());
        assert_eq!(
            String::from_utf8_lossy(&workspace_head.stdout).trim(),
            pinned_base,
            "the later moving base was not used"
        );
        assert_ne!(pinned_base, later_base);
    }

    #[test]
    fn provider_convergence_refuses_dirty_worktree_without_mutation() {
        let (_root, workspace) = linked_workspace("branch");
        let evidence = tempfile::NamedTempFile::new().expect("convergence evidence file");
        std::fs::remove_file(evidence.path()).expect("remove untouched evidence file");
        let evidence_path = evidence.path().display().to_string();
        let mut provider = default_command_provider();
        provider.commands.resolve_identity = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("printf '%s' '{{\"schema\":\"homeboy/worktree-provider-identity/v1\",\"provider_id\":\"fixture\",\"token\":\"opaque-identity\",\"handle\":\"fixture@branch\",\"path\":\"{}\",\"branch\":\"branch\",\"primary\":false,\"latency_ms\":0,\"budget_ms\":0}}'", workspace.display()),
        ]);
        provider.commands.attest_safety = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf '%s' '{\"schema\":\"homeboy/worktree-provider-safety/v1\",\"identity_token\":\"opaque-identity\",\"observed_at\":\"2026-01-01T00:00:00Z\",\"dirty\":true,\"unpushed\":false,\"fresh\":true,\"latency_ms\":0,\"budget_ms\":0}'".to_string(),
        ]);
        provider.commands.converge = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("touch '{evidence_path}'"),
        ]);

        let error = converge_apply_enabled_worktree_provider_to_base_from_config(
            "fixture@branch",
            "0123456789012345678901234567890123456789",
            &config_with_provider(provider),
        )
        .expect_err("dirty managed worktree is never converged");

        assert!(error.message.contains("not safe for mutation"));
        assert!(!evidence.path().exists());
    }

    #[test]
    fn pinned_split_identity_ignores_provider_reordering_and_rejects_config_drift() {
        let (_root, workspace) = linked_workspace("branch");
        let split_provider = |provider_id: &str, token: &str| {
            WorktreeProviderConfig {
            commands: WorktreeProviderCommands {
                resolve_identity: Some(vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    format!(
                        "printf '%s' '{{\"schema\":\"homeboy/worktree-provider-identity/v1\",\"provider_id\":\"{provider_id}\",\"token\":\"{token}\",\"handle\":\"fixture@branch\",\"path\":\"{}\",\"branch\":\"branch\",\"primary\":false,\"latency_ms\":0,\"budget_ms\":0}}'", workspace.display()
                    ),
                ]),
                attest_safety: Some(vec!["true".to_string()]),
                ..Default::default()
            },
            ..default_command_provider()
        }
        };
        let mut config = config_with_provider(split_provider("fixture", "fixture-token"));
        config.worktree_providers.insert(
            "another".to_string(),
            split_provider("another", "another-token"),
        );

        let identity = resolve_apply_enabled_worktree_provider_identity_by_id_from_config(
            "fixture@branch",
            "fixture",
            &config,
        )
        .expect("persisted provider remains authoritative despite another provider");
        assert_eq!(identity.provider_id, "fixture");
        assert_eq!(identity.token, "fixture-token");

        config.worktree_providers.remove("fixture");
        let error = resolve_apply_enabled_worktree_provider_identity_by_id_from_config(
            "fixture@branch",
            "fixture",
            &config,
        )
        .expect_err("configuration drift cannot select another provider");
        assert!(error.message.contains("no longer configured"));
    }

    #[test]
    fn split_identity_probes_multiple_providers_until_one_owns_the_exact_handle() {
        let (_root, workspace) = linked_workspace("branch");
        let split_provider = |script: &str| WorktreeProviderConfig {
            commands: WorktreeProviderCommands {
                resolve_identity: Some(vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    script.to_string(),
                ]),
                attest_safety: Some(vec!["true".to_string()]),
                ..Default::default()
            },
            ..default_command_provider()
        };
        let mut config =
            config_with_provider(split_provider("printf '%s' '{\"status\":\"not_owned\"}'"));
        config.worktree_providers.insert(
            "owner".to_string(),
            split_provider(
                &format!("printf '%s' '{{\"schema\":\"homeboy/worktree-provider-identity/v1\",\"provider_id\":\"owner\",\"token\":\"owner-token\",\"handle\":\"fixture@branch\",\"path\":\"{}\",\"branch\":\"branch\",\"primary\":false,\"latency_ms\":0,\"budget_ms\":0}}'", workspace.display()),
            ),
        );

        let identity =
            resolve_apply_enabled_worktree_provider_identity_from_config("fixture@branch", &config)
                .expect("the owning split provider resolves after a typed miss");
        assert_eq!(identity.provider_id, "owner");
        assert_eq!(identity.token, "owner-token");
    }

    #[test]
    fn split_identity_rejects_a_different_clean_worktree_before_selection_or_restart() {
        let (_root, workspace) = linked_workspace("branch");
        let mut provider = default_command_provider();
        provider.commands.resolve_identity = Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "printf '%s' '{{\"schema\":\"homeboy/worktree-provider-identity/v1\",\"provider_id\":\"fixture\",\"token\":\"other-clean-worktree\",\"handle\":\"fixture@other\",\"path\":\"{}\",\"branch\":\"branch\",\"primary\":false,\"latency_ms\":0,\"budget_ms\":0}}'",
                workspace.display()
            ),
        ]);
        provider.commands.attest_safety = Some(vec!["true".to_string()]);
        let config = config_with_provider(provider);

        let initial =
            resolve_apply_enabled_worktree_provider_identity_from_config("fixture@branch", &config)
                .expect_err("a different clean worktree cannot be selected");
        assert!(initial.message.contains("different handle"));
        let restart = resolve_apply_enabled_worktree_provider_identity_by_id_from_config(
            "fixture@branch",
            "fixture",
            &config,
        )
        .expect_err("a different clean worktree cannot replace pinned identity on restart");
        assert!(restart.message.contains("different handle"));
    }

    fn config_with_provider(provider: WorktreeProviderConfig) -> HomeboyConfig {
        let mut providers = HashMap::new();
        providers.insert("fixture".to_string(), provider);
        HomeboyConfig {
            worktree_providers: providers,
            ..HomeboyConfig::default()
        }
    }

    fn default_command_provider() -> WorktreeProviderConfig {
        WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: true,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands::default(),
            list_result_mapping: None,
        }
    }

    fn worktrees_mapping() -> WorktreeProviderListResultMapping {
        WorktreeProviderListResultMapping {
            items: "$.worktrees".to_string(),
            handle: "$.handle".to_string(),
            path: "$.path".to_string(),
            branch: "$.branch".to_string(),
            dirty: "$.safety.dirty".to_string(),
            unpushed: "$.safety.unpushed".to_string(),
            primary: "$.safety.primary".to_string(),
            task_url: None,
        }
    }

    fn list_provider(
        script: String,
        mapping: WorktreeProviderListResultMapping,
    ) -> WorktreeProviderConfig {
        WorktreeProviderConfig {
            enabled: true,
            kind: WorktreeProviderKind::Command,
            apply_enabled: false,
            lookup_timeout_ms: 10_000,
            mutation_timeout_ms: 30_000,
            lookup_output_limit_bytes: 64 * 1024,
            commands: WorktreeProviderCommands {
                list: Some(vec![script]),
                ..Default::default()
            },
            list_result_mapping: Some(mapping),
        }
    }

    /// Shared, process-wide root for fixture provider scripts.
    ///
    /// Each fixture script needs a stable on-disk path that outlives the helper
    /// that creates it (the test executes it later). Previously each helper
    /// `.keep()`-ed its own `tempfile::tempdir()`, which permanently disables
    /// `TempDir`'s `Drop` cleanup — leaking one directory per fixture on every
    /// run (see #9173 follow-up). Instead, anchor all fixture scripts under a
    /// single `TempDir` owned by this `OnceLock`: it is created once, cleans up
    /// when the test process exits normally, and is `hb-test-` prefixed so the
    /// startup sweep (#9177) reclaims it even if the process is killed.
    fn fixture_script_root() -> &'static std::path::Path {
        static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| {
            tempfile::Builder::new()
                .prefix("hb-test-worktree-fixtures-")
                .tempdir()
                .expect("fixture script root tempdir")
        })
        .path()
    }

    /// Allocate a fresh, unique subdirectory under [`fixture_script_root`] for a
    /// single fixture script. Uniqueness avoids collisions between fixtures
    /// within one test run; cleanup is handled by the shared root.
    fn unique_fixture_script_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = fixture_script_root().join(format!("fixture-{id}"));
        fs::create_dir_all(&dir).expect("create fixture script dir");
        dir
    }

    fn fake_provider_script() -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(&script, "#!/bin/sh\nprintf '{\"mode\":\"%s\"}\n' \"$1\"\n")
            .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn fake_provider_script_body(body: &str) -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(&script, format!("#!/bin/sh\n{body}")).expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn waiting_provider_script(
        started: &std::path::Path,
        release: &std::path::Path,
        mode: &str,
    ) -> String {
        fake_provider_script_body(&format!(
            "touch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nprintf '{{\"mode\":\"{}\"}}\\n'\n",
            started.display(),
            release.display(),
            mode,
        ))
    }

    fn wait_for_path(path: &std::path::Path) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            std::thread::yield_now();
        }
    }

    fn assert_path_remains_absent(path: &std::path::Path) {
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            assert!(
                !path.exists(),
                "timed-out provider child must not outlive its process tree"
            );
            std::thread::yield_now();
        }
    }

    fn fake_provider_script_with_refs() -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(
            &script,
            concat!(
                "#!/bin/sh\n",
                "printf 'starting cleanup\\n' >&2\n",
                "printf '{\"phase\":\"running\",\"last_progress\":\"removed 10/20\",\"run_id\":\"cleanup-run-1\",\"status_command\":\"provider status cleanup-run-1\"}\\n'\n"
            ),
        )
        .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    #[test]
    fn typed_worktree_not_found_is_distinct_from_provider_execution_failure() {
        assert!(provider_declared_lookup_not_found(&serde_json::json!({
            "status": "error",
            "error": { "code": "worktree_not_found" }
        })));
        assert!(!provider_declared_lookup_not_found(&serde_json::json!({
            "status": "error",
            "error": { "code": "provider_execution_failed" }
        })));
    }

    fn fake_list_provider_script(output: Value) -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", output))
            .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn fake_list_provider_script_with_marker(output: Value, marker: &std::path::Path) -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ntouch '{}'\nprintf '%s\\n' '{}'\n",
                marker.display(),
                output
            ),
        )
        .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn fake_failing_provider_script() -> String {
        let dir = unique_fixture_script_dir();
        let script = dir.join("provider");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'provider failed\\n' >&2\nexit 23\n",
        )
        .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn git_init(path: &std::path::Path, branch: &str) {
        let output = std::process::Command::new("git")
            .args(["init", "-b", branch])
            .current_dir(path)
            .output()
            .expect("initialize git repository");
        assert!(output.status.success());
    }

    fn linked_workspace(branch: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let root = tempfile::tempdir().expect("workspace root");
        let source = root.path().join("source");
        let workspace = root.path().join("workspace");
        fs::create_dir(&source).expect("source directory");
        git_init(&source, "main");
        for args in [
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Homeboy Test"],
            vec!["commit", "--allow-empty", "-m", "initial"],
        ] {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&source)
                .output()
                .expect("git command");
            assert!(output.status.success());
        }
        let output = std::process::Command::new("git")
            .args(["worktree", "add", "--quiet", "-b", branch])
            .arg(&workspace)
            .current_dir(&source)
            .output()
            .expect("create linked worktree");
        assert!(output.status.success());
        (root, workspace.canonicalize().expect("canonical workspace"))
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod");
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &std::path::Path) {}
}
