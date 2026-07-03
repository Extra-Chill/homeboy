use serde::Serialize;

use crate::command_contract::{
    registered_contract, RUNNER_ARTIFACT_MANIFEST_FILE, RUNNER_ARTIFACT_MANIFEST_REF_NAME,
    RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA, RUNNER_ARTIFACT_MANIFEST_SCHEMA,
    RUNNER_ARTIFACT_ROOT_DIR_SUFFIX,
};
use crate::core::agent_task::AGENT_TASK_ARTIFACT_SCHEMA;
use crate::core::agent_task_batch::AGENT_TASK_BATCH_ARTIFACTS_SCHEMA;
use crate::core::agent_task_contract::AGENT_TASK_LOOP_ACTION_SCHEMA;
use crate::core::agent_task_executor_evidence::{
    EXECUTOR_INPUT_EVIDENCE_KIND, EXECUTOR_INPUT_FILE, EXECUTOR_RESULT_EVIDENCE_KIND,
    EXECUTOR_RESULT_FILE,
};
use crate::core::agent_task_loop_controller::{
    AGENT_TASK_LOOP_CONTROLLER_SCHEMA, AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA,
};
use crate::core::agent_task_loop_definition::AGENT_TASK_LOOP_DEFINITION_SCHEMA;
use crate::core::artifact_address::ARTIFACT_ADDRESS_SCHEMA;
use crate::core::artifact_contract::{ARTIFACT_CONTRACT_SCHEMA, EVIDENCE_CONTRACT_SCHEMA};
use crate::core::artifact_dom_boxes::ARTIFACT_DOM_BOXES_SCHEMA;
use crate::core::artifact_ref::{
    ARTIFACT_REF_SCHEMA, HOMEBOY_REF_SCHEME, RUNNER_ARTIFACT_REF_SCHEME,
};
use crate::core::artifacts::{
    ARTIFACT_POSTPROCESS_PLAN_SCHEMA, ARTIFACT_POSTPROCESS_RESULT_SCHEMA,
    ARTIFACT_POSTPROCESS_SCHEMA, RUNTIME_AGENT_ARTIFACT_PATHS_SCHEMA,
    RUNTIME_AGENT_FINAL_OUTPUT_ARTIFACT_PATH, RUNTIME_AGENT_PATCH_DIFF_ARTIFACT_FILE,
    RUNTIME_AGENT_PATCH_PATCH_ARTIFACT_FILE, RUNTIME_AGENT_RESULT_ARTIFACT_FILE,
    RUNTIME_AGENT_RESULT_ARTIFACT_FILE_LEGACY_UNDERSCORE, RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_FILE,
    RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_PATH, RUN_ARTIFACT_EVENTS_FILE, RUN_ARTIFACT_FANOUT_RUN_FILE,
    RUN_ARTIFACT_LOOP_POLICY_FILE, RUN_ARTIFACT_LOOP_RESULT_FILE, RUN_ARTIFACT_OUTCOME_FILE,
    RUN_ARTIFACT_RESULTS_FILE, RUN_ARTIFACT_STATUS_FILE,
};
use crate::core::change_artifact::CHANGE_ARTIFACT_SCHEMA;
use crate::core::fuzz::FUZZ_ARTIFACT_SCHEMA;
use crate::core::matrix_artifact_summary::{
    GENERIC_MATRIX_SUMMARY_SCHEMA, MATRIX_ARTIFACT_SUMMARY_SCHEMA,
};
use crate::core::run_outcome_envelope::{RUN_OUTCOME_ENVELOPE_FILE, RUN_OUTCOME_ENVELOPE_SCHEMA};
use crate::core::runner_execution_envelope::{
    PATH_MATERIALIZATION_MODE_EXISTING_REMOTE, PATH_MATERIALIZATION_MODE_GIT,
    PATH_MATERIALIZATION_MODE_SNAPSHOT, PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_REQUIRE_PATHS,
    PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT, PATH_MATERIALIZATION_PLAN_SCHEMA,
    PATH_MATERIALIZATION_ROLE_PRIMARY_WORKSPACE, PATH_MATERIALIZATION_ROLE_REQUIRED_PATH,
    PATH_MATERIALIZATION_STATUS_MATERIALIZED, PATH_MATERIALIZATION_STATUS_VALIDATED,
};

