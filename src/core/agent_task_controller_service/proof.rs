//! One-command controller proof workflow (#6222).
//!
//! A "proof" runs a controller loop end-to-end from a clean source checkout
//! given only operator *intent and policy* — a named profile plus a runner —
//! instead of filesystem choreography (hand-written specs, manual env exports,
//! manual runner cleanup, ad-hoc Lab routing flags).
//!
//! This module owns the generic orchestration:
//!
//! 1. Fresh run-scoped identity (run-id / randomness seed / loop-id) so reruns
//!    never collide with stale persisted controller state.
//! 2. Profile resolution — a [`ControllerProofProfile`] is opaque intent+policy
//!    *data* (spec source, dispatch backend/selector, complexity policy,
//!    required secret env, runtime components). The orchestration never branches
//!    on a profile's identity, keeping homeboy core language- and
//!    ecosystem-agnostic. Concrete profiles live in a small generic registry
//!    keyed by name and may be supplied by the operator as a JSON file.
//! 3. Preflight reconciliation — compose the existing readiness/reconcile
//!    primitives (provider/runner readiness, extension/runtime parity, secret
//!    readiness) and FAIL BEFORE DISPATCH with a Homeboy-owned fix command for
//!    any unmet dependency.
//! 4. Dispatch handoff — the resolved spec source + dispatch defaults the CLI
//!    feeds into the existing controller run-from-spec/offload path.
//!
//! The CLI adapter (`agent-task controller proof`) is responsible only for
//! argument parsing, calling [`prepare_controller_proof`], running the resolved
//! dispatch through the existing offload path, and rendering the JSON envelope.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::agent_task_provider;
use crate::core::{Error, Result};

/// Schema for the proof preparation (preflight) envelope.
pub const CONTROLLER_PROOF_PREFLIGHT_SCHEMA: &str =
    "homeboy/agent-task-controller-proof-preflight/v1";

/// Generic, profile-driven proof intent+policy.
///
/// A profile carries only opaque data: which spec source to materialize, which
/// dispatch backend/selector/provider-config the controller actions default to,
/// the complexity policy knobs, the secret env contracts that must be present
/// before dispatch, and the runtime component ids that must be reconciled. None
/// of these fields are interpreted as ecosystem-specific behavior by core; they
/// are forwarded to the existing generic primitives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControllerProofProfile {
    /// Stable profile name (CLI `--profile <name>`). Opaque identifier.
    pub name: String,
    /// Human description of what this proof exercises.
    #[serde(default)]
    pub description: Option<String>,
    /// Spec source forwarded to the controller run-from-spec path: a literal
    /// spec, an `@file`, `-` for stdin, or a generator manifest path.
    pub spec_source: String,
    /// Default executor backend for controller-spawned dispatch actions.
    #[serde(default)]
    pub dispatch_backend: Option<String>,
    /// Default extension-provider selector (Homeboy executor provider id).
    #[serde(default)]
    pub dispatch_selector: Option<String>,
    /// Default agent/model provider config (nested AI runtime selection).
    #[serde(default)]
    pub dispatch_provider_config: Option<String>,
    /// Complexity policy knobs materialized into the controller run inputs.
    #[serde(default)]
    pub complexity_policy: BTreeMap<String, Value>,
    /// Secret env var names that MUST be present before dispatch.
    #[serde(default)]
    pub required_secret_env: Vec<String>,
    /// Runtime component ids that must be reconciled/synced before dispatch.
    #[serde(default)]
    pub runtime_components: Vec<String>,
}

/// A single preflight check outcome.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ControllerProofPreflightCheck {
    /// Stable check id (`proof.identity`, `proof.runner_readiness`, ...).
    pub id: String,
    /// `ok` or `error`.
    pub status: String,
    /// Operator-facing message.
    pub message: String,
    /// Homeboy-owned fix command when the check failed before dispatch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_command: Option<String>,
    /// Structured detail for the envelope.
    pub details: Value,
}

impl ControllerProofPreflightCheck {
    fn ok(id: impl Into<String>, message: impl Into<String>, details: Value) -> Self {
        Self {
            id: id.into(),
            status: "ok".to_string(),
            message: message.into(),
            fix_command: None,
            details,
        }
    }

