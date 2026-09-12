//! Provider credential readiness: is a discovered provider actually
//! *dispatchable*, or merely *declared*?
//!
//! `agent-task providers` used to report `status: "available"` for every
//! discovered provider, because availability was derived from discovery alone —
//! a provider that parsed was "available". That made a catalog claim that
//! Homeboy could not honor: a backend whose required credential was absent from
//! the environment still advertised itself, a Cook dispatched to it, and the
//! credential gap was only discovered *inside* the provider, after a workspace
//! had been materialized and one provider execution had been spent against the
//! task's budget (#11479).
//!
//! Runtimes already declare their own credential requirements. This module
//! reads those declarations and answers one question per provider: are the
//! credentials this provider said it requires resolvable *here*, on this
//! machine, right now?
//!
//! ## What counts as an unconditional requirement
//!
//! Only declarations that apply regardless of the dispatch request are read
//! here, because catalog status and pre-dispatch preflight both run before a
//! request exists:
//!
//! - `runner_readiness[].secret_env`
//! - `secret_requirements[]` that are not explicitly `required: false`
//! - `secret_env_requirements[]` with no `when` condition
//!
//! Account-scoped `provider_defaults` stay request-scoped. Catalog status does
//! not treat a sole unused alternative as unconditionally required. Native
//! provider-owned auth is not a Homeboy-invented credential list.
//!
//! Request-conditional requirements (`secret_env_requirements[].when`, and the
//! `secret_env` of a provider default named by the request) stay owned by the
//! existing plan-level `preflight_dispatch_provider_secrets`, which runs once a
//! request exists and can evaluate the condition. The two preflights are
//! complementary: this one answers "can this backend run at all", the plan-level
//! one answers "can this backend run *this* request".
//!
//! A provider that declares nothing required stays `available`. Silence is not
//! evidence of a missing credential, so this never invents a requirement.

use super::secrets::provider_declared_secret_sources;
use super::*;

pub const AGENT_TASK_PROVIDER_CREDENTIAL_READINESS_SCHEMA: &str =
    "homeboy/agent-task-provider-credential-readiness/v1";

/// One credential a provider declared it requires, plus whether it resolves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderCredentialRequirement {
    /// Secret-env name, e.g. `AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN`.
    pub env: String,
    /// Which provider declaration produced this requirement.
    pub declared_by: String,
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

/// Whether a provider's declared credentials resolve in the observed scope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderCredentialReadiness {
    pub schema: String,
    pub provider_id: String,
    pub backend: String,
    /// False only when a declared, unconditionally-required credential is
    /// missing. A provider that declares nothing stays dispatchable.
    pub dispatchable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirements: Vec<AgentTaskProviderCredentialRequirement>,
    /// The missing credential env names, in declaration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

impl AgentTaskProviderCredentialReadiness {
    /// Operator-facing reason a provider is not dispatchable, naming the exact
    /// credential(s). `None` when the provider is dispatchable.
    pub fn reason(&self) -> Option<String> {
        if self.dispatchable {
            return None;
        }
        Some(format!("missing credential {}", self.missing.join(", ")))
    }

    /// Remediation lines declared alongside the missing credentials, plus the
    /// generic Homeboy remediation. Deduplicated, order-preserving.
    pub fn remediation(&self) -> Vec<String> {
        let mut hints = Vec::new();
        for requirement in &self.requirements {
            if requirement.configured {
                continue;
            }
            if let Some(remediation) = requirement
                .remediation
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if !hints.iter().any(|hint| hint == remediation) {
                    hints.push(remediation.to_string());
                }
            }
        }
        if !self.missing.is_empty() {
            hints.push(format!(
                "Configure {} with `homeboy agent-task auth`, or inspect redacted readiness with `homeboy agent-task providers --backend {} --secret-env {}`.",
                self.missing.join(", "),
                self.backend,
                self.missing.join(" --secret-env "),
            ));
        }
        hints
    }
}

/// A credential requirement before resolution has been attempted.
struct DeclaredCredential {
    env: String,
    declared_by: String,
    purpose: Option<String>,
    remediation: Option<String>,
}

/// Record a declared credential once, keeping the first declaration's context.
fn push_declared_credential(
    declared: &mut Vec<DeclaredCredential>,
    env: &str,
    declared_by: &str,
    purpose: Option<String>,
    remediation: Option<String>,
) {
    let env = env.trim();
    if env.is_empty() || declared.iter().any(|entry| entry.env == env) {
        return;
    }
    declared.push(DeclaredCredential {
        env: env.to_string(),
        declared_by: declared_by.to_string(),
        purpose,
        remediation,
    });
}

