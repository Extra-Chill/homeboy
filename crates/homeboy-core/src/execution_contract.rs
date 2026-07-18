use crate::artifact_ref::{
    ArtifactReference, METADATA_ONLY_REF_SCHEME, RUNNER_ARTIFACT_REF_SCHEME,
};

// The URI codec primitives live in `homeboy-engine-primitives` (the slim shared
// base) alongside the artifact-ref schemes, so `core::artifact_ref` and this
// module can both use them without a mutual `execution_contract <-> artifact_ref`
// cycle. Re-exported here to preserve existing
// `execution_contract::{encode_uri_component, decode_uri_component, ...}` call
// sites, including cross-crate consumers.
pub use homeboy_engine_primitives::artifact_ref_scheme::{
    decode_uri_component, decode_uri_component_strict, encode_uri_component,
};

/// Typed runtime-facing execution surface shared by runner, Lab, daemon, and
/// extension code paths.
///
/// `HomeboyPlan` describes workflow steps. `ExecutionContract` describes the
/// concrete runtime values those steps exchange across process boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionContract {
    pub artifacts: ArtifactUriContract,
    pub lab_offload: LabOffloadExecutionContract,
    pub apply: ApplyChangeContract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactUriContract {
    pub runner_artifact_scheme: &'static str,
    pub metadata_only_scheme: &'static str,
}

impl ArtifactUriContract {
    pub fn is_runner_artifact_ref(self, path: &str) -> bool {
        path.starts_with(self.runner_artifact_scheme)
    }

    pub fn is_metadata_only_ref(self, path: &str) -> bool {
        path.starts_with(self.metadata_only_scheme)
    }

    pub fn strip_runner_artifact_scheme(self, path: &str) -> Option<&str> {
        path.strip_prefix(self.runner_artifact_scheme)
    }

    pub fn runner_artifact_ref(self, runner_id: &str, run_id: &str, artifact_id: &str) -> String {
        ArtifactReference::parse(format!(
            "{}{}/{}/{}",
            self.runner_artifact_scheme,
            encode_uri_component(runner_id),
            encode_uri_component(run_id),
            encode_uri_component(artifact_id)
        ))
        .to_string()
    }

    pub fn metadata_only_ref(self, label: &str) -> String {
        ArtifactReference::parse(format!("{}{label}", self.metadata_only_scheme)).to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabOffloadExecutionContract {
    pub metadata_schema: &'static str,
}

/// Canonical apply/change wire contract shared by Lab, runners, and adapters.
///
/// `core::change_artifact` owns this schema's serializable payload structs.
/// `core::execution` owns higher-level lifecycle envelopes that may reference
/// these artifacts when describing execute/artifact/approve/apply/publish flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyChangeContract {
    pub change_artifact_schema: &'static str,
    pub change_apply_result_schema: &'static str,
    pub runner_workspace_apply_adapter: &'static str,
    pub unified_diff_patch_format: &'static str,
    pub digest_algorithm_sha256: &'static str,
}

pub const EXECUTION_CONTRACT: ExecutionContract = ExecutionContract {
    artifacts: ArtifactUriContract {
        runner_artifact_scheme: RUNNER_ARTIFACT_REF_SCHEME,
        metadata_only_scheme: METADATA_ONLY_REF_SCHEME,
    },
    lab_offload: LabOffloadExecutionContract {
        metadata_schema: "homeboy/lab-offload/v1",
    },
    apply: ApplyChangeContract {
        change_artifact_schema: "homeboy/change-artifact/v1",
        change_apply_result_schema: "homeboy/change-apply-result/v1",
        runner_workspace_apply_adapter: "homeboy/runner-workspace-apply/v1",
        unified_diff_patch_format: "unified_diff",
        digest_algorithm_sha256: "sha256",
    },
};

/// Whether an artifact path is a remote-runner artifact reference, per the
/// execution contract's artifact rules. Lives here (not in the runner module)
/// so core code can classify artifact paths without a core -> runner edge — the
/// classification is a contract concern, not runner behavior.
pub fn is_remote_runner_artifact_path(path: &str) -> bool {
    EXECUTION_CONTRACT.artifacts.is_runner_artifact_ref(path)
}

/// Whether an artifact path is worth reporting as retrievable evidence: a
/// retrievable runner-artifact token, a metadata-only ref, a relative path, or
/// a real local file/dir. Contract-driven classification (not runner behavior),
/// so it lives in core.
pub fn is_reportable_artifact_evidence_path(path: &str) -> bool {
    EXECUTION_CONTRACT
        .artifacts
        .strip_runner_artifact_scheme(path)
        .is_some()
        || EXECUTION_CONTRACT.artifacts.is_metadata_only_ref(path)
        || !std::path::Path::new(path).is_absolute()
        || std::fs::metadata(path)
            .map(|metadata| metadata.is_file() || metadata.is_dir())
            .unwrap_or(false)
}

/// The path itself when it is reportable artifact evidence, else `None`.
pub fn reportable_artifact_evidence_path(path: Option<&String>) -> Option<String> {
    path.filter(|path| is_reportable_artifact_evidence_path(path))
        .cloned()
}

/// A runner-artifact token addressing an artifact-store locator. Contract-driven
/// token construction (base64 URL-safe locator), not runner behavior, so it
/// lives in core alongside the artifact URI contract.
pub fn runner_artifact_store_token(runner_id: &str, run_id: &str, locator: &str) -> String {
    use base64::Engine;
    let encoded_locator = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(locator);
    EXECUTION_CONTRACT.artifacts.runner_artifact_ref(
        runner_id,
        run_id,
        &format!("artifact-store:{encoded_locator}"),
    )
}

/// The artifact-store locator encoded in a runner artifact id, if present.
pub fn artifact_store_locator_from_runner_artifact_id(artifact_id: &str) -> Option<String> {
    use base64::Engine;
    let encoded_locator = artifact_id.strip_prefix("artifact-store:")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded_locator)
        .ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_contract_builds_and_classifies_runtime_refs() {
        let artifacts = EXECUTION_CONTRACT.artifacts;
        let token = artifacts.runner_artifact_ref("runner/a", "run b", "artifact:c");
        assert_eq!(token, "runner-artifact://runner%2Fa/run%20b/artifact%3Ac");
        assert!(artifacts.is_runner_artifact_ref(&token));
        assert!(artifacts.is_metadata_only_ref("metadata-only:trace.zip"));
        assert_eq!(
            artifacts.metadata_only_ref("trace.zip"),
            "metadata-only:trace.zip"
        );
        assert_eq!(decode_uri_component("runner%2Fa"), "runner/a");
    }

    #[test]
    fn apply_contract_names_canonical_wire_schemas() {
        let apply = EXECUTION_CONTRACT.apply;

        assert_eq!(apply.change_artifact_schema, "homeboy/change-artifact/v1");
        assert_eq!(
            apply.change_apply_result_schema,
            "homeboy/change-apply-result/v1"
        );
        assert_eq!(
            apply.runner_workspace_apply_adapter,
            "homeboy/runner-workspace-apply/v1"
        );
        assert_eq!(apply.unified_diff_patch_format, "unified_diff");
        assert_eq!(apply.digest_algorithm_sha256, "sha256");
    }
}
