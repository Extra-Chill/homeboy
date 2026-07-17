use base64::Engine;

use homeboy_core::error::{Error, Result};
use homeboy_core::execution_contract::{decode_uri_component, EXECUTION_CONTRACT};

// Moved to core's execution_contract module (it's a contract concern, not
// runner behavior) so core code can classify artifact paths without a
// core -> runner edge. Re-exported so runner-internal call sites resolve.
pub use homeboy_core::execution_contract::is_remote_runner_artifact_path;

// Contract-driven token construction moved to core's execution_contract (not
// runner behavior) so core code can build/parse them without a core -> runner
// edge. Re-exported so runner-internal call sites resolve unchanged.
pub use homeboy_core::execution_contract::{
    artifact_store_locator_from_runner_artifact_id, runner_artifact_store_token,
};

pub fn is_retrievable_runner_artifact(path: &str) -> bool {
    RemoteArtifactToken::parse(path).is_ok()
}

// Moved to core's execution_contract module (contract-driven classification,
// not runner behavior) so core code can use them without a core -> runner edge.
// Re-exported so runner-internal call sites resolve unchanged.
pub use homeboy_core::execution_contract::{
    is_reportable_artifact_evidence_path, reportable_artifact_evidence_path,
};

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
