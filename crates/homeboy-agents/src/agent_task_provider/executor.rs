use super::command_runner::{failure_outcome, run_materialized_provider_command};
use super::*;

impl AgentTaskExecutorAdapter for ExtensionProviderAgentTaskExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let mut request = match materialize_executor_request(request, &context) {
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
        // Compiles to `None` in a production build: the test double is gated
        // behind `test-support` so no backend-name comparison sits ahead of
        // provider resolution in the shipped binary (#11118).
        if let Some(outcome) = fixture_provider_outcome(&request) {
            return outcome;
        }
        if is_repo_local_gate_request(&request) {
            return run_repo_local_gate_task(&request);
        }

        let requirements = match request.capability_requirements() {
            Ok(requirements) => requirements,
            Err(message) => {
                return failure_outcome(
                    &request,
                    AgentTaskOutcomeStatus::Failed,
                    AgentTaskFailureClassification::CapabilityMissing,
                    "agent_task.capability_requirements_invalid",
                    message,
                    json!({ "layer": "declaration" }),
                )
            }
        };
        let provider = match resolved_provider_from_request(&request) {
            Ok(Some(provider)) => provider,
            Ok(None) => match resolve_provider_for_backend(
                &self
                    .providers()
                    .iter()
                    .filter(|provider| {
                        requirements
                            .provider
                            .iter()
                            .all(|capability| provider.capabilities.contains(capability))
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
                &request.executor.backend,
                request.executor.selector.as_deref(),
            ) {
                ProviderResolution::Resolved(provider) => provider.clone(),
                resolution => {
                    if self
                        .providers()
                        .iter()
                        .any(|provider| provider.backend == request.executor.backend)
                        && requirements.provider.iter().any(|capability| {
                            !self.providers().iter().any(|provider| {
                                provider.backend == request.executor.backend
                                    && provider.capabilities.contains(capability)
                            })
                        })
                    {
                        return failure_outcome(
                            &request,
                            AgentTaskOutcomeStatus::Failed,
                            AgentTaskFailureClassification::CapabilityMissing,
                            "agent_task.provider_capability_unavailable",
                            format!(
                                "no provider for backend '{}' advertises required capabilities: {}",
                                request.executor.backend,
                                requirements.provider.join(", ")
                            ),
                            json!({
                                "layer": "provider",
                                "required_capabilities": requirements.provider,
                                "candidates": self.providers().iter().filter(|provider| provider.backend == request.executor.backend).map(|provider| json!({ "id": provider.id, "advertised_capabilities": provider.capabilities })).collect::<Vec<_>>(),
                                "remediation": "Select a provider that advertises every required provider capability or change the provider requirement."
                            }),
                        );
                    }
                    return provider_resolution_failure_outcome(
                        &request,
                        resolution,
                        self.diagnostics(),
                    );
                }
            },
            Err(message) => {
                return failure_outcome(
                    &request,
                    AgentTaskOutcomeStatus::ProviderError,
                    AgentTaskFailureClassification::Provider,
                    "agent_task.runtime_identity_invalid",
                    message,
                    Value::Null,
                )
            }
        };
        let workspace_root = request.request.workspace.root.clone();
        if let Err(message) = validate_provider_immediate_failure_patterns(&provider) {
            return failure_outcome(
                &request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::InvalidInput,
                "agent_task.provider_immediate_failure_configuration_invalid",
                format!(
                    "provider '{}' has invalid immediate failure configuration: {message}",
                    provider.id
                ),
                json!({ "provider_id": provider.id, "backend": provider.backend }),
            );
        }
        bind_workspace_permission_root(
            &mut request.request.executor,
            workspace_root.as_deref(),
            &provider,
        );

        if let Err(error) = resolve_runtime_tools(&mut request, &provider) {
            return failure_outcome(
                &request,
                AgentTaskOutcomeStatus::Failed,
                error.failure_classification,
                error.class,
                error.message,
                error.data,
            );
        }

        let missing_capabilities: Vec<String> = requirements
            .provider
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
                json!({
                    "layer": "provider",
                    "provider": provider.id,
                    "missing_capabilities": missing_capabilities,
                    "advertised_capabilities": provider.capabilities,
                }),
            );
        }

