use super::*;
use std::fmt;
use std::str::FromStr;

pub const AGENT_TASK_APPLY_BACK_STRATEGY_MUTATION_ARTIFACTS: &str = "mutation_artifacts";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTaskApplyBackStrategy {
    MutationArtifacts,
}

impl AgentTaskApplyBackStrategy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MutationArtifacts => AGENT_TASK_APPLY_BACK_STRATEGY_MUTATION_ARTIFACTS,
        }
    }
}

impl fmt::Display for AgentTaskApplyBackStrategy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for AgentTaskApplyBackStrategy {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            AGENT_TASK_APPLY_BACK_STRATEGY_MUTATION_ARTIFACTS => Ok(Self::MutationArtifacts),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeContract {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskRuntimeLifecycleStates::is_empty"
    )]
    pub lifecycle_states: AgentTaskRuntimeLifecycleStates,
    #[serde(
        default,
        skip_serializing_if = "AgentTaskRuntimeNormalization::is_empty"
    )]
    pub normalization: AgentTaskRuntimeNormalization,
    #[serde(default, skip_serializing_if = "AgentTaskRuntimeApplyBack::is_empty")]
    pub apply_back: AgentTaskRuntimeApplyBack,
    /// Runtime dependency reconciliation contract: packages the runtime/overlay
    /// (e.g. "WordPress 7.0 supplies this Composer package") provides, which a
    /// staged provider plugin must NOT vendor/shadow. Homeboy validates the
    /// effective staged plugin against this before dispatch so a stale vendored
    /// runtime library fails with an actionable owner/contract message instead of
    /// a raw PHP fatal during plugin activation (#6223).
    #[serde(
        default,
        skip_serializing_if = "AgentTaskRuntimeStagingContract::is_empty"
    )]
    pub staging: AgentTaskRuntimeStagingContract,
}

impl AgentTaskRuntimeContract {
    pub(super) fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
            && self.lifecycle_states.is_empty()
            && self.normalization.is_empty()
            && self.apply_back.is_empty()
            && self.staging.is_empty()
    }
}

/// Declares the runtime dependency reconciliation surface for a provider's
/// staged plugins. Core is runtime-agnostic: it does not know that a package is
/// a Composer dependency or that the owner is WordPress — the declaring
/// extension supplies the package name, the owner that provides it at runtime,
/// and the vendor subpaths a staged plugin would use to shadow it. Homeboy uses
/// this to reconcile staged plugins before dispatch (#6223).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeStagingContract {
    /// Packages the runtime/overlay owns. A staged provider plugin that vendors
    /// any of these would shadow the runtime-provided version and is refused
    /// before dispatch with an actionable owner/package/contract error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reconciled_packages: Vec<AgentTaskRuntimeReconciledPackage>,
    /// Optional human-facing remediation appended to reconciliation conflict
    /// errors (e.g. how to rebuild the staged plugin without the vendored copy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    /// When true (default), Homeboy validates staged plugins against this
    /// contract before dispatch. Set false to declare the contract for evidence
    /// without enforcing the pre-dispatch gate (e.g. when Managed Sandbox owns the
    /// authoritative readiness check and returns a structured result).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validate_before_dispatch: Option<bool>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl AgentTaskRuntimeStagingContract {
    pub fn is_empty(&self) -> bool {
        self.reconciled_packages.is_empty()
            && self.remediation.is_none()
            && self.validate_before_dispatch.is_none()
            && self.extra.is_empty()
    }

    /// Whether the pre-dispatch validation gate is enforced. Defaults to true
    /// when any reconciled package is declared, so declaring a package is enough
    /// to opt into the gate; an explicit `validate_before_dispatch: false`
    /// records the contract for evidence/Sandbox delegation without gating.
    pub fn enforces_pre_dispatch(&self) -> bool {
        self.validate_before_dispatch.unwrap_or(true) && !self.reconciled_packages.is_empty()
    }
}

