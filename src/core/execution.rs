//! Generic execution, change artifact, and apply contracts.
//!
//! `HomeboyPlan` remains the planning substrate: it describes what can run and
//! why. These contracts describe what was requested, what ran, which proposed
//! change artifacts were produced, and how those artifacts were applied.
//! Domain commands and extensions should project their existing output into
//! these ecosystem-agnostic shapes when they need a shared execution boundary.
//!
//! Apply and publish are intentionally separate phases. Apply adapters verify an
//! approved artifact and mutate a local worktree; publish requests cover durable
//! externalization such as commit, push, pull request, release, or deploy steps.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::core::plan::{HomeboyPlan, PlanSubject};

/// Normalized execution intent shared by CLI flags and extension adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Build or inspect a plan without executing its steps.
    Plan,
    /// Run enough to preview effects without writing durable changes.
    DryRun,
    /// Execute and capture a proposed change artifact for review/approval.
    CapturePatch,
    /// Apply an approved or otherwise permitted change artifact.
    Apply,
    /// Execute the requested workflow directly.
    Execute,
}

/// Canonical lifecycle phase vocabulary shared by commands and extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    /// Produce proposed results or change artifacts.
    Execute,
    /// Preserve proposed changes with provenance and digest metadata.
    Artifact,
    /// Record the exact artifact, run, step, or file scope approved by a human
    /// or policy gate.
    Approve,
    /// Materialize an approved artifact in a local worktree.
    Apply,
    /// Commit, push, open a pull request, release, deploy, or otherwise expose
    /// an applied change outside the local worktree.
    Publish,
}

impl ExecutionMode {
    /// Normalize common CLI spellings into the shared mode names.
    pub(crate) fn from_cli_value(value: &str) -> Option<Self> {
        match value {
            "plan" | "preview" => Some(Self::Plan),
            "dry-run" | "dry_run" => Some(Self::DryRun),
            "capture-patch" | "capture_patch" => Some(Self::CapturePatch),
            "apply" | "write" => Some(Self::Apply),
            "execute" | "run" => Some(Self::Execute),
            _ => None,
        }
    }
}

impl std::str::FromStr for ExecutionMode {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Self::from_cli_value(value).ok_or_else(|| format!("unknown execution mode: {value}"))
    }
}

/// High-level request to execute a plan or command-shaped workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub id: String,
    pub mode: ExecutionMode,
    pub subject: PlanSubject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<HomeboyPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_scope: Option<ApprovalScope>,
    #[serde(flatten)]
    pub controls: ExecutionControls,
}

/// Shared arbitrary input/policy payload for execution and apply requests.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExecutionControls {
    #[serde(default, rename = "inputs", skip_serializing_if = "HashMap::is_empty")]
    pub execution_inputs: HashMap<String, serde_json::Value>,
    #[serde(default, rename = "policy", skip_serializing_if = "HashMap::is_empty")]
    pub execution_policy: HashMap<String, serde_json::Value>,
}

/// Result of executing a request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRun {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub mode: ExecutionMode,
    pub subject: PlanSubject,
    pub status: ExecutionStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<ExecutionStepResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ChangeArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Shared lifecycle status for execution runs, steps, and apply results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Pending,
    Running,
    AwaitingApproval,
    ArtifactProduced,
    Approved,
    Applied,
    Published,
    Success,
    PartialSuccess,
    Skipped,
    Missing,
    Failed,
}

/// Result for one executed step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionStepResult {
    pub id: String,
    pub kind: String,
    pub status: ExecutionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A proposed or captured change that can be approved and applied later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeArtifact {
    pub id: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub provenance: ChangeArtifactProvenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_scope: Option<ApprovalScope>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Where a change artifact came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeArtifactProvenance {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
}

/// Approval boundary for a request or produced artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope")]
pub enum ApprovalScope {
    Step { step_id: String },
    Artifact { artifact_id: String },
    Run { run_id: String },
    External { id: String },
}

/// Request to apply a captured change artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyRequest {
    pub id: String,
    #[serde(default = "apply_phase")]
    pub phase: ExecutionPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    pub artifact: ChangeArtifact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_scope: Option<ApprovalScope>,
    #[serde(flatten)]
    pub controls: ExecutionControls,
}

