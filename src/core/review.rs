//! Reusable review orchestration contract and artifact helpers.

use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use std::process::Command;

use crate::core::ci_profile::CiRunOutput;
use crate::core::code_audit::AuditCommandOutput;
use crate::core::execution::{self, PlanExecutionRun};
use crate::core::extension::lint::LintCommandOutput;
use crate::core::extension::test::TestCommandOutput;
use crate::core::finding::HomeboyFinding;
use crate::core::plan::{HomeboyPlan, PlanStep};
use crate::core::ObservationOutputMetadata;

mod artifact_findings;
pub mod render;

pub use artifact_findings::ReviewArtifactFindings;

/// Per-stage section of the consolidated review output.
#[derive(Serialize)]
pub struct ReviewStage<T: Serialize> {
    /// Stage name (`"audit"`, `"lint"`, `"test"`, or `"ci"`).
    pub stage: String,
    /// Whether the stage ran or was skipped.
    pub ran: bool,
    /// Stage-level pass/fail (only meaningful when `ran` is true).
    pub passed: bool,
    /// Stage exit code (0 when skipped).
    pub exit_code: i32,
    /// Number of findings the stage reported.
    pub finding_count: usize,
    /// Human-readable hint pointing to the per-stage command for deep dive.
    pub hint: String,
    /// Skip reason (only present when `ran` is false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    /// Full structured output from the underlying command. None if skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<T>,
}

/// Top-level summary block — what a reviewer would skim first.
#[derive(Serialize)]
pub struct ReviewSummary {
    /// True when every stage that ran exited 0.
    pub passed: bool,
    /// Top-line status string.
    pub status: String,
    /// Component label.
    pub component: String,
    /// Scope mode applied: `"changed-since"`, `"changed-only"`, or `"full"`.
    pub scope: String,
    /// The git ref passed to `--changed-since`, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<String>,
    /// Total findings across all stages that ran.
    pub total_findings: usize,
    /// Count of files in the changed set (None when not in scoped mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_file_count: Option<usize>,
    /// Top-level hints (e.g., empty changeset, scope warnings).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub hints: Vec<String>,
}

/// Unified output envelope for review orchestration.
#[derive(Serialize)]
pub struct ReviewCommandOutput {
    pub command: String,
    pub plan: HomeboyPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<ObservationOutputMetadata>,
    pub artifact: ReviewArtifact,
    pub summary: ReviewSummary,
    pub audit: ReviewStage<AuditCommandOutput>,
    pub lint: ReviewStage<LintCommandOutput>,
    pub test: ReviewStage<TestCommandOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_profile: Option<ReviewStage<CiRunOutput>>,
}

/// Stable machine-readable artifact for automated PR review consumers.
#[derive(Serialize, Clone)]
pub struct ReviewArtifact {
    pub schema: String,
    pub component: String,
    pub status: String,
    pub generated_at: String,
    pub base_ref: String,
    pub head_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<ObservationOutputMetadata>,
    pub commands: Vec<ReviewArtifactCommand>,
}

#[derive(Serialize, Clone)]
pub struct ReviewArtifactCommand {
    pub name: String,
    pub status: String,
    pub exit_code: i32,
    pub summary: String,
    pub findings: Vec<HomeboyFinding>,
    pub artifacts: Vec<Value>,
}

pub struct ReviewOutputInput {
    pub component: String,
    pub plan: HomeboyPlan,
    pub observation: Option<ObservationOutputMetadata>,
    pub scope: String,
    pub changed_since: Option<String>,
    pub changed_file_count: Option<usize>,
    pub head_ref: String,
    pub hints: Vec<String>,
}

pub struct ReviewStages {
    pub audit: ReviewStage<AuditCommandOutput>,
    pub lint: ReviewStage<LintCommandOutput>,
    pub test: ReviewStage<TestCommandOutput>,
    pub ci_profile: Option<ReviewStage<CiRunOutput>>,
}

pub struct ReviewService;

impl ReviewService {
    pub fn output_from_stages(
        input: ReviewOutputInput,
        stages: ReviewStages,
    ) -> (ReviewCommandOutput, i32) {
        let overall_passed = stages.audit.passed
            && stages.lint.passed
            && stages.test.passed
            && stages
                .ci_profile
                .as_ref()
                .map(|stage| stage.passed)
                .unwrap_or(true);
        let overall_exit = review_exit_code(
            overall_passed,
            [
                stages.audit.exit_code,
                stages.lint.exit_code,
                stages.test.exit_code,
                stages
                    .ci_profile
                    .as_ref()
                    .map(|stage| stage.exit_code)
                    .unwrap_or(0),
            ],
        );
        let total_findings = stages.audit.finding_count
            + stages.lint.finding_count
            + stages.test.finding_count
            + stages
                .ci_profile
                .as_ref()
                .map(|stage| stage.finding_count)
                .unwrap_or(0);
        let mut artifact = build_artifact(
            &input.component,
            input.changed_since.as_deref().unwrap_or(""),
            &input.head_ref,
            artifact_commands(&stages),
        );
        artifact.observation = input.observation.clone();

        let output = ReviewCommandOutput {
            command: "review".to_string(),
            plan: input.plan,
            observation: input.observation,
            artifact,
            summary: ReviewSummary {
                passed: overall_passed,
                status: if overall_passed { "passed" } else { "failed" }.to_string(),
                component: input.component,
                scope: input.scope,
                changed_since: input.changed_since,
                total_findings,
                changed_file_count: input.changed_file_count,
                hints: input.hints,
            },
            audit: stages.audit,
            lint: stages.lint,
            test: stages.test,
            ci_profile: stages.ci_profile,
        };

        (output, overall_exit)
    }

