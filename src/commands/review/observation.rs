use std::path::Path;

use homeboy::core::git::short_head_revision_at;
use homeboy::core::observation::{merge_metadata, ActiveObservation, NewRunRecord, RunStatus};
use homeboy::core::ObservationOutputMetadata;

use super::{
    artifact_command, ReviewArgs, ReviewArtifactFindings, ReviewCommandOutput, ReviewStage,
};

pub(super) struct ReviewObservation(ActiveObservation);

impl ReviewObservation {
    pub(super) fn output_metadata(&self) -> ObservationOutputMetadata {
        ObservationOutputMetadata::for_run(&self.0.run().kind, self.0.run_id())
    }
}

pub(super) struct ReviewObservationStart<'a> {
    pub component_id: &'a str,
    pub component_label: &'a str,
    pub source_path: &'a Path,
    pub args: &'a ReviewArgs,
    pub scope: &'a str,
    pub changed_file_count: Option<usize>,
}

pub(super) fn start(start: ReviewObservationStart<'_>) -> Option<ReviewObservation> {
    let metadata = review_observation_initial_metadata(
        start.component_label,
        start.args,
        start.scope,
        start.changed_file_count,
    );
    ActiveObservation::start_best_effort(
        NewRunRecord::builder("review")
            .component_id(start.component_id)
            .command(review_observation_command(start.component_id, start.args))
            .cwd_path(start.source_path)
            .current_homeboy_version()
            .git_sha(short_head_revision_at(start.source_path))
            .metadata(metadata.clone())
            .build(),
    )
    .map(ReviewObservation)
}

pub(super) fn finish_success(
    observation: Option<ReviewObservation>,
    output: &ReviewCommandOutput,
    exit_code: i32,
) {
    let Some(observation) = observation else {
        return;
    };

    let status = if !output.audit.ran && !output.lint.ran && !output.test.ran {
        RunStatus::Skipped
    } else if output.summary.passed {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    };
    let metadata = review_observation_finish_metadata(
        observation.0.initial_metadata().clone(),
        output,
        exit_code,
        None,
    );
    observation.0.finish(status, Some(metadata));
}

pub(super) fn finish_error(observation: Option<ReviewObservation>, error: &homeboy::core::Error) {
    let Some(observation) = observation else {
        return;
    };

    let metadata = merge_metadata(
        observation.0.initial_metadata().clone(),
        serde_json::json!({
            "observation_status": "error",
            "error": error.to_string(),
        }),
    );
    observation.0.finish_error(Some(metadata));
}

fn review_observation_command(component_id: &str, args: &ReviewArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "review".to_string(),
        component_id.to_string(),
    ];
    if let Some(changed_since) = args.changed_since.as_ref() {
        parts.push(format!("--changed-since={changed_since}"));
    }
    if args.changed_only {
        parts.push("--changed-only".to_string());
    }
    if args.summary {
        parts.push("--summary".to_string());
    }
    if let Some(report) = args.report.as_ref() {
        parts.push(format!("--report={report}"));
    }
    parts.join(" ")
}

pub(super) fn review_observation_initial_metadata(
    component_label: &str,
    args: &ReviewArgs,
    scope: &str,
    changed_file_count: Option<usize>,
) -> serde_json::Value {
    serde_json::json!({
        "schema": "homeboy/review-observation/v1",
        "component_label": component_label,
        "scope": scope,
        "changed_since": args.changed_since,
        "changed_only": args.changed_only,
        "summary": args.summary,
        "ci_profile": args.ci_profile,
        "report": args.report,
        "changed_file_count": changed_file_count,
        "observation_status": "running",
    })
}

pub(super) fn review_observation_finish_metadata(
    initial_metadata: serde_json::Value,
    output: &ReviewCommandOutput,
    exit_code: i32,
    error: Option<&str>,
) -> serde_json::Value {
    let mut stages = vec![
        stage_observation(&output.audit),
        stage_observation(&output.lint),
        stage_observation(&output.test),
    ];
    if let Some(ref stage) = output.ci_profile {
        stages.push(stage_observation(stage));
    }

    merge_metadata(
        initial_metadata,
        serde_json::json!({
            "observation_status": output.artifact.status,
            "exit_code": exit_code,
            "passed": output.summary.passed,
            "status": output.summary.status,
            "total_findings": output.summary.total_findings,
            "changed_file_count": output.summary.changed_file_count,
            "hints": output.summary.hints,
            "artifact": output.artifact,
            "stages": stages,
            "error": error,
        }),
    )
}

