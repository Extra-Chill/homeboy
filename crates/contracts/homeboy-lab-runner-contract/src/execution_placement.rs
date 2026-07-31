//! Immutable execution-placement routing record.
//!
//! Routing owns this record. Providers consume it and append verified execution
//! outcomes; they never derive a replacement decision from process state.

use serde::{Deserialize, Serialize};

use crate::Placement;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveExecutionPlacement {
    Local,
    Lab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPlacementRequirement {
    Local,
    Lab,
    Either,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerSelectionSource {
    Explicit,
    Policy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPlacementRunnerSelection {
    pub runner_id: String,
    pub source: RunnerSelectionSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPlacementFallback {
    pub local_allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPlacementOverrideAuthorization {
    pub authorized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPlacementIdentity {
    pub repository: String,
    pub workspace: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

/// The canonical, immutable result of execution-placement policy resolution.
///
/// `decision_id` is a deterministic content identity. A changed routing input
/// deliberately produces a distinct decision and invalidates any persisted
/// routing result that referenced the old identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPlacementDecision {
    pub decision_id: String,
    pub policy_id: String,
    pub policy_revision: String,
    pub identity: ExecutionPlacementIdentity,
    pub requested: Placement,
    pub required: ExecutionPlacementRequirement,
    pub selected: EffectiveExecutionPlacement,
    pub runner: Option<ExecutionPlacementRunnerSelection>,
    pub fallback: ExecutionPlacementFallback,
    pub override_authorization: ExecutionPlacementOverrideAuthorization,
}

/// Verified execution fact appended after the provider has run. It cannot
/// replace the routing decision and is intentionally authentication-free:
/// `RunnerJobExecutionContext` from #10943 supplies the authenticated attempt
/// binding when available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPlacementOutcome {
    pub decision_id: String,
    pub effective: EffectiveExecutionPlacement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
}

impl ExecutionPlacementDecision {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        policy_id: impl Into<String>,
        policy_revision: impl Into<String>,
        identity: ExecutionPlacementIdentity,
        requested: Placement,
        required: ExecutionPlacementRequirement,
        selected: EffectiveExecutionPlacement,
        runner: Option<ExecutionPlacementRunnerSelection>,
        fallback: ExecutionPlacementFallback,
        override_authorization: ExecutionPlacementOverrideAuthorization,
    ) -> Self {
        let policy_id = policy_id.into();
        let policy_revision = policy_revision.into();
        let decision_id = stable_id(
            &policy_id,
            &policy_revision,
            &identity,
            requested,
            required,
            selected,
            runner.as_ref(),
            &fallback,
            &override_authorization,
        );
        Self {
            decision_id,
            policy_id,
            policy_revision,
            identity,
            requested,
            required,
            selected,
            runner,
            fallback,
            override_authorization,
        }
    }

    pub fn permits_local_execution(&self) -> bool {
        self.selected == EffectiveExecutionPlacement::Local
            && self.required != ExecutionPlacementRequirement::Lab
            && (self.requested != Placement::Lab || self.override_authorization.authorized)
    }

    /// A policy-selected Lab attempt may fall back only when its immutable
    /// decision explicitly authorizes a verified local outcome. This is
    /// intentionally separate from a required Lab placement, even when both
    /// selected the same runner.
    pub fn permits_local_fallback(&self) -> bool {
        self.selected == EffectiveExecutionPlacement::Lab
            && self.fallback.local_allowed
            && self.required != ExecutionPlacementRequirement::Lab
    }

    pub fn verifies_outcome(&self, actual: EffectiveExecutionPlacement) -> bool {
        actual == self.selected
            || (actual == EffectiveExecutionPlacement::Local && self.permits_local_fallback())
    }

    pub fn outcome(
        &self,
        effective: EffectiveExecutionPlacement,
        runner_id: Option<String>,
    ) -> Option<ExecutionPlacementOutcome> {
        (self.verifies_outcome(effective)
            && (effective != EffectiveExecutionPlacement::Lab
                || self.runner.as_ref().map(|runner| runner.runner_id.as_str())
                    == runner_id.as_deref()))
        .then(|| ExecutionPlacementOutcome {
            decision_id: self.decision_id.clone(),
            effective,
            runner_id,
        })
    }

    /// Names immutable routing inputs that make this decision stale relative to
    /// a replacement. Callers persist these reasons before entering the
    /// lifecycle transition that creates the replacement decision.
    pub fn stale_reasons(&self, replacement: &Self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if self.identity.repository != replacement.identity.repository {
            reasons.push("repository_changed");
        }
        if self.identity.workspace != replacement.identity.workspace {
            reasons.push("workspace_changed");
        }
        if self.identity.task != replacement.identity.task {
            reasons.push("task_changed");
        }
        if self.identity.candidate != replacement.identity.candidate {
            reasons.push("candidate_changed");
        }
        if self.identity.base != replacement.identity.base {
            reasons.push("base_changed");
        }
        if self.policy_id != replacement.policy_id {
            reasons.push("policy_changed");
        }
        if self.policy_revision != replacement.policy_revision {
            reasons.push("policy_revision_changed");
        }
        if self.requested != replacement.requested {
            reasons.push("requested_placement_changed");
        }
        if self.required != replacement.required {
            reasons.push("placement_requirement_changed");
        }
        if self.selected != replacement.selected {
            reasons.push("selected_placement_changed");
        }
        if self.runner != replacement.runner {
            reasons.push("runner_selection_changed");
        }
        if self.fallback != replacement.fallback {
            reasons.push("fallback_policy_changed");
        }
        if self.override_authorization != replacement.override_authorization {
            reasons.push("override_authorization_changed");
        }
        reasons
    }
}

#[allow(clippy::too_many_arguments)]
fn stable_id(
    policy_id: &str,
    policy_revision: &str,
    identity: &ExecutionPlacementIdentity,
    requested: Placement,
    required: ExecutionPlacementRequirement,
    selected: EffectiveExecutionPlacement,
    runner: Option<&ExecutionPlacementRunnerSelection>,
    fallback: &ExecutionPlacementFallback,
    override_authorization: &ExecutionPlacementOverrideAuthorization,
) -> String {
    // FNV-1a is sufficient here: this is a stable evidence identity, not an
    // authorization token. #10943 supplies authenticated job context separately.
    let value = format!(
        "{policy_id}\0{policy_revision}\0{}\0{}\0{}\0{:?}\0{:?}\0{:?}\0{}\0{}\0{:?}\0{:?}\0{}\0{:?}\0{}\0{:?}",
        identity.repository,
        identity.workspace,
        identity.task,
        requested,
        required,
        selected,
        identity.candidate.as_deref().unwrap_or_default(),
        identity.base.as_deref().unwrap_or_default(),
        runner.map(|runner| &runner.runner_id),
        runner.map(|runner| runner.source),
        fallback.local_allowed,
        fallback.reason,
        override_authorization.authorized,
        override_authorization.authority,
    );
    let hash = value.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    format!("epd-{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(candidate: Option<&str>, revision: &str) -> ExecutionPlacementDecision {
        ExecutionPlacementDecision::new(
            "lab-route",
            revision,
            ExecutionPlacementIdentity {
                repository: "repo".to_string(),
                workspace: "workspace".to_string(),
                task: "test".to_string(),
                candidate: candidate.map(str::to_string),
                base: Some("base".to_string()),
            },
            Placement::Lab,
            ExecutionPlacementRequirement::Lab,
            EffectiveExecutionPlacement::Lab,
            Some(ExecutionPlacementRunnerSelection {
                runner_id: "lab-a".to_string(),
                source: RunnerSelectionSource::Explicit,
            }),
            ExecutionPlacementFallback {
                local_allowed: false,
                reason: None,
            },
            ExecutionPlacementOverrideAuthorization {
                authorized: false,
                authority: None,
            },
        )
    }

    #[test]
    fn candidate_and_policy_revision_invalidate_a_decision() {
        assert_ne!(
            decision(Some("a"), "1").decision_id,
            decision(Some("b"), "1").decision_id
        );
        assert_ne!(
            decision(Some("a"), "1").decision_id,
            decision(Some("a"), "2").decision_id
        );
    }

    #[test]
    fn required_lab_refuses_a_local_fallback() {
        let decision = decision(None, "1");
        assert!(!decision.verifies_outcome(EffectiveExecutionPlacement::Local));
        assert!(!decision.permits_local_fallback());
    }

    #[test]
    fn policy_selected_lab_can_verify_an_authorized_local_fallback() {
        let mut decision = decision(None, "1");
        decision.requested = Placement::LabOrLocal;
        decision.required = ExecutionPlacementRequirement::Either;
        decision.fallback.local_allowed = true;

        assert!(decision.permits_local_fallback());
        assert!(decision.verifies_outcome(EffectiveExecutionPlacement::Local));
    }

    #[test]
    fn stale_reasons_name_only_changed_routing_inputs() {
        let original = decision(Some("a"), "1");
        assert!(original.stale_reasons(&decision(Some("a"), "1")).is_empty());
        assert_eq!(
            original.stale_reasons(&decision(Some("b"), "2")),
            ["candidate_changed", "policy_revision_changed"]
        );
    }

    #[test]
    fn workspace_and_task_changes_invalidate_a_decision() {
        let original = decision(Some("a"), "1");
        let mut replacement = original.clone();
        replacement.identity.workspace = "other-workspace".to_string();
        replacement.identity.task = "other-task".to_string();

        assert_eq!(
            original.stale_reasons(&replacement),
            ["workspace_changed", "task_changed"]
        );
    }

    #[test]
    fn runner_selection_source_changes_the_stable_decision_identity() {
        let original = decision(Some("a"), "1");
        let mut replacement = original.clone();
        replacement
            .runner
            .as_mut()
            .expect("Lab runner selection")
            .source = RunnerSelectionSource::Policy;
        let replacement = ExecutionPlacementDecision::new(
            replacement.policy_id,
            replacement.policy_revision,
            replacement.identity,
            replacement.requested,
            replacement.required,
            replacement.selected,
            replacement.runner,
            replacement.fallback,
            replacement.override_authorization,
        );

        assert_eq!(
            original.stale_reasons(&replacement),
            ["runner_selection_changed"]
        );
        assert_ne!(original.decision_id, replacement.decision_id);
    }
}
