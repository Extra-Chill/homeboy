//! Lifecycle contract pipeline step.
//!
//! Consumes `homeboy/lifecycle-contract/v1` — the vocabulary Homeboy already
//! ships for disposable, resettable workloads. This module owns contract
//! validation, variable expansion, phase selection, and phase invocation.
//!
//! The phases themselves are runtime-owned. A phase names either an
//! `extension_hook` (an extension ability, `<extension-id>.<action-id>`) or a
//! `command`, and a `snapshot` phase hands back a `LifecycleSnapshotRef` that
//! Homeboy treats as an opaque handle. That is the whole point: an environment
//! that can be created and reaped becomes declarable without the orchestrator
//! learning what it is, where it lives, or how it is materialized.

use std::time::Duration;

use super::super::expand::{expand_vars, settings_env};
use super::super::spec::{
    LifecycleContract, LifecyclePhaseContract, LifecyclePhaseKind, LifecyclePhaseResult,
    LifecyclePhaseStatus, LifecycleResultMetadata, LifecycleSnapshotRef, RigSpec,
};
use super::super::state::now_rfc3339;
use super::super::toolchain;
use super::labels::serialize_lifecycle_op;
use homeboy_core::error::{Error, Result};
use homeboy_core::lifecycle::{
    LIFECYCLE_CONTRACT_SCHEMA, LIFECYCLE_CONTRACT_VERSION, LIFECYCLE_RESULT_SCHEMA,
    LIFECYCLE_SNAPSHOT_REF_SCHEMA,
};
use homeboy_core::server::{
    execute_local_command_in_dir, execute_local_command_in_dir_with_timeout,
};

/// Snapshot kind recorded when a runtime hands back a bare locator instead of
/// a full `LifecycleSnapshotRef`. Deliberately generic — the rig layer never
/// names the thing it is holding a handle to.
const DEFAULT_SNAPSHOT_KIND: &str = "lifecycle_snapshot";

pub(super) fn run_lifecycle_step(
    rig: &RigSpec,
    component: Option<&str>,
    contract: &LifecycleContract,
    op: LifecyclePhaseKind,
    settings: &[(String, String)],
) -> Result<()> {
    execute_lifecycle_phases(rig, component, contract, op, settings).map(|_| ())
}

/// Execute every contract phase matching `op`, in declared order, and return
/// the `homeboy/lifecycle-result/v1` metadata describing what happened.
pub(super) fn execute_lifecycle_phases(
    rig: &RigSpec,
    component: Option<&str>,
    contract: &LifecycleContract,
    op: LifecyclePhaseKind,
    settings: &[(String, String)],
) -> Result<LifecycleResultMetadata> {
    let contract = expand_contract(rig, contract);
    validate_contract(rig, &contract)?;

    let cwd = match component {
        // Fail on an undeclared component before any phase runs, the same way
        // `build` / `extension` steps do.
        Some(component) => Some(super::super::resolve_component_path(rig, component)?),
        None => None,
    };

    let phases = selected_phases(rig, &contract, op)?;

    let mut result = LifecycleResultMetadata {
        schema: LIFECYCLE_RESULT_SCHEMA.to_string(),
        version: LIFECYCLE_CONTRACT_VERSION,
        phases: Vec::with_capacity(phases.len()),
        snapshot_refs: Vec::new(),
        metadata: contract.metadata.clone(),
    };

    let mut failure: Option<Error> = None;

    for phase in phases {
        let started_at = now_rfc3339();
        // The most recent handle is visible to later phases so a `reset`,
        // `rollback` or `teardown` phase can address what `snapshot` created.
        let handle = result.snapshot_refs.last().cloned();
        let outcome = run_phase(
            rig,
            phase,
            op,
            component,
            cwd.as_deref(),
            handle.as_ref(),
            settings,
        );

        match outcome {
            Ok(output) => {
                let snapshot = capture_snapshot(phase, op, &output);
                if let Some(snapshot) = &snapshot {
                    homeboy_core::log_status!(
                        "rig",
                        "lifecycle {} captured handle {}",
                        serialize_lifecycle_op(op),
                        snapshot.id
                    );
                }
                result.phases.push(LifecyclePhaseResult {
                    id: phase.id.clone(),
                    phase: op,
                    status: LifecyclePhaseStatus::Passed,
                    snapshot_ref: snapshot.as_ref().map(|snapshot| snapshot.id.clone()),
                    started_at: Some(started_at),
                    finished_at: Some(now_rfc3339()),
                    message: None,
                });
                if let Some(snapshot) = snapshot {
                    result.snapshot_refs.push(snapshot);
                }
            }
            Err(error) => {
                // `required` defaults to true: a phase that cannot state
                // otherwise is load-bearing.
                let required = phase.required.unwrap_or(true);
                result.phases.push(LifecyclePhaseResult {
                    id: phase.id.clone(),
                    phase: op,
                    status: if required {
                        LifecyclePhaseStatus::Failed
                    } else {
                        LifecyclePhaseStatus::Skipped
                    },
                    snapshot_ref: None,
                    started_at: Some(started_at),
                    finished_at: Some(now_rfc3339()),
                    message: Some(error.to_string()),
                });
                if required {
                    failure = Some(error);
                    break;
                }
            }
        }
    }

    match failure {
        Some(error) => Err(error),
        None => Ok(result),
    }
}

