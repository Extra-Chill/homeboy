use homeboy::core::execution_contract::{encode_uri_component, EXECUTION_CONTRACT};
use homeboy::core::observation::ArtifactRecord;
use homeboy::core::runners as runner;
use homeboy::core::Error;

use super::types::{RunsArtifactGetArgs, RunsArtifactPathGuide, RunsArtifactsOutput};
use super::{remote_artifact, CmdResult, RunsListArgs, RunsListOutput, RunsOutput};

pub fn list_runner_runs(
    runner_id: &str,
    args: RunsListArgs,
    command: &'static str,
) -> CmdResult<RunsOutput> {
    let mut query = Vec::new();
    if let Some(kind) = args.kind {
        query.push(("kind", kind));
    }
    if let Some(component_id) = args.component_id {
        query.push(("component", component_id));
    }
    if let Some(status) = args.status {
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
    let runs = serde_json::from_value(data["body"]["runs"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse runner daemon runs list".to_string()),
        )
    })?;

    Ok((RunsOutput::List(RunsListOutput { command, runs }), 0))
}

pub fn runner_artifacts(runner_id: &str, run_id: &str) -> CmdResult<RunsOutput> {
    let data = runner::daemon_api_get(
        runner_id,
        &format!("/runs/{}/artifacts", encode_uri_component(run_id)),
    )?;
    let artifacts = parse_runner_artifacts(&data)?;

    Ok((
        RunsOutput::Artifacts(RunsArtifactsOutput {
            command: "runs.artifacts",
            run_id: run_id.to_string(),
            runner_id: Some(runner_id.to_string()),
            path_guide: RunsArtifactPathGuide::for_listing(run_id, Some(runner_id)),
            artifacts,
            preview_entrypoints: Vec::new(),
            matrix_summary: None,
            fuzz_result_envelopes: Vec::new(),
        }),
        0,
    ))
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
}
