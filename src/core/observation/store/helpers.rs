use std::fs;
use std::path::{Path, PathBuf};

use super::*;

pub(crate) fn validate_required(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            field,
            "value cannot be empty",
            None,
            None,
        ));
    }
    Ok(())
}

pub(crate) fn ensure_identical<T: PartialEq>(
    kind: &str,
    id: &str,
    existing: &T,
    incoming: &T,
) -> Result<()> {
    if existing == incoming {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        format!("{kind}.id"),
        format!("existing {kind} record conflicts with imported bundle record: {id}"),
        Some(id.to_string()),
        None,
    ))
}

pub(crate) fn serialize_metadata(metadata_json: &serde_json::Value) -> Result<String> {
    serde_json::to_string(metadata_json).map_err(|e| {
        Error::internal_json(e.to_string(), Some("serialize run metadata".to_string()))
    })
}

pub(crate) fn with_run_context_metadata(
    mut metadata: serde_json::Value,
    context: &RunContext,
) -> serde_json::Value {
    let owner = serde_json::json!({
        "pid": std::process::id(),
        "recorded_at": chrono::Utc::now().to_rfc3339(),
    });

    let mut additions = vec![("homeboy_run_owner".to_string(), owner)];
    if let Some(source_snapshot) = &context.provenance.source_snapshot {
        additions.push(("source_snapshot".to_string(), source_snapshot.clone()));
    }
    if let Some(lab_offload) = &context.provenance.lab_offload {
        additions.push(("lab_offload".to_string(), lab_offload.clone()));
    }
    if let Some(preview) = &context.provenance.preview {
        additions.push(("preview".to_string(), preview.clone()));
    }
    if let Some(artifact_mirror) = &context.provenance.artifact_mirror {
        additions.push(("artifact_mirror".to_string(), artifact_mirror.clone()));
    }

    let target = if metadata.is_object() {
        &mut metadata
    } else {
        metadata = serde_json::json!({
            "homeboy_original_metadata": metadata,
        });
        &mut metadata
    };

    if let Some(object) = target.as_object_mut() {
        for (key, value) in additions {
            object.insert(key, value);
        }
    }

    metadata
}

pub(crate) fn parse_metadata(raw: String) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(&raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            raw.len(),
            rusqlite::types::Type::Text,
            Box::new(e),
        )
    })
}

pub(crate) fn row_to_run_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        component_id: row.get(2)?,
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        status: row.get(5)?,
        command: row.get(6)?,
        cwd: row.get(7)?,
        homeboy_version: row.get(8)?,
        git_sha: row.get(9)?,
        rig_id: row.get(10)?,
        metadata_json: parse_metadata(row.get(11)?)?,
    })
}

pub(crate) fn row_to_artifact_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactRecord> {
    row_to_artifact_record_at(row, 0)
}

pub(crate) fn row_to_artifact_record_at(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<ArtifactRecord> {
    Ok(ArtifactRecord {
        id: row.get(offset)?,
        run_id: row.get(offset + 1)?,
        kind: row.get(offset + 2)?,
        artifact_type: row.get(offset + 3)?,
        path: row.get(offset + 4)?,
        url: if row.get_ref(offset + 3)?.as_str()? == "url" {
            Some(row.get(offset + 4)?)
        } else {
            None
        },
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: row.get(offset + 5)?,
        size_bytes: row.get(offset + 6)?,
        mime: row.get(offset + 7)?,
        metadata_json: parse_metadata(row.get(offset + 8)?)?,
        created_at: row.get(offset + 9)?,
    })
}

pub(crate) fn row_to_run_artifact_record(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<RunArtifactRecord> {
    Ok(RunArtifactRecord {
        run: row_to_run_record(row)?,
        artifact: row_to_artifact_record_at(row, 12)?,
    })
}

pub(crate) fn row_to_artifact_cleanup_candidate(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ArtifactCleanupCandidateRecord> {
    Ok(ArtifactCleanupCandidateRecord {
        artifact: row_to_artifact_record(row)?,
        run_kind: row.get(10)?,
        component_id: row.get(11)?,
        run_started_at: row.get(12)?,
        run_status: row.get(13)?,
    })
}

pub(crate) fn row_to_trace_run_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TraceRunRecord> {
    Ok(TraceRunRecord {
        run_id: row.get(0)?,
        component_id: row.get(1)?,
        rig_id: row.get(2)?,
        scenario_id: row.get(3)?,
        status: row.get(4)?,
        baseline_status: row.get(5)?,
        metadata_json: parse_metadata(row.get(6)?)?,
    })
}

pub(crate) fn row_to_trace_span_record(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<TraceSpanRecord> {
    Ok(TraceSpanRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        span_id: row.get(2)?,
        status: row.get(3)?,
        duration_ms: row.get(4)?,
        from_event: row.get(5)?,
        to_event: row.get(6)?,
        metadata_json: parse_metadata(row.get(7)?)?,
    })
}

/// Run the joined run-artifact `query_map` on a prepared statement with the
/// given `params` and collect the rows, using the shared error contexts.
pub(crate) fn query_run_artifact_records(
    statement: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<Vec<RunArtifactRecord>> {
    let rows = statement
        .query_map(params, row_to_run_artifact_record)
        .map_err(sqlite_error("list joined run artifact records"))?;
    collect_rows(rows, "collect joined run artifact records")
}

pub(crate) fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
    context: &'static str,
) -> Result<Vec<T>> {
    let mut records = Vec::new();
    for row in rows {
        records.push(row.map_err(sqlite_error(context))?);
    }
    Ok(records)
}

pub(crate) fn persisted_artifact_path(
    run_id: &str,
    artifact_id: &str,
    source: &Path,
) -> Result<PathBuf> {
    let file_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(|name| format!("{artifact_id}-{name}"))
        .unwrap_or_else(|| artifact_id.to_string());
    Ok(paths::artifact_root()?.join(run_id).join(file_name))
}

pub(crate) fn copy_artifact_file(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("create artifact directory {}", parent.display())),
            )
        })?;
    }
    fs::copy(source, target).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!(
                "persist artifact {} to {}",
                source.display(),
                target.display()
            )),
        )
    })?;
    Ok(())
}