/// Raw output of one executed phase.
struct PhaseOutput {
    /// Standard output text, whatever the invocation channel was.
    stdout: String,
    /// Structured result, present only for extension-hook invocations.
    payload: Option<serde_json::Value>,
}

fn run_phase(
    rig: &RigSpec,
    phase: &LifecyclePhaseContract,
    op: LifecyclePhaseKind,
    component: Option<&str>,
    cwd: Option<&str>,
    handle: Option<&LifecycleSnapshotRef>,
    settings: &[(String, String)],
) -> Result<PhaseOutput> {
    let env = phase_env(rig, phase, op, component, handle, settings);

    if let Some(hook) = phase.extension_hook.as_deref() {
        return run_extension_hook(rig, phase, op, component, hook, handle, &env);
    }

    let command = phase.command.as_deref().ok_or_else(|| {
        step_error(
            rig,
            format!(
                "lifecycle phase '{}' declares neither extension_hook nor command",
                phase.id
            ),
        )
    })?;

    let env_refs = env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect::<Vec<_>>();

    let output = match phase.timeout_seconds {
        Some(seconds) => execute_local_command_in_dir_with_timeout(
            command,
            cwd,
            Some(&env_refs),
            Duration::from_secs(seconds),
        ),
        None => execute_local_command_in_dir(command, cwd, Some(&env_refs)),
    };

    if output.timed_out {
        return Err(step_error(
            rig,
            format!(
                "lifecycle phase '{}' timed out after {}s",
                phase.id,
                phase.timeout_seconds.unwrap_or_default()
            ),
        ));
    }
    if !output.success {
        return Err(step_error(
            rig,
            format!(
                "lifecycle phase '{}' exited {}{}",
                phase.id,
                output.exit_code,
                failure_tail(&output.stderr)
            ),
        ));
    }

    Ok(PhaseOutput {
        stdout: output.stdout,
        payload: None,
    })
}

/// Invoke an extension ability named `<extension-id>.<action-id>`.
///
/// The hook string is the entire product-specific surface: Homeboy resolves it
/// through the normal extension action path and never interprets what the
/// action does.
fn run_extension_hook(
    rig: &RigSpec,
    phase: &LifecyclePhaseContract,
    op: LifecyclePhaseKind,
    component: Option<&str>,
    hook: &str,
    handle: Option<&LifecycleSnapshotRef>,
    env: &[(String, String)],
) -> Result<PhaseOutput> {
    let (extension_id, action_id) = hook.trim().rsplit_once('.').ok_or_else(|| {
        step_error(
            rig,
            format!(
                "lifecycle phase '{}' extension_hook '{}' must be '<extension-id>.<action-id>'",
                phase.id, hook
            ),
        )
    })?;
    if extension_id.trim().is_empty() || action_id.trim().is_empty() {
        return Err(step_error(
            rig,
            format!(
                "lifecycle phase '{}' extension_hook '{}' must be '<extension-id>.<action-id>'",
                phase.id, hook
            ),
        ));
    }

    let env_map = env
        .iter()
        .cloned()
        .collect::<std::collections::BTreeMap<String, String>>();
    let payload = serde_json::json!({
        "rig": rig.id,
        "component": component,
        "phase": serialize_lifecycle_op(op),
        "phase_id": phase.id,
        "snapshot": handle,
        "env": env_map,
    });

    let value = homeboy_extension::execute_action(
        extension_id.trim(),
        action_id.trim(),
        None,
        None,
        Some(&payload),
    )?;

    // Command-backed extension actions report their own exit status inside the
    // returned JSON rather than failing the call.
    if value.get("success").and_then(serde_json::Value::as_bool) == Some(false) {
        let exit_code = value
            .get("exitCode")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(-1);
        let stderr = value
            .get("stderr")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        return Err(step_error(
            rig,
            format!(
                "lifecycle phase '{}' hook '{}' exited {}{}",
                phase.id,
                hook,
                exit_code,
                failure_tail(stderr)
            ),
        ));
    }

    let stdout = value
        .get("stdout")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();

    Ok(PhaseOutput {
        stdout,
        payload: Some(value),
    })
}

