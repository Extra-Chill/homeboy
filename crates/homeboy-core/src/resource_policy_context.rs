//! Shared resource-policy context: the process-captured resource snapshot and
//! the runner-placement environment probes.
//!
//! This lives in `core` rather than `commands` because both the CLI layer
//! (which captures the context during preflight) and `core::runner` (which
//! reads the captured severity and placement markers when routing lab offload)
//! depend on it. Keeping it here avoids a `core -> commands` dependency edge,
//! which would block extracting `commands` into its own crate.
//!
//! The CLI-specific evaluation logic that *builds* a [`ResourcePolicyContext`]
//! from a `DoctorOutput` remains in `commands::utils::resource_policy`.

use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

/// Structured, serde-friendly snapshot of the resource-policy decision captured
/// once per process at preflight time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourcePolicyContext {
    /// Hot command label (e.g. `bench`, `lint`).
    pub command: String,
    /// Overall severity: `ok`, `warm`, or `hot`.
    pub severity: String,
    /// Requested controller placement. This records an intentional local
    /// override without preserving deprecated CLI terminology.
    pub local_override: bool,
    /// Whether a warning was emitted (or would have been) for this run.
    pub warned: bool,
    /// Human-readable warning message produced by the policy, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Controller-side routing decision available at resource-policy preflight.
    pub runner_selection: ResourcePolicyRunnerSelection,
    /// Structured host snapshot used to derive the severity.
    pub host: ResourcePolicyHostSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourcePolicyRunnerSelection {
    /// Runner selected before command execution, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    /// Why this command is expected to run locally or through Lab.
    pub reason: String,
}

/// Subset of the doctor resource report that explains why the resource policy
/// fired. Stored as plain numbers/strings so it round-trips cleanly through
/// JSON without depending on internal `DoctorOutput` types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourcePolicyHostSnapshot {
    /// Severity of the load average alone.
    pub load_severity: String,
    /// 1-minute load average if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_one: Option<f64>,
    /// 5-minute load average if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_five: Option<f64>,
    /// 15-minute load average if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_fifteen: Option<f64>,
    /// CPU count reported by the host.
    pub cpu_count: usize,
    /// Memory severity if memory data was collected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_severity: Option<String>,
    /// Memory used percent if memory data was collected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_used_percent: Option<f64>,
    /// Memory available in MB if memory data was collected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_available_mb: Option<u64>,
    /// Number of Homeboy-adjacent processes already active.
    pub relevant_process_count: usize,
    /// Severity classification of the active process set.
    pub process_severity: String,
    /// Number of active rig run leases.
    pub active_rig_lease_count: usize,
    /// Severity classification of the active rig lease set.
    pub rig_lease_severity: String,
}

fn captured_storage() -> &'static RwLock<Option<ResourcePolicyContext>> {
    static STORAGE: OnceLock<RwLock<Option<ResourcePolicyContext>>> = OnceLock::new();
    STORAGE.get_or_init(|| RwLock::new(None))
}

/// Capture the resource policy context for the current process. Idempotent
/// against later overwrites in production: once a context is recorded, repeat
/// captures are dropped so the preflight decision wins. Tests can override
/// this by calling [`reset_captured_context_for_test`] first.
pub fn capture_context(context: ResourcePolicyContext) {
    let mut slot = match captured_storage().write() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    if slot.is_none() {
        *slot = Some(context);
    }
}

/// Return a clone of the resource policy context captured at preflight time,
/// if any.
pub fn captured_context() -> Option<ResourcePolicyContext> {
    captured_storage().read().ok().and_then(|slot| slot.clone())
}

/// Clear the captured context. Test-only: production code never resets the
/// preflight decision so that the persisted observation matches the warning
/// the user actually saw on stderr.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_captured_context_for_test() {
    if let Ok(mut slot) = captured_storage().write() {
        *slot = None;
    }
}

pub fn is_runner_hosted_exec() -> bool {
    std::env::var(crate::runner::RUNNER_HOSTED_EXEC_ENV)
        .ok()
        .is_some_and(|value| value == "1")
}

/// True when Homeboy is executing inside a GitHub Actions CI job.
///
/// CI runners are ephemeral, single-purpose, and non-interactive by design.
/// The resource-policy warm-machine refusal exists to protect a shared
/// developer/controller host from added load and to steer work onto Lab; inside
/// CI there is no shared host to protect, no human to "rerun later", and no Lab
/// runner to route to. Refusing there turns the guard itself into the outage
/// (the PR check goes red on good code), so callers use this to bypass or
/// downgrade the non-interactive refusal in CI.
pub fn is_ci_execution() -> bool {
    std::env::var("GITHUB_ACTIONS")
        .ok()
        .is_some_and(|value| value == "true")
}

/// True for the complete environment prepared by a managed remote runner exec.
///
/// Environment variables are not cryptographic provenance: a process with
/// arbitrary environment control can forge them. They are an internal runner
/// transport boundary, so require all three values emitted by RunnerExecOptions
/// and daemon job preparation rather than trusting the resolved marker alone.
pub fn is_managed_runner_placement_context() -> bool {
    is_runner_hosted_exec()
        && std::env::var(crate::runner::RUNNER_PLACEMENT_RESOLVED_ENV)
            .ok()
            .is_some_and(|value| value == "1")
        && std::env::var(crate::runner::RUNNER_ID_ENV)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
}

pub fn clear_runner_hosted_exec() {
    std::env::remove_var(crate::runner::RUNNER_HOSTED_EXEC_ENV);
}

/// Remove the private runner-placement transport context after CLI routing.
/// Child workloads and persisted artifacts must not inherit controller routing
/// markers once their single placement decision has been consumed.
pub fn clear_managed_runner_placement_context() {
    std::env::remove_var(crate::runner::RUNNER_HOSTED_EXEC_ENV);
    std::env::remove_var(crate::runner::RUNNER_PLACEMENT_RESOLVED_ENV);
    std::env::remove_var(crate::runner::RUNNER_ID_ENV);
}
