//! The generic destructive-workload gate.
//!
//! A kind declaring `safety_class: destructive` may mutate the environment it
//! runs in. Running, replaying or minimizing it requires both an explicit
//! `--allow-destructive` and an explicit `homeboy/isolation-proof/v1` proving
//! the run is disposable. Nothing is generated on the caller's behalf: the
//! proof is the operator's statement, so it has to come from the operator.

use std::path::{Path, PathBuf};

use homeboy_extension_contract::WorkloadSafetyClass;
use serde_json::Value;

use crate::error::{Error, Result};

pub const ISOLATION_PROOF_SCHEMA: &str = "homeboy/isolation-proof/v1";
const ISOLATION_PROOF_VERSION: u64 = 1;

/// What the caller asked for.
#[derive(Debug, Clone, Default)]
pub struct DestructiveRequest {
    pub allow_destructive: bool,
    pub isolation_proof: Option<PathBuf>,
}

/// A destructive execution the gate admitted.
#[derive(Debug, Clone, PartialEq)]
pub struct DestructiveAuthorization {
    pub isolation_proof_file: PathBuf,
    pub isolation_proof: Value,
}

/// Admit or refuse an execution of a kind with `safety_class`.
///
/// Read-only kinds always pass and carry no authorization. Destructive kinds
/// need `--allow-destructive` and a valid isolation proof; anything less is
/// refused before a run is created.
pub fn authorize(
    kind: &str,
    safety_class: WorkloadSafetyClass,
    request: &DestructiveRequest,
) -> Result<Option<DestructiveAuthorization>> {
    if safety_class.is_read_only() {
        return Ok(None);
    }
    if !request.allow_destructive {
        return Err(Error::validation_invalid_argument(
            "allow_destructive",
            format!(
                "workload kind `{kind}` is declared destructive; refusing to execute without --allow-destructive and --isolation-proof"
            ),
            None,
            Some(vec![
                "Run it in a disposable environment and pass --allow-destructive --isolation-proof <path> with a homeboy/isolation-proof/v1 document.".to_string(),
            ]),
        ));
    }
    let Some(path) = request.isolation_proof.as_deref() else {
        return Err(Error::validation_invalid_argument(
            "isolation_proof",
            format!(
                "workload kind `{kind}` is declared destructive; --allow-destructive also requires --isolation-proof <path>"
            ),
            None,
            None,
        ));
    };
    let proof = load_isolation_proof(path)?;
    Ok(Some(DestructiveAuthorization {
        isolation_proof_file: path.to_path_buf(),
        isolation_proof: proof,
    }))
}

pub fn load_isolation_proof(path: &Path) -> Result<Value> {
    let invalid = |problem: String| {
        Error::validation_invalid_argument(
            "isolation_proof",
            problem,
            Some(path.display().to_string()),
            None,
        )
    };
    let raw = std::fs::read_to_string(path)
        .map_err(|error| invalid(format!("failed to read isolation proof: {error}")))?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| invalid(format!("invalid isolation proof JSON: {error}")))?;
    validate_isolation_proof(&value).map_err(invalid)?;
    Ok(value)
}

/// The `homeboy/isolation-proof/v1` invariants: a disposable runtime that is
/// torn down, a named mutation boundary, who verified it, and at least one
/// proof artifact.
pub fn validate_isolation_proof(value: &Value) -> std::result::Result<(), String> {
    let field = |name: &str| value.get(name);
    let non_empty = |name: &str| {
        field(name)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty())
    };
    if field("schema").and_then(Value::as_str) != Some(ISOLATION_PROOF_SCHEMA) {
        return Err(format!(
            "isolation proof schema must be {ISOLATION_PROOF_SCHEMA}"
        ));
    }
    if field("version")
        .and_then(Value::as_u64)
        .is_some_and(|version| version != ISOLATION_PROOF_VERSION)
    {
        return Err(format!(
            "isolation proof version must be {ISOLATION_PROOF_VERSION}"
        ));
    }
    for name in ["runtime_kind", "mutation_boundary", "verified_by"] {
        if !non_empty(name) {
            return Err(format!("isolation_proof.{name} must be a non-empty string"));
        }
    }
    for name in ["disposable", "teardown_required"] {
        if field(name).and_then(Value::as_bool) != Some(true) {
            return Err(format!("isolation_proof.{name} must be true"));
        }
    }
    if !field("proof_artifacts")
        .and_then(Value::as_array)
        .is_some_and(|artifacts| !artifacts.is_empty())
    {
        return Err("isolation_proof.proof_artifacts must be non-empty".to_string());
    }
    Ok(())
}

/// The runner env a gate decision produces.
pub fn env(authorization: Option<&DestructiveAuthorization>) -> Vec<(String, String)> {
    let mut env = vec![(
        super::env::ALLOW_DESTRUCTIVE.to_string(),
        if authorization.is_some() { "1" } else { "0" }.to_string(),
    )];
    if let Some(authorization) = authorization {
        env.push((
            super::env::ISOLATION_PROOF_FILE.to_string(),
            authorization
                .isolation_proof_file
                .to_string_lossy()
                .to_string(),
        ));
    }
    env
}
