//! Lifecycle contract pipeline step.
//!
//! Consumes `homeboy/lifecycle-contract/v1` — the vocabulary Homeboy already
//! ships for disposable, resettable workloads. This module owns contract
//! validation, variable expansion, and phase selection; the phases themselves
//! are runtime-owned, so nothing here knows what is being prepared, snapshotted
//! or reaped.

use super::super::expand::expand_vars;
use super::super::spec::{LifecycleContract, LifecyclePhaseContract, LifecyclePhaseKind, RigSpec};
use super::labels::serialize_lifecycle_op;
use homeboy_core::error::{Error, Result};
use homeboy_core::lifecycle::{LIFECYCLE_CONTRACT_SCHEMA, LIFECYCLE_CONTRACT_VERSION};

pub(super) fn run_lifecycle_step(
    rig: &RigSpec,
    component: Option<&str>,
    contract: &LifecycleContract,
    op: LifecyclePhaseKind,
) -> Result<()> {
    let contract = expand_contract(rig, contract);
    validate_contract(rig, &contract)?;

    if let Some(component) = component {
        // Fail on an undeclared component before any phase runs, the same way
        // `build` / `extension` steps do.
        super::super::resolve_component_path(rig, component)?;
    }

    selected_phases(rig, &contract, op)?;
    Ok(())
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
        *value = expand_vars(rig, value);
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

pub(super) fn step_error(rig: &RigSpec, reason: impl Into<String>) -> Error {
    Error::rig_pipeline_failed(&rig.id, "lifecycle", reason)
}
