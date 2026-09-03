//! Lab routing and handoff contracts used by `homeboy-core` and
//! the optional `homeboy-runner` feature crate.
//!
//! Runner is an optional Lab-offload feature; core must not depend on runner
//! *behavior*. Generic runner concepts are canonical in
//! `homeboy-runner-contract`; this crate owns only Lab-specific policy and
//! handoff contracts.
//!
//! Two single-type crates were folded in here for the same reason they existed
//! separately — both are behavior-free runner contracts below core, and both
//! were already depended on by a subset of this crate's dependents:
//!
//! - [`Placement`] (was `homeboy-cli-contract`): the requested execution
//!   location, read by core's Lab routing and by the runner's Lab selection.
//! - [`AgentTaskProviderRunnerSource`] (was `homeboy-agents-contract`): a
//!   managed source checkout homeboy keeps synced on the runner, read by core's
//!   agent-runtime manifest and by `homeboy-agents`.

mod execution_placement;
mod placement;
mod provider_source_types;

pub use execution_placement::{
    EffectiveExecutionPlacement, ExecutionPlacementDecision, ExecutionPlacementFallback,
    ExecutionPlacementIdentity, ExecutionPlacementOutcome, ExecutionPlacementOverrideAuthorization,
    ExecutionPlacementRequirement, ExecutionPlacementRunnerSelection, RunnerSelectionSource,
    CONTROLLER_LOCAL_SUBMISSION_POLICY_ID,
};
pub use placement::Placement;
pub use provider_source_types::AgentTaskProviderRunnerSource;

use std::collections::{BTreeMap, BTreeSet};

use homeboy_runner_contract::{
    RunnerCapabilityPreflight, RunnerRequiredTool, RunnerWorkspaceSyncMode,
};
use serde::{Deserialize, Serialize};

/// Options controlling how a runner workspace is synced before a job runs.
#[derive(Debug, Clone, Default)]
pub struct RunnerWorkspaceSyncOptions {
    pub path: String,
    pub mode: RunnerWorkspaceSyncMode,
    pub controller_routed_git: bool,
    pub changed_since_base: Option<String>,
    pub git_fetch_refs: Vec<String>,
    pub snapshot_includes: Vec<String>,
    pub allow_dirty_lab_workspace: bool,
    /// Opaque job-owned token folded into the deterministic remote workspace
    /// path so two distinct executions at the same source HEAD never share a
    /// mutable remote checkout.
    ///
    /// Without this, the git-mode remote path is keyed only on
    /// `(source path, HEAD)`, so a later unrelated job reuses the earlier job's
    /// workspace directory and can observe or delete its state. Every Lab
    /// execution supplies this token; callers that explicitly opt into a stable
    /// identity must reject an already-owned path before materialization.
    pub run_isolation_token: Option<String>,
}

/// A lab runner capability prepared from a contract, ready to preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedLabRunnerCapability {
    pub command: &'static str,
    pub required_tools: Vec<RunnerRequiredTool>,
    /// Declared workload capabilities retained from the command contract.
    /// Consumers that have a capability inventory can make admission decisions
    /// without reconstructing the original command contract.
    pub required_capabilities: Vec<String>,
}

impl From<PreparedLabRunnerCapability> for RunnerCapabilityPreflight {
    fn from(plan: PreparedLabRunnerCapability) -> Self {
        Self {
            command: plan.command.to_string(),
            required_tools: plan.required_tools,
            required_commands: Vec::new(),
            required_tool_capabilities: Vec::new(),
            required_toolchain_probes: Vec::new(),
            // Capability IDs are opaque to the runner core. Providers and
            // extensions advertise their IDs through the runner inventory.
            required_components: plan.required_capabilities,
            required_env: Vec::new(),
            timeout: None,
        }
    }
}

/// The capability contract a lab runner must satisfy for a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabRunnerCapabilityContract {
    pub command: &'static str,
    pub required_tools: Vec<RunnerRequiredTool>,
    pub required_capabilities: Vec<String>,
}