/// Environment every phase sees. This is the sandbox handle contract: a
/// `teardown` phase can reap what a `snapshot` phase created without Homeboy
/// knowing anything about either.
fn phase_env(
    rig: &RigSpec,
    phase: &LifecyclePhaseContract,
    op: LifecyclePhaseKind,
    component: Option<&str>,
    handle: Option<&LifecycleSnapshotRef>,
    settings: &[(String, String)],
) -> Vec<(String, String)> {
    let mut env = Vec::new();

    if let Some(path) = toolchain::command_step_path(Some(rig)) {
        env.push(("PATH".to_string(), path.to_string_lossy().into_owned()));
    }

    env.push(("HOMEBOY_RIG_ID".to_string(), rig.id.clone()));
    env.push((
        "HOMEBOY_LIFECYCLE_PHASE".to_string(),
        serialize_lifecycle_op(op).to_string(),
    ));
    env.push(("HOMEBOY_LIFECYCLE_PHASE_ID".to_string(), phase.id.clone()));
    if let Some(component) = component {
        env.push((
            "HOMEBOY_LIFECYCLE_COMPONENT".to_string(),
            component.to_string(),
        ));
    }
    if let Some(handle) = handle {
        env.push((
            "HOMEBOY_LIFECYCLE_SNAPSHOT_ID".to_string(),
            handle.id.clone(),
        ));
        env.push((
            "HOMEBOY_LIFECYCLE_SNAPSHOT_KIND".to_string(),
            handle.kind.clone(),
        ));
        if let Some(locator) = handle.locator.as_deref() {
            env.push((
                "HOMEBOY_LIFECYCLE_SNAPSHOT_LOCATOR".to_string(),
                locator.to_string(),
            ));
        }
    }

    for (key, value) in settings_env(settings) {
        env.push((key, value));
    }

    env
}

/// Turn a successful `snapshot` phase into an opaque handle.
///
/// A runtime can hand back a full `LifecycleSnapshotRef` (as JSON on stdout or
/// under a `snapshot` key in an extension action result) or just print a
/// locator. Either way the rig layer stores an id it can pass back later.
fn capture_snapshot(
    phase: &LifecyclePhaseContract,
    op: LifecyclePhaseKind,
    output: &PhaseOutput,
) -> Option<LifecycleSnapshotRef> {
    if op != LifecyclePhaseKind::Snapshot {
        return None;
    }

    let declared = output
        .payload
        .as_ref()
        .and_then(|payload| payload.get("snapshot"))
        .and_then(|value| serde_json::from_value::<LifecycleSnapshotRef>(value.clone()).ok())
        .or_else(|| {
            output.payload.as_ref().and_then(|payload| {
                serde_json::from_value::<LifecycleSnapshotRef>(payload.clone()).ok()
            })
        })
        .or_else(|| serde_json::from_str::<LifecycleSnapshotRef>(output.stdout.trim()).ok());

    let locator = output.stdout.trim();
    let mut snapshot = declared.unwrap_or_else(|| LifecycleSnapshotRef {
        schema: LIFECYCLE_SNAPSHOT_REF_SCHEMA.to_string(),
        id: phase.id.clone(),
        kind: DEFAULT_SNAPSHOT_KIND.to_string(),
        phase_id: Some(phase.id.clone()),
        artifact_id: None,
        artifact: None,
        locator: (!locator.is_empty()).then(|| locator.to_string()),
        created_at: None,
        metadata: Default::default(),
    });

    if snapshot.schema.trim().is_empty() {
        snapshot.schema = LIFECYCLE_SNAPSHOT_REF_SCHEMA.to_string();
    }
    if snapshot.id.trim().is_empty() {
        snapshot.id = phase.id.clone();
    }
    if snapshot.kind.trim().is_empty() {
        snapshot.kind = DEFAULT_SNAPSHOT_KIND.to_string();
    }
    if snapshot.phase_id.is_none() {
        snapshot.phase_id = Some(phase.id.clone());
    }
    if snapshot.created_at.is_none() {
        snapshot.created_at = Some(now_rfc3339());
    }

    Some(snapshot)
}

