use super::command_runner::{failure_outcome, run_materialized_provider_command_with_credentials};
use super::*;

const PROVIDER_ACCOUNT_BLOCK_TTL: chrono::Duration = chrono::Duration::minutes(5);

pub(super) fn effective_provider_for_request(
    request: &AgentTaskRequest,
    providers: &[AgentTaskExecutorProvider],
) -> std::result::Result<Option<AgentTaskExecutorProvider>, String> {
    if let Some(identity) = request
        .metadata
        .get("resolved_runtime_identity")
        .filter(|identity| !identity.is_null())
    {
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
        if provider.id != identity.provider_id || provider.backend != request.executor.backend {
            return Err(format!(
                "controller runtime identity does not match requested provider backend '{}' and id '{}'",
                request.executor.backend, identity.provider_id
            ));
        }
        let requirements = request.capability_requirements()?;
        if requirements
            .provider
            .iter()
            .any(|capability| !provider.capabilities.contains(capability))
        {
            return Ok(None);
        }
        return Ok(Some(provider));
    }

    let requirements = request.capability_requirements()?;
    let capable = providers
        .iter()
        .filter(|provider| {
            requirements
                .provider
                .iter()
                .all(|capability| provider.capabilities.contains(capability))
        })
        .cloned()
        .collect::<Vec<_>>();
    Ok(resolve_provider_for_backend(
        &capable,
        &request.executor.backend,
        request.executor.selector.as_deref(),
    )
    .resolved()
    .cloned())
}

fn request_with_effective_provider_contract(
    request: &AgentTaskRequest,
    providers: &[AgentTaskExecutorProvider],
) -> AgentTaskRequest {
    let mut request = request.clone();
    let base_secret_env = request
        .metadata
        .get("provider_admission")
        .and_then(|value| value.get("base_secret_env"))
        .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
        .unwrap_or_else(|| request.executor.secret_env.clone());
    if let Ok(Some(provider)) = effective_provider_for_request(&request, providers) {
        super::secrets::apply_provider_runner_secret_env_contract_for_request(
            &mut request,
            &provider,
            &base_secret_env,
        );
    }
    request
}

fn provider_capacity_key(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
) -> homeboy_core::Result<String> {
    let credential_env =
        super::secrets::provider_request_credential_env(request, provider).unwrap_or_default();
    provider_capacity_key_with_credentials(request, provider, &credential_env)
}

fn provider_capacity_key_with_credentials(
    request: &AgentTaskRequest,
    provider: &AgentTaskExecutorProvider,
    credential_env: &[(String, String)],
) -> homeboy_core::Result<String> {
    let effective_config = provider_capacity_config(request);
    let value = serde_json::json!({
        "provider": provider,
        "probe_request": readiness_request_key(provider, &effective_config)?,
        "runtime_selection": request.executor.runtime_selection(),
        "required_capabilities": request.executor.required_capabilities,
        "effective_config": effective_config,
        "resolved_runtime_identity": request.metadata.get("resolved_runtime_identity"),
        "credential_identity": credential_env.iter().map(|(name, value)| (
            name,
            homeboy_engine_primitives::content_hash::sha256_hex(value.as_bytes()),
        )).collect::<Vec<_>>(),
    });
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| homeboy_core::Error::internal_json(error.to_string(), None))?;
    Ok(homeboy_engine_primitives::content_hash::sha256_hex(
        &encoded,
    ))
}

fn reported_capacity_key(request_key: &str, evidence: &AgentTaskProviderRuntimeEvidence) -> String {
    let encoded = serde_json::to_vec(&(
        request_key,
        evidence.cache_identity.as_deref(),
        evidence.provider_identity.as_deref(),
    ))
    .unwrap_or_default();
    homeboy_engine_primitives::content_hash::sha256_hex(&encoded)
}

