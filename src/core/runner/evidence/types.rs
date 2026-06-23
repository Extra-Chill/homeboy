use std::path::PathBuf;

use serde_json::Value;

use crate::core::api_jobs::{Job, JobEvent};
use crate::core::observation::RunRecord;

use super::super::RunnerArtifactRef;

#[derive(Debug)]
pub struct RemoteArtifactDownload {
    pub output_path: PathBuf,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub artifact_ref: RunnerArtifactRef,
}

#[derive(Debug)]
pub struct MirroredDaemonEvidence {
    pub run: RunRecord,
    pub patch: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct RunnerJobLogSnapshot {
    pub job: Job,
    pub events: Vec<JobEvent>,
}
