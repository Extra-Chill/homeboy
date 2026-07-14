use super::command_runner::{failure_outcome, run_materialized_provider_command};
use super::fixtures::run_fixture_provider;
use super::*;

impl AgentTaskExecutorAdapter for ExtensionProviderAgentTaskExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let request = match materialize_executor_request(request, &context) {
            Ok(request) => request,
            Err((request, path, error)) => {
                return failure_outcome(
                    &request,
                    AgentTaskOutcomeStatus::ProviderError,
                    AgentTaskFailureClassification::Provider,
                    "agent_task.artifacts_path_materialization_failed",
                    format!(
                        "Homeboy could not materialize the runner-local executor artifact directory '{}': {error}",
                        path.display()
                    ),
                    json!({
                        "artifacts_path": path,
                        "locality": "runner",
                        "owner": "homeboy",
                        "remediation": "Ensure the runner artifact root exists on the execution host and is writable by the Homeboy process."
                    }),
                )
            }
        };
        if request.executor.backend == "fixture" {
            return run_fixture_provider(&request, &request.artifacts_path);
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
            resolution => {
                return provider_resolution_failure_outcome(
                    &request,
                    resolution,
                    self.diagnostics(),
                )
            }
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

        run_materialized_provider_command(&request, provider, context.run_id.as_deref())
    }
}

fn materialize_executor_request(
    request: AgentTaskRequest,
    context: &AgentTaskExecutionContext,
) -> Result<AgentTaskExecutorRequest, (AgentTaskRequest, PathBuf, std::io::Error)> {
    let root = match crate::core::artifacts::root() {
        Ok(root) => root,
        Err(error) => {
            return Err((
                request,
                PathBuf::from("<unresolved-runner-artifact-root>"),
                std::io::Error::other(error.to_string()),
            ))
        }
    };
    materialize_executor_request_at_root(request, context, root)
}

fn materialize_executor_request_at_root(
    request: AgentTaskRequest,
    context: &AgentTaskExecutionContext,
    root: PathBuf,
) -> Result<AgentTaskExecutorRequest, (AgentTaskRequest, PathBuf, std::io::Error)> {
    let path = root
        .join("agent-task")
        .join("executor-artifacts")
        .join(crate::core::paths::sanitize_path_segment(
            context.run_id.as_deref().unwrap_or(&context.plan_id),
        ))
        .join(crate::core::paths::sanitize_path_segment(&request.task_id))
        .join(format!("attempt-{}", context.attempt));

    if let Err(error) = ensure_writable_directory(&path) {
        return Err((request, path, error));
    }
    let artifacts_root_identity = crate::core::agent_task_provider::artifact_finalization::ExecutorArtifactRootIdentity::capture(&path)
        .map_err(|error| (request.clone(), path.clone(), std::io::Error::other(error.to_string())))?;

    let provenance = AgentTaskArtifactsPathProvenance {
        owner: "homeboy".to_string(),
        locality: "runner".to_string(),
        plan_id: context.plan_id.clone(),
        run_id: context.run_id.clone(),
        task_id: request.task_id.clone(),
        attempt: context.attempt,
    };
    Ok(AgentTaskExecutorRequest {
        request,
        artifacts_path: path,
        artifacts_path_provenance: provenance,
        artifacts_root_identity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
    };

    #[test]
    fn materialization_fails_before_execution_when_runner_root_is_not_a_directory() {
        let blocked_root = tempfile::NamedTempFile::new().expect("blocked artifact root");
        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "blocked-artifacts".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "run".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        };
        let context = AgentTaskExecutionContext {
            plan_id: "blocked-plan".to_string(),
            run_id: Some("blocked-run".to_string()),
            attempt: 1,
            cancellation: Default::default(),
        };

        let (_, path, error) = materialize_executor_request_at_root(
            request,
            &context,
            blocked_root.path().to_path_buf(),
        )
        .expect_err("file root must fail");

        assert!(path.starts_with(blocked_root.path()));
        assert!(!error.to_string().is_empty());
    }
}

fn ensure_writable_directory(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    if !std::fs::metadata(path)?.is_dir() {
        return Err(std::io::Error::other(
            "materialized path is not a directory",
        ));
    }
    let probe = path.join(format!(".homeboy-write-probe-{}", std::process::id()));
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(probe)?;
    Ok(())
}

fn provider_resolution_failure_outcome(
    request: &AgentTaskRequest,
    resolution: ProviderResolution<'_>,
    diagnostics: &[AgentRuntimeDiscoveryDiagnostic],
) -> AgentTaskOutcome {
    match resolution {
        ProviderResolution::Resolved(_) => unreachable!("resolved provider handled before failure"),
        ProviderResolution::NotFound => {
            let matching_diagnostics = runtime_discovery_diagnostics_for_backend(
                diagnostics,
                &request.executor.backend,
            );
            failure_outcome(
                request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::CapabilityMissing,
                "agent_task.provider_missing",
                provider_not_found_message(&request.executor.backend, &matching_diagnostics),
                json!({
                    "backend": request.executor.backend,
                    "runtime_discovery_diagnostics": matching_diagnostics,
                }),
            )
        }
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
        ProviderResolution::SelectorMismatch {
            available_ids,
            selector_hint,
        } => failure_outcome(
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
                "hint": selector_hint,
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
