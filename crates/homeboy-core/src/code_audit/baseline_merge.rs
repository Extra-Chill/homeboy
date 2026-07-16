//! Conflict-aware merge for generated audit baseline data in `homeboy.json`.
//!
//! Issue #3515 added a deterministic audit baseline refresh command that
//! intentionally *bails* when `homeboy.json` already contains merge-conflict
//! markers. This module is the follow-up (#3518): when the only conflicting
//! content is the generated `baselines.audit` data, the conflict can be resolved
//! deterministically rather than requiring manual intervention.
//!
//! The generated baseline is a deterministic projection of audit findings — its
//! `known_fingerprints` are a sorted/deduped set. The canonical merge of two
//! conflicting baseline arrays is therefore the *union* of their fingerprints
//! (each side accepted some debt; the merge accepts the union). Non-baseline
//! component configuration is never guessed at: if `ours` and `theirs` differ in
//! anything outside `baselines`, the merge refuses and asks for manual
//! resolution.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

/// Key under which all generated baselines live in `homeboy.json`.
const BASELINES_KEY: &str = "baselines";

/// Outcome of attempting a baseline-only conflict merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineMergeResult {
    /// Fully merged `homeboy.json` document.
    pub merged: Value,
    /// Fingerprints present in the merge that were absent from the base stage.
    pub added_fingerprints: Vec<String>,
    /// Fingerprints present in the base stage that are absent from the merge.
    pub resolved_fingerprints: Vec<String>,
}

/// Why a baseline-only conflict merge was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineMergeError {
    /// `ours`/`theirs` differ outside the generated `baselines` section.
    NonBaselineConflict {
        /// Top-level keys (or `baselines.*` sub-keys) that conflict beyond audit baselines.
        conflicting_keys: Vec<String>,
    },
    /// A conflict stage could not be parsed as a JSON object.
    InvalidJson {
        stage: &'static str,
        message: String,
    },
}

impl std::fmt::Display for BaselineMergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BaselineMergeError::NonBaselineConflict { conflicting_keys } => write!(
                f,
                "homeboy.json has non-baseline conflicts in: {}. Resolve these manually, then rerun.",
                conflicting_keys.join(", ")
            ),
            BaselineMergeError::InvalidJson { stage, message } => {
                write!(f, "failed to parse {stage} homeboy.json: {message}")
            }
        }
    }
}

/// Attempt to resolve a baseline-only `homeboy.json` conflict deterministically.
///
/// `base` is the merge-base stage (`:1:`), `ours` is the current branch (`:2:`),
/// and `theirs` is the incoming branch (`:3:`). When `base` is absent (no common
/// ancestor recorded for the path) pass `None`; counts are then computed against
/// `ours` so the summary still reports what the incoming side added.
///
/// Returns the merged document on success, or refuses with a clear error when the
/// sides disagree on anything outside generated baseline data.
pub fn merge_baseline_only_conflict(
    base: Option<&Value>,
    ours: &Value,
    theirs: &Value,
) -> Result<BaselineMergeResult, BaselineMergeError> {
    let ours_obj = as_object(ours, "ours")?;
    let theirs_obj = as_object(theirs, "theirs")?;

    // 1. Everything outside `baselines` must be identical between the two sides.
    let conflicting_keys = non_baseline_conflicting_keys(ours_obj, theirs_obj);
    if !conflicting_keys.is_empty() {
        return Err(BaselineMergeError::NonBaselineConflict { conflicting_keys });
    }

    // 2. Within `baselines`, only generated baseline payloads may differ. Any
    //    other `baselines.*` key conflict (a foreign baseline domain) is treated
    //    as a non-baseline conflict so we never guess at unrelated generated data
    //    we don't own the merge semantics for.
    let baseline_key_conflicts = conflicting_baseline_subkeys(ours_obj, theirs_obj);
    if !baseline_key_conflicts.is_empty() {
        return Err(BaselineMergeError::NonBaselineConflict {
            conflicting_keys: baseline_key_conflicts,
        });
    }

    // 3. Deterministically merge each generated baseline payload (union of
    //    fingerprints). Start from `ours` so non-baseline config is preserved
    //    verbatim from the resolved/current side.
    let mut merged_obj = ours_obj.clone();
    let mut all_added: BTreeSet<String> = BTreeSet::new();
    let mut all_resolved: BTreeSet<String> = BTreeSet::new();

    let ours_baselines = baselines_map(ours_obj);
    let theirs_baselines = baselines_map(theirs_obj);
    let base_baselines = base
        .and_then(|value| value.as_object())
        .map(baselines_map)
        .unwrap_or_default();

    let mut merged_baselines = Map::new();
    let baseline_keys: BTreeSet<&String> = ours_baselines
        .keys()
        .chain(theirs_baselines.keys())
        .collect();

    for key in baseline_keys {
        let ours_payload = ours_baselines.get(key);
        let theirs_payload = theirs_baselines.get(key);
        let base_payload = base_baselines.get(key);

        let merged_payload = match (ours_payload, theirs_payload) {
            (Some(ours_value), Some(theirs_value)) => {
                let (payload, added, resolved) =
                    merge_baseline_payload(base_payload, ours_value, theirs_value);
                all_added.extend(added);
                all_resolved.extend(resolved);
                payload
            }
            // Only one side has this baseline domain — take it as-is.
            (Some(only), None) | (None, Some(only)) => (*only).clone(),
            (None, None) => continue,
        };

        merged_baselines.insert(key.clone(), merged_payload);
    }

    if merged_baselines.is_empty() {
        merged_obj.remove(BASELINES_KEY);
    } else {
        merged_obj.insert(BASELINES_KEY.to_string(), Value::Object(merged_baselines));
    }

    Ok(BaselineMergeResult {
        merged: Value::Object(merged_obj),
        added_fingerprints: all_added.into_iter().collect(),
        resolved_fingerprints: all_resolved.into_iter().collect(),
    })
}