/// Expand rig variables in every value a phase can hand to a runtime.
///
/// Phase ids and kinds are contract vocabulary and are never expanded.
pub(super) fn expand_contract(rig: &RigSpec, contract: &LifecycleContract) -> LifecycleContract {
    let mut contract = contract.clone();
    for phase in &mut contract.phases {
        phase.command = phase
            .command
            .as_deref()
            .map(|value| expand_vars(rig, value));
        phase.extension_hook = phase
            .extension_hook
            .as_deref()
            .map(|value| expand_vars(rig, value));
        phase.label = phase.label.as_deref().map(|value| expand_vars(rig, value));
    }
    for value in contract.metadata.values_mut() {
        let expanded = expand_vars(rig, value.as_str());
        *value = expanded;
    }
    contract
}

/// Reject a contract Homeboy cannot honour before executing anything.
pub(super) fn validate_contract(rig: &RigSpec, contract: &LifecycleContract) -> Result<()> {
    if contract.schema != LIFECYCLE_CONTRACT_SCHEMA {
        return Err(step_error(
            rig,
            format!(
                "expected schema {}, found '{}'",
                LIFECYCLE_CONTRACT_SCHEMA, contract.schema
            ),
        ));
    }
    if contract.version != LIFECYCLE_CONTRACT_VERSION {
        return Err(step_error(
            rig,
            format!(
                "expected version {}, found {}",
                LIFECYCLE_CONTRACT_VERSION, contract.version
            ),
        ));
    }
    if contract.phases.is_empty() {
        return Err(step_error(rig, "lifecycle contract declares no phases"));
    }

    let mut seen = std::collections::BTreeSet::new();
    for phase in &contract.phases {
        if phase.id.trim().is_empty() {
            return Err(step_error(rig, "lifecycle phase id must not be empty"));
        }
        if !seen.insert(phase.id.as_str()) {
            return Err(step_error(
                rig,
                format!("duplicate lifecycle phase id '{}'", phase.id),
            ));
        }
        if phase.extension_hook.is_none() && phase.command.is_none() {
            return Err(step_error(
                rig,
                format!(
                    "lifecycle phase '{}' declares neither extension_hook nor command",
                    phase.id
                ),
            ));
        }
    }

    Ok(())
}

/// Phases matching the requested op, in declared order.
pub(super) fn selected_phases<'a>(
    rig: &RigSpec,
    contract: &'a LifecycleContract,
    op: LifecyclePhaseKind,
) -> Result<Vec<&'a LifecyclePhaseContract>> {
    let phases = contract
        .phases
        .iter()
        .filter(|phase| phase.phase == op)
        .collect::<Vec<_>>();

    if phases.is_empty() {
        return Err(step_error(
            rig,
            format!(
                "lifecycle contract declares no '{}' phase",
                serialize_lifecycle_op(op)
            ),
        ));
    }

    Ok(phases)
}

fn failure_tail(stderr: &str) -> String {
    let tail = stderr
        .lines()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    if tail.trim().is_empty() {
        String::new()
    } else {
        format!(" — {}", tail)
    }
}

pub(super) fn step_error(rig: &RigSpec, reason: impl Into<String>) -> Error {
    Error::rig_pipeline_failed(&rig.id, "lifecycle", reason)
}
