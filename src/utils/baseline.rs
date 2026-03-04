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
//! use homeboy::utils::baseline::{self, Fingerprintable, BaselineConfig};
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
//!
//! // First run: save baseline
//! baseline::save(&config, "my-component", &items, my_metadata)?;
//!
//! // Subsequent runs: compare
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

// ============================================================================
// Trait: Fingerprintable
// ============================================================================

/// Any item that can produce a stable identity for baseline comparison.
///
/// The fingerprint must be stable across runs for the same logical item.
/// Volatile values (line counts, timestamps) should be excluded.
pub trait Fingerprintable {
    /// Stable identity string for this item.
    ///
    /// Two items with the same fingerprint are considered "the same finding."
    /// Exclude volatile values — if line counts change but the finding is
    /// conceptually the same, the fingerprint must not change.
    fn fingerprint(&self) -> String;

    /// Human-readable description for reporting.
    fn description(&self) -> String;

    /// Context label (e.g., convention name, lint rule, category).
    /// Used in [`NewItem`] for human-readable reports.
    fn context_label(&self) -> String;
}

// ============================================================================
// Config
// ============================================================================

/// Configuration for a baseline: where to store it and under what key.
///
/// Baselines are stored in `homeboy.json` under `baselines.<key>`.
pub struct BaselineConfig {
    /// Root directory containing `homeboy.json`.
    root: PathBuf,
    /// Key within the `baselines` object (e.g., "audit", "lint", "test").
    key: String,
}

const HOMEBOY_JSON: &str = "homeboy.json";
const BASELINES_KEY: &str = "baselines";

impl BaselineConfig {
    /// Create a baseline config.
    ///
    /// - `root`: directory containing `homeboy.json` (component source path)
    /// - `key`: baseline key within `baselines` object (e.g., "audit", "lint")
    pub fn new(root: impl Into<PathBuf>, key: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            key: key.into(),
        }
    }

    /// Path to the `homeboy.json` file.
    pub fn json_path(&self) -> PathBuf {
        self.root.join(HOMEBOY_JSON)
    }

    /// The baseline key.
    pub fn key(&self) -> &str {
        &self.key
    }
}

// ============================================================================
// Stored baseline
// ============================================================================

/// A saved baseline snapshot.
///
/// `M` is caller-defined metadata (e.g., alignment score, coverage percentage).
/// Use `()` if no metadata is needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline<M: Serialize> {
    /// ISO 8601 timestamp when the baseline was created.
    pub created_at: String,
    /// Identifier for what was baselined (component ID, lint target, etc.).
    pub context_id: String,
    /// Total item count at baseline time.
    pub item_count: usize,
    /// Fingerprints of all known items (the snapshot).
    pub known_fingerprints: Vec<String>,
    /// Domain-specific metadata (alignment score, coverage %, etc.).
    pub metadata: M,
}

// ============================================================================
// Comparison result
// ============================================================================

/// Result of comparing current items against a saved baseline.
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    /// Items that are new since the baseline.
    pub new_items: Vec<NewItem>,
    /// Fingerprints that were in the baseline but are now gone (resolved).
    pub resolved_fingerprints: Vec<String>,
    /// Net change in item count (positive = more items, negative = fewer).
    pub delta: i64,
    /// Whether drift increased (true = new items appeared = ratchet failure).
    pub drift_increased: bool,
}

/// An item that wasn't in the baseline.
#[derive(Debug, Clone, Serialize)]
pub struct NewItem {
    /// The item's fingerprint.
    pub fingerprint: String,
    /// Human-readable description.
    pub description: String,
    /// Context label (convention, rule, category).
    pub context_label: String,
}

// ============================================================================
// Core operations
// ============================================================================

