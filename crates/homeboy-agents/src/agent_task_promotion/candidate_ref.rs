//! Durable destination refs, independent of the staged destination index.
use std::path::Path;

use homeboy_core::{Error, Result};
use serde_json::Value;

/// Best-effort: the ref is an operator convenience, so a candidate object that
/// is absent from the destination (for example a Lab-produced commit) records
/// `candidate_ref_error` instead of failing the promotion it annotates.
pub(crate) fn pin_applied_candidate(cook_id: &str, promotion: &mut Value) -> Result<()> {
    if let Err(error) = try_pin_applied_candidate(cook_id, promotion) {
        promotion["candidate_ref_error"] = Value::String(error.message);
    }
    Ok(())
}

fn try_pin_applied_candidate(cook_id: &str, promotion: &mut Value) -> Result<()> {
    if !matches!(
        promotion["status"].as_str(),
        Some("verification_pending" | "applied" | "gate_failed" | "gate_deferred")
    ) {
        return Ok(());
    }
    let (Some(path), Some(sha), Some(tree)) = (
        promotion.pointer("/target/path").and_then(Value::as_str),
        promotion
            .pointer("/candidate/candidate_sha")
            .and_then(Value::as_str),
        promotion
            .pointer("/candidate/tree_sha")
            .and_then(Value::as_str),
    ) else {
        return Ok(());
    };
    super::destination::check(Path::new(path), Some(cook_id))?;
    let reference = format!("refs/homeboy/candidates/{cook_id}");
    let git = |args: &[&str]| -> Result<String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(Path::new(path))
            .args(args)
            .output()
            .map_err(|error| Error::internal_io(error.to_string(), None))?;
        if !output.status.success() {
            return Err(Error::validation_invalid_argument(
                "candidate_ref",
                String::from_utf8_lossy(&output.stderr).trim(),
                Some(reference.clone()),
                None,
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let actual_tree = git(&["rev-parse", &format!("{sha}^{{commit}}^{{tree}}")])?;
    if actual_tree != tree {
        return Err(Error::validation_invalid_argument(
            "candidate_ref",
            "candidate commit tree differs from the applied candidate",
            None,
            None,
        ));
    }
    git(&["check-ref-format", &reference])?;
    git(&["update-ref", &reference, sha])?;
    promotion["candidate_ref"] = Value::String(reference);
    Ok(())
}

#[cfg(test)]
#[test]
fn missing_candidate_object_records_error_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .status()
        .unwrap()
        .success());
    let mut promotion = serde_json::json!({
        "status": "applied",
        "target": {"path": dir.path()},
        "candidate": {"candidate_sha": "0123456789abcdef0123456789abcdef01234567", "tree_sha": "abc"}
    });
    pin_applied_candidate("cook", &mut promotion).unwrap();
    assert!(promotion.get("candidate_ref").is_none());
    assert!(promotion["candidate_ref_error"].is_string());
}
