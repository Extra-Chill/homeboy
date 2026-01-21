use crate::error::Result;
use crate::utils::validation;

use super::types::ReleaseArtifact;

pub(crate) fn parse_module_inputs(values: &[serde_json::Value]) -> Result<Vec<(String, String)>> {
    let mut inputs = Vec::new();
    for value in values {
        let entry = validation::require(
            value.as_object(),
            "release.steps",
            "module.run inputs must be objects with 'id' and 'value'",
        )?;
        let id = validation::require(
            entry.get("id").and_then(|v| v.as_str()),
            "release.steps",
            "module.run inputs require 'id'",
        )?;
        let value = validation::require(
            entry.get("value").and_then(|v| v.as_str()),
            "release.steps",
            "module.run inputs require 'value'",
        )?;
        inputs.push((id.to_string(), value.to_string()));
    }

    Ok(inputs)
}

pub(crate) fn parse_module_args(values: &[serde_json::Value]) -> Result<Vec<String>> {
    let mut args = Vec::new();
    for value in values {
        let arg = validation::require(
            value.as_str(),
            "release.steps",
            "module.run args must be strings",
        )?;
        args.push(arg.to_string());
    }
    Ok(args)
}

pub fn extract_latest_notes(content: &str) -> Option<String> {
    let mut in_section = false;
    let mut buffer = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            if in_section {
                break;
            }
            if extract_version_from_heading(trimmed).is_some() {
                in_section = true;
                continue;
            }
        }

        if in_section {
            buffer.push(line);
        }
    }

    let notes = buffer.join("\n").trim().to_string();
    if notes.is_empty() {
        None
    } else {
        Some(notes)
    }
}

fn extract_version_from_heading(label: &str) -> Option<String> {
    let semver_pattern = regex::Regex::new(r"\[?(\d+\.\d+\.\d+)\]?").ok()?;
    semver_pattern
        .captures(label)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

pub fn parse_release_artifacts(value: &serde_json::Value) -> Result<Vec<ReleaseArtifact>> {
    let mut artifacts = Vec::new();
    let items = match value {
        serde_json::Value::Array(arr) => arr.clone(),
        serde_json::Value::Object(_) => vec![value.clone()],
        _ => Vec::new(),
    };

    use crate::error::Error;
    for item in items {
        let artifact = match item {
            serde_json::Value::String(path) => ReleaseArtifact {
                path,
                artifact_type: None,
                platform: None,
            },
            serde_json::Value::Object(map) => {
                let path = validation::require(
                    map.get("path").and_then(|v| v.as_str()),
                    "release.artifacts",
                    "Artifact is missing 'path'",
                )?
                .to_string();
                let artifact_type = map
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                let platform = map
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                ReleaseArtifact {
                    path,
                    artifact_type,
                    platform,
                }
            }
            _ => {
                return Err(Error::validation_invalid_argument(
                    "release.artifacts",
                    "Artifact entry is invalid",
                    None,
                    None,
                ))
            }
        };
        artifacts.push(artifact);
    }

    Ok(artifacts)
}
