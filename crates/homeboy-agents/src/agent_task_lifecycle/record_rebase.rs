//! Rebase a stale record commit onto the record that won its revision race
//! (#17777).
//!
//! A writer prepares a record from the copy it read, and the store commits it
//! only while the stored revision is unchanged (#15718). Long-running writers,
//! such as Lab staging, recovery, and gate finalization, hold their copy while
//! heartbeats, progress, and handoff acceptance commit beside them. Refusing
//! those commits outright left Cooks terminal after their gates passed.
//!
//! The rebase is a three-way merge of JSON values: the copy the writer read
//! (`base`), what it prepared (`ours`), and what is stored now (`theirs`).
//! Each side's changes are kept. When both writers changed the same leaf to
//! different values, the merge refuses, except for `*_at` timestamps, which
//! take the later value.

use serde_json::{Map, Value};

use super::AgentTaskRunRecord;

/// `prepared` rebased onto `stored`, or `None` when `prepared` carries no
/// read base or both writers changed the same field differently.
pub(crate) fn rebase_onto(
    prepared: &AgentTaskRunRecord,
    stored: &AgentTaskRunRecord,
) -> Option<AgentTaskRunRecord> {
    let base = prepared.read_base.0.as_deref()?;
    let value = |record: &AgentTaskRunRecord| {
        let mut record = record.clone();
        record.revision = stored.revision;
        serde_json::to_value(record).ok()
    };
    let merged = merge(
        Some(&value(base)?),
        Some(&value(prepared)?),
        Some(&value(stored)?),
        "",
    )
    .ok()??;
    let mut rebased: AgentTaskRunRecord = serde_json::from_value(merged).ok()?;
    rebased.read_base = stored.read_base.clone();
    Some(rebased)
}

struct Conflict;

/// Three-way merge of one value. `None` is an absent key.
fn merge(
    base: Option<&Value>,
    ours: Option<&Value>,
    theirs: Option<&Value>,
    key: &str,
) -> Result<Option<Value>, Conflict> {
    if ours == base || ours == theirs {
        return Ok(theirs.cloned());
    }
    if theirs == base {
        return Ok(ours.cloned());
    }
    match (base, ours, theirs) {
        (Some(Value::Object(base)), Some(Value::Object(ours)), Some(Value::Object(theirs))) => {
            let mut merged = Map::new();
            for key in ours.keys().chain(theirs.keys()).chain(base.keys()) {
                if merged.contains_key(key) {
                    continue;
                }
                if let Some(value) = merge(base.get(key), ours.get(key), theirs.get(key), key)? {
                    merged.insert(key.clone(), value);
                }
            }
            Ok(Some(Value::Object(merged)))
        }
        (_, Some(Value::String(ours)), Some(Value::String(theirs))) if key.ends_with("_at") => {
            Ok(Some(Value::String(ours.max(theirs).clone())))
        }
        _ => Err(Conflict),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_keeps_both_writers_and_refuses_overlapping_changes() {
        let base = json!({"a": 1, "m": {"x": 1, "y": 1}, "updated_at": "2026-01-01T00:00:00Z"});
        let ours = json!({"a": 2, "m": {"x": 2, "y": 1}, "updated_at": "2026-01-01T00:00:05Z"});
        let theirs =
            json!({"a": 1, "m": {"x": 1, "y": 3, "z": 1}, "updated_at": "2026-01-01T00:00:09Z"});
        let merged = merge(Some(&base), Some(&ours), Some(&theirs), "")
            .ok()
            .flatten();
        assert_eq!(
            merged,
            Some(
                json!({"a": 2, "m": {"x": 2, "y": 3, "z": 1}, "updated_at": "2026-01-01T00:00:09Z"})
            )
        );
        let clash = json!({"a": 3, "m": {"x": 1, "y": 1}, "updated_at": "2026-01-01T00:00:00Z"});
        assert!(merge(Some(&base), Some(&ours), Some(&clash), "").is_err());
    }
}