fn stage_observation<T: serde::Serialize + ReviewArtifactFindings>(
    stage: &ReviewStage<T>,
) -> serde_json::Value {
    let command = artifact_command(stage);
    serde_json::json!({
        "name": command.name,
        "status": command.status,
        "ran": stage.ran,
        "passed": stage.passed,
        "exit_code": command.exit_code,
        "finding_count": stage.finding_count,
        "summary": command.summary,
        "hint": stage.hint,
        "skipped_reason": stage.skipped_reason,
        "run_id": null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::utils::args::{
        BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs,
    };

    fn review_args() -> ReviewArgs {
        ReviewArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            changed_since: Some("origin/main".to_string()),
            changed_only: false,
            summary: true,
            ci_profile: None,
            report: Some("pr-comment".to_string()),
            banner: Vec::new(),
            baseline_args: BaselineArgs::default(),
        }
    }

    #[test]
    fn initial_metadata_captures_review_scope() {
        let args = review_args();
        let metadata =
            review_observation_initial_metadata("homeboy", &args, "changed-since", Some(3));

        assert_eq!(metadata["schema"], "homeboy/review-observation/v1");
        assert_eq!(metadata["component_label"], "homeboy");
        assert_eq!(metadata["scope"], "changed-since");
        assert_eq!(metadata["changed_since"], "origin/main");
        assert_eq!(metadata["changed_file_count"], 3);
        assert_eq!(metadata["observation_status"], "running");
    }

    #[test]
    fn finish_metadata_captures_aggregate_and_linkable_stages() {
        use homeboy::core::code_audit::AuditCommandOutput;
        use homeboy::core::extension::lint::LintCommandOutput;
        use homeboy::core::extension::test::TestCommandOutput;

        let initial = serde_json::json!({
            "schema": "homeboy/review-observation/v1",
            "component_label": "homeboy",
            "scope": "changed-since",
            "observation_status": "running",
        });
        let audit = ReviewStage {
            stage: "audit".to_string(),
            ran: true,
            passed: true,
            exit_code: 0,
            finding_count: 0,
            hint: "Deep dive: homeboy audit homeboy --changed-since=origin/main".to_string(),
            skipped_reason: None,
            output: None::<AuditCommandOutput>,
        };
        let lint = ReviewStage {
            stage: "lint".to_string(),
            ran: true,
            passed: false,
            exit_code: 1,
            finding_count: 2,
            hint: "Deep dive: homeboy lint homeboy --changed-since=origin/main".to_string(),
            skipped_reason: None,
            output: None::<LintCommandOutput>,
        };
        let test = ReviewStage {
            stage: "test".to_string(),
            ran: false,
            passed: true,
            exit_code: 0,
            finding_count: 0,
            hint: "Run individually: homeboy test".to_string(),
            skipped_reason: Some("no tests".to_string()),
            output: None::<TestCommandOutput>,
        };
        let artifact = super::super::build_artifact(
            "homeboy",
            "origin/main",
            "abc123",
            vec![
                super::super::artifact_command(&audit),
                super::super::artifact_command(&lint),
                super::super::artifact_command(&test),
            ],
        );
        let output = ReviewCommandOutput {
            command: "review".to_string(),
            plan: homeboy::core::quality::build_quality_plan(
                homeboy::core::quality::QualityPlanOptions::review("homeboy"),
            ),
            observation: None,
            artifact,
            summary: super::super::ReviewSummary {
                passed: false,
                status: "failed".to_string(),
                component: "homeboy".to_string(),
                scope: "changed-since".to_string(),
                changed_since: Some("origin/main".to_string()),
                total_findings: 2,
                changed_file_count: Some(3),
                hints: vec!["hint".to_string()],
            },
            audit,
            lint,
            test,
            ci_profile: None,
        };

        let metadata = review_observation_finish_metadata(initial, &output, 1, None);

        assert_eq!(metadata["observation_status"], "failed");
        assert_eq!(metadata["exit_code"], 1);
        assert_eq!(metadata["total_findings"], 2);
        assert_eq!(metadata["artifact"]["schema"], "homeboy/review/v1");
        assert_eq!(metadata["stages"].as_array().expect("stages").len(), 3);
        assert_eq!(metadata["stages"][1]["name"], "lint");
        assert_eq!(metadata["stages"][1]["status"], "failed");
        assert_eq!(metadata["stages"][1]["finding_count"], 2);
        assert!(metadata["stages"][1].get("run_id").is_some());
    }
}