/// Result of applying a captured change artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyResult {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default = "apply_phase")]
    pub phase: ExecutionPhase,
    pub status: ExecutionStatus,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_changed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preflight_failures: Vec<ApplyPreflightFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ChangeArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Contract advertised by an apply adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyAdapterContract {
    /// Stable adapter id, for example `homeboy/lab-apply-adapter/v1` or
    /// `homeboy/wp-codebox-apply-adapter/v1`.
    pub id: String,
    #[serde(default = "apply_phase")]
    pub phase: ExecutionPhase,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_types: Vec<String>,
    pub preflight_policy: ApplyPreflightPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish_boundaries: Vec<PublishOperation>,
}

/// Shared safety policy an apply adapter enforces before mutating a worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyPreflightPolicy {
    pub require_clean_worktree: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_branches: Vec<String>,
    pub require_approval_coverage: bool,
    pub require_snapshot_match: bool,
    pub require_path_confinement: bool,
    pub require_staged_file_match: bool,
}

impl Default for ApplyPreflightPolicy {
    fn default() -> Self {
        Self {
            require_clean_worktree: true,
            protected_branches: vec![
                "main".to_string(),
                "master".to_string(),
                "trunk".to_string(),
            ],
            require_approval_coverage: true,
            require_snapshot_match: true,
            require_path_confinement: true,
            require_staged_file_match: true,
        }
    }
}

/// Machine-readable preflight checks shared by Lab and WP Codebox-style apply
/// adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPreflightCheck {
    CleanWorktree,
    ProtectedBranch,
    ApprovalCoverage,
    SnapshotDrift,
    PathConfinement,
    StagedFileExpectation,
}

/// Reason an apply request cannot safely mutate the local worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyPreflightFailure {
    pub check: ApplyPreflightCheck,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

/// Durable externalization step that happens after apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishOperation {
    Commit,
    Push,
    PullRequest,
    Release,
    Deploy,
}

/// Request to publish an already-applied change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishRequest {
    pub id: String,
    #[serde(default = "publish_phase")]
    pub phase: ExecutionPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_result_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<PublishOperation>,
    #[serde(flatten)]
    pub controls: ExecutionControls,
}

/// Result of publishing an already-applied change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishResult {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default = "publish_phase")]
    pub phase: ExecutionPhase,
    pub status: ExecutionStatus,
    pub published: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<PublishOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

const fn apply_phase() -> ExecutionPhase {
    ExecutionPhase::Apply
}