fn production_execution_preflight(
    request: &AgentTaskRequest,
    providers: &[AgentTaskExecutorProvider],
    diagnostics: &[AgentRuntimeDiscoveryDiagnostic],
    evidence: &Arc<Mutex<ProviderEvidenceStore>>,
    credential_env: Option<&[(String, String)]>,
) -> Option<AgentTaskOutcome> {
    let Ok(Some(provider)) = effective_provider_for_request(request, providers) else {
        return None;
    };
    let catalog = AgentTaskProviderCatalog {
        providers: providers.to_vec(),
        diagnostics: diagnostics.to_vec(),
        version: None,
    };
    let execution_plan = AgentTaskPlan::new(
        format!("execution-preflight-{}", request.task_id),
        vec![request.clone()],
    );
    for preflight in [
        catalog.validate_selected_models(&execution_plan),
        catalog.enforce_runtime_preflight_checks_for_plan(&execution_plan),
        super::config_preflight::preflight_plan_provider_config_with_providers(
            &execution_plan,
            catalog.providers(),
        ),
    ] {
        if let Err(error) = preflight {
            return Some(AgentTaskOutcome {
                task_id: request.task_id.clone(),
                status: AgentTaskOutcomeStatus::Failed,
                failure_classification: Some(AgentTaskFailureClassification::InvalidInput),
                summary: Some(format!(
                    "provider route failed execution preflight: {}",
                    error.message
                )),
                diagnostics: vec![AgentTaskDiagnostic {
                    class: "agent_task.provider_execution_preflight_failed".to_string(),
                    message: error.message,
                    data: error.details,
                }],
                ..Default::default()
            });
        }
    }
    let mut readiness_cache = evidence
        .lock()
        .map(|evidence| evidence.readiness.clone())
        .unwrap_or_default();
    let dispatchability = match credential_env {
        Some(credential_env) => {
            super::dispatchability::evaluate_request_dispatchability_with_credentials(
                &catalog,
                request,
                provider,
                credential_env,
                &mut readiness_cache,
            )
        }
        None => super::dispatchability::evaluate_request_dispatchability(
            &catalog,
            request,
            &mut readiness_cache,
        ),
    }
    .dispatchability;
    if dispatchability.ready {
        return None;
    }
    let timeout = dispatchability
        .checks
        .runtime
        .reason
        .as_deref()
        .is_some_and(|reason| reason.contains("execution deadline expired"));
    Some(AgentTaskOutcome {
        task_id: request.task_id.clone(),
        status: if timeout {
            AgentTaskOutcomeStatus::Timeout
        } else {
            AgentTaskOutcomeStatus::Failed
        },
        failure_classification: Some(if timeout {
            AgentTaskFailureClassification::Timeout
        } else if !dispatchability.checks.model.ready || !dispatchability.checks.route.ready {
            AgentTaskFailureClassification::CapabilityMissing
        } else {
            AgentTaskFailureClassification::InvalidInput
        }),
        summary: Some(format!(
            "provider route failed execution preflight: {}",
            dispatchability.reason
        )),
        diagnostics: vec![AgentTaskDiagnostic {
            class: "agent_task.provider_execution_preflight_failed".to_string(),
            message: dispatchability.reason.clone(),
            data: serde_json::to_value(dispatchability).unwrap_or(Value::Null),
        }],
        ..Default::default()
    })
}

impl AgentTaskExecutorAdapter for ExtensionProviderAgentTaskExecutor {
    fn provider_route_capacity_key(&self, request: &AgentTaskRequest) -> String {
        let request = request_with_effective_provider_contract(request, self.providers());
        effective_provider_for_request(&request, self.providers())
            .ok()
            .flatten()
            .and_then(|provider| provider_capacity_key(&request, &provider).ok())
            .unwrap_or_else(|| provider_usage_cap_key_for_request(&request))
    }

