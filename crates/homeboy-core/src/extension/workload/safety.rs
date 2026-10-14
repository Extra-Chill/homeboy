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

/// Where an effective safety class came from.
pub const SAFETY_SOURCE_KIND: &str = "kind";
/// The selected workload file declared a class.
pub const SAFETY_SOURCE_WORKLOAD_FILE: &str = "workload_file";

/// The safety class an execution actually runs under: the kind's declared
/// class, possibly raised by the selected workload file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveSafetyClass {
    pub class: WorkloadSafetyClass,
    /// [`SAFETY_SOURCE_KIND`] or [`SAFETY_SOURCE_WORKLOAD_FILE`].
    pub source: &'static str,
}

/// The `safety_class` a workload file declares, when the file is JSON with a
/// top-level class this build knows.
///
/// Unknown, unreadable or unparseable files declare nothing; the caller keeps
/// the kind's class.
pub fn workload_file_safety_class(path: &Path) -> Option<WorkloadSafetyClass> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    match value.get("safety_class")?.as_str()? {
        "destructive" => Some(WorkloadSafetyClass::Destructive),
        "read_only" => Some(WorkloadSafetyClass::ReadOnly),
        _ => None,
    }
}

/// Effective safety class of one execution.
///
/// A workload file can only raise the kind's class, never lower it: a
/// `read_only` kind whose selected workload file declares
/// `"safety_class": "destructive"` runs destructive and needs the same
/// authorization a destructive kind needs, while a destructive kind stays
/// destructive no matter what the file declares. Files that declare nothing
/// readable keep the kind's class.
pub fn effective_safety_class(
    kind_class: WorkloadSafetyClass,
    workload_file: Option<&Path>,
) -> EffectiveSafetyClass {
    let kind = EffectiveSafetyClass {
        class: kind_class,
        source: SAFETY_SOURCE_KIND,
    };
    let Some(path) = workload_file else {
        return kind;
    };
    match workload_file_safety_class(path) {
        Some(WorkloadSafetyClass::Destructive) => EffectiveSafetyClass {
            class: WorkloadSafetyClass::Destructive,
            source: SAFETY_SOURCE_WORKLOAD_FILE,
        },
        Some(WorkloadSafetyClass::ReadOnly) if kind_class.is_read_only() => EffectiveSafetyClass {
            class: WorkloadSafetyClass::ReadOnly,
            source: SAFETY_SOURCE_WORKLOAD_FILE,
        },
        _ => kind,
    }
}
