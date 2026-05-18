use std::path::PathBuf;

use homeboy::core::observation::ArtifactRecord;
use homeboy::core::runner;

use super::{CmdResult, RunsArtifactGetOutput, RunsOutput};

pub fn is_remote_artifact(artifact: &ArtifactRecord) -> bool {
    artifact.artifact_type == "remote_file"
        || runner::is_remote_runner_artifact_path(&artifact.path)
}

pub fn get(artifact: ArtifactRecord, output: Option<PathBuf>) -> CmdResult<RunsOutput> {
    let download = runner::download_remote_artifact(&artifact.path, output)?;
    Ok((
        RunsOutput::ArtifactGet(RunsArtifactGetOutput {
            command: "runs.artifact.get",
            run_id: artifact.run_id,
            artifact_id: artifact.id,
            output_path: download.output_path.display().to_string(),
            content_type: download.content_type,
            size_bytes: download.size_bytes,
            sha256: download.sha256,
        }),
        0,
    ))
}