/// Deterministically merge a single generated baseline payload.
///
/// Returns the merged payload plus the fingerprints added/resolved relative to
/// `base` (or `ours` when no base is available).
fn merge_baseline_payload(
    base: Option<&Value>,
    ours: &Value,
    theirs: &Value,
) -> (Value, Vec<String>, Vec<String>) {
    let ours_fps = fingerprints_of(ours);
    let theirs_fps = fingerprints_of(theirs);

    // Canonical merge of two deterministic baseline arrays = union.
    let merged_fps: BTreeSet<String> = ours_fps.union(&theirs_fps).cloned().collect();

    let comparison_base: BTreeSet<String> = match base {
        Some(value) => fingerprints_of(value),
        None => ours_fps.clone(),
    };

    let added: Vec<String> = merged_fps.difference(&comparison_base).cloned().collect();
    let resolved: Vec<String> = comparison_base.difference(&merged_fps).cloned().collect();

    // Prefer `theirs` as the structural template when it is the newer side
    // (incoming change), but overwrite the generated arrays/count deterministically.
    // `ours` and `theirs` only differ in generated baseline data here, so either
    // template is equivalent for non-array metadata; use `ours` to stay anchored
    // to the resolved working tree.
    let mut payload = ours.clone();
    let sorted: Vec<Value> = merged_fps
        .iter()
        .map(|fp| Value::String(fp.clone()))
        .collect();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("known_fingerprints".to_string(), Value::Array(sorted));
        obj.insert(
            "item_count".to_string(),
            Value::Number(serde_json::Number::from(merged_fps.len())),
        );
    }

    (payload, added, resolved)
}

