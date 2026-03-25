//! read_json_empty — extracted from baseline.rs.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::error::{Error, Result};
use std::collections::HashSet;
use super::Baseline;
use super::BaselineConfig;


pub fn load<M: for<'de> Deserialize<'de> + Serialize>(
    config: &BaselineConfig,
) -> Result<Option<Baseline<M>>> {
    let path = config.json_path();
    if !path.exists() {
        return Ok(None);
    }

    let root = read_json_or_empty(&path)?;
    let baseline_value = root
        .get(BASELINES_KEY)
        .and_then(|baselines| baselines.get(config.key()))
        .cloned();

    let Some(baseline_value) = baseline_value else {
        return Ok(None);
    };

    let baseline = serde_json::from_value(baseline_value).map_err(|error| {
        Error::internal_io(
            format!(
                "Failed to deserialize baseline '{}': {}",
                config.key(),
                error
            ),
            Some("baseline.load".to_string()),
        )
    })?;

    Ok(Some(baseline))
}

pub(crate) fn read_json_or_empty(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(serde_json::Map::new()));
    }

    let content = std::fs::read_to_string(path).map_err(|error| {
        Error::internal_io(
            format!("Failed to read {}: {}", path.display(), error),
            Some("baseline.read_json".to_string()),
        )
    })?;

    if content.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }

    serde_json::from_str(&content).map_err(|error| {
        Error::internal_io(
            format!("Failed to parse {}: {}", path.display(), error),
            Some("baseline.read_json".to_string()),
        )
    })
}