/// How a lab runner capability gate is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabRunnerGateMode {
    Automatic,
    Explicit,
}

/// The outcome of evaluating a lab runner capability gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabRunnerGateDecision {
    Eligible,
    Missing {
        runner_id: String,
        command: &'static str,
        missing_tools: Vec<RunnerRequiredTool>,
        reason: String,
        remediation: Vec<String>,
    },
}

/// A versioned Lab contract required by a controller or advertised by an
/// executing runner/daemon. Versions are explicit compatibility declarations,
/// not inferred from a Homeboy semver or commit relationship.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct LabCapabilityVersion {
    pub id: String,
    pub version: u32,
}

/// The complete set of Lab contracts every current handoff requires.
///
/// Keep these names protocol-focused. Product-specific capabilities belong in
/// the caller's own contract, not in the generic Lab admission handshake.
pub fn required_lab_handoff_capabilities() -> Vec<LabCapabilityVersion> {
    [
        "daemon-protocol",
        "workspace-materialization",
        "lifecycle-gate-execution",
        "result-schema",
        "artifact-transport",
    ]
    .into_iter()
    .map(|id| LabCapabilityVersion {
        id: id.to_string(),
        version: 1,
    })
    .collect()
}

/// Capability id for the lease-less daemon orphan reconciliation contract.
pub const DAEMON_RECOVERY_LEASELESS_CAPABILITY: &str = "daemon-recovery-leaseless";

/// Capability id for the exact state-loss daemon recovery contract.
pub const DAEMON_RECOVERY_STATE_LOSS_CAPABILITY: &str = "daemon-recovery-state-loss";

/// Capability id for `daemon ensure-running --replacement-operation-id`.
pub const DAEMON_ENSURE_RUNNING_OPERATION_ID_CAPABILITY: &str =
    "daemon-ensure-running-operation-id";

/// Capability id for bounded unleased daemon candidate reconciliation.
pub const DAEMON_RECOVERY_UNLEASED_CANDIDATES_CAPABILITY: &str =
    "daemon-recovery-unleased-candidates";

/// Whether this binary can safely reconcile an unleased daemon candidate.
/// Linux pidfds bind a signal to the observed process instance and prevent PID
/// reuse from turning reconciliation into a signal for a different process.
pub const fn unleased_candidate_reconciliation_supported() -> bool {
    cfg!(target_os = "linux")
}

/// The conditionally-needed daemon recovery contracts a runner may advertise.
///
/// These are NOT part of [`required_lab_handoff_capabilities`]: they are
/// recovery contracts negotiated only when a controller must repair a remote
/// daemon, not admission requirements every ordinary handoff needs. A runner
/// that omits them can still execute work; controllers that need the recovery
/// path require the typed capability. Keep these names protocol-focused.
pub fn daemon_recovery_capabilities() -> Vec<LabCapabilityVersion> {
    let mut capabilities = vec![
        DAEMON_RECOVERY_LEASELESS_CAPABILITY,
        DAEMON_RECOVERY_STATE_LOSS_CAPABILITY,
        DAEMON_ENSURE_RUNNING_OPERATION_ID_CAPABILITY,
    ];
    if unleased_candidate_reconciliation_supported() {
        capabilities.push(DAEMON_RECOVERY_UNLEASED_CANDIDATES_CAPABILITY);
    }
    capabilities
        .into_iter()
        .map(|id| LabCapabilityVersion {
            id: id.to_string(),
            version: 1,
        })
        .collect()
}

/// Decide whether a remote daemon advertises a recovery capability.
pub fn daemon_recovery_capability_advertised(
    advertised: Option<&[LabCapabilityVersion]>,
    capability_id: &str,
) -> bool {
    advertised.is_some_and(|capabilities| {
        capabilities
            .iter()
            .any(|capability| capability.id == capability_id)
    })
}

/// Immutable runtime provenance supplied by a controller, runner command, or
/// active daemon. Dirty or incomplete provenance is never eligible for a
/// compatibility admission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LabRuntimeIdentity {
    pub build_identity: String,
    pub source_revision: String,
    pub clean: bool,
}

