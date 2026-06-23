use std::fs;

use base64::Engine;

use crate::core::error::{Error, Result};
use crate::core::execution_contract::{decode_uri_component, EXECUTION_CONTRACT};

pub fn is_remote_runner_artifact_path(path: &str) -> bool {
    EXECUTION_CONTRACT.artifacts.is_runner_artifact_ref(path)
}

pub fn runner_artifact_store_token(runner_id: &str, run_id: &str, locator: &str) -> String {
    let encoded_locator = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(locator);
    runner_artifact_token(
        runner_id,
        run_id,
        &format!("artifact-store:{encoded_locator}"),
    )
}

pub(crate) fn artifact_store_locator_from_runner_artifact_id(artifact_id: &str) -> Option<String> {
    let encoded_locator = artifact_id.strip_prefix("artifact-store:")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded_locator)
        .ok()?;
    String::from_utf8(bytes).ok()
}

pub fn is_retrievable_runner_artifact(path: &str) -> bool {
    RemoteArtifactToken::parse(path).is_ok()
}

pub fn is_reportable_artifact_evidence_path(path: &str) -> bool {
    is_retrievable_runner_artifact(path)
        || EXECUTION_CONTRACT.artifacts.is_metadata_only_ref(path)
        || !std::path::Path::new(path).is_absolute()
        || fs::metadata(path)
            .map(|metadata| metadata.is_file() || metadata.is_dir())
            .unwrap_or(false)
}

pub fn reportable_artifact_evidence_path(path: Option<&String>) -> Option<String> {
    path.filter(|path| is_reportable_artifact_evidence_path(path))
        .cloned()
}

pub(crate) fn runner_artifact_token(runner_id: &str, run_id: &str, artifact_id: &str) -> String {
    EXECUTION_CONTRACT
        .artifacts
        .runner_artifact_ref(runner_id, run_id, artifact_id)
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteArtifactToken {
    pub(crate) runner_id: String,
    pub(crate) run_id: String,
    pub(crate) artifact_id: String,
}

impl RemoteArtifactToken {
    pub(crate) fn parse(path: &str) -> Result<Self> {
        let token = EXECUTION_CONTRACT
            .artifacts
            .strip_runner_artifact_scheme(path)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "artifact_id",
                    "artifact is not a runner artifact token",
                    Some(path.to_string()),
                    None,
                )
            })?;
        let mut parts = token.split('/');
        let runner_id = parts.next().unwrap_or_default();
        let run_id = parts.next().unwrap_or_default();
        let artifact_id = parts.next().unwrap_or_default();
        if runner_id.is_empty()
            || run_id.is_empty()
            || artifact_id.is_empty()
            || parts.next().is_some()
        {
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                format!(
                    "runner artifact token must be {}<runner>/<run>/<artifact>",
                    EXECUTION_CONTRACT.artifacts.runner_artifact_scheme
                ),
                Some(path.to_string()),
                None,
            ));
        }
        Ok(Self {
            runner_id: decode_uri_component(runner_id),
            run_id: decode_uri_component(run_id),
            artifact_id: decode_uri_component(artifact_id),
        })
    }
}
