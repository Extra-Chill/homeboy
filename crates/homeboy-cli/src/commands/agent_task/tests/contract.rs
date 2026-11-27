//! Agent-task command provider/contract metadata export tests.

use super::support::*;

#[test]
fn providers_output_declares_its_observed_scope_and_capability_contract() {
    // A provider catalog is only interpretable together with where it was
    // read: controller and runner differ in extensions, runtime defaults,
    // secrets, and provider readiness (#9763).
    with_isolated_home(|_| {
        let execution_runner = homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV;
        let previous = std::env::var_os(execution_runner);
        std::env::remove_var(execution_runner);

        let result = review::providers(ProvidersArgs {
            backend: None,
            selector: None,
            model: None,
            status: None,
            secret_env: Vec::new(),
            validate_readiness: false,
            refresh: false,
            catalog: false,
            full: false,
            machine_catalog: false,
            set_default: false,
        });

        if let Some(value) = previous {
            std::env::set_var(execution_runner, value);
        }
        let (value, status) = result.expect("providers output");

        assert_eq!(status, 0);
        let scope = &value["observed_scope"];
        assert_eq!(scope["schema"], "homeboy/agent-task-provider-scope/v1");
        assert_eq!(scope["location"], "controller");
        assert!(scope["runner_id"].is_null());
        assert_eq!(scope["label"], "controller");
        assert!(scope["homeboy_identity"]["version"].is_string());
        assert!(scope["observed_at"].is_string());
        assert_eq!(
            scope["runner_scoped_command"],
            "homeboy runner exec <runner-id> -- homeboy agent-task providers"
        );

        // Additive only: the pre-existing `scope` object keeps its meaning for
        // callers that already parse this output; an absent `--backend`
        // leaves the presentation unscoped (#9654).
        assert_eq!(value["schema"], "homeboy/agent-task-providers/v2");
        assert_eq!(value["scope"]["filtered"], false);
        assert!(value["scope"]["backend"].is_null());
        assert!(value["providers"].is_array());
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
fn providers_output_labels_runner_scope() {
    with_isolated_home(|_| {
        let execution_runner = homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV;
        let previous = std::env::var_os(execution_runner);
        std::env::set_var(execution_runner, "homeboy-lab");

        let result = review::providers(ProvidersArgs {
            backend: Some("pi".to_string()),
            selector: Some("pi.agent-task-executor".to_string()),
            model: Some("openai/gpt-5.6-luna".to_string()),
            status: None,
            secret_env: vec!["PI_TOKEN".to_string()],
            validate_readiness: false,
            refresh: false,
            catalog: false,
            full: false,
            machine_catalog: false,
            set_default: false,
        });

        match previous {
            Some(value) => std::env::set_var(execution_runner, value),
            None => std::env::remove_var(execution_runner),
        }
        let (value, status) = result.expect("runner-scoped provider output");

        assert_eq!(status, 0);
        assert_eq!(value["observed_scope"]["location"], "runner");
        assert_eq!(value["observed_scope"]["runner_id"], "homeboy-lab");
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
        value["schemas"]["plan_validation"],
        "homeboy/agent-task-plan-validation/v1"
    );
    assert_eq!(
        value["schemas"]["control_plane_action_eligibility"],
        "homeboy/control-plane-action-eligibility/v1"
    );
    assert_eq!(
        value["schemas"]["artifact_declaration"],
        "homeboy/agent-task-artifact-declaration/v1"
    );
    assert_eq!(
        value["schemas"]["artifact_handoff"],
        "homeboy/agent-task-artifact-handoff/v1"
    );
    assert_eq!(
        value["schemas"]["evidence_ref"],
        "homeboy/agent-task-evidence-ref/v1"
    );
    assert_eq!(
        value["schemas"]["provider_outcome_contract"],
        "homeboy/agent-task-provider-outcome-contract/v1"
    );
    assert_eq!(
        value["schemas"]["diagnostic_ranking"],
        "homeboy/agent-task-diagnostic-ranking/v1"
    );
    assert_eq!(
        value["schemas"]["secret_env_requirement"],
        "homeboy/secret-env-requirement/v1"
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