    pub fn skipped_output(
        input: ReviewOutputInput,
        reason: &str,
        include_ci_profile: bool,
    ) -> ReviewCommandOutput {
        let stages = ReviewStages {
            audit: stage_skipped("audit", reason),
            lint: stage_skipped("lint", reason),
            test: stage_skipped("test", reason),
            ci_profile: include_ci_profile.then(|| stage_skipped("ci", reason)),
        };
        let (output, _) = Self::output_from_stages(input, stages);
        output
    }
}

fn review_exit_code(passed: bool, codes: [i32; 4]) -> i32 {
    if passed {
        0
    } else if codes.iter().any(|&code| code >= 2) {
        2
    } else {
        1
    }
}

fn artifact_commands(stages: &ReviewStages) -> Vec<ReviewArtifactCommand> {
    let mut commands = vec![
        artifact_command(&stages.audit),
        artifact_command(&stages.lint),
        artifact_command(&stages.test),
    ];
    if let Some(ref stage) = stages.ci_profile {
        commands.push(artifact_command(stage));
    }
    commands
}

pub fn execute_review_plan_steps<R, Dispatch>(
    steps: &[PlanStep],
    dispatch: Dispatch,
) -> crate::core::Result<PlanExecutionRun<R>>
where
    Dispatch: FnMut(&PlanStep) -> crate::core::Result<Option<R>>,
{
    execution::execute_plan_steps(steps, dispatch, |_| false)
}

pub fn stage_skipped<T: Serialize>(stage: &str, reason: &str) -> ReviewStage<T> {
    ReviewStage {
        stage: stage.to_string(),
        ran: false,
        passed: true,
        exit_code: 0,
        finding_count: 0,
        hint: format!("Run individually: homeboy {}", stage),
        skipped_reason: Some(reason.to_string()),
        output: None,
    }
}

pub fn build_artifact(
    component: &str,
    base_ref: &str,
    head_ref: &str,
    commands: Vec<ReviewArtifactCommand>,
) -> ReviewArtifact {
    let status = artifact_status(&commands).to_string();
    ReviewArtifact {
        schema: "homeboy/review/v1".to_string(),
        component: component.to_string(),
        status,
        generated_at: generated_at_now(),
        base_ref: base_ref.to_string(),
        head_ref: head_ref.to_string(),
        observation: None,
        commands,
    }
}

pub fn artifact_command<T: Serialize + ReviewArtifactFindings>(
    stage: &ReviewStage<T>,
) -> ReviewArtifactCommand {
    ReviewArtifactCommand {
        name: stage.stage.clone(),
        status: if !stage.ran {
            "skipped"
        } else if stage.passed {
            "passed"
        } else {
            "failed"
        }
        .to_string(),
        exit_code: stage.exit_code,
        summary: if !stage.ran {
            stage
                .skipped_reason
                .clone()
                .unwrap_or_else(|| "skipped".to_string())
        } else {
            format!(
                "{} finding(s); {}",
                stage.finding_count,
                if stage.passed { "passed" } else { "failed" }
            )
        },
        findings: stage
            .output
            .as_ref()
            .map(ReviewArtifactFindings::review_artifact_findings)
            .unwrap_or_default(),
        artifacts: Vec::new(),
    }
}

pub fn artifact_status(commands: &[ReviewArtifactCommand]) -> &'static str {
    let ran = commands
        .iter()
        .filter(|command| command.status != "skipped")
        .count();
    if ran == 0 {
        return "skipped";
    }
    if commands.iter().any(|command| command.status == "failed") {
        return "failed";
    }
    if ran < commands.len() {
        return "partial";
    }
    "passed"
}