pub(crate) fn copy_artifact_directory(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("create artifact directory {}", target.display())),
        )
    })?;
    for entry in fs::read_dir(source).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read artifact directory {}", source.display())),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "read artifact directory entry {}",
                    source.display()
                )),
            )
        })?;
        let entry_source = entry.path();
        let entry_target = target.join(entry.file_name());
        let entry_type = entry.file_type().map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "read artifact entry type {}",
                    entry_source.display()
                )),
            )
        })?;
        if entry_type.is_dir() {
            copy_artifact_directory(&entry_source, &entry_target)?;
        } else if entry_type.is_file() {
            copy_artifact_file(&entry_source, &entry_target)?;
        }
    }
    Ok(())
}

pub(crate) fn sqlite_error(context: impl Into<String>) -> impl FnOnce(rusqlite::Error) -> Error {
    let context = context.into();
    move |error| {
        Error::internal_unexpected(format!(
            "SQLite observation store error: {context}: {error}"
        ))
    }
}

/// Number of attempts (1 initial + retries) used for transient-lock recovery.
pub(crate) const SQLITE_WRITE_MAX_ATTEMPTS: u32 = 6;
/// Base backoff between retries; doubles each attempt (25, 50, 100, 200, ...ms).
pub(crate) const SQLITE_WRITE_BASE_BACKOFF_MS: u64 = 25;

/// Returns true when the SQLite error is a transient busy/locked condition that
/// is expected to self-heal once a competing writer releases the lock.
pub(crate) fn is_transient_lock_error(error: &rusqlite::Error) -> bool {
    use rusqlite::ffi::ErrorCode;
    match error {
        rusqlite::Error::SqliteFailure(inner, _) => {
            matches!(
                inner.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            )
        }
        _ => false,
    }
}

/// Runs a SQLite write closure with bounded exponential backoff, retrying only
/// when the failure is a transient "database is locked"/"database is busy"
/// condition. Genuine, persistent errors surface immediately, and the lock
/// error is surfaced with context if every attempt is exhausted.
pub(crate) fn execute_with_retry<T>(
    context: impl Into<String>,
    mut op: impl FnMut() -> rusqlite::Result<T>,
) -> Result<T> {
    execute_with_retry_inner(
        context.into(),
        SQLITE_WRITE_MAX_ATTEMPTS,
        SQLITE_WRITE_BASE_BACKOFF_MS,
        |attempt, backoff_ms| {
            if backoff_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
            }
            let _ = attempt;
        },
        &mut op,
    )
}

/// Backoff-injectable core so the retry policy can be unit-tested without
/// sleeping. `sleep` is invoked with the upcoming attempt index and the
/// computed backoff (ms) before each retry.
pub(crate) fn execute_with_retry_inner<T>(
    context: String,
    max_attempts: u32,
    base_backoff_ms: u64,
    mut sleep: impl FnMut(u32, u64),
    op: &mut impl FnMut() -> rusqlite::Result<T>,
) -> Result<T> {
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match op() {
            Ok(value) => return Ok(value),
            Err(error) => {
                let transient = is_transient_lock_error(&error);
                if transient && attempt < max_attempts {
                    let backoff_ms = base_backoff_ms.saturating_mul(1u64 << (attempt - 1));
                    sleep(attempt, backoff_ms);
                    continue;
                }
                let detail = if transient {
                    format!("{context} (after {attempt} attempts, lock did not clear)")
                } else {
                    context.clone()
                };
                return Err(sqlite_error(detail)(error));
            }
        }
    }
}
