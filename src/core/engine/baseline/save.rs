//! save — extracted from baseline.rs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::error::{Error, Result};
use super::Baseline;
use super::Fingerprintable;
use super::BaselineConfig;


pub fn save<M: Serialize + for<'de> Deserialize<'de>>(
    config: &BaselineConfig,
    context_id: &str,
    items: &[impl Fingerprintable],
    metadata: M,
) -> Result<PathBuf> {
    let mut known_fingerprints: Vec<String> = items.iter().map(|item| item.fingerprint()).collect();
    known_fingerprints.sort();

    if !known_fingerprints.is_empty() {
        if let Ok(Some(existing)) = load::<M>(config) {
            let mut existing_sorted = existing.known_fingerprints.clone();
            existing_sorted.sort();
            if existing_sorted == known_fingerprints {
                return Ok(config.json_path());
            }
        }
    }

    let baseline = Baseline {
        created_at: utc_now_iso8601(),
        context_id: context_id.to_string(),
        item_count: items.len(),
        known_fingerprints,
        metadata,
    };

    let baseline_value = serde_json::to_value(&baseline).map_err(|error| {
        Error::internal_io(
            format!("Failed to serialize baseline: {}", error),
            Some("baseline.save".to_string()),
        )
    })?;

    let json_path = config.json_path();
    let mut root = read_json_or_empty(&json_path)?;

    let baselines = root
        .as_object_mut()
        .ok_or_else(|| {
            Error::internal_io(
                "homeboy.json root is not an object".to_string(),
                Some("baseline.save".to_string()),
            )
        })?
        .entry(BASELINES_KEY)
        .or_insert_with(|| Value::Object(serde_json::Map::new()));

    baselines
        .as_object_mut()
        .ok_or_else(|| {
            Error::internal_io(
                "baselines key is not an object".to_string(),
                Some("baseline.save".to_string()),
            )
        })?
        .insert(config.key.clone(), baseline_value);

    write_json(&json_path, &root)?;

    Ok(json_path)
}

pub fn save_scoped<M: Serialize + for<'de> Deserialize<'de> + Clone>(
    config: &BaselineConfig,
    context_id: &str,
    current_items: &[impl Fingerprintable],
    metadata: M,
    scope: &[String],
    file_from_fingerprint: impl Fn(&str) -> Option<String>,
) -> Result<PathBuf> {
    let json_path = config.json_path();
    let existing: Option<Baseline<M>> = load(config)?;
    let Some(existing) = existing else {
        return save(config, context_id, current_items, metadata);
    };

    let scope_set: HashSet<&str> = scope.iter().map(|value| value.as_str()).collect();
    let existing_fingerprints_snapshot = existing.known_fingerprints.clone();

    let mut merged_fingerprints: Vec<String> = existing
        .known_fingerprints
        .into_iter()
        .filter(|fingerprint| {
            file_from_fingerprint(fingerprint)
                .as_deref()
                .is_none_or(|file| !scope_set.contains(file))
        })
        .collect();

    for item in current_items {
        merged_fingerprints.push(item.fingerprint());
    }

    merged_fingerprints.sort();
    merged_fingerprints.dedup();

    let mut existing_sorted = existing_fingerprints_snapshot.clone();
    existing_sorted.sort();
    if existing_sorted == merged_fingerprints {
        return Ok(json_path);
    }

    let baseline = Baseline {
        created_at: utc_now_iso8601(),
        context_id: context_id.to_string(),
        item_count: merged_fingerprints.len(),
        known_fingerprints: merged_fingerprints,
        metadata,
    };

    let baseline_value = serde_json::to_value(&baseline).map_err(|error| {
        Error::internal_io(
            format!("Failed to serialize scoped baseline: {}", error),
            Some("baseline.save_scoped".to_string()),
        )
    })?;

    let mut root = read_json_or_empty(&json_path)?;
    let baselines = root
        .as_object_mut()
        .ok_or_else(|| {
            Error::internal_io(
                "homeboy.json root is not an object".to_string(),
                Some("baseline.save_scoped".to_string()),
            )
        })?
        .entry(BASELINES_KEY)
        .or_insert_with(|| Value::Object(serde_json::Map::new()));

    baselines
        .as_object_mut()
        .ok_or_else(|| {
            Error::internal_io(
                "baselines key is not an object".to_string(),
                Some("baseline.save_scoped".to_string()),
            )
        })?
        .insert(config.key.clone(), baseline_value);

    write_json(&json_path, &root)?;

    Ok(json_path)
}
