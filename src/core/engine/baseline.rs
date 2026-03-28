//! Generic baseline & ratchet primitive for drift detection.
//!
//! Captures a snapshot of "current state" (any set of fingerprintable items)
//! and compares future runs against it. Only NEW items (not in the baseline)
//! trigger a failure — resolved items are celebrated, same-state passes.
//!
//! Zero domain knowledge. The caller decides:
//! - What gets fingerprinted (via [`Fingerprintable`])
//! - What metadata to store (via the `M` type parameter)
//! - What key to use in `homeboy.json` (via [`BaselineConfig`])
//!
//! Baselines are stored in the project's `homeboy.json` under a `baselines`
//! key, keeping all component configuration in a single portable file.
//!
//! # Usage
//!
//! ```ignore
//! use homeboy::baseline::{self, Fingerprintable, BaselineConfig};
//!
//! impl Fingerprintable for MyFinding {
//!     fn fingerprint(&self) -> String {
//!         format!("{}::{}", self.category, self.file)
//!     }
//!     fn description(&self) -> String {
//!         self.message.clone()
//!     }
//!     fn context_label(&self) -> String {
//!         self.category.clone()
//!     }
//! }
//!
//! let config = BaselineConfig::new(source_path, "audit");
//! baseline::save(&config, "my-component", &items, my_metadata)?;
//! if let Some(saved) = baseline::load::<MyMeta>(&config)? {
//!     let comparison = baseline::compare(&items, &saved);
//!     if comparison.drift_increased {
//!         // CI fails — new findings introduced
//!     }
//! }
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

pub trait Fingerprintable {
    fn fingerprint(&self) -> String;
    fn description(&self) -> String;
    fn context_label(&self) -> String;
}

pub struct BaselineConfig {
    root: PathBuf,
    key: String,
}

const HOMEBOY_JSON: &str = "homeboy.json";
const BASELINES_KEY: &str = "baselines";

impl BaselineConfig {
    pub fn new(root: impl Into<PathBuf>, key: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            key: key.into(),
        }
    }

    pub fn json_path(&self) -> PathBuf {
        self.root.join(HOMEBOY_JSON)
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline<M: Serialize> {
    pub created_at: String,
    pub context_id: String,
    pub item_count: usize,
    pub known_fingerprints: Vec<String>,
    pub metadata: M,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub new_items: Vec<NewItem>,
    pub resolved_fingerprints: Vec<String>,
    pub delta: i64,
    pub drift_increased: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewItem {
    pub fingerprint: String,
    pub description: String,
    pub context_label: String,
}

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

fn read_json_or_empty(path: &Path) -> Result<Value> {
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

fn write_json(path: &Path, value: &Value) -> Result<()> {
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

fn utc_now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let secs_per_day = 86400u64;
    let secs_per_hour = 3600u64;
    let secs_per_min = 60u64;

    let days = now / secs_per_day;
    let remaining = now % secs_per_day;
    let hours = remaining / secs_per_hour;
    let remaining = remaining % secs_per_hour;
    let minutes = remaining / secs_per_min;
    let seconds = remaining % secs_per_min;

    let (year, month, day) = days_to_date(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

fn days_to_date(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap_year(year);
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31];

    let mut month = 1u64;
    for &month_days in &month_days {
        if days < month_days {
            break;
        }
        days -= month_days;
        month += 1;
    }

    (year, month, days + 1)
}

fn is_leap_year(year: u64) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}