const fn publish_phase() -> ExecutionPhase {
    ExecutionPhase::Publish
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plan::{HomeboyPlan, PlanKind};
    use crate::core::release::{
        ReleaseRun, ReleaseRunResult, ReleaseStepResult, ReleaseStepStatus,
    };

    #[test]
    fn execution_mode_normalizes_cli_spellings() {
        assert_eq!(
            ExecutionMode::from_cli_value("dry-run"),
            Some(ExecutionMode::DryRun)
        );
        assert_eq!(
            ExecutionMode::from_cli_value("write"),
            Some(ExecutionMode::Apply)
        );
        assert_eq!(
            ExecutionMode::from_cli_value("execute"),
            Some(ExecutionMode::Execute)
        );
        assert_eq!(ExecutionMode::from_cli_value("unknown"), None);
    }

    #[test]
    fn request_round_trips_with_plan() {
        let plan = HomeboyPlan::for_component(PlanKind::Refactor, "homeboy");
        let request = ExecutionRequest {
            id: "exec-1".to_string(),
            mode: ExecutionMode::CapturePatch,
            subject: plan.subject.clone(),
            plan: Some(plan),
            approval_scope: Some(ApprovalScope::Run {
                run_id: "run-1".to_string(),
            }),
            controls: ExecutionControls::default(),
        };

        let json = serde_json::to_string(&request).expect("serialize request");
        let parsed: ExecutionRequest = serde_json::from_str(&json).expect("parse request");

        assert_eq!(parsed.mode, ExecutionMode::CapturePatch);
        assert_eq!(parsed.subject.component_id.as_deref(), Some("homeboy"));
        assert!(matches!(
            parsed.approval_scope,
            Some(ApprovalScope::Run { .. })
        ));
    }

    #[test]
    fn change_artifact_round_trips_with_provenance() {
        let artifact = ChangeArtifact {
            id: "patch-1".to_string(),
            artifact_type: "patch".to_string(),
            provenance: ChangeArtifactProvenance {
                source: "refactor".to_string(),
                run_id: Some("run-1".to_string()),
                step_id: Some("collect".to_string()),
                command: Some("homeboy refactor --dry-run".to_string()),
                captured_at: Some("2026-05-30T00:00:00Z".to_string()),
            },
            title: Some("Refactor preview".to_string()),
            summary: Some("One file would change".to_string()),
            path: Some("artifacts/refactor.patch".to_string()),
            files: vec!["src/lib.rs".to_string()],
            diff: None,
            approval_scope: Some(ApprovalScope::Artifact {
                artifact_id: "patch-1".to_string(),
            }),
            metadata: HashMap::new(),
        };

        let value = serde_json::to_value(&artifact).expect("serialize artifact");

        assert_eq!(value["type"], "patch");
        assert_eq!(value["provenance"]["source"], "refactor");
        assert_eq!(value["approval_scope"]["scope"], "artifact");
        let parsed: ChangeArtifact = serde_json::from_value(value).expect("parse artifact");
        assert_eq!(parsed.files, vec!["src/lib.rs"]);
    }

    #[test]
    fn apply_result_round_trips() {
        let result = ApplyResult {
            id: "apply-1".to_string(),
            request_id: Some("request-1".to_string()),
            phase: ExecutionPhase::Apply,
            status: ExecutionStatus::Applied,
            applied: true,
            files_changed: vec!["src/lib.rs".to_string()],
            preflight_failures: Vec::new(),
            artifacts: Vec::new(),
            warnings: Vec::new(),
            error: None,
            metadata: HashMap::new(),
        };

        let json = serde_json::to_string(&result).expect("serialize apply result");
        let parsed: ApplyResult = serde_json::from_str(&json).expect("parse apply result");

        assert!(parsed.applied);
        assert_eq!(parsed.status, ExecutionStatus::Applied);
        assert_eq!(parsed.files_changed, vec!["src/lib.rs"]);
    }

    #[test]
    fn apply_request_defaults_to_apply_phase_and_names_adapter() {
        let artifact = patch_artifact();
        let request = ApplyRequest {
            id: "apply-request-1".to_string(),
            phase: ExecutionPhase::Apply,
            adapter: Some("homeboy/lab-apply-adapter/v1".to_string()),
            artifact,
            approval_scope: Some(ApprovalScope::Artifact {
                artifact_id: "patch-1".to_string(),
            }),
            controls: ExecutionControls::default(),
        };

        let value = serde_json::to_value(&request).expect("serialize request");

        assert_eq!(value["phase"], "apply");
        assert_eq!(value["adapter"], "homeboy/lab-apply-adapter/v1");
        assert!(value.get("operations").is_none());
    }

    #[test]
    fn publish_request_is_separate_from_apply_request() {
        let request = PublishRequest {
            id: "publish-request-1".to_string(),
            phase: ExecutionPhase::Publish,
            apply_result_id: Some("apply-1".to_string()),
            operations: vec![PublishOperation::Commit, PublishOperation::PullRequest],
            controls: ExecutionControls::default(),
        };

        let value = serde_json::to_value(&request).expect("serialize publish request");

        assert_eq!(value["phase"], "publish");
        assert_eq!(value["apply_result_id"], "apply-1");
        assert_eq!(
            value["operations"],
            serde_json::json!(["commit", "pull_request"])
        );
        assert!(value.get("artifact").is_none());
    }

    #[test]
    fn apply_adapter_contract_documents_wp_codebox_boundaries() {
        let contract = ApplyAdapterContract {
            id: "homeboy/wp-codebox-apply-adapter/v1".to_string(),
            phase: ExecutionPhase::Apply,
            artifact_types: vec![
                "wp_codebox.bundle".to_string(),
                "wp_codebox.file".to_string(),
            ],
            preflight_policy: ApplyPreflightPolicy::default(),
            publish_boundaries: vec![
                PublishOperation::Commit,
                PublishOperation::Push,
                PublishOperation::PullRequest,
            ],
        };

        assert_eq!(contract.phase, ExecutionPhase::Apply);
        assert!(contract.preflight_policy.require_clean_worktree);
        assert!(contract.preflight_policy.require_approval_coverage);
        assert!(contract.preflight_policy.require_snapshot_match);
        assert_eq!(contract.publish_boundaries[0], PublishOperation::Commit);
    }

    #[test]
    fn apply_result_reports_core_preflight_failures() {
        let result = ApplyResult {
            id: "apply-1".to_string(),
            request_id: Some("request-1".to_string()),
            phase: ExecutionPhase::Apply,
            status: ExecutionStatus::Failed,
            applied: false,
            files_changed: Vec::new(),
            preflight_failures: vec![
                ApplyPreflightFailure {
                    check: ApplyPreflightCheck::CleanWorktree,
                    reason: "worktree has uncommitted changes".to_string(),
                    subject: Some("/repo".to_string()),
                    details: vec!["M src/lib.rs".to_string()],
                },
                ApplyPreflightFailure {
                    check: ApplyPreflightCheck::ProtectedBranch,
                    reason: "refusing to apply directly on protected branch".to_string(),
                    subject: Some("main".to_string()),
                    details: Vec::new(),
                },
                ApplyPreflightFailure {
                    check: ApplyPreflightCheck::ApprovalCoverage,
                    reason: "approval does not cover every artifact file".to_string(),
                    subject: Some("patch-1".to_string()),
                    details: vec!["missing src/other.rs".to_string()],
                },
                ApplyPreflightFailure {
                    check: ApplyPreflightCheck::SnapshotDrift,
                    reason: "source snapshot hash changed".to_string(),
                    subject: Some("snapshot".to_string()),
                    details: vec!["expected abc, current def".to_string()],
                },
            ],
            artifacts: Vec::new(),
            warnings: Vec::new(),
            error: Some("apply preflight failed".to_string()),
            metadata: HashMap::new(),
        };

        let parsed: ApplyResult =
            serde_json::from_value(serde_json::to_value(&result).expect("serialize apply result"))
                .expect("parse apply result");

        assert!(!parsed.applied);
        assert_eq!(parsed.preflight_failures.len(), 4);
        assert_eq!(
            parsed.preflight_failures[0].check,
            ApplyPreflightCheck::CleanWorktree
        );
        assert_eq!(
            parsed.preflight_failures[3].check,
            ApplyPreflightCheck::SnapshotDrift
        );
    }

    #[test]
    fn release_run_projects_into_execution_run() {
        let release = ReleaseRun {
            component_id: "homeboy".to_string(),
            enabled: true,
            result: ReleaseRunResult {
                steps: vec![ReleaseStepResult {
                    id: "package".to_string(),
                    step_type: "release.package".to_string(),
                    status: ReleaseStepStatus::Success,
                    missing: Vec::new(),
                    warnings: Vec::new(),
                    hints: Vec::new(),
                    data: Some(serde_json::json!([
                        {
                            "path": "artifacts/homeboy.tar.gz",
                            "artifact_type": "archive",
                            "platform": "darwin"
                        }
                    ])),
                    error: None,
                }],
                status: ReleaseStepStatus::Success,
                warnings: vec!["signed artifact missing".to_string()],
                summary: None,
            },
        };

        let execution = ExecutionRun::from(&release);

        assert_eq!(execution.id, "release.homeboy");
        assert_eq!(execution.mode, ExecutionMode::Execute);
        assert_eq!(execution.status, ExecutionStatus::Success);
        assert_eq!(execution.steps[0].status, ExecutionStatus::Success);
        assert_eq!(execution.steps[0].artifacts, vec!["package.artifact.1"]);
        assert_eq!(execution.artifacts.len(), 1);
        assert_eq!(execution.artifacts[0].artifact_type, "archive");
        assert_eq!(
            execution.artifacts[0].path.as_deref(),
            Some("artifacts/homeboy.tar.gz")
        );
        assert_eq!(execution.artifacts[0].provenance.source, "release");
        assert_eq!(execution.warnings, vec!["signed artifact missing"]);
    }

    fn patch_artifact() -> ChangeArtifact {
        ChangeArtifact {
            id: "patch-1".to_string(),
            artifact_type: "lab.patch.unified_diff".to_string(),
            provenance: ChangeArtifactProvenance {
                source: "runner.workspace.apply".to_string(),
                run_id: Some("run-1".to_string()),
                step_id: Some("capture".to_string()),
                command: Some("homeboy runner workspace apply".to_string()),
                captured_at: Some("2026-05-30T00:00:00Z".to_string()),
            },
            title: Some("Lab patch".to_string()),
            summary: Some("One file would change".to_string()),
            path: Some("artifacts/lab.patch".to_string()),
            files: vec!["src/lib.rs".to_string()],
            diff: None,
            approval_scope: Some(ApprovalScope::Artifact {
                artifact_id: "patch-1".to_string(),
            }),
            metadata: HashMap::new(),
        }
    }
}