    fn error(
        id: impl Into<String>,
        message: impl Into<String>,
        fix_command: impl Into<String>,
        details: Value,
    ) -> Self {
        Self {
            id: id.into(),
            status: "error".to_string(),
            message: message.into(),
            fix_command: Some(fix_command.into()),
            details,
        }
    }

    fn failed(&self) -> bool {
        self.status == "error"
    }
}

/// Fresh, run-scoped identity for a single proof invocation.
///
/// Deriving the loop-id from the run-id/seed keeps reruns isolated from stale
/// persisted controller state without the operator hand-picking ids.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ControllerProofIdentity {
    pub run_id: String,
    pub seed: String,
    pub loop_id: String,
}

/// Resolved, preflighted proof ready to hand off to the dispatch path.
#[derive(Debug, Clone, Serialize)]
pub struct ControllerProofPreparation {
    pub schema: &'static str,
    pub profile: ControllerProofProfile,
    pub runner: String,
    pub identity: ControllerProofIdentity,
    /// Run inputs materialized from the profile's complexity policy + identity.
    pub run_inputs: Value,
    /// True when every required preflight check passed and dispatch may proceed.
    pub preflight_passed: bool,
    pub checks: Vec<ControllerProofPreflightCheck>,
}

impl ControllerProofPreparation {
    /// Spec source the dispatch path should materialize and run.
    pub fn spec_source(&self) -> &str {
        &self.profile.spec_source
    }
}

/// Deterministically derive a run-scoped identity from a profile + runner + seed
/// material. The seed material is hashed so callers can pass any entropy source
/// (a UUID, a timestamp, test-fixed bytes) and still get a stable, collision-
/// resistant loop-id derived from it.
pub fn derive_proof_identity(
    profile_name: &str,
    runner: &str,
    seed_material: &str,
) -> ControllerProofIdentity {
    let seed = hex_digest(&format!("seed:{profile_name}:{runner}:{seed_material}"));
    let run_id = format!("proof-{profile_name}-{}", &seed[..12]);
    let loop_id = format!("proof/{profile_name}/{}", &seed[..16]);
    ControllerProofIdentity {
        run_id,
        seed,
        loop_id,
    }
}