/// The independently verified relationship between a requested controller
/// source revision and the executing runner source revision.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LabRuntimeAncestry {
    ExactSource,
    VerifiedNewerDescendant,
    Older,
    Diverged,
    Unknown,
}

/// The controller requirement and runner/daemon evidence that participate in a
/// single Lab handoff admission decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LabCapabilityHandshake {
    pub controller: LabRuntimeIdentity,
    pub required_capabilities: Vec<LabCapabilityVersion>,
    pub runner_command: LabRuntimeIdentity,
    pub runner_command_capabilities: Vec<LabCapabilityVersion>,
    pub daemon: LabRuntimeIdentity,
    pub daemon_capabilities: Vec<LabCapabilityVersion>,
    pub ancestry: LabRuntimeAncestry,
}

/// Serialized record of a Lab capability negotiation. Persist this unchanged
/// from preflight through reservation and execution so all phases use one
/// decision rather than recomputing against drifting runtime state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LabCapabilityNegotiationProvenance {
    pub schema: String,
    pub controller_requirement: LabRuntimeIdentity,
    pub executed_runner_command: LabRuntimeIdentity,
    pub active_daemon: LabRuntimeIdentity,
    pub ancestry: LabRuntimeAncestry,
    pub negotiated_capabilities: Vec<LabCapabilityVersion>,
    pub compatible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<String>,
}

/// Result of a fail-closed Lab capability admission decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LabCapabilityAdmission {
    pub compatible: bool,
    pub provenance: LabCapabilityNegotiationProvenance,
}

/// Evaluate a controller-to-Lab handoff using only explicit compatibility and
/// provenance evidence. A newer source revision is accepted only when its
/// ancestry has already been verified and both the executing command and active
/// daemon advertise every required capability version.
pub fn negotiate_lab_capability_handshake(
    handshake: &LabCapabilityHandshake,
) -> LabCapabilityAdmission {
    let mut rejection_reason = runtime_identity_reason("controller", &handshake.controller)
        .or_else(|| runtime_identity_reason("runner command", &handshake.runner_command))
        .or_else(|| runtime_identity_reason("active daemon", &handshake.daemon));

    if rejection_reason.is_none()
        && handshake.runner_command.source_revision != handshake.daemon.source_revision
    {
        rejection_reason =
            Some("runner command and active daemon source revisions differ".to_string());
    }

    if rejection_reason.is_none()
        && !matches!(
            handshake.ancestry,
            LabRuntimeAncestry::ExactSource | LabRuntimeAncestry::VerifiedNewerDescendant
        )
    {
        rejection_reason = Some(format!(
            "runner provenance is not an exact source or verified newer descendant ({:?})",
            handshake.ancestry
        ));
    }

    if rejection_reason.is_none()
        && handshake.ancestry == LabRuntimeAncestry::ExactSource
        && handshake.controller.source_revision != handshake.runner_command.source_revision
    {
        rejection_reason =
            Some("exact-source ancestry does not match the controller source revision".to_string());
    }

    let required = capability_versions(&handshake.required_capabilities);
    if rejection_reason.is_none()
        && (handshake
            .required_capabilities
            .iter()
            .any(|capability| capability.id.trim().is_empty())
            || required.len()
                != handshake
                    .required_capabilities
                    .iter()
                    .map(|capability| capability.id.as_str())
                    .collect::<BTreeSet<_>>()
                    .len())
    {
        rejection_reason =
            Some("controller capability requirements are incomplete or contradictory".to_string());
    }
    let command = advertised_capability_versions(&handshake.runner_command_capabilities);
    let daemon = advertised_capability_versions(&handshake.daemon_capabilities);
    let negotiated_capabilities = required
        .iter()
        .filter(|&(id, version)| {
            command
                .get(id)
                .is_some_and(|versions| versions.contains(version))
                && daemon
                    .get(id)
                    .is_some_and(|versions| versions.contains(version))
        })
        .map(|(id, version)| LabCapabilityVersion {
            id: id.clone(),
            version: *version,
        })
        .collect();

    if rejection_reason.is_none() {
        for (id, version) in &required {
            if !command
                .get(id)
                .is_some_and(|versions| versions.contains(version))
                || !daemon
                    .get(id)
                    .is_some_and(|versions| versions.contains(version))
            {
                rejection_reason = Some(format!(
                    "required capability `{id}` version {version} is not advertised by both runner command and active daemon"
                ));
                break;
            }
        }
    }

    let compatible = rejection_reason.is_none();
    LabCapabilityAdmission {
        compatible,
        provenance: LabCapabilityNegotiationProvenance {
            schema: "homeboy/lab-capability-negotiation/v1".to_string(),
            controller_requirement: handshake.controller.clone(),
            executed_runner_command: handshake.runner_command.clone(),
            active_daemon: handshake.daemon.clone(),
            ancestry: handshake.ancestry,
            negotiated_capabilities,
            compatible,
            rejection_reason,
        },
    }
}

