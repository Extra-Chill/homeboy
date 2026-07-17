use homeboy::core::api_jobs;
use homeboy::core::artifact_address::ArtifactAddress;
use homeboy::core::execution_contract::{encode_uri_component, EXECUTION_CONTRACT};
use homeboy::core::observation::evidence_report::directory_publication_guidance;
use homeboy::core::observation::ArtifactRecord;
use homeboy::core::resource_lifecycle_index::resource_lifecycle_index_from_artifacts;
use homeboy::core::Error;
use homeboy::runner::runners as runner;

use super::types::{
    actionable_for_run_list, RunsArtifactGetArgs, RunsArtifactPathGuide, RunsArtifactsOutput,
    RunsDirectoryArtifactPublicationGuidance,
};
use super::{remote_artifact, CmdResult, RunSummary, RunsListArgs, RunsListOutput, RunsOutput};

pub fn list_runner_runs(
    runner_id: &str,
    args: RunsListArgs,
    command: &'static str,
) -> CmdResult<RunsOutput> {
    let mut query = Vec::new();
    let kind_filter = args.kind.clone();
    if let Some(kind) = args.kind {
        query.push(("kind", kind));
    }
    if let Some(component_id) = args.component_id {
        query.push(("component", component_id));
    }
    let status = if args.running {
        Some("running".to_string())
    } else {
        args.status
    };
    let status_filter = status.clone();
    if let Some(status) = status {
        query.push(("status", status));
    }
    if let Some(rig) = args.rig {
        query.push(("rig", rig));
    }
    query.push(("limit", args.limit.to_string()));
    let query = query
        .into_iter()
        .map(|(key, value)| format!("{}={}", key, url_encode_component(&value)))
        .collect::<Vec<_>>()
        .join("&");
    let data = runner::daemon_api_get(runner_id, &format!("/runs?{query}"))?;
    let mut runs: Vec<RunSummary> =
        serde_json::from_value(data["body"]["runs"].clone()).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse runner daemon runs list".to_string()),
            )
        })?;
    merge_active_runner_jobs(
        &mut runs,
        runner_id,
        kind_filter.as_deref(),
        status_filter.as_deref(),
        args.limit as usize,
    );

    let actionable = actionable_for_run_list(&runs);
    Ok((
        RunsOutput::List(RunsListOutput {
            command,
            runs,
            actionable,
        }),
        0,
    ))
}

fn merge_active_runner_jobs(
    runs: &mut Vec<RunSummary>,
    runner_id: &str,
    kind: Option<&str>,
    status: Option<&str>,
    limit: usize,
) {
    if runs.len() >= limit {
        return;
    }
    let Ok(report) = runner::status(runner_id) else {
        return;
    };
    if !report.connected {
        return;
    }
    let jobs = report
        .active_jobs
        .into_iter()
        .filter(|job| job.runner_id == runner_id)
        .filter(|job| match kind {
            Some(kind) => kind == job.kind,
            None => true,
        })
        .filter(|job| match status {
            Some(status) => status == job.status.run_status_label(),
            None => true,
        })
        .filter_map(active_runner_job_run_summary_if_durable)
        .collect::<Vec<_>>();
    append_missing_run_summaries(runs, jobs, limit);
}

fn append_missing_run_summaries(runs: &mut Vec<RunSummary>, jobs: Vec<RunSummary>, limit: usize) {
    for job in jobs {
        if runs.len() >= limit {
            break;
        }
        if !runs.iter().any(|run| run.id == job.id) {
            runs.push(job);
        }
    }
}

fn active_runner_job_run_summary_if_durable(
    job: api_jobs::ActiveRunnerJobSummary,
) -> Option<RunSummary> {
    let summary = api_jobs::active_runner_job_run_summary_if_durable(job)?;
    Some(RunSummary {
        id: summary.id,
        kind: summary.kind,
        status: summary.status,
        started_at: summary.started_at,
        finished_at: None,
        component_id: None,
        rig_id: None,
        git_sha: None,
        command: Some(summary.command),
        cwd: summary.cwd,
        status_note: Some(summary.status_note),
        artifact_index: None,
    })
}

