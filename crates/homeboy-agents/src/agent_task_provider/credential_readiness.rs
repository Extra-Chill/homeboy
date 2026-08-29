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
//! - the `required_secret_env` of a *sole* `provider_defaults` entry — when a
//!   provider declares exactly one provider default, that default is what
//!   dispatch uses unless the request overrides it, so its explicitly-required
//!   credentials are unconditionally required.
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
use crate::agent_task_secrets::{
    secret_env_status_for_scopes, AgentTaskSecretEnvScope, AgentTaskSecretEnvStatus,
};

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

    for env in sole_provider_default_required_secret_env(provider) {
        push_declared_credential(
            &mut declared,
            &env,
            "provider_defaults.required_secret_env",
            None,
            None,
        );
    }

    declared
}

/// `required_secret_env` of the provider's sole declared provider default.
///
/// When a provider declares exactly one provider default, dispatch uses it
/// unless the request names another, so anything that default marks explicitly
/// required is unconditionally required. With two or more declared defaults the
/// choice is request-dependent and this returns nothing rather than guessing.
///
/// Only `required_secret_env` is read — never the broader `secret_env` list,
/// which routinely carries derivable or optional companions (access tokens,
/// expiry stamps) that must not make a provider look undispatchable.
fn sole_provider_default_required_secret_env(provider: &AgentTaskExecutorProvider) -> Vec<String> {
    if provider.provider_defaults.len() != 1 {
        return Vec::new();
    }
    let Some(provider_default) = provider.provider_defaults.values().next() else {
        return Vec::new();
    };
    let Some(provider_default) = provider_default.as_object() else {
        return Vec::new();
    };
    for key in ["required_secret_env", "requiredSecretEnv"] {
        match provider_default.get(key) {
            Some(Value::String(name)) => return vec![name.clone()],
            Some(Value::Array(items)) => {
                return items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            }
            _ => {}
        }
    }
    Vec::new()
}