fn runtime_identity_reason(role: &str, identity: &LabRuntimeIdentity) -> Option<String> {
    if !identity.clean {
        return Some(format!("{role} runtime provenance is dirty"));
    }
    if identity.build_identity.trim().is_empty() || identity.source_revision.trim().is_empty() {
        return Some(format!("{role} runtime provenance is incomplete"));
    }
    None
}

fn capability_versions(capabilities: &[LabCapabilityVersion]) -> BTreeMap<String, u32> {
    let mut versions = BTreeMap::new();
    for capability in capabilities {
        // Conflicting duplicate declarations are deliberately absent from the
        // result, so they cannot accidentally satisfy a requirement.
        match versions.get(&capability.id) {
            Some(previous) if *previous != capability.version => {
                versions.remove(&capability.id);
            }
            Some(_) => {}
            None => {
                versions.insert(capability.id.clone(), capability.version);
            }
        }
    }
    versions
}

fn advertised_capability_versions(
    capabilities: &[LabCapabilityVersion],
) -> BTreeMap<String, BTreeSet<u32>> {
    let mut versions = BTreeMap::new();
    for capability in capabilities {
        versions
            .entry(capability.id.clone())
            .or_insert_with(BTreeSet::new)
            .insert(capability.version);
    }
    versions
}

#[cfg(test)]
mod capability_handshake_tests {
    use super::*;

    fn identity(build: &str, source: &str) -> LabRuntimeIdentity {
        LabRuntimeIdentity {
            build_identity: build.to_string(),
            source_revision: source.to_string(),
            clean: true,
        }
    }

    fn handshake(ancestry: LabRuntimeAncestry) -> LabCapabilityHandshake {
        let capabilities = required_lab_handoff_capabilities();
        LabCapabilityHandshake {
            controller: identity("homeboy 1.0.0+controller", "a"),
            required_capabilities: capabilities.clone(),
            runner_command: identity("homeboy 1.0.1+runner", "b"),
            runner_command_capabilities: capabilities.clone(),
            daemon: identity("homeboy 1.0.1+daemon", "b"),
            daemon_capabilities: capabilities,
            ancestry,
        }
    }

    #[test]
    fn admits_controller_n_with_verified_runner_n_plus_one_and_records_provenance() {
        let admission = negotiate_lab_capability_handshake(&handshake(
            LabRuntimeAncestry::VerifiedNewerDescendant,
        ));
        assert!(admission.compatible);
        assert_eq!(
            admission.provenance.schema,
            "homeboy/lab-capability-negotiation/v1"
        );
        assert_eq!(admission.provenance.negotiated_capabilities.len(), 5);
    }

    #[test]
    fn rejects_controller_n_plus_one_with_older_runner() {
        let admission = negotiate_lab_capability_handshake(&handshake(LabRuntimeAncestry::Older));
        assert!(!admission.compatible);
    }

    #[test]
    fn admits_cross_platform_same_source_builds() {
        let mut handshake = handshake(LabRuntimeAncestry::ExactSource);
        handshake.runner_command = identity("homeboy 1.0.0+linux", "a");
        handshake.daemon = identity("homeboy 1.0.0+macos", "a");
        assert!(negotiate_lab_capability_handshake(&handshake).compatible);
    }