/// A single package the runtime owns that staged provider plugins must not
/// vendor. Generic by design: `name` is the package identity, `owner` is the
/// runtime/overlay that supplies it (named in conflict errors), and
/// `vendor_subpaths` are the staged-plugin-relative paths whose presence proves
/// the staged plugin shadows the runtime copy (e.g. `vendor/<org>/<pkg>`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeReconciledPackage {
    /// Package identity (e.g. a Composer package name `org/library`).
    pub name: String,
    /// The runtime/overlay that supplies this package at runtime. Surfaced in
    /// conflict errors so the operator knows who owns the canonical copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Staged-plugin-relative paths that, if present in the effective staged
    /// plugin, prove it vendors (and would shadow) the runtime-owned package.
    /// When omitted, `vendor/<name>` is checked by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vendor_subpaths: Vec<String>,
    /// Optional reason/why this package is reconciled, surfaced in diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Optional per-package remediation, surfaced when this package conflicts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl AgentTaskRuntimeReconciledPackage {
    /// The staged-plugin-relative subpaths whose presence indicates a shadowed
    /// runtime package. Falls back to `vendor/<name>` when none are declared.
    pub fn effective_vendor_subpaths(&self) -> Vec<String> {
        let declared: Vec<String> = self
            .vendor_subpaths
            .iter()
            .map(|subpath| subpath.trim().trim_matches('/').to_string())
            .filter(|subpath| !subpath.is_empty())
            .collect();
        if !declared.is_empty() {
            return declared;
        }
        let name = self.name.trim().trim_matches('/');
        if name.is_empty() {
            return Vec::new();
        }
        vec![format!("vendor/{name}")]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeApplyBack {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutation_artifacts: Vec<AgentTaskRuntimeMutationArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_git_checkout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
}

impl AgentTaskRuntimeApplyBack {
    pub(super) fn is_empty(&self) -> bool {
        self.mutation_artifacts.is_empty()
            && self.requires_git_checkout.is_none()
            && self.strategy.is_none()
    }

    pub fn strategy(&self) -> Option<AgentTaskApplyBackStrategy> {
        self.strategy
            .as_deref()
            .and_then(|value| AgentTaskApplyBackStrategy::from_str(value).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_back_strategy_parses_known_contract_value() {
        let apply_back = AgentTaskRuntimeApplyBack {
            strategy: Some(AgentTaskApplyBackStrategy::MutationArtifacts.to_string()),
            ..AgentTaskRuntimeApplyBack::default()
        };

        assert_eq!(
            apply_back.strategy(),
            Some(AgentTaskApplyBackStrategy::MutationArtifacts)
        );
    }

    #[test]
    fn apply_back_strategy_keeps_unknown_strings_non_breaking() {
        let apply_back = AgentTaskRuntimeApplyBack {
            strategy: Some("provider_owned_strategy".to_string()),
            ..AgentTaskRuntimeApplyBack::default()
        };

        assert_eq!(apply_back.strategy(), None);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeMutationArtifact {
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_method: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeLifecycleStates {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub execution_states: BTreeMap<String, AgentTaskExecutionState>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outcome_statuses: BTreeMap<String, AgentTaskOutcomeStatus>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failure_classifications: BTreeMap<String, AgentTaskFailureClassification>,
}

impl AgentTaskRuntimeLifecycleStates {
    pub(super) fn is_empty(&self) -> bool {
        self.execution_states.is_empty()
            && self.outcome_statuses.is_empty()
            && self.failure_classifications.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeNormalization {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_artifacts: Vec<AgentTaskRuntimeOutputArtifactMapping>,
}

impl AgentTaskRuntimeNormalization {
    pub(super) fn is_empty(&self) -> bool {
        self.status_path.is_none()
            && self.summary_path.is_none()
            && self.output_artifacts.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskRuntimeOutputArtifactMapping {
    pub name: String,
    #[serde(
        default,
        rename = "type",
        alias = "artifact_type",
        skip_serializing_if = "Option::is_none"
    )]
    pub artifact_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_schema: Option<String>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}
