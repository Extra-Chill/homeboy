//! Content-addressed payloads for large run metadata: known Cook payloads
//! above the threshold, and any other value needed to stay under the row cap.
use super::*;
use rusqlite::OptionalExtension;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

pub const METADATA_PAYLOAD_THRESHOLD: usize = 16 * 1024;
pub const RUN_METADATA_CAP: usize = 256 * 1024;
pub const REFERENCE: &str = "metadata_payload";

fn io(error: std::io::Error) -> Error {
    Error::internal_io(error.to_string(), Some("run metadata payload".into()))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn externalize(value: &mut Value, root: &Path) -> Result<()> {
    if value.get(REFERENCE).is_some() {
        return Ok(());
    }
    let bytes = serde_json::to_vec(value).map_err(|e| Error::internal_json(e.to_string(), None))?;
    if bytes.len() <= METADATA_PAYLOAD_THRESHOLD {
        return Ok(());
    }
    let sha256 = digest(&bytes);
    let directory = root.join("metadata-payloads");
    fs::create_dir_all(&directory).map_err(io)?;
    let path = directory.join(&sha256);
    if !path.exists() {
        let mut temporary = tempfile::NamedTempFile::new_in(&directory).map_err(io)?;
        use std::io::Write;
        temporary.write_all(&bytes).map_err(io)?;
        temporary.as_file().sync_all().map_err(io)?;
        temporary.persist(&path).map_err(|e| io(e.error))?;
        fs::File::open(&directory)
            .and_then(|f| f.sync_all())
            .map_err(io)?;
    }
    *value = serde_json::json!({REFERENCE: {"sha256": sha256, "bytes": bytes.len()}});
    Ok(())
}

fn prepare(value: &mut Value, root: &Path) -> Result<()> {
    match value {
        Value::Object(object) => {
            // Process reports before their nested diffs so identical reports at
            // all three locations have the same digest.
            if let Some(artifacts) = object
                .get_mut("cook_delivery")
                .and_then(|v| v.get_mut("artifacts"))
            {
                if let Some(report) = artifacts.get_mut("promotion") {
                    externalize(report, root)?;
                }
                if let Some(history) = artifacts
                    .get_mut("promotions")
                    .and_then(Value::as_array_mut)
                {
                    for report in history {
                        externalize(report, root)?;
                    }
                }
            }
            if let Some(claims) = object
                .get_mut("cook_operation_claims")
                .and_then(Value::as_array_mut)
            {
                for claim in claims {
                    if claim
                        .get("operation_key")
                        .and_then(Value::as_str)
                        .is_some_and(|key| key.starts_with("promote:"))
                    {
                        if let Some(report) = claim.get_mut("result") {
                            externalize(report, root)?;
                        }
                    }
                }
            }
            if let Some(diff) = object
                .get_mut("gate_feedback_baseline")
                .and_then(|v| v.get_mut("current_diff"))
            {
                externalize(diff, root)?;
            }
            for child in object.values_mut() {
                prepare(child, root)?;
            }
        }
        Value::Array(array) => {
            for child in array {
                prepare(child, root)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The deepest value on the largest-child path that is still above the
/// externalization threshold and not already a payload reference.
fn largest_inline_payload(value: &mut Value) -> Option<&mut Value> {
    fn size(value: &Value) -> usize {
        serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
    }
    if value.get(REFERENCE).is_some() || size(value) <= METADATA_PAYLOAD_THRESHOLD {
        return None;
    }
    let largest_child = match value {
        Value::Object(object) => object
            .iter()
            .max_by_key(|(_, child)| size(child))
            .map(|(key, _)| key.clone()),
        Value::Array(array) => array
            .iter()
            .enumerate()
            .max_by_key(|(_, child)| size(child))
            .map(|(index, _)| index.to_string()),
        _ => None,
    };
    let descends = largest_child.as_ref().is_some_and(|key| {
        let child = match &*value {
            Value::Object(object) => object.get(key),
            Value::Array(array) => key.parse::<usize>().ok().and_then(|i| array.get(i)),
            _ => None,
        };
        child.is_some_and(|child| {
            child.get(REFERENCE).is_none() && size(child) > METADATA_PAYLOAD_THRESHOLD
        })
    });
    if !descends {
        return Some(value);
    }
    let key = largest_child.expect("descends implies a child");
    match value {
        Value::Object(object) => largest_inline_payload(object.get_mut(&key)?),
        Value::Array(array) => largest_inline_payload(array.get_mut(key.parse::<usize>().ok()?)?),
        _ => None,
    }
}

/// Resolve only at a content-consuming boundary, using the caller's roots.
pub fn resolve_metadata_payload(value: &Value, root: &Path) -> Result<Value> {
    if let Some(reference) = value.get(REFERENCE) {
        let sha = reference
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or("");
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::internal_unexpected(
                "Invalid metadata payload digest",
            ));
        }
        let bytes = fs::read(root.join("metadata-payloads").join(sha)).map_err(io)?;
        if digest(&bytes) != sha
            || reference.get("bytes").and_then(Value::as_u64) != Some(bytes.len() as u64)
        {
            return Err(Error::internal_unexpected(
                "Metadata payload integrity mismatch",
            ));
        }
        let value = serde_json::from_slice(&bytes)
            .map_err(|e| Error::internal_json(e.to_string(), None))?;
        return resolve_metadata_payload(&value, root);
    }
    let mut result = value.clone();
    match &mut result {
        Value::Object(object) => {
            for child in object.values_mut() {
                *child = resolve_metadata_payload(child, root)?;
            }
        }
        Value::Array(array) => {
            for child in array {
                *child = resolve_metadata_payload(child, root)?;
            }
        }
        _ => {}
    }
    Ok(result)
}

impl ObservationStore {
    pub(crate) fn metadata_payload_lock(&self, exclusive: bool) -> Result<fs::File> {
        use fs4::fs_std::FileExt;
        // Lock a sidecar, never the database inode: closing any descriptor for
        // the database would drop the POSIX locks SQLite holds on it.
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("sqlite.metadata-payloads.lock"))
            .map_err(io)?;
        if exclusive {
            file.lock_exclusive().map_err(io)?;
        } else {
            file.lock_shared().map_err(io)?;
        }
        Ok(file)
    }
    pub(crate) fn serialize_run_metadata(&self, value: &Value) -> Result<String> {
        let mut value = value.clone();
        let root = self.artifact_root()?;
        prepare(&mut value, &root)?;
        let mut serialized = serialize_metadata(&value)?;
        // Any other oversized value moves to the same content-addressed store,
        // largest first, so the row stays bounded without losing evidence.
        while serialized.len() > RUN_METADATA_CAP {
            let Some(oversized) = largest_inline_payload(&mut value) else {
                return Err(Error::internal_unexpected(format!(
                    "run metadata exceeds {RUN_METADATA_CAP} byte cap with no value above {METADATA_PAYLOAD_THRESHOLD} bytes left to externalize"
                )));
            };
            externalize(oversized, &root)?;
            serialized = serialize_metadata(&value)?;
        }
        Ok(serialized)
    }

    /// Explicit offline maintenance; never invoked by run retention.
    pub fn compact_run_metadata(&self) -> Result<()> {
        let _guard = self.metadata_payload_lock(false)?;
        let mut after = String::new();
        loop {
            let row: Option<(String, String)> = self
                .connection
                .query_row(
                    "SELECT id, metadata_json FROM runs WHERE id > ?1 ORDER BY id LIMIT 1",
                    [&after],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(sqlite_error("read metadata for compaction"))?;
            let Some((id, raw)) = row else {
                break;
            };
            after.clone_from(&id);
            let value = serde_json::from_str(&raw)
                .map_err(|e| Error::internal_json(e.to_string(), None))?;
            let serialized = self.serialize_run_metadata(&value)?;
            self.connection
                .execute(
                    "UPDATE runs SET metadata_json = ?1 WHERE id = ?2 AND metadata_json = ?3",
                    rusqlite::params![serialized, id, raw],
                )
                .map_err(sqlite_error("compact run metadata"))?;
        }
        self.connection
            .execute_batch("VACUUM")
            .map_err(sqlite_error("vacuum compacted run metadata"))
    }

    /// Mark all surviving rows, including references nested in canonical reports.
    pub fn sweep_metadata_payloads(&self, apply: bool) -> Result<(usize, u64)> {
        let _guard = self.metadata_payload_lock(true)?;
        fn mark(value: &Value, root: &Path, live: &mut HashSet<String>) -> Result<()> {
            if let Some(sha) = value
                .get(REFERENCE)
                .and_then(|v| v.get("sha256"))
                .and_then(Value::as_str)
            {
                if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(Error::internal_unexpected(
                        "Invalid metadata payload digest",
                    ));
                }
                if live.insert(sha.to_string()) {
                    let bytes = fs::read(root.join("metadata-payloads").join(sha)).map_err(io)?;
                    let nested = serde_json::from_slice(&bytes)
                        .map_err(|e| Error::internal_json(e.to_string(), None))?;
                    mark(&nested, root, live)?;
                }
            }
            match value {
                Value::Object(object) => {
                    for child in object.values() {
                        mark(child, root, live)?;
                    }
                }
                Value::Array(array) => {
                    for child in array {
                        mark(child, root, live)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        let root = self.artifact_root()?;
        let directory = root.join("metadata-payloads");
        if !directory.exists() {
            return Ok((0, 0));
        }
        let mut live = HashSet::new();
        let mut statement = self
            .connection
            .prepare("SELECT metadata_json FROM runs")
            .map_err(sqlite_error("mark metadata payloads"))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_error("scan metadata payload references"))?;
        for row in rows {
            let raw = row.map_err(sqlite_error("read metadata payload references"))?;
            let value = serde_json::from_str(&raw)
                .map_err(|e| Error::internal_json(e.to_string(), None))?;
            mark(&value, &root, &mut live)?;
        }
        let mut count = 0;
        let mut bytes = 0;
        for entry in fs::read_dir(directory).map_err(io)? {
            let entry = entry.map_err(io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.len() == 64 && !live.contains(&name) && entry.file_type().map_err(io)?.is_file()
            {
                count += 1;
                bytes += entry.metadata().map_err(io)?.len();
                if apply {
                    fs::remove_file(entry.path()).map_err(io)?;
                }
            }
        }
        Ok((count, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_lock_never_opens_the_database_inode() {
        use std::os::unix::fs::MetadataExt;
        crate::test_support::with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().unwrap();
            let guard = store.metadata_payload_lock(false).unwrap();
            assert_ne!(
                guard.metadata().unwrap().ino(),
                fs::metadata(&store.path).unwrap().ino()
            );
        });
    }

    #[test]
    fn artifact_cleanup_sweeps_unreferenced_metadata_payloads() {
        crate::test_support::with_isolated_home(|_| {
            let mut store = ObservationStore::open_initialized().unwrap();
            let first = store
                .start_run(NewRunRecord::builder("test").build())
                .unwrap();
            let second = store
                .start_run(NewRunRecord::builder("test").build())
                .unwrap();
            for (run, diff) in [
                (&first, "a".repeat(32 * 1024)),
                (&second, "b".repeat(32 * 1024)),
            ] {
                store
                    .update_run_metadata(
                        &run.id,
                        serde_json::json!({"gate_feedback_baseline": {"current_diff": diff}}),
                    )
                    .unwrap();
                store.finish_run(&run.id, RunStatus::Pass, None).unwrap();
            }
            let root = store.artifact_root().unwrap();
            let first_value = store.get_run(&first.id).unwrap().unwrap().metadata_json;
            let second_value = store.get_run(&second.id).unwrap().unwrap().metadata_json;
            let first_sha = first_value["gate_feedback_baseline"]["current_diff"][REFERENCE]
                ["sha256"]
                .as_str()
                .unwrap();
            let second_sha = second_value["gate_feedback_baseline"]["current_diff"][REFERENCE]
                ["sha256"]
                .as_str()
                .unwrap();
            store.delete_terminal_runs(&[first.id]).unwrap();
            let options = super::super::super::runs_service::PersistedArtifactCleanupOptions {
                apply: true,
                older_than_days: 0,
                run_id: None,
                kind: None,
                artifact_type: None,
                run_kind: None,
                component_id: None,
                limit: 100,
                terminal_only: true,
            };
            super::super::super::runs_service::cleanup_persisted_artifacts_with_store(
                &store, &root, options,
            )
            .unwrap();
            assert!(!root.join("metadata-payloads").join(first_sha).exists());
            assert!(root.join("metadata-payloads").join(second_sha).exists());
            assert_eq!(
                resolve_metadata_payload(&second_value, &root).unwrap()["gate_feedback_baseline"]
                    ["current_diff"],
                "b".repeat(32 * 1024)
            );
        });
    }
}
