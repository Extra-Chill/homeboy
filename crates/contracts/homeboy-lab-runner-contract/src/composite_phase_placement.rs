//! Placement planning for controller-owned composite workflows.
//!
//! A composite workflow can prepare controller state before handing a workload
//! to Lab. Each phase therefore owns its requested placement instead of sharing
//! one ambient command-level placement.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{EffectiveExecutionPlacement, Placement};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositeWorkflowPhaseKind {
    ControllerSetup,
    Workload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositeWorkflowPhase {
    pub phase: String,
    pub capability: String,
    pub kind: CompositeWorkflowPhaseKind,
    pub requested: Placement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositePhasePlacement {
    pub phase: String,
    pub capability: String,
    pub placement: EffectiveExecutionPlacement,
    pub reason: String,
}

/// The immutable placement decisions a composite controller must preflight
/// before its first mutation. This same record is handoff evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositePhasePlacementPlan {
    pub schema: String,
    pub phases: Vec<CompositePhasePlacement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositePhasePlacementError {
    pub phase: String,
    pub capability: String,
    pub reason: String,
}

impl fmt::Display for CompositePhasePlacementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "phase '{}' ({}) cannot use its requested placement: {}",
            self.phase, self.capability, self.reason
        )
    }
}

impl std::error::Error for CompositePhasePlacementError {}

impl CompositePhasePlacementPlan {
    pub const SCHEMA: &'static str = "homeboy/composite-phase-placement/v1";

    /// Resolve controller setup locally and delegate every workload decision to
    /// the caller's workload-placement policy. The closure prevents a workload
    /// policy from becoming ambient input to unlike controller phases.
    pub fn resolve<F>(
        phases: impl IntoIterator<Item = CompositeWorkflowPhase>,
        mut resolve_workload: F,
    ) -> Result<Self, CompositePhasePlacementError>
    where
        F: FnMut(
            &CompositeWorkflowPhase,
        )
            -> Result<(EffectiveExecutionPlacement, String), CompositePhasePlacementError>,
    {
        let mut resolved = Vec::new();
        for phase in phases {
            let (placement, reason) = match phase.kind {
                CompositeWorkflowPhaseKind::ControllerSetup => {
                    if phase.requested == Placement::Lab {
                        return Err(CompositePhasePlacementError {
                            phase: phase.phase,
                            capability: phase.capability,
                            reason: "Lab is unavailable for controller-local setup".to_string(),
                        });
                    }
                    (
                        EffectiveExecutionPlacement::Local,
                        "controller-local setup".to_string(),
                    )
                }
                CompositeWorkflowPhaseKind::Workload => resolve_workload(&phase)?,
            };
            resolved.push(CompositePhasePlacement {
                phase: phase.phase,
                capability: phase.capability,
                placement,
                reason,
            });
        }
        Ok(Self {
            schema: Self::SCHEMA.to_string(),
            phases: resolved,
        })
    }

    /// The preflight record is also the exact evidence retained with handoff.
    pub fn handoff_evidence(&self) -> Self {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(
        phase: &str,
        capability: &str,
        kind: CompositeWorkflowPhaseKind,
        requested: Placement,
    ) -> CompositeWorkflowPhase {
        CompositeWorkflowPhase {
            phase: phase.to_string(),
            capability: capability.to_string(),
            kind,
            requested,
        }
    }

    #[test]
    fn resolves_controller_setup_locally_and_a_lab_workload_independently() {
        let plan = CompositePhasePlacementPlan::resolve(
            [
                phase(
                    "install",
                    "rig.install",
                    CompositeWorkflowPhaseKind::ControllerSetup,
                    Placement::Auto,
                ),
                phase(
                    "sync",
                    "rig.sync",
                    CompositeWorkflowPhaseKind::ControllerSetup,
                    Placement::Auto,
                ),
                phase(
                    "workload",
                    "fixture.workload",
                    CompositeWorkflowPhaseKind::Workload,
                    Placement::Lab,
                ),
            ],
            |workload| {
                assert_eq!(workload.requested, Placement::Lab);
                Ok((
                    EffectiveExecutionPlacement::Lab,
                    "explicit Lab workload policy".to_string(),
                ))
            },
        )
        .expect("independent controller and workload placement resolves");

        assert_eq!(plan.schema, CompositePhasePlacementPlan::SCHEMA);
        assert_eq!(
            plan.phases,
            [
                CompositePhasePlacement {
                    phase: "install".to_string(),
                    capability: "rig.install".to_string(),
                    placement: EffectiveExecutionPlacement::Local,
                    reason: "controller-local setup".to_string(),
                },
                CompositePhasePlacement {
                    phase: "sync".to_string(),
                    capability: "rig.sync".to_string(),
                    placement: EffectiveExecutionPlacement::Local,
                    reason: "controller-local setup".to_string(),
                },
                CompositePhasePlacement {
                    phase: "workload".to_string(),
                    capability: "fixture.workload".to_string(),
                    placement: EffectiveExecutionPlacement::Lab,
                    reason: "explicit Lab workload policy".to_string(),
                },
            ]
        );
        assert_eq!(plan.handoff_evidence(), plan);
    }

    #[test]
    fn rejects_explicit_lab_for_controller_setup_before_workload_resolution() {
        let error = CompositePhasePlacementPlan::resolve(
            [phase(
                "install",
                "source.install",
                CompositeWorkflowPhaseKind::ControllerSetup,
                Placement::Lab,
            )],
            |_| panic!("controller setup must fail before workload resolution"),
        )
        .expect_err("controller setup is not Lab portable");

        assert_eq!(error.phase, "install");
        assert_eq!(error.capability, "source.install");
        assert_eq!(
            error.reason,
            "Lab is unavailable for controller-local setup"
        );
    }
}
