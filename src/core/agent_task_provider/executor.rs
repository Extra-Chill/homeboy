use super::command_runner::{failure_outcome, run_provider_command};
use super::fixtures::run_fixture_provider;
use super::*;

impl AgentTaskExecutorAdapter for ExtensionProviderAgentTaskExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        if request.executor.backend == "fixture" {
            return run_fixture_provider(&request);
        }
        if is_repo_local_gate_request(&request) {
            return run_repo_local_gate_task(&request);
        }

        let provider = match resolve_provider_for_backend(
            self.providers(),
            &request.executor.backend,
            request.executor.selector.as_deref(),
        ) {
            ProviderResolution::Resolved(provider) => provider,
            resolution => return provider_resolution_failure_outcome(&request, resolution),
        };

        let missing_capabilities: Vec<String> = request
            .executor
            .required_capabilities
            .iter()
            .filter(|capability| !provider.capabilities.contains(capability))
            .cloned()
            .collect();
        if !missing_capabilities.is_empty() {
            return failure_outcome(
                &request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::CapabilityMissing,
                "agent_task.capability_missing",
                format!(
                    "provider '{}' is missing required capabilities: {}",
                    provider.id,
                    missing_capabilities.join(", ")
                ),
                json!({ "provider": provider.id, "missing_capabilities": missing_capabilities }),
            );
        }

        run_provider_command(&request, provider)
    }
}

fn provider_resolution_failure_outcome(
    request: &AgentTaskRequest,
    resolution: ProviderResolution<'_>,
) -> AgentTaskOutcome {
    match resolution {
        ProviderResolution::Resolved(_) => unreachable!("resolved provider handled before failure"),
        ProviderResolution::NotFound => failure_outcome(
            request,
            AgentTaskOutcomeStatus::Failed,
            AgentTaskFailureClassification::CapabilityMissing,
            "agent_task.provider_missing",
            format!(
                "no extension agent-task provider found for backend '{}'",
                request.executor.backend
            ),
            json!({ "backend": request.executor.backend }),
        ),
        ProviderResolution::AmbiguousExtensionAlias { candidate_ids } => failure_outcome(
            request,
            AgentTaskOutcomeStatus::Failed,
            AgentTaskFailureClassification::CapabilityMissing,
            "agent_task.provider_ambiguous",
            format!(
                "multiple extension agent-task providers match backend '{}'; pass --selector with one provider id",
                request.executor.backend
            ),
            json!({
                "backend": request.executor.backend,
                "available_provider_ids": candidate_ids,
            }),
        ),
        ProviderResolution::SelectorMismatch { available_ids } => failure_outcome(
            request,
            AgentTaskOutcomeStatus::Failed,
            AgentTaskFailureClassification::CapabilityMissing,
            "agent_task.provider_selector_mismatch",
            selector_mismatch_message(
                &request.executor.backend,
                request.executor.selector.as_deref(),
            ),
            json!({
                "backend": request.executor.backend,
                "selector": request.executor.selector,
                "available_provider_ids": available_ids,
                "hint": selector_runtime_provider_hint(
                    &request.executor.backend,
                    request.executor.selector.as_deref(),
                ),
            }),
        ),
    }
}

fn selector_mismatch_message(backend: &str, selector: Option<&str>) -> String {
    let base = format!(
        "no extension agent-task provider for backend '{}' matched selector '{}'",
        backend,
        selector.unwrap_or("")
    );
    match selector_runtime_provider_hint(backend, selector) {
        Some(hint) => format!("{base}; {hint}"),
        None => base,
    }
}