pub fn runner_artifacts(runner_id: &str, run_id: &str) -> CmdResult<RunsOutput> {
    let data = runner::daemon_api_get(
        runner_id,
        &format!("/runs/{}/artifacts", encode_uri_component(run_id)),
    )?;
    let artifacts = parse_runner_artifacts(&data)?;
    let directory_publication = directory_publication_guidance_for_artifacts(&artifacts);
    let resource_lifecycle_index = resource_lifecycle_index_from_artifacts(&artifacts)?;

    Ok((
        RunsOutput::Artifacts(RunsArtifactsOutput {
            command: "runs.artifacts",
            run_id: run_id.to_string(),
            runner_id: Some(runner_id.to_string()),
            path_guide: RunsArtifactPathGuide::for_listing(run_id, Some(runner_id)),
            artifacts,
            next_commands: Vec::new(),
            resource_lifecycle_index,
            directory_publication,
            preview_entrypoints: Vec::new(),
            matrix_summary: None,
            fuzz_result_envelopes: Vec::new(),
            pull: None,
        }),
        0,
    ))
}

fn directory_publication_guidance_for_artifacts(
    artifacts: &[ArtifactRecord],
) -> Vec<RunsDirectoryArtifactPublicationGuidance> {
    artifacts
        .iter()
        .filter_map(|artifact| {
            let address = ArtifactAddress::from_record(artifact);
            directory_publication_guidance(artifact, &address).map(|guidance| {
                RunsDirectoryArtifactPublicationGuidance {
                    artifact_id: artifact.id.clone(),
                    kind: artifact.kind.clone(),
                    guidance,
                }
            })
        })
        .collect()
}

pub fn runner_artifact_get(
    runner_id: &str,
    mut args: RunsArtifactGetArgs,
) -> CmdResult<RunsOutput> {
    let content_url = format!(
        "/runs/{}/artifacts/{}/content",
        encode_uri_component(&args.run_id),
        encode_uri_component(&args.artifact_id)
    );
    let artifact = ArtifactRecord {
        id: args.artifact_id.clone(),
        run_id: args.run_id.clone(),
        kind: "runner_artifact".to_string(),
        artifact_type: "remote_file".to_string(),
        path: EXECUTION_CONTRACT.artifacts.runner_artifact_ref(
            runner_id,
            &args.run_id,
            &args.artifact_id,
        ),
        url: None,
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: None,
        size_bytes: None,
        mime: None,
        metadata_json: serde_json::json!({
            "source": "connected_runner_daemon",
            "runner_id": runner_id,
            "content_url": content_url,
        }),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let output = args.output.take();
    let (output, exit_code) = remote_artifact::get(artifact, output)?;
    let RunsOutput::ArtifactGet(mut output) = output else {
        return Err(Error::internal_unexpected(
            "runner artifact get returned non-artifact output",
        ));
    };
    output.runner_id = Some(runner_id.to_string());
    output.source_content_url = Some(content_url);
    Ok((RunsOutput::ArtifactGet(output), exit_code))
}

fn parse_runner_artifacts(data: &serde_json::Value) -> homeboy::core::Result<Vec<ArtifactRecord>> {
    serde_json::from_value(data["body"]["artifacts"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse runner daemon artifacts list".to_string()),
        )
    })
}