/// Env names this provider unconditionally requires, in declaration order.
pub fn provider_required_secret_env_names(provider: &AgentTaskExecutorProvider) -> Vec<String> {
    declared_credentials(provider)
        .into_iter()
        .map(|entry| entry.env)
        .collect()
}

/// Every unconditionally-required credential a provider declares.
///
/// Order is declaration order, deduplicated on the env name so a credential
/// declared twice reports once (first declaration wins its purpose/remediation).
fn declared_credentials(provider: &AgentTaskExecutorProvider) -> Vec<DeclaredCredential> {
    let mut declared: Vec<DeclaredCredential> = Vec::new();

    for readiness in &provider.runner_readiness {
        for env in &readiness.secret_env {
            push_declared_credential(
                &mut declared,
                env,
                &format!("runner_readiness.{}", readiness.id),
                Some(readiness.label.clone()),
                readiness.remediation.clone(),
            );
        }
    }

    for requirement in &provider.secret_requirements {
        // `required: false` is an explicit opt-out; absent means required.
        if requirement.required == Some(false) {
            continue;
        }
        let names = requirement
            .name
            .iter()
            .chain(requirement.env.iter())
            .cloned()
            .collect::<Vec<_>>();
        for env in names {
            push_declared_credential(
                &mut declared,
                &env,
                "secret_requirements",
                requirement.purpose.clone(),
                None,
            );
        }
    }

    for requirement in &provider.secret_env_requirements {
        // A `when` condition is request-scoped; the plan-level preflight owns it.
        if requirement.when.is_some() {
            continue;
        }
        for env in &requirement.env {
            push_declared_credential(
                &mut declared,
                env,
                "secret_env_requirements",
                requirement.source.clone(),
                None,
            );
        }
    }

    declared
}

/// Resolve a provider's declared credentials against the observed scope.
pub fn provider_credential_readiness(
    provider: &AgentTaskExecutorProvider,
) -> AgentTaskProviderCredentialReadiness {
    let declared = declared_credentials(provider);
    if declared.is_empty() {
        // Nothing declared, nothing to resolve — and no reason to touch the
        // secret store just to say so.
        return AgentTaskProviderCredentialReadiness {
            schema: AGENT_TASK_PROVIDER_CREDENTIAL_READINESS_SCHEMA.to_string(),
            provider_id: provider.id.clone(),
            backend: provider.backend.clone(),
            dispatchable: true,
            requirements: Vec::new(),
            missing: Vec::new(),
        };
    }
    let names = declared
        .iter()
        .map(|entry| entry.env.clone())
        .collect::<Vec<_>>();
    let status =
        secret_env_status_with_fallbacks(&names, &provider_declared_secret_sources(provider));

    let requirements = declared
        .into_iter()
        .map(|entry| {
            let configured = status
                .iter()
                .find(|status| status.name == entry.env)
                .map(|status| status.configured)
                // No status row can only happen if the status helper drops a
                // name. Treat that as unknown-but-present rather than inventing
                // a blocking failure out of a bookkeeping gap.
                .unwrap_or(true);
            AgentTaskProviderCredentialRequirement {
                env: entry.env,
                declared_by: entry.declared_by,
                configured,
                purpose: entry.purpose,
                remediation: entry.remediation,
            }
        })
        .collect::<Vec<_>>();

    let missing = requirements
        .iter()
        .filter(|requirement| !requirement.configured)
        .map(|requirement| requirement.env.clone())
        .collect::<Vec<_>>();

    AgentTaskProviderCredentialReadiness {
        schema: AGENT_TASK_PROVIDER_CREDENTIAL_READINESS_SCHEMA.to_string(),
        provider_id: provider.id.clone(),
        backend: provider.backend.clone(),
        dispatchable: missing.is_empty(),
        requirements,
        missing,
    }
}

/// Fail a provider whose declared credentials are absent, before any workspace
/// is materialized and before any provider execution is spent.
pub fn preflight_provider_credentials(
    provider: &AgentTaskExecutorProvider,
) -> homeboy_core::Result<()> {
    let readiness = provider_credential_readiness(provider);
    if readiness.dispatchable {
        return Ok(());
    }
    Err(credential_preflight_error(&readiness))
}