    fn provider_route_readiness(&self, request: &AgentTaskRequest) -> ProviderRouteReadiness {
        if is_fixture_backend(&request.executor.backend) || is_repo_local_gate_request(request) {
            return ProviderRouteReadiness::dispatchable();
        }
        let request = request_with_effective_provider_contract(request, self.providers());
        let provider = match effective_provider_for_request(&request, self.providers()) {
            Ok(Some(provider)) => provider,
            Ok(None) => {
                let resolution = resolve_provider_for_backend(
                    self.providers(),
                    &request.executor.backend,
                    request.executor.selector.as_deref(),
                );
                let route_exists = resolution.clone().resolved().is_some();
                let requires_provider_capabilities = request
                    .capability_requirements()
                    .is_ok_and(|requirements| !requirements.provider.is_empty());
                if route_exists && requires_provider_capabilities {
                    return ProviderRouteReadiness {
                        ready: false,
                        state: "provider_capability_unavailable".to_string(),
                        reason: "the route does not satisfy required provider capabilities"
                            .to_string(),
                        reset_at: None,
                        classification: Some("capability".to_string()),
                        retryable: false,
                        remediation: None,
                        cache_identity: None,
                        provider_identity: None,
                        capacity_key: None,
                        diagnostic_data: Some(
                            ProviderRouteDiagnosticData::ProviderCapabilityUnavailable {
                                layer: "provider".to_string(),
                                required_capabilities: request
                                    .executor
                                    .required_capabilities
                                    .clone(),
                            },
                        ),
                    };
                }
                let (state, reason, diagnostic_data) = match resolution {
                    ProviderResolution::NotFound => {
                        let diagnostics = runtime_discovery_diagnostics_for_backend(
                            self.diagnostics(),
                            &request.executor.backend,
                        );
                        (
                            "provider_missing",
                            provider_not_found_message(&request.executor.backend, &diagnostics),
                            ProviderRouteDiagnosticData::ProviderMissing {
                                backend: request.executor.backend.clone(),
                                runtime_discovery_diagnostics: diagnostics,
                            },
                        )
                    }
                    ProviderResolution::SelectorMismatch {
                        available_ids,
                        selector_hint,
                    } => (
                            "provider_selector_mismatch",
                            selector_mismatch_message(
                                &request.executor.backend,
                                request.executor.selector.as_deref(),
                            ),
                            ProviderRouteDiagnosticData::ProviderSelectorMismatch {
                                backend: request.executor.backend.clone(),
                                selector: request.executor.selector.clone(),
                                available_provider_ids: available_ids,
                                hint: selector_hint,
                            },
                        ),
                    ProviderResolution::AmbiguousExtensionAlias { candidate_ids } => (
                        "provider_ambiguous",
                        format!(
                            "multiple extension agent-task providers match backend '{}'; pass --selector with one provider id: {}",
                            request.executor.backend,
                            candidate_ids.join(", ")
                        ),
                        ProviderRouteDiagnosticData::ProviderAmbiguous {
                            backend: request.executor.backend.clone(),
                            available_provider_ids: candidate_ids,
                        },
                    ),
                    ProviderResolution::Resolved(_) => (
                        "provider_capability_unavailable",
                        "the route does not satisfy required provider capabilities".to_string(),
                        ProviderRouteDiagnosticData::ProviderCapabilityUnavailable {
                            layer: "provider".to_string(),
                            required_capabilities: request.executor.required_capabilities.clone(),
                        },
                    ),
                };
                return ProviderRouteReadiness {
                    ready: false,
                    state: state.to_string(),
                    reason,
                    reset_at: None,
                    classification: Some("capability".to_string()),
                    retryable: false,
                    remediation: None,
                    cache_identity: None,
                    provider_identity: None,
                    capacity_key: None,
                    diagnostic_data: Some(diagnostic_data),
                };
            }
            Err(reason) => {
                return ProviderRouteReadiness {
                    ready: false,
                    state: "provider_identity_invalid".to_string(),
                    reason,
                    reset_at: None,
                    classification: Some("identity".to_string()),
                    retryable: false,
                    remediation: None,
                    cache_identity: None,
                    provider_identity: None,
                    capacity_key: None,
                    diagnostic_data: None,
                }
            }
        };
        let credential_env =
            match super::secrets::provider_request_credential_env(&request, &provider) {
                Ok(env) => env,
                Err(error) => {
                    return ProviderRouteReadiness {
                        ready: false,
                        state: "provider_credentials_unavailable".to_string(),
                        reason: error.message,
                        reset_at: None,
                        classification: Some("auth_failure".to_string()),
                        retryable: false,
                        remediation: Some(
                            "repair the selected provider credential source".to_string(),
                        ),
                        cache_identity: None,
                        provider_identity: Some(provider.id.clone()),
                        capacity_key: None,
                        diagnostic_data: None,
                    };
                }
            };
        let request_capacity_key =
            match provider_capacity_key_with_credentials(&request, &provider, &credential_env) {
                Ok(key) => key,
                Err(error) => {
                    return ProviderRouteReadiness {
                        ready: false,
                        state: "provider_identity_invalid".to_string(),
                        reason: error.message,
                        reset_at: None,
                        classification: Some("identity".to_string()),
                        retryable: false,
                        remediation: None,
                        cache_identity: None,
                        provider_identity: Some(provider.id.clone()),
                        capacity_key: None,
                        diagnostic_data: None,
                    }
                }
            };
        let mut readiness_cache = match self.evidence.lock() {
            Ok(evidence) => evidence,
            Err(_) => {
                return ProviderRouteReadiness {
                    ready: false,
                    state: "provider_evidence_unavailable".to_string(),
                    reason: "provider evidence lock was poisoned".to_string(),
                    reset_at: None,
                    classification: Some("evidence".to_string()),
                    retryable: true,
                    remediation: None,
                    cache_identity: None,
                    provider_identity: Some(provider.id.clone()),
                    capacity_key: None,
                    diagnostic_data: None,
                }
            }
        }
        .readiness
        .clone();
        let catalog = AgentTaskProviderCatalog {
            providers: vec![provider.clone()],
            diagnostics: Vec::new(),
            version: None,
        };
        let evaluated = super::dispatchability::evaluate_request_dispatchability_with_credentials(
            &catalog,
            &request,
            provider.clone(),
            &credential_env,
            &mut readiness_cache,
        );
        let verdict = evaluated.dispatchability;
        let runtime_evidence = evaluated.runtime_evidence;
        let capacity_key = runtime_evidence
            .as_ref()
            .filter(|evidence| {
                evidence.cache_identity.is_some() || evidence.provider_identity.is_some()
            })
            .map(|evidence| reported_capacity_key(&request_capacity_key, evidence))
            .unwrap_or_else(|| request_capacity_key.clone());
        let now = chrono::Utc::now();
        let mut evidence = match self.evidence.lock() {
            Ok(evidence) => evidence,
            Err(_) => {
                return ProviderRouteReadiness {
                    ready: false,
                    state: "provider_evidence_unavailable".to_string(),
                    reason: "provider evidence lock was poisoned".to_string(),
                    reset_at: None,
                    classification: Some("evidence".to_string()),
                    retryable: true,
                    remediation: None,
                    cache_identity: None,
                    provider_identity: Some(provider.id.clone()),
                    capacity_key: None,
                    diagnostic_data: None,
                }
            }
        };
        if let Some(reset_at) = evidence.usage_caps.active(&capacity_key, now) {
            return ProviderRouteReadiness {
                ready: false,
                state: "usage_capped".to_string(),
                reason: "known provider usage cap is active".to_string(),
                reset_at: Some(reset_at),
                classification: Some("capacity".to_string()),
                retryable: true,
                remediation: None,
                cache_identity: Some(capacity_key.clone()),
                provider_identity: Some(provider.id.clone()),
                capacity_key: Some(capacity_key.clone()),
                diagnostic_data: None,
            };
        }

        if evidence
            .account_blocks
            .get(&capacity_key)
            .is_some_and(|block| block.expires_at > now)
        {
            return ProviderRouteReadiness {
                ready: false,
                state: "provider_account_blocked".to_string(),
                reason: "provider account was recently rejected for this exact credential/config identity".to_string(),
                reset_at: evidence.account_blocks.get(&capacity_key).map(|block| block.expires_at),
                classification: Some("account".to_string()),
                retryable: true,
                remediation: Some("switch provider account credentials or wait for the bounded account block to expire".to_string()),
                cache_identity: Some(capacity_key.clone()),
                provider_identity: Some(provider.id.clone()),
                capacity_key: Some(capacity_key.clone()),
                diagnostic_data: None,
            };
        }
        evidence.account_blocks.remove(&capacity_key);
        if verdict.ready {
            evidence
                .launch_credentials
                .entry(capacity_key.clone())
                .or_default()
                .push_back(BoundProviderCredentials {
                    env: credential_env,
                });
        }
        drop(evidence);
        ProviderRouteReadiness {
            ready: verdict.ready,
            state: verdict.state.to_string(),
            reason: verdict.checks.runtime.reason.unwrap_or(verdict.reason),
            reset_at: None,
            classification: runtime_evidence
                .as_ref()
                .map(|evidence| evidence.classification.clone()),
            retryable: runtime_evidence
                .as_ref()
                .is_some_and(|evidence| evidence.retryable),
            remediation: runtime_evidence
                .as_ref()
                .and_then(|evidence| evidence.remediation.clone()),
            cache_identity: runtime_evidence
                .as_ref()
                .and_then(|evidence| evidence.cache_identity.clone()),
            provider_identity: runtime_evidence
                .and_then(|evidence| evidence.provider_identity)
                .or_else(|| Some(provider.id)),
            capacity_key: Some(capacity_key),
            diagnostic_data: None,
        }
    }