fn url_encode_component(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::api_jobs::{ActiveRunnerJobSummary, JobClaimMetadata, JobStatus};
    use serde_json::json;

    #[test]
    fn parses_runner_artifacts_from_daemon_body() {
        let data = json!({
            "body": {
                "artifacts": [{
                    "id": "report-json",
                    "run_id": "run-123",
                    "kind": "report",
                    "artifact_type": "file",
                    "path": "/runner/artifacts/report.json",
                    "url": null,
                    "public_url": null,
                    "viewer_url": null,
                    "viewer_links": [],
                    "sha256": "abc123",
                    "size_bytes": 42,
                    "mime": "application/json",
                    "metadata_json": {"source": "runner"},
                    "created_at": "2026-06-26T00:00:00Z"
                }]
            }
        });

        let artifacts = parse_runner_artifacts(&data).expect("artifacts");

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].id, "report-json");
        assert_eq!(artifacts[0].run_id, "run-123");
        assert_eq!(artifacts[0].mime.as_deref(), Some("application/json"));
    }

    #[test]
    fn connected_runner_path_guide_labels_runner_resident_refs() {
        let guide = RunsArtifactPathGuide::for_listing("run-a", Some("runner-a"));

        assert_eq!(guide.listing_source, "connected_runner:runner-a");
        assert!(guide
            .runner_path_fields
            .iter()
            .any(|field| field.contains("runner-artifact://")));
        assert!(guide
            .runner_path_scope
            .contains("not operator-local filesystem paths"));
        assert!(guide.fetch_hint.contains("--runner <runner-id>"));
    }

    #[test]
    fn append_missing_run_summaries_adds_active_runner_jobs_without_duplicates() {
        let mut runs = vec![RunSummary {
            id: "durable-run-1".to_string(),
            kind: "bench".to_string(),
            status: "running".to_string(),
            started_at: "2026-07-06T00:00:00Z".to_string(),
            finished_at: None,
            component_id: Some("homeboy".to_string()),
            rig_id: None,
            git_sha: None,
            command: Some("homeboy bench homeboy".to_string()),
            cwd: None,
            status_note: None,
            artifact_index: None,
        }];
        let jobs = vec![
            RunSummary {
                id: "durable-run-1".to_string(),
                kind: "runner.exec".to_string(),
                status: "running".to_string(),
                started_at: "2026-07-06T00:00:00Z".to_string(),
                finished_at: None,
                component_id: None,
                rig_id: None,
                git_sha: None,
                command: Some("duplicate active job".to_string()),
                cwd: None,
                status_note: Some("active runner job".to_string()),
                artifact_index: None,
            },
            RunSummary {
                id: "runner-job-job-2".to_string(),
                kind: "runner.exec".to_string(),
                status: "running".to_string(),
                started_at: "2026-07-06T00:00:01Z".to_string(),
                finished_at: None,
                component_id: None,
                rig_id: None,
                git_sha: None,
                command: Some("new active job".to_string()),
                cwd: Some("/srv/homeboy".to_string()),
                status_note: Some("active runner job".to_string()),
                artifact_index: None,
            },
        ];

        append_missing_run_summaries(&mut runs, jobs, 20);

        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, "durable-run-1");
        assert_eq!(runs[1].id, "runner-job-job-2");
        assert_eq!(runs[1].cwd.as_deref(), Some("/srv/homeboy"));
    }

    #[test]
    fn append_missing_run_summaries_honors_limit() {
        let mut runs = vec![RunSummary {
            id: "persisted-run".to_string(),
            kind: "bench".to_string(),
            status: "running".to_string(),
            started_at: "2026-07-06T00:00:00Z".to_string(),
            finished_at: None,
            component_id: Some("homeboy".to_string()),
            rig_id: None,
            git_sha: None,
            command: Some("homeboy bench homeboy".to_string()),
            cwd: None,
            status_note: None,
            artifact_index: None,
        }];
        let jobs = vec![RunSummary {
            id: "runner-job-job-2".to_string(),
            kind: "bench".to_string(),
            status: "queued".to_string(),
            started_at: "2026-07-06T00:00:01Z".to_string(),
            finished_at: None,
            component_id: None,
            rig_id: None,
            git_sha: None,
            command: Some("queued remote bench job".to_string()),
            cwd: Some("/srv/homeboy".to_string()),
            status_note: Some("active runner job".to_string()),
            artifact_index: None,
        }];

        append_missing_run_summaries(&mut runs, jobs, 1);

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, "persisted-run");
    }

    #[test]
    fn active_runner_job_summary_uses_durable_run_id_when_available() {
        let job = ActiveRunnerJobSummary {
            runner_id: "homeboy-lab".to_string(),
            job_id: "job-123".to_string(),
            operation: "runner.exec".to_string(),
            source: "runner-daemon".to_string(),
            kind: "runner.exec".to_string(),
            status: JobStatus::Running,
            command: "homeboy bench homeboy".to_string(),
            cwd: Some("/srv/homeboy".to_string()),
            started_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_001_000,
            elapsed_ms: 1_000,
            heartbeat_age_ms: 0,
            claim: JobClaimMetadata::default(),
            claim_expires_in_ms: None,
            lifecycle: None,
            durable_run_id: Some("bench-run-123".to_string()),
            stale_reason: None,
            lifecycle_state: Some("active".to_string()),
            retryable: Some(true),
            active_child_count: None,
            active_cell_count: None,
        };

        let summary =
            active_runner_job_run_summary_if_durable(job).expect("durable runner job summary");

        assert_eq!(summary.id, "bench-run-123");
        assert_eq!(summary.status, "running");
        assert!(summary
            .status_note
            .as_deref()
            .unwrap()
            .contains("job=job-123"));
        assert!(summary
            .command
            .as_deref()
            .unwrap()
            .contains("durable_run=bench-run-123"));
    }
}
