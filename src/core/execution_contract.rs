use crate::core::artifact_ref::{
    ArtifactReference, METADATA_ONLY_REF_SCHEME, RUNNER_ARTIFACT_REF_SCHEME,
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

pub fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

pub fn decode_uri_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    decoded.push(byte);
                    index += 3;
                    continue;
                }
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).to_string()
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