/// Save a baseline snapshot into `homeboy.json`.
///
/// Reads the existing `homeboy.json` (or creates one), sets
/// `baselines.<key>` to the new baseline, and writes it back.
pub fn save<M: Serialize>(
    config: &BaselineConfig,
    context_id: &str,
    items: &[impl Fingerprintable],
    metadata: M,
) -> Result<PathBuf> {
    let known_fingerprints: Vec<String> = items.iter().map(|i| i.fingerprint()).collect();

    let baseline = Baseline {
        created_at: utc_now_iso8601(),
        context_id: context_id.to_string(),
        item_count: items.len(),
        known_fingerprints,
        metadata,
    };

    let baseline_value = serde_json::to_value(&baseline).map_err(|e| {
        Error::internal_io(
            format!("Failed to serialize baseline: {}", e),
            Some("baseline.save".to_string()),
        )
    })?;

    // Read existing homeboy.json or start fresh
    let json_path = config.json_path();
    let mut root = read_json_or_empty(&json_path)?;

    // Ensure baselines object exists
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

    // Set the specific baseline key
    baselines
        .as_object_mut()
        .ok_or_else(|| {
            Error::internal_io(
                "baselines key is not an object".to_string(),
                Some("baseline.save".to_string()),
            )
        })?
        .insert(config.key.clone(), baseline_value);

    // Write back
    write_json(&json_path, &root)?;

    Ok(json_path)
}

/// Load a baseline if one exists in `homeboy.json`.
///
/// Returns `Ok(None)` if:
/// - No `homeboy.json` exists
/// - No `baselines` key exists
/// - No baseline for the configured key exists
///
/// Returns `Err` if the file exists but the baseline data is malformed.
pub fn load<M: for<'de> Deserialize<'de> + Serialize>(
    config: &BaselineConfig,
) -> Result<Option<Baseline<M>>> {
    let json_path = config.json_path();
    if !json_path.exists() {
        return Ok(None);
    }

    let root = read_json_or_empty(&json_path)?;

    let baseline_value = root.get(BASELINES_KEY).and_then(|b| b.get(&config.key));

    let Some(value) = baseline_value else {
        return Ok(None);
    };

    let baseline: Baseline<M> = serde_json::from_value(value.clone()).map_err(|e| {
        Error::internal_io(
            format!(
                "Malformed baseline '{}' in {}: {}",
                config.key,
                json_path.display(),
                e
            ),
            Some("baseline.load".to_string()),
        )
    })?;

    Ok(Some(baseline))
}

/// Compare current items against a saved baseline.
///
/// This is the ratchet check:
/// - `drift_increased = true` if ANY new items appeared (not in baseline)
/// - Resolved items (in baseline but not in current) are tracked but don't fail
/// - `delta` is the net change in item count
pub fn compare<M: Serialize>(
    current_items: &[impl Fingerprintable],
    baseline: &Baseline<M>,
) -> Comparison {
    let current_fingerprints: HashSet<String> =
        current_items.iter().map(|i| i.fingerprint()).collect();
    let baseline_fingerprints: HashSet<String> =
        baseline.known_fingerprints.iter().cloned().collect();

    // New = in current but not in baseline
    let new_items: Vec<NewItem> = current_items
        .iter()
        .filter(|item| !baseline_fingerprints.contains(&item.fingerprint()))
        .map(|item| NewItem {
            fingerprint: item.fingerprint(),
            description: item.description(),
            context_label: item.context_label(),
        })
        .collect();

    // Resolved = in baseline but not in current
    let resolved_fingerprints: Vec<String> = baseline_fingerprints
        .difference(&current_fingerprints)
        .cloned()
        .collect();

    let delta = current_items.len() as i64 - baseline.item_count as i64;
    let drift_increased = !new_items.is_empty();

    Comparison {
        new_items,
        resolved_fingerprints,
        delta,
        drift_increased,
    }
}

// ============================================================================
// JSON helpers
// ============================================================================

/// Read homeboy.json as a serde_json::Value, or return an empty object.
fn read_json_or_empty(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(serde_json::Map::new()));
    }

    let content = std::fs::read_to_string(path).map_err(|e| {
        Error::internal_io(
            format!("Failed to read {}: {}", path.display(), e),
            Some("baseline.read_json".to_string()),
        )
    })?;

    serde_json::from_str(&content).map_err(|e| {
        Error::internal_io(
            format!("Failed to parse {}: {}", path.display(), e),
            Some("baseline.read_json".to_string()),
        )
    })
}