pub const CONTRACT_CONSTANTS_SCHEMA: &str = "homeboy/contract-constants/v1";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContractConstantsOutput {
    pub schema: &'static str,
    pub contract_id: String,
    pub constants: ContractConstants,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ContractConstants {
    All(AllContractConstants),
    ArtifactManifest(ArtifactManifestConstants),
    ArtifactPaths(ArtifactPathsConstants),
    ArtifactPostprocess(ArtifactPostprocessConstants),
    Loop(LoopConstants),
    SecretEnvPlan(SecretEnvPlanConstants),
    ResourceLifecycleIndex(ResourceLifecycleIndexConstants),
    HostMutationLifecycle(HostMutationLifecycleConstants),
    RunLocationIndex(RunLocationIndexConstants),
    RunnerExecutionRecord(RunnerExecutionRecordConstants),
    PathMaterializationPlan(PathMaterializationPlanConstants),
    RunOutcomeEnvelope(RunOutcomeEnvelopeConstants),
    RunArtifactFiles(RunArtifactFilesConstants),
    RuntimeArtifacts(RuntimeArtifactConstants),
    RunnerArtifactManifestRef(RunnerArtifactManifestRefConstants),
    ReviewerFacingRef(ReviewerFacingRefConstants),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AllContractConstants {
    pub artifact_manifest: ArtifactManifestConstants,
    pub artifact_paths: ArtifactPathsConstants,
    pub artifact_postprocess: ArtifactPostprocessConstants,
    pub loop_contracts: LoopConstants,
    pub secret_env_plan: SecretEnvPlanConstants,
    pub resource_lifecycle_index: ResourceLifecycleIndexConstants,
    pub host_mutation_lifecycle: HostMutationLifecycleConstants,
    pub run_location_index: RunLocationIndexConstants,
    pub runner_execution_record: RunnerExecutionRecordConstants,
    pub path_materialization_plan: PathMaterializationPlanConstants,
    pub run_outcome_envelope: RunOutcomeEnvelopeConstants,
    pub run_artifact_files: RunArtifactFilesConstants,
    pub runtime_artifacts: RuntimeArtifactConstants,
    pub runner_artifact_manifest_ref: RunnerArtifactManifestRefConstants,
    pub reviewer_facing_ref: ReviewerFacingRefConstants,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactManifestConstants {
    pub schema_id: String,
    pub file_name: String,
    pub runner_manifest_ref_schema_id: String,
    pub runner_manifest_ref_name: String,
    pub runner_artifact_root_dir_suffix: String,
    pub represented_artifact_schema_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactPathsConstants {
    pub schema_id: String,
    pub runtime_agent_paths: RuntimeAgentArtifactPaths,
    pub canonical_filenames: RuntimeArtifactFilenames,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactPostprocessConstants {
    pub schema_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LoopConstants {
    pub controller_schema_id: String,
    pub controller_status_schema_id: String,
    pub definition_schema_id: String,
    pub action_schema_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SecretEnvPlanConstants {
    pub schema_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResourceLifecycleIndexConstants {
    pub schema_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostMutationLifecycleConstants {
    pub schema_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunLocationIndexConstants {
    pub schema_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerExecutionRecordConstants {
    pub schema_id: String,
    pub projection_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PathMaterializationPlanConstants {
    pub schema_id: String,
    pub roles: Vec<String>,
    pub owners: Vec<String>,
    pub materialization_modes: Vec<String>,
    pub validation_statuses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunOutcomeEnvelopeConstants {
    pub schema_id: String,
    pub file_name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunArtifactFilesConstants {
    pub events: String,
    pub status: String,
    pub results: String,
    pub outcome: String,
    pub run_outcome_envelope: String,
    pub fanout_run: String,
    pub loop_result: String,
    pub loop_policy: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeArtifactConstants {
    pub runtime_agent_paths: RuntimeAgentArtifactPaths,
    pub canonical_filenames: RuntimeArtifactFilenames,
    pub run_artifact_files: RunArtifactFilesConstants,
    pub executor_evidence: ExecutorEvidenceConstants,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeAgentArtifactPaths {
    pub transcript: String,
    pub final_output: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeArtifactFilenames {
    pub transcript: String,
    pub agent_result: String,
    pub agent_result_legacy_underscore: String,
    pub patch_diff: String,
    pub patch_patch: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExecutorEvidenceConstants {
    pub input_kind: String,
    pub input_file_name: String,
    pub result_kind: String,
    pub result_file_name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerArtifactManifestRefConstants {
    pub schema_id: String,
    pub name: String,
    pub manifest_schema_id: String,
    pub manifest_file_name: String,
    pub artifact_root_dir_suffix: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReviewerFacingRefConstants {
    pub accepted_schemes: Vec<String>,
}

pub fn contract_constants(contract_id: &str) -> Option<ContractConstantsOutput> {
    let normalized = contract_id.trim();
    let constants = match normalized {
        "all" => ContractConstants::All(AllContractConstants {
            artifact_manifest: artifact_manifest_constants(),
            artifact_paths: artifact_paths_constants(),
            artifact_postprocess: artifact_postprocess_constants(),
            loop_contracts: loop_constants(),
            secret_env_plan: secret_env_plan_constants(),
            resource_lifecycle_index: resource_lifecycle_index_constants(),
            host_mutation_lifecycle: host_mutation_lifecycle_constants(),
            run_location_index: run_location_index_constants(),
            runner_execution_record: runner_execution_record_constants(),
            path_materialization_plan: path_materialization_plan_constants(),
            run_outcome_envelope: run_outcome_envelope_constants(),
            run_artifact_files: run_artifact_files_constants(),
            runtime_artifacts: runtime_artifact_constants(),
            runner_artifact_manifest_ref: runner_artifact_manifest_ref_constants(),
            reviewer_facing_ref: reviewer_facing_ref_constants(),
        }),
        "artifact-manifest" => ContractConstants::ArtifactManifest(artifact_manifest_constants()),
        "artifact-paths" => ContractConstants::ArtifactPaths(artifact_paths_constants()),
        "artifact-postprocess" => {
            ContractConstants::ArtifactPostprocess(artifact_postprocess_constants())
        }
        "loop" | "loop-contracts" => ContractConstants::Loop(loop_constants()),
        "secret-env-plan" => ContractConstants::SecretEnvPlan(secret_env_plan_constants()),
        "resource-lifecycle-index" => {
            ContractConstants::ResourceLifecycleIndex(resource_lifecycle_index_constants())
        }
        "host-mutation-lifecycle" => {
            ContractConstants::HostMutationLifecycle(host_mutation_lifecycle_constants())
        }
        "run-location-index" => ContractConstants::RunLocationIndex(run_location_index_constants()),
        "runner-execution-record" => {
            ContractConstants::RunnerExecutionRecord(runner_execution_record_constants())
        }
        "path-materialization-plan" => {
            ContractConstants::PathMaterializationPlan(path_materialization_plan_constants())
        }
        "run-outcome-envelope" => {
            ContractConstants::RunOutcomeEnvelope(run_outcome_envelope_constants())
        }
        "run-artifact-files" => ContractConstants::RunArtifactFiles(run_artifact_files_constants()),
        "runtime-artifacts" | "runtime-agent-artifacts" => {
            ContractConstants::RuntimeArtifacts(runtime_artifact_constants())
        }
        "runner-artifact-manifest-ref" => {
            ContractConstants::RunnerArtifactManifestRef(runner_artifact_manifest_ref_constants())
        }
        "reviewer-facing-ref" | "reviewer-ref" => {
            ContractConstants::ReviewerFacingRef(reviewer_facing_ref_constants())
        }
        _ => return None,
    };

    Some(ContractConstantsOutput {
        schema: CONTRACT_CONSTANTS_SCHEMA,
        contract_id: normalized.to_string(),
        constants,
    })
}

pub fn artifact_manifest_constants() -> ArtifactManifestConstants {
    ArtifactManifestConstants {
        schema_id: registry_schema_id("artifact-manifest"),
        file_name: RUNNER_ARTIFACT_MANIFEST_FILE.to_string(),
        runner_manifest_ref_schema_id: RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA.to_string(),
        runner_manifest_ref_name: RUNNER_ARTIFACT_MANIFEST_REF_NAME.to_string(),
        runner_artifact_root_dir_suffix: RUNNER_ARTIFACT_ROOT_DIR_SUFFIX.to_string(),
        represented_artifact_schema_ids: represented_artifact_schema_ids(),
    }
}

pub fn artifact_paths_constants() -> ArtifactPathsConstants {
    ArtifactPathsConstants {
        schema_id: RUNTIME_AGENT_ARTIFACT_PATHS_SCHEMA.to_string(),
        runtime_agent_paths: runtime_agent_artifact_paths(),
        canonical_filenames: runtime_artifact_filenames(),
    }
}

pub fn artifact_postprocess_constants() -> ArtifactPostprocessConstants {
    ArtifactPostprocessConstants {
        schema_id: registry_schema_id("artifact-postprocess"),
    }
}

pub fn loop_constants() -> LoopConstants {
    LoopConstants {
        controller_schema_id: AGENT_TASK_LOOP_CONTROLLER_SCHEMA.to_string(),
        controller_status_schema_id: AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA.to_string(),
        definition_schema_id: AGENT_TASK_LOOP_DEFINITION_SCHEMA.to_string(),
        action_schema_id: AGENT_TASK_LOOP_ACTION_SCHEMA.to_string(),
    }
}

pub fn secret_env_plan_constants() -> SecretEnvPlanConstants {
    SecretEnvPlanConstants {
        schema_id: registry_schema_id("secret-env-plan"),
    }
}

pub fn resource_lifecycle_index_constants() -> ResourceLifecycleIndexConstants {
    ResourceLifecycleIndexConstants {
        schema_id: registry_schema_id("resource-lifecycle-index"),
    }
}

pub fn host_mutation_lifecycle_constants() -> HostMutationLifecycleConstants {
    HostMutationLifecycleConstants {
        schema_id: registry_schema_id("host-mutation-lifecycle"),
    }
}

pub fn run_location_index_constants() -> RunLocationIndexConstants {
    RunLocationIndexConstants {
        schema_id: registry_schema_id("run-location-index"),
    }
}

pub fn runner_execution_record_constants() -> RunnerExecutionRecordConstants {
    RunnerExecutionRecordConstants {
        schema_id: registry_schema_id("runner-execution-record"),
        projection_fields: vec![
            "execution_id".to_string(),
            "runner_id".to_string(),
            "transport".to_string(),
            "status".to_string(),
            "job_id".to_string(),
            "local_run_id".to_string(),
            "remote_run_id".to_string(),
            "agent_task_run_id".to_string(),
            "mirror_run_id".to_string(),
            "materialized_paths".to_string(),
            "artifact_refs".to_string(),
            "next_actions".to_string(),
        ],
    }
}

pub fn path_materialization_plan_constants() -> PathMaterializationPlanConstants {
    PathMaterializationPlanConstants {
        schema_id: PATH_MATERIALIZATION_PLAN_SCHEMA.to_string(),
        roles: vec![
            PATH_MATERIALIZATION_ROLE_PRIMARY_WORKSPACE.to_string(),
            PATH_MATERIALIZATION_ROLE_REQUIRED_PATH.to_string(),
        ],
        owners: vec![
            PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_SOURCE_SNAPSHOT.to_string(),
            PATH_MATERIALIZATION_OWNER_RUNNER_EXEC_REQUIRE_PATHS.to_string(),
        ],
        materialization_modes: vec![
            PATH_MATERIALIZATION_MODE_EXISTING_REMOTE.to_string(),
            PATH_MATERIALIZATION_MODE_GIT.to_string(),
            PATH_MATERIALIZATION_MODE_SNAPSHOT.to_string(),
        ],
        validation_statuses: vec![
            PATH_MATERIALIZATION_STATUS_MATERIALIZED.to_string(),
            PATH_MATERIALIZATION_STATUS_VALIDATED.to_string(),
        ],
    }
}

pub fn run_outcome_envelope_constants() -> RunOutcomeEnvelopeConstants {
    RunOutcomeEnvelopeConstants {
        schema_id: RUN_OUTCOME_ENVELOPE_SCHEMA.to_string(),
        file_name: RUN_OUTCOME_ENVELOPE_FILE.to_string(),
    }
}

pub fn run_artifact_files_constants() -> RunArtifactFilesConstants {
    RunArtifactFilesConstants {
        events: RUN_ARTIFACT_EVENTS_FILE.to_string(),
        status: RUN_ARTIFACT_STATUS_FILE.to_string(),
        results: RUN_ARTIFACT_RESULTS_FILE.to_string(),
        outcome: RUN_ARTIFACT_OUTCOME_FILE.to_string(),
        run_outcome_envelope: RUN_OUTCOME_ENVELOPE_FILE.to_string(),
        fanout_run: RUN_ARTIFACT_FANOUT_RUN_FILE.to_string(),
        loop_result: RUN_ARTIFACT_LOOP_RESULT_FILE.to_string(),
        loop_policy: RUN_ARTIFACT_LOOP_POLICY_FILE.to_string(),
    }
}

pub fn runtime_artifact_constants() -> RuntimeArtifactConstants {
    RuntimeArtifactConstants {
        runtime_agent_paths: runtime_agent_artifact_paths(),
        canonical_filenames: runtime_artifact_filenames(),
        run_artifact_files: run_artifact_files_constants(),
        executor_evidence: ExecutorEvidenceConstants {
            input_kind: EXECUTOR_INPUT_EVIDENCE_KIND.to_string(),
            input_file_name: EXECUTOR_INPUT_FILE.to_string(),
            result_kind: EXECUTOR_RESULT_EVIDENCE_KIND.to_string(),
            result_file_name: EXECUTOR_RESULT_FILE.to_string(),
        },
    }
}

pub fn runner_artifact_manifest_ref_constants() -> RunnerArtifactManifestRefConstants {
    RunnerArtifactManifestRefConstants {
        schema_id: RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA.to_string(),
        name: RUNNER_ARTIFACT_MANIFEST_REF_NAME.to_string(),
        manifest_schema_id: RUNNER_ARTIFACT_MANIFEST_SCHEMA.to_string(),
        manifest_file_name: RUNNER_ARTIFACT_MANIFEST_FILE.to_string(),
        artifact_root_dir_suffix: RUNNER_ARTIFACT_ROOT_DIR_SUFFIX.to_string(),
    }
}

fn runtime_agent_artifact_paths() -> RuntimeAgentArtifactPaths {
    RuntimeAgentArtifactPaths {
        transcript: RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_PATH.to_string(),
        final_output: RUNTIME_AGENT_FINAL_OUTPUT_ARTIFACT_PATH.to_string(),
    }
}

fn runtime_artifact_filenames() -> RuntimeArtifactFilenames {
    RuntimeArtifactFilenames {
        transcript: RUNTIME_AGENT_TRANSCRIPT_ARTIFACT_FILE.to_string(),
        agent_result: RUNTIME_AGENT_RESULT_ARTIFACT_FILE.to_string(),
        agent_result_legacy_underscore: RUNTIME_AGENT_RESULT_ARTIFACT_FILE_LEGACY_UNDERSCORE
            .to_string(),
        patch_diff: RUNTIME_AGENT_PATCH_DIFF_ARTIFACT_FILE.to_string(),
        patch_patch: RUNTIME_AGENT_PATCH_PATCH_ARTIFACT_FILE.to_string(),
    }
}

pub fn reviewer_facing_ref_constants() -> ReviewerFacingRefConstants {
    ReviewerFacingRefConstants {
        accepted_schemes: vec![
            "http://".to_string(),
            "https://".to_string(),
            HOMEBOY_REF_SCHEME.to_string(),
            RUNNER_ARTIFACT_REF_SCHEME.to_string(),
        ],
    }
}

fn registry_schema_id(name: &str) -> String {
    registered_contract(name)
        .expect("contract constants must reference registered contracts")
        .schema_id
        .to_string()
}

fn represented_artifact_schema_ids() -> Vec<String> {
    vec![
        RUNNER_ARTIFACT_MANIFEST_SCHEMA.to_string(),
        ARTIFACT_CONTRACT_SCHEMA.to_string(),
        EVIDENCE_CONTRACT_SCHEMA.to_string(),
        ARTIFACT_REF_SCHEMA.to_string(),
        ARTIFACT_ADDRESS_SCHEMA.to_string(),
        ARTIFACT_DOM_BOXES_SCHEMA.to_string(),
        ARTIFACT_POSTPROCESS_SCHEMA.to_string(),
        ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
        ARTIFACT_POSTPROCESS_RESULT_SCHEMA.to_string(),
        AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        AGENT_TASK_BATCH_ARTIFACTS_SCHEMA.to_string(),
        CHANGE_ARTIFACT_SCHEMA.to_string(),
        FUZZ_ARTIFACT_SCHEMA.to_string(),
        GENERIC_MATRIX_SUMMARY_SCHEMA.to_string(),
        MATRIX_ARTIFACT_SUMMARY_SCHEMA.to_string(),
    ]
}
