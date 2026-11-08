//! Durable destination refs, independent of the staged destination index.
use std::path::Path;

use homeboy_core::{Error, Result};
use serde_json::Value;

pub(crate) fn pin_applied_candidate(cook_id: &str, promotion: &mut Value) -> Result<()> {
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
