//! Agent-task command provider/contract metadata export tests.

use super::support::*;

#[test]
fn providers_output_includes_core_capability_contract() {
    with_isolated_home(|_| {
        let (value, status) = review::providers(ProvidersArgs {
            secret_env: Vec::new(),
        })
        .expect("providers output");

        assert_eq!(status, 0);
        assert_eq!(
            value["capability_contract"]["schema"],
            AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA
        );
        assert_eq!(
            value["capability_contract"]["provider_schema"],
            AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA
        );
        assert_eq!(
            value["capability_contract"]["request_schema"],
            AGENT_TASK_REQUEST_SCHEMA
        );
        assert_eq!(
            value["capability_contract"]["outcome_schema"],
            AGENT_TASK_OUTCOME_SCHEMA
        );
    });
}

#[test]
fn contract_output_exports_core_agent_task_metadata() {
    let (value, status) = contract::contract(ContractArgs {
        format: ContractFormat::Json,
    })
    .expect("contract output");

    assert_eq!(status, 0);
    assert_eq!(value["schema"], "homeboy/agent-task-core-contract/v1");
    assert_eq!(value["schemas"]["request"], AGENT_TASK_REQUEST_SCHEMA);
    assert_eq!(
        value["schemas"]["artifact_declaration"],
        "homeboy/agent-task-artifact-declaration/v1"
    );
    assert_eq!(
        value["schemas"]["evidence_ref"],
        "homeboy/agent-task-evidence-ref/v1"
    );
    assert_eq!(
        value["schemas"]["secret_env_requirement"],
        "homeboy/secret-env-requirement/v1"
    );
    assert_eq!(
        value["schemas"]["loop_definition"],
        "homeboy/agent-task-loop-definition/v1"
    );
    assert_eq!(
        value["schemas"]["provider"],
        AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA
    );
    assert_eq!(
        value["provider_capability"]["schema"],
        AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA
    );
    assert_eq!(
        value["provider_capability"]["request_required_fields"],
        json!(["schema", "task_id", "executor.backend", "instructions"])
    );
    assert_eq!(
        value["provider_capability"]["redacted_metadata_keys"],
        json!(["secret_env_values", "secretEnvValues", "secrets"])
    );
    assert!(value["enums"]["outcome_status"]
        .as_array()
        .expect("outcome statuses")
        .contains(&json!("provider_error")));
    assert!(value["enums"]["failure_classification"]
        .as_array()
        .expect("failure classifications")
        .contains(&json!("capability_missing")));
    assert!(value["redaction_defaults"]["sensitive_keys"]
        .as_array()
        .expect("sensitive keys")
        .contains(&json!("refresh_token")));
}