pub fn git_ref(path: &str, git_ref: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", git_ref])
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn generated_at_now() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_skipped_helper_marks_not_ran() {
        let stage: ReviewStage<serde_json::Value> = stage_skipped("audit", "no files changed");
        assert!(!stage.ran);
        assert!(stage.passed);
        assert_eq!(stage.exit_code, 0);
        assert_eq!(stage.skipped_reason.as_deref(), Some("no files changed"));
    }

    #[test]
    fn artifact_command_maps_stage_statuses() {
        let skipped: ReviewStage<serde_json::Value> = stage_skipped("audit", "no files changed");
        let skipped_command = artifact_command(&skipped);
        assert_eq!(skipped_command.name, "audit");
        assert_eq!(skipped_command.status, "skipped");
        assert_eq!(skipped_command.exit_code, 0);
        assert_eq!(skipped_command.summary, "no files changed");
        assert!(skipped_command.findings.is_empty());
        assert!(skipped_command.artifacts.is_empty());

        let failed = ReviewStage {
            stage: "lint".to_string(),
            ran: true,
            passed: false,
            exit_code: 1,
            finding_count: 3,
            hint: "Deep dive: homeboy lint".to_string(),
            skipped_reason: None,
            output: Some(serde_json::json!({ "ok": false })),
        };
        let failed_command = artifact_command(&failed);
        assert_eq!(failed_command.name, "lint");
        assert_eq!(failed_command.status, "failed");
        assert_eq!(failed_command.exit_code, 1);
        assert_eq!(failed_command.summary, "3 finding(s); failed");
    }

    #[test]
    fn artifact_status_covers_contract_values() {
        let passed = ReviewArtifactCommand {
            name: "lint".to_string(),
            status: "passed".to_string(),
            exit_code: 0,
            summary: "0 finding(s); passed".to_string(),
            findings: Vec::new(),
            artifacts: Vec::new(),
        };
        let skipped = ReviewArtifactCommand {
            name: "test".to_string(),
            status: "skipped".to_string(),
            exit_code: 0,
            summary: "no files changed".to_string(),
            findings: Vec::new(),
            artifacts: Vec::new(),
        };
        let failed = ReviewArtifactCommand {
            name: "audit".to_string(),
            status: "failed".to_string(),
            exit_code: 1,
            summary: "1 finding(s); failed".to_string(),
            findings: Vec::new(),
            artifacts: Vec::new(),
        };

        assert_eq!(artifact_status(std::slice::from_ref(&skipped)), "skipped");
        assert_eq!(artifact_status(std::slice::from_ref(&passed)), "passed");
        assert_eq!(artifact_status(&[passed.clone(), skipped]), "partial");
        assert_eq!(artifact_status(&[passed, failed]), "failed");
    }

    #[test]
    fn execute_review_plan_steps_preserves_quality_order() {
        let steps = vec![
            PlanStep::ready("review.audit", "review.audit").build(),
            PlanStep::ready("review.lint", "review.lint").build(),
            PlanStep::ready("review.test", "review.test").build(),
        ];
        let mut observed = Vec::new();

        let run = execute_review_plan_steps(&steps, |step| {
            observed.push(step.id.clone());
            Ok(Some(step.id.clone()))
        })
        .expect("review execution should run ready steps");

        assert_eq!(observed, vec!["review.audit", "review.lint", "review.test"]);
        assert_eq!(run.results, observed);
        assert!(!run.stopped);
    }

    #[test]
    fn execute_review_plan_steps_skips_disabled_and_skipped_steps() {
        let steps = vec![
            PlanStep::ready("review.audit", "review.audit").build(),
            PlanStep::disabled_with_reason("review.lint", "review.lint", "disabled").build(),
            PlanStep::builder(
                "review.test",
                "review.test",
                crate::core::plan::PlanStepStatus::Skipped,
            )
            .build(),
        ];

        let run = execute_review_plan_steps(&steps, |step| Ok(Some(step.id.clone())))
            .expect("review execution should ignore non-executable steps");

        assert_eq!(run.results, vec!["review.audit"]);
        assert!(!run.stopped);
    }

    #[test]
    fn execute_review_plan_steps_does_not_treat_failures_as_show_stoppers() {
        let steps = vec![
            PlanStep::ready("review.audit", "review.audit").build(),
            PlanStep::ready("review.lint", "review.lint").build(),
            PlanStep::ready("review.test", "review.test").build(),
        ];

        let run = execute_review_plan_steps(&steps, |step| {
            let exit_code = if step.id == "review.audit" { 2 } else { 0 };
            Ok(Some((step.id.clone(), exit_code)))
        })
        .expect("review execution should continue after stage failures");

        assert_eq!(
            run.results,
            vec![
                ("review.audit".to_string(), 2),
                ("review.lint".to_string(), 0),
                ("review.test".to_string(), 0),
            ]
        );
        assert!(!run.stopped);
    }

    #[test]
    fn build_artifact_uses_review_schema_and_refs() {
        let command = ReviewArtifactCommand {
            name: "lint".to_string(),
            status: "passed".to_string(),
            exit_code: 0,
            summary: "0 finding(s); passed".to_string(),
            findings: Vec::new(),
            artifacts: Vec::new(),
        };

        let artifact = build_artifact("homeboy", "origin/main", "abc123", vec![command]);

        assert_eq!(artifact.schema, "homeboy/review/v1");
        assert_eq!(artifact.component, "homeboy");
        assert_eq!(artifact.status, "passed");
        assert_eq!(artifact.base_ref, "origin/main");
        assert_eq!(artifact.head_ref, "abc123");
        assert_eq!(artifact.commands.len(), 1);
        assert!(artifact.generated_at.contains('T'));
    }
}