    #[test]
    fn rejects_diverged_unknown_and_dirty_provenance() {
        for ancestry in [LabRuntimeAncestry::Diverged, LabRuntimeAncestry::Unknown] {
            assert!(!negotiate_lab_capability_handshake(&handshake(ancestry)).compatible);
        }
        let mut dirty = handshake(LabRuntimeAncestry::VerifiedNewerDescendant);
        dirty.daemon.clean = false;
        assert!(!negotiate_lab_capability_handshake(&dirty).compatible);
    }

    #[test]
    fn rejects_capability_removal_schema_incompatibility_and_command_daemon_divergence() {
        let mut removed = handshake(LabRuntimeAncestry::VerifiedNewerDescendant);
        removed.daemon_capabilities.pop();
        assert!(!negotiate_lab_capability_handshake(&removed).compatible);

        let mut incompatible = handshake(LabRuntimeAncestry::VerifiedNewerDescendant);
        incompatible.runner_command_capabilities[3].version = 2;
        assert!(!negotiate_lab_capability_handshake(&incompatible).compatible);

        let mut divergent = handshake(LabRuntimeAncestry::VerifiedNewerDescendant);
        divergent.daemon.source_revision = "c".to_string();
        assert!(!negotiate_lab_capability_handshake(&divergent).compatible);
    }

    #[test]
    fn accepts_a_newer_capability_that_explicitly_advertises_the_required_version() {
        let mut handshake = handshake(LabRuntimeAncestry::VerifiedNewerDescendant);
        handshake
            .runner_command_capabilities
            .push(LabCapabilityVersion {
                id: "result-schema".to_string(),
                version: 2,
            });
        handshake.daemon_capabilities.push(LabCapabilityVersion {
            id: "result-schema".to_string(),
            version: 2,
        });
        assert!(negotiate_lab_capability_handshake(&handshake).compatible);
    }
}

#[cfg(test)]
mod daemon_recovery_capability_tests {
    use super::*;

    #[test]
    fn recovery_capabilities_are_their_own_protocol_focused_list() {
        let recovery = daemon_recovery_capabilities();
        let ids: BTreeSet<_> = recovery
            .iter()
            .map(|capability| capability.id.as_str())
            .collect();
        let mut expected = BTreeSet::from([
            DAEMON_RECOVERY_LEASELESS_CAPABILITY,
            DAEMON_RECOVERY_STATE_LOSS_CAPABILITY,
            DAEMON_ENSURE_RUNNING_OPERATION_ID_CAPABILITY,
        ]);
        if unleased_candidate_reconciliation_supported() {
            expected.insert(DAEMON_RECOVERY_UNLEASED_CANDIDATES_CAPABILITY);
        }
        assert_eq!(ids, expected);
        assert!(recovery.iter().all(|capability| capability.version == 1));

        let handoff_capabilities = required_lab_handoff_capabilities();
        let handoff: BTreeSet<_> = handoff_capabilities
            .iter()
            .map(|capability| capability.id.as_str())
            .collect();
        assert!(
            ids.is_disjoint(&handoff),
            "recovery capabilities must not leak into the admission handshake"
        );
    }

    #[test]
    fn recovery_capability_requires_typed_advertisement() {
        let advertised: [LabCapabilityVersion; 1] = [LabCapabilityVersion {
            id: DAEMON_RECOVERY_LEASELESS_CAPABILITY.to_string(),
            version: 1,
        }];
        assert!(daemon_recovery_capability_advertised(
            Some(advertised.as_slice()),
            DAEMON_RECOVERY_LEASELESS_CAPABILITY,
        ));
        assert!(!daemon_recovery_capability_advertised(
            Some(advertised.as_slice()),
            DAEMON_RECOVERY_STATE_LOSS_CAPABILITY,
        ));
        assert!(!daemon_recovery_capability_advertised(
            None,
            DAEMON_RECOVERY_LEASELESS_CAPABILITY,
        ));
    }
}
