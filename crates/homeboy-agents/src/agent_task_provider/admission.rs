//! Mutation-free provider admission shared by placement and execution.

use sha2::{Digest, Sha256};

use super::{resolve_provider_for_backend, AgentTaskExecutorProvider, ProviderResolution};

pub const AGENT_TASK_PROVIDER_ADMISSION_PLAN_SCHEMA: &str =
    "homeboy/agent-task-provider-admission-plan/v1";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderAdmissionRequest {
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_identity: Option<homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskProviderAdmissionAction {
    ObserveProviderCatalog,
    SyncProviderExtension {
        provider_id: String,
    },
    MaterializeRuntime {
        provider_id: String,
        source_revision: String,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderAdmissionPredicate {
    pub id: String,
    pub satisfied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A serializable, mutation-free provider admission decision.
///
/// The plan deliberately contains repairs as data. Placement preflight can
/// report them without changing a runner; execution chooses when to queue or
/// perform its existing materialization/session work and then revalidates.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderAdmissionPlan {
    pub schema: String,
    pub request: AgentTaskProviderAdmissionRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_extension_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_runtime_sources: Vec<String>,
    pub predicates: Vec<AgentTaskProviderAdmissionPredicate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<AgentTaskProviderAdmissionAction>,
    pub hash: String,
}

impl AgentTaskProviderAdmissionPlan {
    /// Public placement cannot infer a remote catalog from the controller.
    pub fn compile_unobserved(request: AgentTaskProviderAdmissionRequest) -> Self {
        let mut plan = Self {
            schema: AGENT_TASK_PROVIDER_ADMISSION_PLAN_SCHEMA.to_string(),
            request,
            resolved_provider_id: None,
            required_extension_ids: Vec::new(),
            required_runtime_sources: Vec::new(),
            predicates: vec![AgentTaskProviderAdmissionPredicate {
                id: "runner_provider_catalog_observed".to_string(),
                satisfied: false,
                detail: Some("runner provider catalog has not been observed".to_string()),
            }],
            actions: vec![AgentTaskProviderAdmissionAction::ObserveProviderCatalog],
            hash: String::new(),
        };
        plan.hash = plan.digest();
        plan
    }

    pub fn compile(
        request: AgentTaskProviderAdmissionRequest,
        providers: &[AgentTaskExecutorProvider],
    ) -> Self {
        // Catalog ordering is transport-dependent and cannot affect admission.
        let mut providers = providers.to_vec();
        providers.sort_by(|left, right| left.id.cmp(&right.id));
        let resolution =
            resolve_provider_for_backend(&providers, &request.backend, request.selector.as_deref());
        let resolved = match resolution {
            ProviderResolution::Resolved(provider) => Some(provider),
            _ => None,
        };
        let mut predicates = vec![predicate_for_resolution(&request, &providers, &resolution)];
        let mut actions = Vec::new();
        let mut required_extension_ids = Vec::new();
        let mut required_runtime_sources = Vec::new();
        let resolved_provider_id = resolved.map(|provider| provider.id.clone());

        if resolved.is_none() {
            actions.push(AgentTaskProviderAdmissionAction::SyncProviderExtension {
                provider_id: request
                    .selector
                    .clone()
                    .unwrap_or_else(|| request.backend.clone()),
            });
        }

        if let Some(identity) = request.runtime_identity.as_ref() {
            let provider_matches_pin =
                resolved_provider_id.as_deref() == Some(identity.provider_id.as_str());
            predicates.push(AgentTaskProviderAdmissionPredicate {
                id: "pinned_provider".to_string(),
                satisfied: provider_matches_pin,
                detail: (!provider_matches_pin).then(|| {
                    format!(
                        "controller pinned provider `{}`, observed `{}`",
                        identity.provider_id,
                        resolved_provider_id.as_deref().unwrap_or("missing")
                    )
                }),
            });
            predicates.push(AgentTaskProviderAdmissionPredicate {
                id: "pinned_runtime_revision".to_string(),
                satisfied: false,
                detail: Some(format!(
                    "controller requires `{}`, observed `missing`",
                    identity.source_revision
                )),
            });
            required_runtime_sources.push(identity.source_revision.clone());
            actions.push(AgentTaskProviderAdmissionAction::MaterializeRuntime {
                provider_id: identity.provider_id.clone(),
                source_revision: identity.source_revision.clone(),
            });
        }

        required_extension_ids.sort();
        required_extension_ids.dedup();
        required_runtime_sources.sort();
        required_runtime_sources.dedup();
        predicates.sort_by(|left, right| left.id.cmp(&right.id));
        actions.sort_by_key(|action| {
            serde_json::to_string(action).expect("admission action serializes")
        });
        let mut plan = Self {
            schema: AGENT_TASK_PROVIDER_ADMISSION_PLAN_SCHEMA.to_string(),
            request,
            resolved_provider_id,
            required_extension_ids,
            required_runtime_sources,
            predicates,
            actions,
            hash: String::new(),
        };
        plan.hash = plan.digest();
        plan
    }

    /// Recompile from the immutable request and reject catalog drift from the
    /// plan the caller previously inspected.
    pub fn revalidate(&self, providers: &[AgentTaskExecutorProvider]) -> Self {
        Self::compile(self.request.clone(), providers)
    }

    pub fn is_ready(&self) -> bool {
        self.predicates.iter().all(|predicate| predicate.satisfied)
    }

    fn digest(&self) -> String {
        let mut copy = self.clone();
        copy.hash.clear();
        let bytes = serde_json::to_vec(&copy).expect("provider admission plan is serializable");
        format!("sha256:{:x}", Sha256::digest(bytes))
    }
}

fn predicate_for_resolution(
    request: &AgentTaskProviderAdmissionRequest,
    providers: &[AgentTaskExecutorProvider],
    resolution: &ProviderResolution<'_>,
) -> AgentTaskProviderAdmissionPredicate {
    let detail = match resolution {
        ProviderResolution::Resolved(_) => None,
        ProviderResolution::NotFound => Some(format!(
            "no provider for backend `{}` in catalog [{}]",
            request.backend,
            providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        ProviderResolution::SelectorMismatch { available_ids, .. } => Some(format!(
            "selector `{}` does not match backend `{}` providers: {}",
            request.selector.as_deref().unwrap_or("<default>"),
            request.backend,
            available_ids.join(", ")
        )),
    };
    AgentTaskProviderAdmissionPredicate {
        id: "provider_catalog_compatible".to_string(),
        satisfied: matches!(resolution, ProviderResolution::Resolved(_)),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, backend: &str) -> AgentTaskExecutorProvider {
        let mut provider =
            super::super::discovery::discover_agent_task_executor_providers().remove(0);
        provider.id = id.into();
        provider.backend = backend.into();
        provider
    }

    #[test]
    fn plan_serializes_hashes_and_revalidates_catalog_drift() {
        let request = AgentTaskProviderAdmissionRequest {
            backend: "alpha".to_string(),
            selector: Some("alpha.a".to_string()),
            model: Some("model".to_string()),
            runtime_identity: None,
        };
        let plan =
            AgentTaskProviderAdmissionPlan::compile(request, &[provider("alpha.a", "alpha")]);
        let encoded = serde_json::to_string(&plan).expect("serialize");
        let decoded: AgentTaskProviderAdmissionPlan =
            serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(plan.hash, decoded.hash);
        let drifted = decoded.revalidate(&[provider("alpha.b", "alpha")]);
        assert_ne!(decoded.hash, drifted.hash);
        assert!(!drifted.is_ready());
    }

    #[test]
    fn plan_distinguishes_unavailable_selector_extension_and_pinned_runtime() {
        let unavailable = AgentTaskProviderAdmissionPlan::compile(
            AgentTaskProviderAdmissionRequest {
                backend: "missing".to_string(),
                selector: None,
                model: None,
                runtime_identity: None,
            },
            &[],
        );
        assert!(!unavailable.is_ready());
        assert!(matches!(
            unavailable.actions[0],
            AgentTaskProviderAdmissionAction::SyncProviderExtension { .. }
        ));
        let mismatch = AgentTaskProviderAdmissionPlan::compile(
            AgentTaskProviderAdmissionRequest {
                backend: "alpha".to_string(),
                selector: Some("wrong".to_string()),
                model: None,
                runtime_identity: None,
            },
            &[provider("alpha.a", "alpha")],
        );
        assert!(!mismatch.is_ready());
        let pinned = homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity {
            runtime_id: "runtime".to_string(),
            provider_id: "alpha.a".to_string(),
            source_selector: "extension:alpha".to_string(),
            source_revision: "0123456789abcdef0123456789abcdef01234567".to_string(),
            freshness: homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeFreshness::Pinned,
            provider: serde_json::json!({"id":"alpha.a","backend":"alpha"}),
            materialization_plan: serde_json::Value::Null,
        };
        let drift = AgentTaskProviderAdmissionPlan::compile(
            AgentTaskProviderAdmissionRequest {
                backend: "alpha".to_string(),
                selector: Some("alpha.a".to_string()),
                model: None,
                runtime_identity: Some(pinned),
            },
            &[provider("alpha.a", "alpha")],
        );
        assert!(!drift.is_ready());
        let revision = drift
            .predicates
            .iter()
            .find(|p| p.id == "pinned_runtime_revision")
            .unwrap();
        assert!(!revision.satisfied);
        assert_eq!(revision.detail.as_deref(), Some("controller requires `0123456789abcdef0123456789abcdef01234567`, observed `missing`"));
        assert!(drift.actions.iter().any(|action| matches!(
            action,
            AgentTaskProviderAdmissionAction::MaterializeRuntime { .. }
        )));
    }

    #[test]
    fn ready_catalog_is_deterministically_revalidated() {
        let request = AgentTaskProviderAdmissionRequest {
            backend: "alpha".to_string(),
            selector: Some("alpha.a".to_string()),
            model: Some("model-a".to_string()),
            runtime_identity: None,
        };
        let planned = AgentTaskProviderAdmissionPlan::compile_unobserved(request);
        let ready = planned.revalidate(&[provider("alpha.a", "alpha")]);
        assert!(ready.is_ready());
        assert!(!ready
            .predicates
            .iter()
            .any(|p| p.id == "pinned_runtime_revision"));
        assert_eq!(ready, ready.revalidate(&[provider("alpha.a", "alpha")]));
        assert_ne!(planned.hash, ready.hash);
    }

    #[test]
    fn catalog_permutations_preserve_admission_identity() {
        let first = provider("alpha.a", "alpha");
        let second = provider("alpha.b", "alpha");
        for request in [
            AgentTaskProviderAdmissionRequest {
                backend: "alpha".to_string(),
                selector: None,
                model: None,
                runtime_identity: None,
            },
            AgentTaskProviderAdmissionRequest {
                backend: "missing".to_string(),
                selector: None,
                model: None,
                runtime_identity: None,
            },
            AgentTaskProviderAdmissionRequest {
                backend: "alpha".to_string(),
                selector: Some("wrong".to_string()),
                model: None,
                runtime_identity: None,
            },
            AgentTaskProviderAdmissionRequest {
                backend: "extension".to_string(),
                selector: None,
                model: None,
                runtime_identity: None,
            },
        ] {
            let forward = AgentTaskProviderAdmissionPlan::compile(
                request.clone(),
                &[first.clone(), second.clone()],
            );
            let reverse =
                AgentTaskProviderAdmissionPlan::compile(request, &[second.clone(), first.clone()]);
            assert_eq!(forward.hash, reverse.hash);
            assert_eq!(forward.actions, reverse.actions);
            assert_eq!(forward.predicates, reverse.predicates);
        }
    }
}
