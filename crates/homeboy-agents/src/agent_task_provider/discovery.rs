//! The Pi executor is built in; provider discovery never reads extensions.

use super::AgentTaskExecutorProvider;

pub(crate) fn discover_agent_task_executor_providers() -> Vec<AgentTaskExecutorProvider> {
    vec![serde_json::from_value(serde_json::json!({
        "id": "pi.agent-task-executor",
        "label": "Pi agent task executor",
        "backend": "pi",
        "runtime": "pi",
        "invocation": { "argv": ["pi"] },
        "capabilities": ["cli_runtime", "workspace_materialization", "structured_outcome", "provider_owned_auth", "provider_owned_session"],
        "workspace_materialization": { "cwd": "request_workspace", "requires_git": false, "write_scope": "workspace" },
        "provider_defaults": { "pi": { "command": "pi", "command_args": ["--mode", "json"] } },
        "request_required_fields": ["schema", "task_id", "executor.backend", "instructions"],
        "outcome_statuses": ["succeeded", "no_op", "unable_to_remediate", "provider_error", "timeout", "candidate_recoverable", "failed", "follow_up_issue", "cancelled"],
        "failure_classifications": ["provider", "transient", "timeout", "stalled", "rate_limited", "provider_account_blocked", "provider_quota_exhausted", "provider_billing_blocked", super::structured_error::PROVIDER_CREDENTIALS_EXHAUSTED, "policy_denied", "capability_missing", "invalid_input", "execution_failed", "unknown"],
        "redacted_metadata_keys": ["secret_env_values", "secretEnvValues", "secrets"],
        "lifecycle": { "completion": "synchronous_process", "cancellation": "process_signal" },
        "status": "experimental",
        "integration_contract": "homeboy-pi-agent-task/v1"
    })).expect("built-in Pi provider is valid")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_is_available_without_runtime_manifests() {
        let providers = discover_agent_task_executor_providers();
        assert_eq!(providers.len(), 1);
        let provider = &providers[0];
        assert_eq!(provider.id, "pi.agent-task-executor");
        assert_eq!(provider.backend, "pi");
        assert!(!provider.extra.contains_key("runtime_materialization_plan"));
    }
}
