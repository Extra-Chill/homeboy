use std::path::PathBuf;
use std::process::Command;

use homeboy::command_contract::{contract_constants, CONTRACT_CONSTANTS_SCHEMA};
use serde_json::Value;

#[test]
fn contract_constants_exports_homeboy_owned_contract_ids() {
    let output = contract_constants("all").expect("all constants");
    let value = serde_json::to_value(output).expect("constants json");

    assert_eq!(value["schema"], CONTRACT_CONSTANTS_SCHEMA);
    assert_eq!(value["contract_id"], "all");
    assert_eq!(
        value["constants"]["artifact_manifest"]["schema_id"],
        "homeboy/artifact-manifest/v1"
    );
    assert_eq!(
        value["constants"]["artifact_manifest"]["file_name"],
        "homeboy-artifact-manifest.json"
    );
    assert_eq!(
        value["constants"]["artifact_manifest"]["runner_manifest_ref_schema_id"],
        "homeboy/runner-artifact-manifest-ref/v1"
    );
    assert_eq!(
        value["constants"]["artifact_manifest"]["runner_manifest_ref_name"],
        "runner-artifact-manifest-ref"
    );
    assert!(
        value["constants"]["artifact_manifest"]["represented_artifact_schema_ids"]
            .as_array()
            .expect("represented schema IDs")
            .iter()
            .any(|schema| schema == "homeboy/agent-task-artifact/v1")
    );
    assert_eq!(
        value["constants"]["artifact_paths"]["schema_id"],
        "homeboy/runtime-agent-artifact-paths/v1"
    );
    assert_eq!(
        value["constants"]["artifact_paths"]["runtime_agent_paths"]["transcript"],
        "artifacts/agent-loop/transcript.json"
    );
    assert_eq!(
        value["constants"]["artifact_paths"]["canonical_filenames"]["agent_result"],
        "agent-result.json"
    );
    assert_eq!(
        value["constants"]["artifact_postprocess"]["schema_id"],
        "homeboy/artifact-postprocess/v1"
    );
    assert_eq!(
        value["constants"]["secret_env_plan"]["schema_id"],
        "homeboy/secret-env-plan/v1"
    );
    assert_eq!(
        value["constants"]["run_location_index"]["schema_id"],
        "homeboy/run-location-index/v1"
    );
    assert_eq!(
        value["constants"]["runner_execution_record"]["schema_id"],
        "homeboy/runner-execution-record/v1"
    );
    assert!(
        value["constants"]["runner_execution_record"]["projection_fields"]
            .as_array()
            .expect("projection fields")
            .iter()
            .any(|field| field == "materialized_paths")
    );
    assert_eq!(
        value["constants"]["path_materialization_plan"]["schema_id"],
        "homeboy/path-materialization-plan/v1"
    );
    assert!(
        value["constants"]["path_materialization_plan"]["materialization_modes"]
            .as_array()
            .expect("materialization modes")
            .iter()
            .any(|mode| mode == "snapshot")
    );
    assert_eq!(
        value["constants"]["runtime_artifacts"]["runtime_agent_paths"]["transcript"],
        "artifacts/agent-loop/transcript.json"
    );
    assert_eq!(
        value["constants"]["runtime_artifacts"]["runtime_agent_paths"]["final_output"],
        "artifacts/agent-loop/final.md"
    );
    assert_eq!(
        value["constants"]["runtime_artifacts"]["canonical_filenames"]["agent_result"],
        "agent-result.json"
    );
    assert_eq!(
        value["constants"]["runtime_artifacts"]["executor_evidence"]["input_file_name"],
        "executor-input.json"
    );
    assert_eq!(
        value["constants"]["runner_artifact_manifest_ref"]["schema_id"],
        "homeboy/runner-artifact-manifest-ref/v1"
    );
    assert_eq!(
        value["constants"]["runner_artifact_manifest_ref"]["manifest_file_name"],
        "homeboy-artifact-manifest.json"
    );
    assert_eq!(
        value["constants"]["host_mutation_lifecycle"]["schema_id"],
        "homeboy/host-mutation-lifecycle/v1"
    );
    assert_eq!(
        value["constants"]["loop_contracts"]["controller_schema_id"],
        "homeboy/agent-task-loop-controller/v1"
    );
    assert!(
        value["constants"]["reviewer_facing_ref"]["accepted_schemes"]
            .as_array()
            .expect("accepted schemes")
            .iter()
            .any(|scheme| scheme == "runner-artifact://")
    );
}

#[test]
fn contract_constants_cli_returns_json_envelope() {
    let output = homeboy_command()
        .args(["contract", "constants", "artifact-manifest"])
        .output()
        .expect("run homeboy");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let value: Value = serde_json::from_slice(&output.stdout).expect("json stdout");
    assert_eq!(value["success"], true);
    assert_eq!(value["data"]["schema"], CONTRACT_CONSTANTS_SCHEMA);
    assert_eq!(value["data"]["contract_id"], "artifact-manifest");
    assert_eq!(
        value["data"]["constants"]["schema_id"],
        "homeboy/artifact-manifest/v1"
    );
    assert_eq!(
        value["data"]["constants"]["file_name"],
        "homeboy-artifact-manifest.json"
    );
}

#[test]
fn contract_constants_cli_rejects_unknown_contract_id() {
    let output = homeboy_command()
        .args(["contract", "constants", "missing"])
        .output()
        .expect("run homeboy");

    assert_eq!(output.status.code(), Some(2));

    let value: Value = serde_json::from_slice(&output.stdout).expect("json stdout");
    assert_eq!(value["success"], false);
    assert_eq!(value["error"]["code"], "validation.invalid_argument");
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

fn homeboy_command() -> Command {
    let mut command = Command::new(homeboy_bin());
    command.env("HOMEBOY_NO_UPDATE_CHECK", "1");
    command
}