/// Extract the sorted set of `known_fingerprints` from a baseline payload.
fn fingerprints_of(payload: &Value) -> BTreeSet<String> {
    payload
        .get("known_fingerprints")
        .and_then(|value| value.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Top-level keys (excluding `baselines`) where `ours` and `theirs` disagree.
fn non_baseline_conflicting_keys(
    ours: &Map<String, Value>,
    theirs: &Map<String, Value>,
) -> Vec<String> {
    let keys: BTreeSet<&String> = ours
        .keys()
        .chain(theirs.keys())
        .filter(|key| key.as_str() != BASELINES_KEY)
        .collect();

    keys.into_iter()
        .filter(|key| ours.get(*key) != theirs.get(*key))
        .cloned()
        .collect()
}

/// `baselines.*` sub-keys that conflict for a domain we do not own the merge of.
///
/// We only know how to deterministically merge *generated audit-style* baseline
/// payloads (those exposing a `known_fingerprints` array on both sides). Any
/// other conflicting baseline sub-key — or a payload that doesn't look like a
/// fingerprint baseline — is reported so the caller refuses rather than guesses.
fn conflicting_baseline_subkeys(
    ours: &Map<String, Value>,
    theirs: &Map<String, Value>,
) -> Vec<String> {
    let ours_baselines = baselines_map(ours);
    let theirs_baselines = baselines_map(theirs);

    let keys: BTreeSet<&String> = ours_baselines
        .keys()
        .chain(theirs_baselines.keys())
        .collect();

    keys.into_iter()
        .filter(|key| {
            let ours_payload = ours_baselines.get(*key);
            let theirs_payload = theirs_baselines.get(*key);
            if ours_payload == theirs_payload {
                return false;
            }
            // Both sides present and both look like fingerprint baselines → mergeable.
            match (ours_payload, theirs_payload) {
                (Some(o), Some(t)) => !(is_fingerprint_baseline(o) && is_fingerprint_baseline(t)),
                // One-sided baseline domains are taken as-is, not a conflict.
                _ => false,
            }
        })
        .map(|key| format!("{BASELINES_KEY}.{key}"))
        .collect()
}

/// A payload is a mergeable fingerprint baseline if it exposes `known_fingerprints`.
fn is_fingerprint_baseline(payload: &Value) -> bool {
    payload
        .get("known_fingerprints")
        .and_then(|value| value.as_array())
        .is_some()
}

/// Borrow the `baselines` object map, or an empty map when absent.
fn baselines_map(root: &Map<String, Value>) -> Map<String, Value> {
    root.get(BASELINES_KEY)
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default()
}

fn as_object<'a>(
    value: &'a Value,
    stage: &'static str,
) -> Result<&'a Map<String, Value>, BaselineMergeError> {
    value
        .as_object()
        .ok_or_else(|| BaselineMergeError::InvalidJson {
            stage,
            message: "root is not a JSON object".to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn baseline_payload(fingerprints: &[&str]) -> Value {
        json!({
            "created_at": "2026-06-04T00:00:00Z",
            "context_id": "homeboy",
            "item_count": fingerprints.len(),
            "known_fingerprints": fingerprints,
            "metadata": { "outliers_count": fingerprints.len() }
        })
    }

    fn doc_with_audit(config: Value, fingerprints: &[&str]) -> Value {
        let mut root = config.as_object().cloned().unwrap_or_default();
        root.insert(
            BASELINES_KEY.to_string(),
            json!({ "audit": baseline_payload(fingerprints) }),
        );
        Value::Object(root)
    }

    #[test]
    fn merges_baseline_only_conflict_as_union() {
        let config = json!({ "name": "homeboy", "version": "1.0.0" });
        let base = doc_with_audit(config.clone(), &["a", "b"]);
        let ours = doc_with_audit(config.clone(), &["a", "b", "c"]);
        let theirs = doc_with_audit(config.clone(), &["a", "b", "d"]);

        let result = merge_baseline_only_conflict(Some(&base), &ours, &theirs).unwrap();

        let merged_fps = fingerprints_of(
            result
                .merged
                .get("baselines")
                .unwrap()
                .get("audit")
                .unwrap(),
        );
        assert_eq!(
            merged_fps,
            ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect()
        );
        // Non-baseline config preserved verbatim.
        assert_eq!(result.merged.get("name"), config.get("name"));
        assert_eq!(result.merged.get("version"), config.get("version"));
        assert_eq!(
            result.added_fingerprints,
            vec!["c".to_string(), "d".to_string()]
        );
        assert!(result.resolved_fingerprints.is_empty());
    }

    #[test]
    fn item_count_recomputed_after_merge() {
        let config = json!({ "name": "homeboy" });
        let ours = doc_with_audit(config.clone(), &["a", "b", "c"]);
        let theirs = doc_with_audit(config, &["a", "d"]);

        let result = merge_baseline_only_conflict(None, &ours, &theirs).unwrap();
        let count = result
            .merged
            .get("baselines")
            .unwrap()
            .get("audit")
            .unwrap()
            .get("item_count")
            .unwrap()
            .as_u64()
            .unwrap();
        assert_eq!(count, 4);
    }

    #[test]
    fn reports_resolved_relative_to_base() {
        // ours dropped `b`, theirs dropped nothing → union keeps b, but a base
        // fingerprint removed by BOTH sides would show as resolved.
        let config = json!({ "name": "homeboy" });
        let base = doc_with_audit(config.clone(), &["a", "b", "stale"]);
        let ours = doc_with_audit(config.clone(), &["a", "b"]);
        let theirs = doc_with_audit(config, &["a", "b"]);

        let result = merge_baseline_only_conflict(Some(&base), &ours, &theirs).unwrap();
        assert_eq!(result.resolved_fingerprints, vec!["stale".to_string()]);
        assert!(result.added_fingerprints.is_empty());
    }

    #[test]
    fn refuses_non_baseline_config_conflict() {
        let ours = doc_with_audit(json!({ "name": "homeboy", "lint": "strict" }), &["a"]);
        let theirs = doc_with_audit(json!({ "name": "homeboy", "lint": "loose" }), &["a", "b"]);

        let error = merge_baseline_only_conflict(None, &ours, &theirs).unwrap_err();
        match error {
            BaselineMergeError::NonBaselineConflict { conflicting_keys } => {
                assert_eq!(conflicting_keys, vec!["lint".to_string()]);
            }
            other => panic!("expected non-baseline conflict, got {other:?}"),
        }
    }

    #[test]
    fn refuses_foreign_baseline_domain_conflict() {
        let mut ours = doc_with_audit(json!({ "name": "homeboy" }), &["a"]);
        let mut theirs = doc_with_audit(json!({ "name": "homeboy" }), &["a"]);
        // A non-fingerprint baseline domain that conflicts between the sides.
        ours.get_mut("baselines")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("custom".to_string(), json!({ "threshold": 10 }));
        theirs
            .get_mut("baselines")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("custom".to_string(), json!({ "threshold": 20 }));

        let error = merge_baseline_only_conflict(None, &ours, &theirs).unwrap_err();
        match error {
            BaselineMergeError::NonBaselineConflict { conflicting_keys } => {
                assert_eq!(conflicting_keys, vec!["baselines.custom".to_string()]);
            }
            other => panic!("expected baseline subkey conflict, got {other:?}"),
        }
    }

    #[test]
    fn takes_one_sided_baseline_domain() {
        let mut ours = doc_with_audit(json!({ "name": "homeboy" }), &["a", "b"]);
        let theirs = doc_with_audit(json!({ "name": "homeboy" }), &["a", "c"]);
        // `lint` baseline exists only on our side — should survive untouched.
        ours.get_mut("baselines")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("lint".to_string(), baseline_payload(&["lint-x"]));

        let result = merge_baseline_only_conflict(None, &ours, &theirs).unwrap();
        let lint = result.merged.get("baselines").unwrap().get("lint").unwrap();
        assert_eq!(
            fingerprints_of(lint),
            ["lint-x".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn no_conflict_identical_sides_passes_through() {
        let config = json!({ "name": "homeboy" });
        let ours = doc_with_audit(config.clone(), &["a", "b"]);
        let theirs = doc_with_audit(config, &["a", "b"]);

        let result = merge_baseline_only_conflict(None, &ours, &theirs).unwrap();
        assert!(result.added_fingerprints.is_empty());
        assert!(result.resolved_fingerprints.is_empty());
        assert_eq!(
            fingerprints_of(
                result
                    .merged
                    .get("baselines")
                    .unwrap()
                    .get("audit")
                    .unwrap()
            ),
            ["a", "b"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    fn rejects_non_object_root() {
        let ours = json!([1, 2, 3]);
        let theirs = json!({ "name": "homeboy" });
        let error = merge_baseline_only_conflict(None, &ours, &theirs).unwrap_err();
        assert!(matches!(
            error,
            BaselineMergeError::InvalidJson { stage: "ours", .. }
        ));
    }
}