/// Preflight the credentials of the provider a backend/selector resolves to.
///
/// Unresolvable backends are *not* an error here: `NotFound`, ambiguous alias,
/// and selector mismatch are already owned by
/// `validate_provider_runner_readiness_for_backend`, and duplicating them would
/// change which error an operator sees for an unrelated problem.
pub fn preflight_provider_credentials_for_backend(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> homeboy_core::Result<()> {
    match resolve_provider_for_backend(providers, backend, selector) {
        ProviderResolution::Resolved(provider) => preflight_provider_credentials(provider),
        _ => Ok(()),
    }
}

/// Preflight credentials for a backend against the discovered catalog.
pub fn preflight_discovered_provider_credentials_for_backend(
    backend: &str,
    selector: Option<&str>,
) -> homeboy_core::Result<()> {
    let catalog = AgentTaskProviderCatalog::discover();
    preflight_provider_credentials_for_backend(catalog.providers(), backend, selector)
}

/// The configuration failure a missing declared credential produces.
///
/// This is deliberately a pre-dispatch validation error rather than a provider
/// outcome: a credential gap is a configuration problem, so it must not be
/// charged to the task's provider-execution budget (#11479).
fn credential_preflight_error(readiness: &AgentTaskProviderCredentialReadiness) -> Error {
    let mut hints = vec![serde_json::json!({
        "kind": "provider_credential_preflight_failed",
        "failure_classification": "configuration",
        "schema": AGENT_TASK_PROVIDER_CREDENTIAL_READINESS_SCHEMA,
        "provider_id": readiness.provider_id,
        "backend": readiness.backend,
        "missing_credentials": readiness.missing,
        "declared_by": readiness
            .requirements
            .iter()
            .filter(|requirement| !requirement.configured)
            .map(|requirement| requirement.declared_by.clone())
            .collect::<Vec<_>>(),
    })
    .to_string()];
    hints.extend(readiness.remediation());

    Error::validation_invalid_argument(
        "provider_credentials",
        format!(
            "agent-task backend '{}' is declared but not dispatchable: provider '{}' requires credential(s) {} which are not configured here",
            readiness.backend,
            readiness.provider_id,
            readiness.missing.join(", ")
        ),
        Some(readiness.backend.clone()),
        Some(hints),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(value: Value) -> AgentTaskExecutorProvider {
        serde_json::from_value(value).expect("valid provider fixture")
    }

    fn unused_account_default_provider() -> AgentTaskExecutorProvider {
        provider(serde_json::json!({
            "id": "sample-runtime.agent-task-executor",
            "backend": "sample-runtime",
            "capabilities": ["cli_runtime", "provider_owned_auth"],
            "secret_env_requirements": [{
                "source": "provider_default",
                "env": ["UNUSED_ACCOUNT_TOKEN"],
                "when": { "any": [{ "path": "executor.config.provider", "equals": "unused-account" }] }
            }],
            "provider_defaults": {
                "unused-account": {
                    "secret_env": [
                        "UNUSED_ACCOUNT_TOKEN",
                        "UNUSED_ACCOUNT_ACCESS_TOKEN",
                        "UNUSED_ACCOUNT_EXPIRES_AT"
                    ],
                    "required_secret_env": ["UNUSED_ACCOUNT_TOKEN"],
                    "optional_secret_env": [
                        "UNUSED_ACCOUNT_ACCESS_TOKEN",
                        "UNUSED_ACCOUNT_EXPIRES_AT"
                    ]
                }
            }
        }))
    }

    fn unconditional_credential_provider(
        env: &str,
        auth_path: Option<&std::path::Path>,
    ) -> AgentTaskExecutorProvider {
        let secret_env_sources = auth_path.map_or_else(
            || serde_json::json!({}),
            |path| {
                serde_json::json!({
                    env: { "source": "json-file", "path": path, "field": "token" }
                })
            },
        );
        provider(serde_json::json!({
            "id": "sample-runtime.agent-task-executor",
            "backend": "sample-runtime",
            "secret_env_requirements": [{
                "source": "provider_default",
                "env": [env],
                "secret_env_sources": secret_env_sources
            }]
        }))
    }

    #[test]
    fn a_provider_with_no_declared_credentials_stays_dispatchable() {
        let readiness = provider_credential_readiness(&provider(serde_json::json!({
            "id": "local-shell.agent-task-executor",
            "backend": "local-shell"
        })));

        assert!(
            readiness.dispatchable,
            "silence is not evidence of a missing credential"
        );
        assert!(readiness.missing.is_empty());
        assert!(readiness.reason().is_none());
    }

    #[test]
    fn an_unselected_account_default_is_not_a_catalog_credential() {
        let readiness = provider_credential_readiness(&unused_account_default_provider());

        assert!(
            readiness.dispatchable,
            "an unused account alternative is request-scoped, not catalog-required"
        );
        assert!(readiness.missing.is_empty());
        assert!(readiness.requirements.is_empty());
        assert!(readiness.reason().is_none());
        assert!(
            !readiness
                .missing
                .iter()
                .any(|env| env.contains("ACCESS_TOKEN") || env.contains("EXPIRES_AT")),
            "account-default companions must not block catalog dispatch: {:?}",
            readiness.missing
        );
    }

    #[test]
    fn a_configured_unconditional_credential_makes_the_provider_dispatchable() {
        let required = format!("HOMEBOY_TEST_CREDENTIAL_{}", uuid::Uuid::new_v4());
        let auth = tempfile::NamedTempFile::new().expect("auth file");
        std::fs::write(auth.path(), r#"{"token":"refresh-token-value"}"#).expect("write auth");
        let provider = unconditional_credential_provider(&required, Some(auth.path()));
        let readiness = provider_credential_readiness(&provider);

        assert!(
            readiness.dispatchable,
            "a configured credential must clear the preflight: {:?}",
            readiness.missing
        );
        preflight_provider_credentials(&provider).expect("configured credential dispatches");
    }

    #[test]
    fn a_request_conditional_requirement_alone_does_not_block_dispatch() {
        // Only a `when`-gated declaration: the plan-level preflight owns it
        // once a request exists, so catalog status must not pre-judge it.
        let readiness = provider_credential_readiness(&provider(serde_json::json!({
            "id": "sample-runtime.agent-task-executor",
            "backend": "sample-runtime",
            "secret_env_requirements": [{
                "source": "provider_default",
                "env": ["SELECTED_ACCOUNT_TOKEN"],
                "when": { "any": [{ "path": "executor.config.provider", "equals": "selected-account" }] }
            }]
        })));

        assert!(readiness.dispatchable);
        assert!(readiness.requirements.is_empty());
    }

    #[test]
    fn an_unconditional_secret_env_requirement_blocks_dispatch() {
        let required = format!("HOMEBOY_TEST_CREDENTIAL_{}", uuid::Uuid::new_v4());
        let readiness = provider_credential_readiness(&provider(serde_json::json!({
            "id": "sample-runtime.agent-task-executor",
            "backend": "sample-runtime",
            "secret_env_requirements": [{
                "source": "provider_default",
                "env": [required.clone()]
            }]
        })));

        assert!(!readiness.dispatchable);
        assert_eq!(readiness.missing, vec![required]);
    }

    #[test]
    fn an_explicitly_optional_secret_requirement_does_not_block_dispatch() {
        let readiness = provider_credential_readiness(&provider(serde_json::json!({
            "id": "sample-runtime.agent-task-executor",
            "backend": "sample-runtime",
            "secret_requirements": [{
                "name": "SAMPLE_OPTIONAL_KEY",
                "required": false
            }]
        })));

        assert!(readiness.dispatchable);
        assert!(readiness.requirements.is_empty());
    }

    #[test]
    fn multiple_provider_defaults_stay_request_scoped() {
        // Which default runs is a request decision when more than one is
        // declared, so nothing here is unconditionally required.
        let readiness = provider_credential_readiness(&provider(serde_json::json!({
            "id": "sample-runtime.agent-task-executor",
            "backend": "sample-runtime",
            "provider_defaults": {
                "first-account": { "required_secret_env": ["FIRST_ACCOUNT_TOKEN"] },
                "second-account": { "required_secret_env": ["SECOND_ACCOUNT_TOKEN"] }
            }
        })));

        assert!(readiness.dispatchable);
        assert!(readiness.missing.is_empty());
    }

    #[test]
    fn the_preflight_error_is_a_configuration_failure_naming_the_credential() {
        let required = format!("HOMEBOY_TEST_CREDENTIAL_{}", uuid::Uuid::new_v4());
        let provider = unconditional_credential_provider(&required, None);
        let error = preflight_provider_credentials(&provider)
            .expect_err("a missing required credential must fail fast");

        assert_eq!(error.details["field"], "provider_credentials");
        assert!(error.message.contains(&required), "{}", error.message);
        let structured = error.details["tried"][0]
            .as_str()
            .expect("structured preflight hint");
        assert!(structured.contains("provider_credential_preflight_failed"));
        assert!(
            structured.contains("\"failure_classification\":\"configuration\""),
            "a credential gap is configuration, not a spent provider execution: {structured}"
        );
    }

    #[test]
    fn backend_preflight_ignores_backends_that_do_not_resolve() {
        let required = format!("HOMEBOY_TEST_CREDENTIAL_{}", uuid::Uuid::new_v4());
        let providers = vec![unconditional_credential_provider(&required, None)];

        // Resolution failures belong to the runner-readiness validator; this
        // preflight must not shadow them with a credential error.
        preflight_provider_credentials_for_backend(&providers, "no-such-backend", None)
            .expect("unresolvable backends are not this preflight's error");

        preflight_provider_credentials_for_backend(&providers, "sample-runtime", None)
            .expect_err("a resolvable backend with a missing credential fails");
    }
}