fn hex_digest(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Build the run inputs the controller materializer consumes: the profile's
/// complexity policy plus the run-scoped identity, so a single proof invocation
/// is fully reproducible from intent+policy.
fn build_run_inputs(profile: &ControllerProofProfile, identity: &ControllerProofIdentity) -> Value {
    let mut policy = serde_json::Map::new();
    for (key, value) in &profile.complexity_policy {
        policy.insert(key.clone(), value.clone());
    }
    serde_json::json!({
        "inputs": {
            "loop_id": identity.loop_id,
            "complexity_policy": Value::Object(policy),
        },
        "metadata": {
            "proof_run_id": identity.run_id,
            "proof_seed": identity.seed,
            "proof_profile": profile.name,
        }
    })
}

/// Environment lookup used by secret readiness. Abstracted so tests can supply a
/// deterministic environment instead of the live process env.
pub trait ProofSecretEnv {
    fn get(&self, key: &str) -> Option<String>;
}

/// Live process environment.
pub struct ProcessSecretEnv;

impl ProofSecretEnv for ProcessSecretEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Readiness probe for provider/runner/extension parity. Abstracted so tests can
/// inject deterministic outcomes without a live provider catalog or runner.
pub trait ProofReadinessProbe {
    /// Validate that the backend/selector resolves to an available provider with
    /// satisfied runner readiness and extension/runtime parity. Returns the
    /// underlying error message on failure.
    fn validate_runner_readiness(
        &self,
        backend: &str,
        selector: Option<&str>,
    ) -> std::result::Result<(), String>;
}

/// Live readiness probe backed by the discovered provider catalog.
pub struct CatalogReadinessProbe;

impl ProofReadinessProbe for CatalogReadinessProbe {
    fn validate_runner_readiness(
        &self,
        backend: &str,
        selector: Option<&str>,
    ) -> std::result::Result<(), String> {
        agent_task_provider::validate_provider_runner_readiness_for_backend(backend, selector)
            .map_err(|error| error.message.clone())
    }
}

/// Prepare a controller proof: derive identity, materialize run inputs, and run
/// preflight reconciliation. The returned preparation is dispatch-ready only
/// when `preflight_passed` is true; otherwise the caller must surface the failed
/// checks (each with a Homeboy-owned fix command) and FAIL BEFORE DISPATCH.
///
/// This is the generic composition core: it never branches on the profile's
/// identity, only forwards the profile's opaque data into the existing readiness
/// primitives.
pub fn prepare_controller_proof(
    profile: ControllerProofProfile,
    runner: &str,
    seed_material: &str,
    env: &dyn ProofSecretEnv,
    readiness: &dyn ProofReadinessProbe,
) -> ControllerProofPreparation {
    let identity = derive_proof_identity(&profile.name, runner, seed_material);
    let run_inputs = build_run_inputs(&profile, &identity);
    let mut checks = Vec::new();

    // 1. Identity is always generated by Homeboy; record it as evidence.
    checks.push(ControllerProofPreflightCheck::ok(
        "proof.identity",
        format!(
            "Generated run-scoped identity run_id={} loop_id={}",
            identity.run_id, identity.loop_id
        ),
        serde_json::json!({
            "run_id": identity.run_id,
            "seed": identity.seed,
            "loop_id": identity.loop_id,
        }),
    ));

    // 2. Runner is required intent — empty runner is unrunnable.
    if runner.trim().is_empty() {
        checks.push(ControllerProofPreflightCheck::error(
            "proof.runner",
            "No runner was provided for the proof",
            "homeboy agent-task controller proof --profile <profile> --runner <runner>",
            serde_json::json!({ "runner": runner }),
        ));
    } else {
        checks.push(ControllerProofPreflightCheck::ok(
            "proof.runner",
            format!("Runner '{runner}' selected for dispatch"),
            serde_json::json!({ "runner": runner }),
        ));
    }

    // 3. Provider/runner readiness + extension/runtime parity (reuses the same
    //    backend/selector resolution dispatch uses). Only run when a backend is
    //    declared; a profile without a backend defers selection to the spec.
    if let Some(backend) = profile.dispatch_backend.as_deref() {
        match readiness.validate_runner_readiness(backend, profile.dispatch_selector.as_deref()) {
            Ok(()) => checks.push(ControllerProofPreflightCheck::ok(
                "proof.runner_readiness",
                format!("Provider readiness satisfied for backend '{backend}'"),
                serde_json::json!({
                    "backend": backend,
                    "selector": profile.dispatch_selector,
                }),
            )),
            Err(message) => checks.push(ControllerProofPreflightCheck::error(
                "proof.runner_readiness",
                format!("Provider/runner readiness failed for backend '{backend}': {message}"),
                format!("homeboy agent-task providers --backend {backend} --runner {runner}"),
                serde_json::json!({
                    "backend": backend,
                    "selector": profile.dispatch_selector,
                    "error": message,
                }),
            )),
        }
    }

    // 4. Secret readiness: every required secret env must be present BEFORE
    //    dispatch, with a Homeboy-owned auth/handoff fix command when missing.
    let missing_secrets: Vec<String> = profile
        .required_secret_env
        .iter()
        .filter(|name| {
            env.get(name)
                .filter(|value| !value.trim().is_empty())
                .is_none()
        })
        .cloned()
        .collect();
    if profile.required_secret_env.is_empty() {
        checks.push(ControllerProofPreflightCheck::ok(
            "proof.secret_readiness",
            "No required secret env declared by profile",
            serde_json::json!({ "required_secret_env": Vec::<String>::new() }),
        ));
    } else if missing_secrets.is_empty() {
        checks.push(ControllerProofPreflightCheck::ok(
            "proof.secret_readiness",
            "All required secret env values are present",
            serde_json::json!({ "required_secret_env": profile.required_secret_env }),
        ));
    } else {
        checks.push(ControllerProofPreflightCheck::error(
            "proof.secret_readiness",
            format!(
                "Required secret env missing before dispatch: {}",
                missing_secrets.join(", ")
            ),
            format!(
                "homeboy agent-task auth status --runner {runner} (then export the missing secret env: {})",
                missing_secrets.join(", ")
            ),
            serde_json::json!({
                "missing_secret_env": missing_secrets,
                "required_secret_env": profile.required_secret_env,
            }),
        ));
    }

    let preflight_passed = !checks.iter().any(ControllerProofPreflightCheck::failed);

    ControllerProofPreparation {
        schema: CONTROLLER_PROOF_PREFLIGHT_SCHEMA,
        profile,
        runner: runner.to_string(),
        identity,
        run_inputs,
        preflight_passed,
        checks,
    }
}

/// Resolve a named proof profile from an optional operator-supplied registry
/// file. The registry is a generic `{ "<name>": ControllerProofProfile }` map,
/// so adding ecosystem profiles is pure data — no core code change. When the
/// registry source is `None`, no built-in profiles exist and the lookup fails
/// with guidance to supply one, keeping core free of hardcoded profile data.
pub fn resolve_proof_profile(
    name: &str,
    registry_source: Option<&str>,
) -> Result<ControllerProofProfile> {
    let Some(source) = registry_source else {
        return Err(Error::validation_invalid_argument(
            "profile",
            format!(
                "no proof profile registry available to resolve profile '{name}'; pass --profiles <file> with a profile registry"
            ),
            Some(name.to_string()),
            Some(vec![
                "A profile registry is a JSON object mapping profile names to profile definitions."
                    .to_string(),
                "homeboy agent-task controller proof --profile <name> --runner <runner> --profiles <file>".to_string(),
            ]),
        ));
    };
    let raw = crate::core::config::read_json_spec_to_string(source)?;
    let registry: BTreeMap<String, ControllerProofProfile> =
        serde_json::from_str(&raw).map_err(|error| {
            Error::validation_invalid_argument(
                "profiles",
                format!("proof profile registry is not a valid name->profile map: {error}"),
                Some(source.to_string()),
                None,
            )
        })?;
    match registry.get(name) {
        Some(profile) => {
            let mut profile = profile.clone();
            // The map key is the canonical name; keep the profile self-consistent.
            profile.name = name.to_string();
            Ok(profile)
        }
        None => {
            let available: Vec<String> = registry.keys().cloned().collect();
            Err(Error::validation_invalid_argument(
                "profile",
                format!("proof profile '{name}' not found in registry"),
                Some(name.to_string()),
                Some(vec![format!(
                    "Available profiles: {}",
                    available.join(", ")
                )]),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeEnv {
        values: BTreeMap<String, String>,
    }

    impl ProofSecretEnv for FakeEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.values.get(key).cloned()
        }
    }

    struct OkProbe;
    impl ProofReadinessProbe for OkProbe {
        fn validate_runner_readiness(
            &self,
            _backend: &str,
            _selector: Option<&str>,
        ) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    struct FailProbe;
    impl ProofReadinessProbe for FailProbe {
        fn validate_runner_readiness(
            &self,
            _backend: &str,
            _selector: Option<&str>,
        ) -> std::result::Result<(), String> {
            Err("no extension agent-task provider found for backend".to_string())
        }
    }

    fn profile_with(required_secret_env: Vec<String>) -> ControllerProofProfile {
        ControllerProofProfile {
            name: "example-proof".to_string(),
            description: Some("example".to_string()),
            spec_source: "@spec.json".to_string(),
            dispatch_backend: Some("example-backend".to_string()),
            dispatch_selector: Some("example.provider".to_string()),
            dispatch_provider_config: None,
            complexity_policy: BTreeMap::from([("max_files".to_string(), serde_json::json!(3))]),
            required_secret_env,
            runtime_components: vec!["example-component".to_string()],
        }
    }

    #[test]
    fn identity_is_deterministic_for_same_seed_material() {
        let a = derive_proof_identity("p", "runner", "seed-material");
        let b = derive_proof_identity("p", "runner", "seed-material");
        assert_eq!(a, b);
        let c = derive_proof_identity("p", "runner", "other");
        assert_ne!(a.loop_id, c.loop_id);
        assert!(a.run_id.starts_with("proof-p-"));
        assert!(a.loop_id.starts_with("proof/p/"));
    }

    #[test]
    fn preflight_fails_before_dispatch_on_missing_secret_with_fix_command() {
        let profile = profile_with(vec!["EXAMPLE_TOKEN".to_string()]);
        let env = FakeEnv {
            values: BTreeMap::new(),
        };
        let prep = prepare_controller_proof(profile, "homeboy-lab", "seed", &env, &OkProbe);

        assert!(
            !prep.preflight_passed,
            "missing secret must fail preflight before dispatch"
        );
        let secret_check = prep
            .checks
            .iter()
            .find(|check| check.id == "proof.secret_readiness")
            .expect("secret readiness check present");
        assert_eq!(secret_check.status, "error");
        let fix = secret_check
            .fix_command
            .as_deref()
            .expect("failed check carries a Homeboy-owned fix command");
        assert!(fix.contains("agent-task auth status"));
        assert!(fix.contains("EXAMPLE_TOKEN"));
        // Identity is still generated even on preflight failure.
        assert!(prep
            .checks
            .iter()
            .any(|check| check.id == "proof.identity" && check.status == "ok"));
    }

    #[test]
    fn preflight_fails_before_dispatch_on_unmet_readiness_with_fix_command() {
        let profile = profile_with(Vec::new());
        let env = FakeEnv {
            values: BTreeMap::new(),
        };
        let prep = prepare_controller_proof(profile, "homeboy-lab", "seed", &env, &FailProbe);

        assert!(!prep.preflight_passed);
        let readiness = prep
            .checks
            .iter()
            .find(|check| check.id == "proof.runner_readiness")
            .expect("readiness check present");
        assert_eq!(readiness.status, "error");
        assert!(readiness
            .fix_command
            .as_deref()
            .expect("readiness fix command")
            .contains("agent-task providers"));
    }

    #[test]
    fn preflight_passes_and_composes_primitives_when_dependencies_met() {
        let profile = profile_with(vec!["EXAMPLE_TOKEN".to_string()]);
        let env = FakeEnv {
            values: BTreeMap::from([("EXAMPLE_TOKEN".to_string(), "secret-value".to_string())]),
        };
        let prep = prepare_controller_proof(profile, "homeboy-lab", "seed", &env, &OkProbe);

        assert!(
            prep.preflight_passed,
            "all dependencies met must pass preflight: {:?}",
            prep.checks
        );
        // Every composed primitive reported ok.
        for id in [
            "proof.identity",
            "proof.runner",
            "proof.runner_readiness",
            "proof.secret_readiness",
        ] {
            let check = prep
                .checks
                .iter()
                .find(|check| check.id == id)
                .unwrap_or_else(|| panic!("check {id} present"));
            assert_eq!(check.status, "ok", "check {id} should be ok");
        }
        // Run inputs carry the run-scoped identity + complexity policy.
        assert_eq!(
            prep.run_inputs["metadata"]["proof_run_id"],
            serde_json::json!(prep.identity.run_id)
        );
        assert_eq!(
            prep.run_inputs["inputs"]["complexity_policy"]["max_files"],
            serde_json::json!(3)
        );
        assert_eq!(prep.spec_source(), "@spec.json");
    }

    #[test]
    fn empty_runner_fails_preflight() {
        let profile = profile_with(Vec::new());
        let env = FakeEnv {
            values: BTreeMap::new(),
        };
        let prep = prepare_controller_proof(profile, "  ", "seed", &env, &OkProbe);
        assert!(!prep.preflight_passed);
        assert!(prep
            .checks
            .iter()
            .any(|check| check.id == "proof.runner" && check.status == "error"));
    }

    #[test]
    fn resolve_profile_without_registry_fails_with_guidance() {
        let error = resolve_proof_profile("example-proof", None).expect_err("no registry");
        assert!(error.message.contains("no proof profile registry"));
    }

    #[test]
    fn resolve_profile_from_inline_registry() {
        let registry = serde_json::json!({
            "example-proof": {
                "name": "ignored-will-be-overwritten",
                "spec_source": "@spec.json",
                "dispatch_backend": "example-backend"
            }
        })
        .to_string();
        let profile =
            resolve_proof_profile("example-proof", Some(&registry)).expect("profile resolves");
        assert_eq!(profile.name, "example-proof");
        assert_eq!(profile.spec_source, "@spec.json");
        assert_eq!(profile.dispatch_backend.as_deref(), Some("example-backend"));
    }

    #[test]
    fn resolve_unknown_profile_lists_available() {
        let registry = serde_json::json!({
            "known": { "name": "known", "spec_source": "@s.json" }
        })
        .to_string();
        let error = resolve_proof_profile("missing", Some(&registry)).expect_err("unknown profile");
        assert!(error.message.contains("not found"));
    }
}