    fn record_provider_outcome(
        &self,
        _request: &AgentTaskRequest,
        capacity_key: &str,
        outcome: &AgentTaskOutcome,
    ) {
        let Ok(mut evidence) = self.evidence.lock() else {
            return;
        };
        let capacity_key = capacity_key.to_string();
        if let Some(reset_at) = reset_at_from_outcome(outcome) {
            evidence.usage_caps.record(capacity_key.clone(), reset_at);
        }
        {
            if outcome.failure_classification
                == Some(AgentTaskFailureClassification::ProviderAccountBlocked)
            {
                let now = chrono::Utc::now();
                evidence
                    .account_blocks
                    .retain(|_, block| block.expires_at > now);
                evidence.account_blocks.insert(
                    capacity_key,
                    AccountBlockEvidence {
                        expires_at: now + PROVIDER_ACCOUNT_BLOCK_TTL,
                    },
                );
            } else if matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
            ) {
                evidence.account_blocks.remove(&capacity_key);
            }
        }
    }

    fn execute(
        &self,
        request: AgentTaskRequest,
        context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let request = request_with_effective_provider_contract(&request, self.providers());
        let materialized = match context.lifecycle_store.as_ref() {
            Some(store) => {
                materialize_executor_request_at_root(request, &context, store.artifact_root())
            }
            None => materialize_executor_request(request, &context),
        };
        let mut request = match materialized {
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
        let bound_credentials = self.evidence.lock().ok().and_then(|mut evidence| {
            let key = context.provider_capacity_key.as_deref()?;
            let bound = evidence.launch_credentials.get_mut(key)?.pop_front();
            if evidence
                .launch_credentials
                .get(key)
                .is_some_and(VecDeque::is_empty)
            {
                evidence.launch_credentials.remove(key);
            }
            bound
        });
        let launch_credentials = bound_credentials.map(|bound| bound.env).or_else(|| {
            effective_provider_for_request(&request.request, self.providers())
                .ok()
                .flatten()
                .and_then(|provider| {
                    super::secrets::provider_request_credential_env(&request.request, &provider)
                        .ok()
                })
        });
        if let Some(outcome) = production_execution_preflight(
            &request.request,
            self.providers(),
            self.diagnostics(),
            &self.evidence,
            launch_credentials.as_deref(),
        ) {
            return outcome;
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
        let provider = match effective_provider_for_request(&request.request, self.providers()) {
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

        run_materialized_provider_command_with_credentials(
            &request,
            &provider,
            &context,
            launch_credentials.as_deref(),
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
    let finalized_root = root.join("executor-finalized");
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
    let artifacts_root_identity = crate::agent_task_provider::artifact_finalization::ExecutorArtifactRootIdentity::capture_with_finalized_root(
        &path,
        finalized_root,
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
        artifact_store_root: root,
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

    fn readiness_request(model: &str) -> AgentTaskRequest {
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "readiness-route".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: Some(model.to_string()),
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
        }
    }

    fn readiness_executor() -> ExtensionProviderAgentTaskExecutor {
        ExtensionProviderAgentTaskExecutor::with_providers(vec![serde_json::from_value(
            json!({ "id": "test.provider", "backend": "test" }),
        )
        .expect("provider")])
    }

    #[test]
    fn provider_capacity_evidence_is_shared_across_plans_and_scoped_to_the_model_identity() {
        let executor = readiness_executor();
        let blocked = readiness_request("blocked-model");
        let mut blocked_outcome = AgentTaskOutcome {
            task_id: blocked.task_id.clone(),
            status: AgentTaskOutcomeStatus::ProviderError,
            failure_classification: Some(AgentTaskFailureClassification::ProviderAccountBlocked),
            ..Default::default()
        };
        let capacity_key = executor.provider_route_capacity_key(&blocked);
        executor.record_provider_outcome(&blocked, &capacity_key, &blocked_outcome);

        assert_eq!(
            executor.provider_route_readiness(&blocked).state,
            "provider_account_blocked"
        );
        assert!(
            executor
                .provider_route_readiness(&readiness_request("changed-model"))
                .ready,
            "a changed model/config identity must not inherit an account block"
        );

        let capped = readiness_request("capped-model");
        let reset_at = chrono::Utc::now() + chrono::Duration::hours(1);
        blocked_outcome.failure_classification = Some(AgentTaskFailureClassification::RateLimited);
        blocked_outcome.diagnostics = vec![AgentTaskDiagnostic {
            class: AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS.to_string(),
            message: "usage cap".to_string(),
            data: json!({ "reset_at": reset_at.to_rfc3339() }),
        }];
        let capacity_key = executor.provider_route_capacity_key(&capped);
        executor.record_provider_outcome(&capped, &capacity_key, &blocked_outcome);

        let separately_constructed_for_later_plan = readiness_executor();
        let readiness = separately_constructed_for_later_plan.provider_route_readiness(&capped);
        assert_eq!(readiness.state, "usage_capped");
        assert_eq!(readiness.reset_at, Some(reset_at));

        let provider = effective_provider_for_request(&blocked, executor.providers())
            .expect("provider resolution")
            .expect("provider");
        let key = provider_capacity_key(&blocked, &provider).expect("capacity key");
        executor
            .evidence
            .lock()
            .expect("evidence")
            .account_blocks
            .get_mut(&key)
            .expect("account block")
            .expires_at = chrono::Utc::now() - chrono::Duration::seconds(1);
        assert!(
            readiness_executor()
                .provider_route_readiness(&blocked)
                .ready,
            "expired negative evidence must re-admit a recovered account"
        );
    }

    #[test]
    fn account_block_registry_preserves_more_than_sixty_four_active_bindings() {
        let executor = readiness_executor();
        for index in 0..65 {
            let request = readiness_request(&format!("model-{index}"));
            let key = executor.provider_route_capacity_key(&request);
            executor.record_provider_outcome(
                &request,
                &key,
                &AgentTaskOutcome {
                    task_id: request.task_id.clone(),
                    status: AgentTaskOutcomeStatus::ProviderError,
                    failure_classification: Some(
                        AgentTaskFailureClassification::ProviderAccountBlocked,
                    ),
                    ..Default::default()
                },
            );
        }

        assert_eq!(
            executor
                .evidence
                .lock()
                .expect("provider evidence")
                .account_blocks
                .len(),
            65
        );
    }

    #[test]
    fn provider_capacity_identity_changes_when_the_reported_account_credential_changes() {
        let root = tempfile::tempdir().expect("tempdir");
        let credential = root.path().join("account.json");
        std::fs::write(&credential, r#"{"token":"account-one"}"#).expect("first credential");
        let provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "test.provider",
            "backend": "test",
            "provider_defaults": {
                "conditional-account": {
                    "secret_env": ["TEST_ACCOUNT_TOKEN"],
                    "secret_env_sources": {
                        "TEST_ACCOUNT_TOKEN": {
                            "source": "json-file",
                            "path": credential,
                            "field": "token"
                        }
                    }
                }
            }
        }))
        .expect("provider");
        let mut request = readiness_request("model");
        request.executor.config = json!({"provider":"conditional-account"});
        let first = provider_capacity_key(&request, &provider).expect("first capacity key");
        std::fs::write(&credential, r#"{"token":"account-two"}"#).expect("rotated credential");
        let second = provider_capacity_key(&request, &provider).expect("second capacity key");
        assert_ne!(first, second);
        assert!(!first.contains("account-one"));
        assert!(!second.contains("account-two"));
    }

    #[test]
    fn provider_reported_account_switch_does_not_inherit_capacity_evidence() {
        let root = tempfile::tempdir().expect("tempdir");
        let account = root.path().join("account");
        let script = root.path().join("readiness.js");
        std::fs::write(&account, "account-a").expect("account A");
        std::fs::write(
            &script,
            "const fs=require('fs');const account=fs.readFileSync(process.argv[2],'utf8').trim();process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:true,classification:'ready',retryable:false,remediation:'',reason:'',cache_key:'capacity-'+account,identity:{account}}));",
        )
        .expect("readiness script");
        let mut provider: AgentTaskExecutorProvider = serde_json::from_value(json!({
            "id": "test.provider",
            "backend": "test"
        }))
        .expect("provider");
        provider.readiness_invocation = Some(homeboy_core::command_invocation::CommandInvocation {
            argv: vec![
                "node".to_string(),
                script.display().to_string(),
                account.display().to_string(),
            ],
            ..Default::default()
        });
        let executor = ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]);
        let request = readiness_request("model");
        let readiness = executor.provider_route_readiness(&request);
        assert!(readiness.ready);
        let capacity_key = readiness.capacity_key.expect("reported capacity binding");

        let reset_at = chrono::Utc::now() + chrono::Duration::hours(1);
        executor.record_provider_outcome(
            &request,
            &capacity_key,
            &AgentTaskOutcome {
                task_id: request.task_id.clone(),
                status: AgentTaskOutcomeStatus::ProviderError,
                failure_classification: Some(AgentTaskFailureClassification::RateLimited),
                diagnostics: vec![AgentTaskDiagnostic {
                    class: AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS.to_string(),
                    message: "usage cap".to_string(),
                    data: json!({"reset_at": reset_at.to_rfc3339()}),
                }],
                ..Default::default()
            },
        );
        assert_eq!(
            executor.provider_route_readiness(&request).state,
            "usage_capped"
        );

        std::fs::write(&account, "account-b").expect("account B");
        executor
            .evidence
            .lock()
            .expect("evidence")
            .readiness
            .expire_all();
        assert!(
            executor.provider_route_readiness(&request).ready,
            "provider-reported account B must not inherit account A's cap"
        );

        std::fs::write(&account, "account-a").expect("restore account A");
        executor
            .evidence
            .lock()
            .expect("evidence")
            .readiness
            .expire_all();
        assert_eq!(
            executor.provider_route_readiness(&request).state,
            "usage_capped"
        );
    }

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
            lifecycle_store: None,
            provider_capacity_key: None,
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