        let unavailable_attached_tools = requirements
            .attached_tools
            .iter()
            .filter_map(|required| {
                let resolved = request
                    .resolved_runtime_tools
                    .iter()
                    .find(|tool| tool.id == required.id);
                let missing = required
                    .contributes
                    .iter()
                    .filter(|capability| {
                        resolved.is_none_or(|tool| !tool.capabilities.contains(capability))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                (!missing.is_empty()).then(|| {
                    json!({
                        "tool_id": required.id,
                        "missing_capabilities": missing,
                        "readiness": resolved.map_or("missing", |tool| tool.readiness.status.as_str()),
                    })
                })
            })
            .collect::<Vec<_>>();
        if !unavailable_attached_tools.is_empty() {
            return failure_outcome(
                &request,
                AgentTaskOutcomeStatus::Failed,
                AgentTaskFailureClassification::CapabilityMissing,
                "agent_task.attached_tool_capability_unavailable",
                "attached runtime tools did not satisfy their required capabilities".to_string(),
                json!({
                    "layer": "attached_tools",
                    "tools": unavailable_attached_tools,
                }),
            );
        }

        // Readiness is owned by the attached runtime tool. A declaration alone
        // cannot contribute capabilities; only its persisted ready observation can.
        let ready_tools = crate::agent_task::ready_attached_tools_from_metadata(&request.metadata);
        request
            .request
            .record_capability_evidence(requirements.evidence(
                provider.capabilities.clone(),
                Vec::new(),
                &ready_tools,
            ));

        run_materialized_provider_command(
            &request,
            &provider,
            context.run_id.as_deref(),
            context.attempt,
        )
    }
}

/// Pass the scheduler-selected workspace without deriving a path from runtime
/// identifiers. The explicit capability keeps this provider-owned config field
/// out of executors that do not consume it.
fn bind_workspace_permission_root(
    executor: &mut crate::agent_task::AgentTaskExecutor,
    workspace_root: Option<&str>,
    provider: &AgentTaskExecutorProvider,
) {
    if !provider
        .capabilities
        .iter()
        .any(|capability| capability == AGENT_TASK_PROVIDER_CAPABILITY_WORKSPACE_PERMISSION_ROOT_V1)
    {
        return;
    }
    let Some(root) = workspace_root else {
        return;
    };
    if !executor.config.is_object() {
        executor.config = Value::Object(Default::default());
    }
    executor
        .config
        .as_object_mut()
        .expect("executor config object")
        .insert(
            "workspace_permission_root".to_string(),
            Value::String(root.to_string()),
        );
}

fn resolved_provider_from_request(
    request: &AgentTaskExecutorRequest,
) -> std::result::Result<Option<AgentTaskExecutorProvider>, String> {
    let Some(identity) = request
        .request
        .metadata
        .get("resolved_runtime_identity")
        .filter(|identity| !identity.is_null())
    else {
        return Ok(None);
    };
    let identity: homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity =
        serde_json::from_value(identity.clone())
            .map_err(|error| format!("invalid controller runtime identity: {error}"))?;
    if !homeboy_core::agent_runtime_manifest::is_immutable_revision(&identity.source_revision) {
        return Err(format!(
            "controller runtime identity for provider '{}' has a non-immutable source revision '{}'",
            identity.provider_id, identity.source_revision
        ));
    }
    let provider: AgentTaskExecutorProvider = serde_json::from_value(identity.provider)
        .map_err(|error| format!("invalid controller-selected provider: {error}"))?;
    if provider.id != identity.provider_id || provider.backend != request.request.executor.backend {
        return Err(format!(
            "controller runtime identity does not match requested provider backend '{}' and id '{}'",
            request.request.executor.backend, identity.provider_id
        ));
    }
    Ok(Some(provider))
}

#[expect(
    clippy::result_large_err,
    reason = "caller must retain the original request and artifact path to record a durable failure"
)]
fn materialize_executor_request(
    request: AgentTaskRequest,
    context: &AgentTaskExecutionContext,
) -> Result<AgentTaskExecutorRequest, (AgentTaskRequest, PathBuf, std::io::Error)> {
    let root = match homeboy_core::artifacts::root() {
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

#[expect(
    clippy::result_large_err,
    reason = "caller must retain the original request and artifact path to record a durable failure"
)]
fn materialize_executor_request_at_root(
    request: AgentTaskRequest,
    context: &AgentTaskExecutionContext,
    root: PathBuf,
) -> Result<AgentTaskExecutorRequest, (AgentTaskRequest, PathBuf, std::io::Error)> {
    let path = root
        .join("agent-task")
        .join("executor-artifacts")
        .join(homeboy_core::paths::sanitize_path_segment(
            context.run_id.as_deref().unwrap_or(&context.plan_id),
        ))
        .join(homeboy_core::paths::sanitize_path_segment(&request.task_id))
        .join(format!("attempt-{}", context.attempt));

    if let Err(error) = ensure_writable_directory(&path) {
        return Err((request, path, error));
    }
    let artifacts_root_identity =
        crate::agent_task_provider::artifact_finalization::ExecutorArtifactRootIdentity::capture(
            &path,
        )
        .map_err(|error| {
            (
                request.clone(),
                path.clone(),
                std::io::Error::other(error.to_string()),
            )
        })?;

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
        resolved_runtime_tools: Vec::new(),
        artifacts_root_identity,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
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
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
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

    #[test]
    fn workspace_permission_root_is_emitted_only_for_the_versioned_capability() {
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "permission-aware",
            "backend": "permission-aware",
            "capabilities": [AGENT_TASK_PROVIDER_CAPABILITY_WORKSPACE_PERMISSION_ROOT_V1]
        }))
        .expect("provider declaration");
        let mut executor = AgentTaskExecutor {
            backend: "permission-aware".to_string(),
            selector: None,
            runtime_selection: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            model: None,
            config: json!({ "preserve": true }),
        };
        let workspace = "/scratch/cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874/workspace";

        bind_workspace_permission_root(&mut executor, Some(workspace), &provider);

        assert_eq!(executor.config["workspace_permission_root"], workspace);
        let provider_without_capability: AgentTaskExecutorProvider =
            serde_json::from_value(json!({
                "id": "unaware",
                "backend": "permission-aware"
            }))
            .expect("provider declaration");
        let mut unaffected = executor.clone();
        unaffected
            .config
            .as_object_mut()
            .expect("config object")
            .remove("workspace_permission_root");

        bind_workspace_permission_root(
            &mut unaffected,
            Some("/different-materialized-workspace"),
            &provider_without_capability,
        );

        assert!(unaffected.config.get("workspace_permission_root").is_none());
        assert_eq!(unaffected.config["preserve"], true);
    }
}
