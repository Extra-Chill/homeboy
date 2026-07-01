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