/// Write a serde_json::Value to homeboy.json with pretty formatting.
fn write_json(path: &Path, value: &Value) -> Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(|e| {
        Error::internal_io(
            format!("Failed to serialize JSON: {}", e),
            Some("baseline.write_json".to_string()),
        )
    })?;

    // Ensure trailing newline (git-friendly)
    let content = if json.ends_with('\n') {
        json
    } else {
        format!("{}\n", json)
    };

    std::fs::write(path, content).map_err(|e| {
        Error::internal_io(
            format!("Failed to write {}: {}", path.display(), e),
            Some("baseline.write_json".to_string()),
        )
    })?;

    Ok(())
}

// ============================================================================
// Timestamp helpers
// ============================================================================

/// Get current UTC timestamp as ISO 8601 (no external dependencies).
pub fn utc_now_iso8601() -> String {
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

/// Convert days since Unix epoch to (year, month, day).
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
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month, days + 1)
}

fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -- Test implementation of Fingerprintable -------------------------------

    #[derive(Clone)]
    struct TestItem {
        category: String,
        file: String,
        message: String,
    }

    impl Fingerprintable for TestItem {
        fn fingerprint(&self) -> String {
            format!("{}::{}", self.category, self.file)
        }
        fn description(&self) -> String {
            self.message.clone()
        }
        fn context_label(&self) -> String {
            self.category.clone()
        }
    }

    fn item(category: &str, file: &str, message: &str) -> TestItem {
        TestItem {
            category: category.to_string(),
            file: file.to_string(),
            message: message.to_string(),
        }
    }

    // -- Save & Load ----------------------------------------------------------

    #[test]
    fn save_creates_homeboy_json() {
        let dir = TempDir::new().unwrap();
        let config = BaselineConfig::new(dir.path(), "audit");
        let items = vec![
            item("lint", "a.rs", "unused import"),
            item("lint", "b.rs", "dead code"),
        ];

        let path = save(&config, "test-component", &items, ()).unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().ends_with("homeboy.json"));
    }

    #[test]
    fn load_returns_none_when_no_file() {
        let dir = TempDir::new().unwrap();
        let config = BaselineConfig::new(dir.path(), "audit");
        let result = load::<()>(&config).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_returns_none_when_no_baselines_key() {
        let dir = TempDir::new().unwrap();
        // Write a homeboy.json without baselines
        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{"remote_path": "wp-content/plugins/test"}"#,
        )
        .unwrap();

        let config = BaselineConfig::new(dir.path(), "audit");
        let result = load::<()>(&config).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_returns_none_when_key_missing() {
        let dir = TempDir::new().unwrap();
        // Write homeboy.json with baselines but different key
        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{"baselines": {"lint": {"created_at": "x", "context_id": "y", "item_count": 0, "known_fingerprints": [], "metadata": null}}}"#,
        )
        .unwrap();

        let config = BaselineConfig::new(dir.path(), "audit");
        let result = load::<()>(&config).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = TempDir::new().unwrap();
        let config = BaselineConfig::new(dir.path(), "audit");
        let items = vec![
            item("lint", "a.rs", "unused import"),
            item("lint", "b.rs", "dead code"),
        ];

        save(&config, "my-component", &items, ()).unwrap();
        let loaded = load::<()>(&config).unwrap().unwrap();

        assert_eq!(loaded.context_id, "my-component");
        assert_eq!(loaded.item_count, 2);
        assert_eq!(loaded.known_fingerprints.len(), 2);
        assert!(loaded
            .known_fingerprints
            .contains(&"lint::a.rs".to_string()));
        assert!(loaded
            .known_fingerprints
            .contains(&"lint::b.rs".to_string()));
    }

    #[test]
    fn save_preserves_existing_config() {
        let dir = TempDir::new().unwrap();

        // Pre-existing homeboy.json with component config
        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{
  "remote_path": "wp-content/plugins/data-machine",
  "extensions": { "wordpress": {} }
}"#,
        )
        .unwrap();

        let config = BaselineConfig::new(dir.path(), "audit");
        let items = vec![item("conv", "a.php", "missing method")];
        save(&config, "data-machine", &items, ()).unwrap();

        // Re-read the full file and verify existing keys are preserved
        let content = std::fs::read_to_string(dir.path().join("homeboy.json")).unwrap();
        let root: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            root.get("remote_path").and_then(|v| v.as_str()),
            Some("wp-content/plugins/data-machine")
        );
        assert!(root.get("extensions").is_some());
        assert!(root.get("baselines").is_some());
        assert!(root.get("baselines").unwrap().get("audit").is_some());
    }

    #[test]
    fn save_preserves_other_baselines() {
        let dir = TempDir::new().unwrap();
        let audit_config = BaselineConfig::new(dir.path(), "audit");
        let lint_config = BaselineConfig::new(dir.path(), "lint");

        // Save audit baseline
        let audit_items = vec![item("conv", "a.php", "missing method")];
        save(&audit_config, "test", &audit_items, ()).unwrap();

        // Save lint baseline
        let lint_items = vec![item("psr12", "b.php", "wrong indent")];
        save(&lint_config, "test", &lint_items, ()).unwrap();

        // Both should exist
        let audit = load::<()>(&audit_config).unwrap().unwrap();
        let lint = load::<()>(&lint_config).unwrap().unwrap();

        assert_eq!(audit.item_count, 1);
        assert_eq!(lint.item_count, 1);
        assert!(audit
            .known_fingerprints
            .contains(&"conv::a.php".to_string()));
        assert!(lint
            .known_fingerprints
            .contains(&"psr12::b.php".to_string()));
    }

    #[test]
    fn save_with_typed_metadata_roundtrips() {
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
        struct AuditMeta {
            alignment_score: f32,
            outlier_count: usize,
        }

        let dir = TempDir::new().unwrap();
        let config = BaselineConfig::new(dir.path(), "audit");
        let items = vec![item("conv", "a.php", "missing method")];
        let meta = AuditMeta {
            alignment_score: 0.85,
            outlier_count: 3,
        };

        save(&config, "data-machine", &items, meta.clone()).unwrap();
        let loaded = load::<AuditMeta>(&config).unwrap().unwrap();

        assert_eq!(loaded.metadata, meta);
    }

    // -- Compare: no drift ----------------------------------------------------

    #[test]
    fn compare_identical_items_shows_no_drift() {
        let items = vec![
            item("lint", "a.rs", "unused import"),
            item("lint", "b.rs", "dead code"),
        ];

        let dir = TempDir::new().unwrap();
        let config = BaselineConfig::new(dir.path(), "test");
        save(&config, "test", &items, ()).unwrap();
        let baseline = load::<()>(&config).unwrap().unwrap();

        let comparison = compare(&items, &baseline);
        assert!(!comparison.drift_increased);
        assert!(comparison.new_items.is_empty());
        assert!(comparison.resolved_fingerprints.is_empty());
        assert_eq!(comparison.delta, 0);
    }

    // -- Compare: new drift ---------------------------------------------------

    #[test]
    fn compare_detects_new_items() {
        let original = vec![item("lint", "a.rs", "unused import")];

        let dir = TempDir::new().unwrap();
        let config = BaselineConfig::new(dir.path(), "test");
        save(&config, "test", &original, ()).unwrap();
        let baseline = load::<()>(&config).unwrap().unwrap();

        let current = vec![
            item("lint", "a.rs", "unused import"),
            item("lint", "c.rs", "missing docs"),
        ];

        let comparison = compare(&current, &baseline);
        assert!(comparison.drift_increased);
        assert_eq!(comparison.new_items.len(), 1);
        assert_eq!(comparison.new_items[0].fingerprint, "lint::c.rs");
        assert_eq!(comparison.delta, 1);
    }

    // -- Compare: resolved items ----------------------------------------------

    #[test]
    fn compare_detects_resolved_items() {
        let original = vec![
            item("lint", "a.rs", "unused import"),
            item("lint", "b.rs", "dead code"),
        ];

        let dir = TempDir::new().unwrap();
        let config = BaselineConfig::new(dir.path(), "test");
        save(&config, "test", &original, ()).unwrap();
        let baseline = load::<()>(&config).unwrap().unwrap();

        let current = vec![item("lint", "a.rs", "unused import")];

        let comparison = compare(&current, &baseline);
        assert!(!comparison.drift_increased);
        assert!(comparison.new_items.is_empty());
        assert_eq!(comparison.resolved_fingerprints.len(), 1);
        assert_eq!(comparison.delta, -1);
    }

    // -- Compare: new AND resolved simultaneously -----------------------------

    #[test]
    fn compare_new_and_resolved_simultaneously() {
        let original = vec![
            item("lint", "a.rs", "unused import"),
            item("lint", "b.rs", "dead code"),
        ];

        let dir = TempDir::new().unwrap();
        let config = BaselineConfig::new(dir.path(), "test");
        save(&config, "test", &original, ()).unwrap();
        let baseline = load::<()>(&config).unwrap().unwrap();

        // b.rs resolved, c.rs introduced
        let current = vec![
            item("lint", "a.rs", "unused import"),
            item("lint", "c.rs", "missing docs"),
        ];

        let comparison = compare(&current, &baseline);
        assert!(comparison.drift_increased); // new item = fail
        assert_eq!(comparison.new_items.len(), 1);
        assert_eq!(comparison.resolved_fingerprints.len(), 1);
        assert_eq!(comparison.delta, 0); // net zero, but still fails
    }

    // -- Fingerprint stability ------------------------------------------------

    #[test]
    fn fingerprint_ignores_description_changes() {
        let item1 = item("structural", "deploy.rs", "File has 2484 lines");
        let item2 = item("structural", "deploy.rs", "File has 2645 lines");
        assert_eq!(item1.fingerprint(), item2.fingerprint());
    }

    // -- Timestamp ------------------------------------------------------------

    #[test]
    fn utc_now_produces_valid_iso8601() {
        let now = utc_now_iso8601();
        assert_eq!(
            now.len(),
            20,
            "Expected 20 chars, got {}: {}",
            now.len(),
            now
        );
        assert!(now.ends_with('Z'));
        assert!(now.contains('T'));
    }

    // -- BaselineConfig -------------------------------------------------------

    #[test]
    fn config_json_path() {
        let config = BaselineConfig::new("/project/root", "audit");
        assert_eq!(
            config.json_path(),
            PathBuf::from("/project/root/homeboy.json")
        );
    }

    // -- Edge cases -----------------------------------------------------------

    #[test]
    fn save_overwrites_same_key() {
        let dir = TempDir::new().unwrap();
        let config = BaselineConfig::new(dir.path(), "audit");

        let items_v1 = vec![item("lint", "a.rs", "v1")];
        save(&config, "test", &items_v1, ()).unwrap();

        let items_v2 = vec![item("lint", "a.rs", "v2"), item("lint", "b.rs", "new")];
        save(&config, "test", &items_v2, ()).unwrap();

        let loaded = load::<()>(&config).unwrap().unwrap();
        assert_eq!(loaded.item_count, 2);
    }

    #[test]
    fn compare_empty_current_against_populated_baseline() {
        let original = vec![item("lint", "a.rs", "unused"), item("lint", "b.rs", "dead")];

        let dir = TempDir::new().unwrap();
        let config = BaselineConfig::new(dir.path(), "test");
        save(&config, "test", &original, ()).unwrap();
        let baseline = load::<()>(&config).unwrap().unwrap();

        let current: Vec<TestItem> = vec![];
        let comparison = compare(&current, &baseline);
        assert!(!comparison.drift_increased);
        assert_eq!(comparison.resolved_fingerprints.len(), 2);
        assert_eq!(comparison.delta, -2);
    }

    #[test]
    fn compare_populated_current_against_empty_baseline() {
        let original: Vec<TestItem> = vec![];

        let dir = TempDir::new().unwrap();
        let config = BaselineConfig::new(dir.path(), "test");
        save(&config, "test", &original, ()).unwrap();
        let baseline = load::<()>(&config).unwrap().unwrap();

        let current = vec![item("lint", "a.rs", "new finding")];
        let comparison = compare(&current, &baseline);
        assert!(comparison.drift_increased);
        assert_eq!(comparison.new_items.len(), 1);
        assert_eq!(comparison.delta, 1);
    }

    #[test]
    fn load_returns_error_for_malformed_baseline() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{"baselines": {"audit": "not a valid baseline object"}}"#,
        )
        .unwrap();

        let config = BaselineConfig::new(dir.path(), "audit");
        let result = load::<()>(&config);
        assert!(result.is_err());
    }

    #[test]
    fn load_returns_error_for_malformed_json() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("homeboy.json"), "not valid json {{{").unwrap();

        let config = BaselineConfig::new(dir.path(), "audit");
        let result = load::<()>(&config);
        assert!(result.is_err());
    }
}
