//! Immutable controller executable provenance for durable orchestration work.

use serde_json::{json, Value};
use std::path::PathBuf;

use crate::{build_identity, paths, Error, Result};

pub(crate) const CONTROLLER_RUNTIME_METADATA_KEY: &str = "controller_runtime";

pub(crate) fn pin_current() -> Result<Value> {
    let identity = build_identity::current();
    let executable = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve controller executable".to_string()),
        )
    })?;
    let pinned_path = pinned_path(&identity.display)?;
    if !pinned_path.exists() {
        let parent = pinned_path.parent().expect("pinned runtime has parent");
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create controller runtime pin".to_string()),
            )
        })?;
        std::fs::copy(&executable, &pinned_path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("pin controller executable".to_string()),
            )
        })?;
    }

    Ok(json!({
        "schema": "homeboy/controller-runtime-pin/v1",
        "requested": identity.display,
        "originating": {
            "build_identity": identity.display,
            "executable": executable,
            "pinned_executable": pinned_path,
        },
        "current": identity.display,
        "executed": identity.display,
    }))
}

pub(crate) fn validate_for_mutation(metadata: &Value, current_identity: &str) -> Result<()> {
    let Some(runtime) = metadata.get(CONTROLLER_RUNTIME_METADATA_KEY) else {
        return Ok(());
    };
    let originating = runtime
        .pointer("/originating/build_identity")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if originating.is_empty() || originating == current_identity {
        return Ok(());
    }
    let pinned = runtime
        .pointer("/originating/pinned_executable")
        .and_then(Value::as_str)
        .unwrap_or("<pinned-controller-runtime>");
    Err(Error::validation_invalid_argument(
        "controller_runtime",
        format!(
            "durable run was created by controller runtime `{originating}`, but this command is `{current_identity}`"
        ),
        Some(current_identity.to_string()),
        Some(vec![format!(
            "Run the lifecycle mutation through the pinned compatible runtime: {pinned} <original homeboy arguments>"
        )]),
    ))
}

fn pinned_path(identity: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("controller-runtimes")
        .join(paths::sanitize_path_segment(identity))
        .join("homeboy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_mismatch_returns_pinned_runtime_recovery_command() {
        let metadata = json!({
            "controller_runtime": {
                "originating": {
                    "build_identity": "homeboy 1.0.0+origin",
                    "pinned_executable": "/tmp/homeboy-origin",
                }
            }
        });

        let error = validate_for_mutation(&metadata, "homeboy 1.0.0+replacement")
            .expect_err("replacement runtime must not mutate the originating lifecycle");

        assert!(error.message.contains("homeboy 1.0.0+origin"));
        assert!(error.details["tried"][0]
            .as_str()
            .is_some_and(|command| command.contains("/tmp/homeboy-origin")));
    }
}
