use serde::Serialize;

use crate::command_contract::{registered_contract, RUNNER_ARTIFACT_MANIFEST_FILE};
use crate::core::agent_task_contract::AGENT_TASK_LOOP_ACTION_SCHEMA;
use crate::core::agent_task_loop_controller::{
    AGENT_TASK_LOOP_CONTROLLER_SCHEMA, AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA,
};
use crate::core::agent_task_loop_definition::AGENT_TASK_LOOP_DEFINITION_SCHEMA;
use crate::core::artifact_ref::{HOMEBOY_REF_SCHEME, RUNNER_ARTIFACT_REF_SCHEME};

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
    ArtifactPostprocess(ArtifactPostprocessConstants),
    Loop(LoopConstants),
    SecretEnvPlan(SecretEnvPlanConstants),
    RunLocationIndex(RunLocationIndexConstants),
    ReviewerFacingRef(ReviewerFacingRefConstants),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AllContractConstants {
    pub artifact_manifest: ArtifactManifestConstants,
    pub artifact_postprocess: ArtifactPostprocessConstants,
    pub loop_contracts: LoopConstants,
    pub secret_env_plan: SecretEnvPlanConstants,
    pub run_location_index: RunLocationIndexConstants,
    pub reviewer_facing_ref: ReviewerFacingRefConstants,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactManifestConstants {
    pub schema_id: String,
    pub file_name: String,
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
pub struct RunLocationIndexConstants {
    pub schema_id: String,
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
            artifact_postprocess: artifact_postprocess_constants(),
            loop_contracts: loop_constants(),
            secret_env_plan: secret_env_plan_constants(),
            run_location_index: run_location_index_constants(),
            reviewer_facing_ref: reviewer_facing_ref_constants(),
        }),
        "artifact-manifest" => ContractConstants::ArtifactManifest(artifact_manifest_constants()),
        "artifact-postprocess" => {
            ContractConstants::ArtifactPostprocess(artifact_postprocess_constants())
        }
        "loop" | "loop-contracts" => ContractConstants::Loop(loop_constants()),
        "secret-env-plan" => ContractConstants::SecretEnvPlan(secret_env_plan_constants()),
        "run-location-index" => ContractConstants::RunLocationIndex(run_location_index_constants()),
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

pub fn run_location_index_constants() -> RunLocationIndexConstants {
    RunLocationIndexConstants {
        schema_id: registry_schema_id("run-location-index"),
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