/// Redacted secret-env status for the shown providers, resolved per provider.
///
/// This is the single catalog answer shared by `agent-task providers` and
/// `agent-task auth status`. Merging every provider's sources before resolving
/// made a required credential look configured from another backend's json-file
/// while that requiring provider still reported it missing (#13629).
pub fn secret_env_status_for_providers(
    explicit_names: &[String],
    providers: &[AgentTaskExecutorProvider],
) -> Vec<AgentTaskSecretEnvStatus> {
    let scopes = providers
        .iter()
        .map(|provider| AgentTaskSecretEnvScope {
            fallback_sources: provider_declared_secret_sources(provider),
            required_names: declared_credentials(provider)
                .into_iter()
                .map(|entry| entry.env)
                .collect(),
        })
        .collect::<Vec<_>>();
    secret_env_status_for_scopes(explicit_names, &scopes)
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

    /// The exact shape the claude-code runtime publishes: one provider default
    /// that names its own required credential (#11479).
    fn claude_code_shaped_provider(
        auth_path: Option<&std::path::Path>,
    ) -> AgentTaskExecutorProvider {
        let required = format!("HOMEBOY_TEST_CREDENTIAL_{}", uuid::Uuid::new_v4());
        let secret_env_sources = auth_path.map_or_else(
            || serde_json::json!({}),
            |path| {
                serde_json::json!({
                    required.clone(): { "source": "json-file", "path": path, "field": "token" }
                })
            },
        );
        provider(serde_json::json!({
            "id": "claude-code.agent-task-executor",
            "backend": "claude-code",
            "capabilities": ["cli_runtime", "provider_owned_auth"],
            "secret_env_requirements": [{
                "source": "provider_default",
                "env": [required.clone()],
                "when": { "any": [{ "path": "executor.config.provider", "equals": "claude-code" }] }
            }],
            "provider_defaults": {
                "claude-code": {
                    "secret_env": [
                        required.clone(),
                        format!("{required}_ACCESS_TOKEN"),
                        format!("{required}_EXPIRES_AT")
                    ],
                    "required_secret_env": [required.clone()],
                    "optional_secret_env": [
                        format!("{required}_ACCESS_TOKEN"),
                        format!("{required}_EXPIRES_AT")
                    ],
                    "secret_env_sources": secret_env_sources
                }
            }
        }))
    }

    fn required_credential(provider: &AgentTaskExecutorProvider) -> String {
        sole_provider_default_required_secret_env(provider)
            .into_iter()
            .next()
            .expect("required credential")
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
    fn a_sole_provider_default_required_credential_makes_an_unconfigured_provider_undispatchable() {
        let provider = claude_code_shaped_provider(None);
        let required = required_credential(&provider);
        let readiness = provider_credential_readiness(&provider);

        assert!(
            !readiness.dispatchable,
            "a backend whose required credential is absent is declared, not dispatchable"
        );
        assert_eq!(
            readiness.missing,
            vec![required.clone()],
            "the reason must name the exact credential"
        );
        assert_eq!(
            readiness.reason(),
            Some(format!("missing credential {required}"))
        );
    }

    #[test]
    fn optional_companions_of_a_required_credential_are_not_treated_as_required() {
        let readiness = provider_credential_readiness(&claude_code_shaped_provider(None));

        // `secret_env` also lists the access token and expiry stamp. Those
        // are derivable/optional; reporting them would make every provider
        // that caches a token look broken.
        assert!(
            !readiness
                .missing
                .iter()
                .any(|env| env.contains("ACCESS_TOKEN") || env.contains("EXPIRES_AT")),
            "only explicitly required credentials block dispatch: {:?}",
            readiness.missing
        );
    }

    #[test]
    fn a_configured_credential_makes_the_provider_dispatchable() {
        let auth = tempfile::NamedTempFile::new().expect("auth file");
        std::fs::write(auth.path(), r#"{"token":"refresh-token-value"}"#).expect("write auth");
        let provider = claude_code_shaped_provider(Some(auth.path()));
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
            "id": "codex.agent-task-executor",
            "backend": "codex",
            "secret_env_requirements": [{
                "source": "provider_default",
                "env": ["AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN"],
                "when": { "any": [{ "path": "executor.config.provider", "equals": "codex" }] }
            }]
        })));

        assert!(readiness.dispatchable);
        assert!(readiness.requirements.is_empty());
    }

    #[test]
    fn an_unconditional_secret_env_requirement_blocks_dispatch() {
        let required = format!("HOMEBOY_TEST_CREDENTIAL_{}", uuid::Uuid::new_v4());
        let readiness = provider_credential_readiness(&provider(serde_json::json!({
            "id": "example.agent-task-executor",
            "backend": "example",
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
            "id": "example.agent-task-executor",
            "backend": "example",
            "secret_requirements": [{
                "name": "EXAMPLE_OPTIONAL_KEY",
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
            "id": "wordpress.codebox-agent-task-executor",
            "backend": "wp-codebox",
            "provider_defaults": {
                "openai": { "required_secret_env": ["OPENAI_API_KEY"] },
                "claude-code": { "required_secret_env": ["AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN"] }
            }
        })));

        assert!(readiness.dispatchable);
        assert!(readiness.missing.is_empty());
    }

    #[test]
    fn the_preflight_error_is_a_configuration_failure_naming_the_credential() {
        let provider = claude_code_shaped_provider(None);
        let required = required_credential(&provider);
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
    fn catalog_secret_env_does_not_inherit_another_provider_source_for_a_required_credential() {
        let required = format!("HOMEBOY_TEST_CREDENTIAL_{}", uuid::Uuid::new_v4());
        let auth = tempfile::NamedTempFile::new().expect("auth file");
        std::fs::write(auth.path(), r#"{"token":"refresh-token-value"}"#).expect("write auth");
        let requiring = provider(serde_json::json!({
            "id": "claude-code.agent-task-executor",
            "backend": "claude-code",
            "provider_defaults": {
                "claude-code": {
                    "secret_env": [required.clone()],
                    "required_secret_env": [required.clone()]
                }
            }
        }));
        let other = provider(serde_json::json!({
            "id": "wordpress.codebox-agent-task-executor",
            "backend": "wp-codebox",
            "provider_defaults": {
                "claude-code": {
                    "secret_env": [required.clone()],
                    "secret_env_sources": {
                        required.clone(): {
                            "source": "json-file",
                            "path": auth.path().display().to_string(),
                            "field": "token"
                        }
                    }
                }
            }
        }));

        let readiness = provider_credential_readiness(&requiring);
        assert!(
            !readiness.dispatchable,
            "the requiring provider still cannot resolve the credential from its own sources"
        );
        assert_eq!(readiness.missing, vec![required.clone()]);

        let catalog = secret_env_status_for_providers(&[], &[requiring.clone(), other.clone()]);
        let entry = catalog
            .iter()
            .find(|entry| entry.name == required)
            .expect("required name is reported");
        assert!(
            !entry.configured,
            "catalog secret_env must not report configured=true from the other provider: {entry:?}"
        );
        assert_eq!(
            entry.configured,
            readiness
                .requirements
                .iter()
                .find(|requirement| requirement.env == required)
                .expect("requirement row")
                .configured
        );

        let explicit =
            secret_env_status_for_providers(std::slice::from_ref(&required), &[requiring, other]);
        assert_eq!(explicit.len(), 1);
        assert!(!explicit[0].configured);
    }

    #[test]
    fn backend_preflight_ignores_backends_that_do_not_resolve() {
        let providers = vec![claude_code_shaped_provider(None)];

        // Resolution failures belong to the runner-readiness validator; this
        // preflight must not shadow them with a credential error.
        preflight_provider_credentials_for_backend(&providers, "no-such-backend", None)
            .expect("unresolvable backends are not this preflight's error");

        preflight_provider_credentials_for_backend(&providers, "claude-code", None)
            .expect_err("a resolvable backend with a missing credential fails");
    }
}
