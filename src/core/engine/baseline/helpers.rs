//! helpers — extracted from baseline.rs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::error::{Error, Result};
use super::NewItem;
use super::Comparison;
use super::Baseline;
use super::Fingerprintable;


pub fn compare<T: Fingerprintable, M: Serialize>(
    current_items: &[T],
    baseline: &Baseline<M>,
) -> Comparison {
    let baseline_set: HashSet<&String> = baseline.known_fingerprints.iter().collect();
    let current_fingerprints: Vec<String> = current_items
        .iter()
        .map(|item| item.fingerprint())
        .collect();
    let current_set: HashSet<&String> = current_fingerprints.iter().collect();

    let new_items = current_items
        .iter()
        .filter(|item| {
            let fingerprint = item.fingerprint();
            !baseline_set.contains(&fingerprint)
        })
        .map(|item| NewItem {
            fingerprint: item.fingerprint(),
            description: item.description(),
            context_label: item.context_label(),
        })
        .collect::<Vec<_>>();

    let resolved_fingerprints = baseline
        .known_fingerprints
        .iter()
        .filter(|fingerprint| !current_set.contains(fingerprint))
        .cloned()
        .collect::<Vec<_>>();

    let delta = current_items.len() as i64 - baseline.item_count as i64;

    Comparison {
        drift_increased: !new_items.is_empty(),
        new_items,
        resolved_fingerprints,
        delta,
    }
}

pub(crate) fn write_json(path: &Path, value: &Value) -> Result<()> {
    let content = serde_json::to_string_pretty(value).map_err(|error| {
        Error::internal_io(
            format!("Failed to serialize {}: {}", path.display(), error),
            Some("baseline.write_json".to_string()),
        )
    })?;

    std::fs::write(path, content).map_err(|error| {
        Error::internal_io(
            format!("Failed to write {}: {}", path.display(), error),
            Some("baseline.write_json".to_string()),
        )
    })
}

pub fn load_from_git_ref<M: for<'de> Deserialize<'de> + Serialize>(
    source_path: &str,
    git_ref: &str,
    key: &str,
) -> Option<Baseline<M>> {
    let git_spec = format!("{}:{}", git_ref, HOMEBOY_JSON);
    let content =
        crate::engine::command::run_in_optional(source_path, "git", &["show", &git_spec])?;

    let root: Value = serde_json::from_str(&content).ok()?;
    let value = root.get(BASELINES_KEY)?.get(key)?;
    serde_json::from_value::<Baseline<M>>(value.clone()).ok()
}
