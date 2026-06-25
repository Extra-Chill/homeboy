use clap::Args;
use serde::Serialize;

use homeboy::core::artifacts::{
    render_matrix_artifact_summary_markdown, summarize_matrix_artifacts, MatrixArtifactSummary,
};
use homeboy::core::observation::{runs_service, FindingListFilter, ObservationStore};
use homeboy::core::Error;

#[derive(Args, Debug, Clone)]
pub struct MatrixArtifactsArgs {
    /// Observation run ID to summarize.
    pub run_id: String,
    /// Output format: json or markdown.
    #[arg(long, default_value = "markdown")]
    pub format: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatrixArtifactsReport {
    pub summary: MatrixArtifactSummary,
    pub markdown: String,
}

pub fn matrix_artifacts_from_args(
    args: &MatrixArtifactsArgs,
) -> homeboy::core::Result<MatrixArtifactsReport> {
    let store = ObservationStore::open_initialized()?;
    runs_service::require_run(&store, &args.run_id)?;
    let artifacts = runs_service::list_artifacts_for_run(&store, &args.run_id)?;
    let findings = store.list_findings(FindingListFilter {
        run_id: Some(args.run_id.clone()),
        tool: None,
        file: None,
        fingerprint: None,
        limit: Some(10_000),
    })?;
    let summary = summarize_matrix_artifacts(&args.run_id, &artifacts, &findings).ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!(
                "run {} does not expose matrix-style artifact metadata or JSON packets",
                args.run_id
            ),
            Some(args.run_id.clone()),
            Some(vec![
                "Run `homeboy runs artifacts <run-id>` to inspect the raw artifact list.".to_string(),
                "Matrix summaries are derived from artifact roles, filenames, schemas, fixtures, findings, and group counts.".to_string(),
            ]),
        )
    })?;
    let markdown = render_matrix_artifact_summary_markdown(&summary);
    Ok(MatrixArtifactsReport { summary, markdown })
}

pub fn render_matrix_artifacts_from_args(
    args: &MatrixArtifactsArgs,
) -> homeboy::core::Result<String> {
    Ok(matrix_artifacts_from_args(args)?.markdown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use homeboy::core::observation::{NewRunRecord, RunStatus};

    #[test]
    fn report_matrix_artifacts_renders_compact_markdown() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("fixture.matrix")
                        .component_id("demo")
                        .metadata(serde_json::json!({ "schema": "example/matrix-run/v1" }))
                        .build(),
                )
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Fail, None)
                .expect("finish run");
            let summary_path = home.path().join("summary.json");
            std::fs::write(
                &summary_path,
                serde_json::to_vec(&serde_json::json!({
                    "schema": "example/matrix-summary/v1",
                    "fixtures": [{ "fixture": "home" }, { "fixture": "about" }],
                    "findings": [
                        { "kind": "missing_title", "fixture": "home", "group": "seo" },
                        { "kind": "missing_title", "fixture": "about", "group": "seo" },
                        { "kind": "broken_link", "fixture": "home", "group": "links" }
                    ]
                }))
                .expect("summary json"),
            )
            .expect("write summary");
            store
                .record_artifact(&run.id, "matrix_summary", &summary_path)
                .expect("record artifact");

            let report = matrix_artifacts_from_args(&MatrixArtifactsArgs {
                run_id: run.id.clone(),
                format: "markdown".to_string(),
            })
            .expect("report");

            assert_eq!(report.summary.fixture_count, 2);
            assert_eq!(report.summary.finding_count, 3);
            assert!(report.markdown.contains("Fixtures: 2"));
            assert!(report.markdown.contains("Findings: 3"));
            assert!(report.markdown.contains("- missing_title: 2"));
            assert!(report.markdown.contains("home"));
            assert!(report.markdown.contains("matrix_summary"));
        });
    }
}
