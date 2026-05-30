//! Generic execution, change artifact, and apply contracts.
//!
//! `HomeboyPlan` remains the planning substrate: it describes what can run and
//! why. These contracts describe what was requested, what ran, which proposed
//! change artifacts were produced, and how those artifacts were applied.
//! Domain commands and extensions should project their existing output into
//! these ecosystem-agnostic shapes when they need a shared execution boundary.
//!
//! Publishing is intentionally left as a follow-up seam. The first shared layer
//! models request/run/artifact/apply because those are the common contracts that
//! runner, refactor, release, and extension workflows already overlap on.

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
    pub status: ExecutionStatus,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_changed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ChangeArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
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
            status: ExecutionStatus::Applied,
            applied: true,
            files_changed: vec!["src/lib.rs".to_string()],
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
}
