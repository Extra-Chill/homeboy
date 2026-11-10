//! Bounded writer diagnostics, isolated from the lock they diagnose.
//!
//! A separate SQLite file is necessary: an exhausted writer cannot append to
//! the locked observation database. No observation schema migration is needed.
use super::*;

const LIMIT: i64 = 256;

fn path(database: &std::path::Path) -> PathBuf {
    database.with_extension("sqlite.write-health")
}

pub(crate) fn record(connection: &Connection, event: serde_json::Value) {
    let Some(database) = connection.path().filter(|path| !path.is_empty()) else {
        return;
    };
    if let Err(error) = append(std::path::Path::new(database), event) {
        crate::log_status!("observation.write_health_error", "{}", error);
    }
}

fn append(database: &std::path::Path, mut event: serde_json::Value) -> rusqlite::Result<()> {
    event["pid"] = serde_json::json!(std::process::id());
    event["finished_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    let mut connection = Connection::open(path(database))?;
    connection.busy_timeout(std::time::Duration::from_millis(750))?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS writes (id INTEGER PRIMARY KEY, event TEXT NOT NULL)",
    )?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    transaction.execute("INSERT INTO writes(event) VALUES (?1)", [event.to_string()])?;
    transaction.execute(
        "DELETE FROM writes WHERE id <= (SELECT MAX(id) - ?1 FROM writes)",
        [LIMIT],
    )?;
    transaction.commit()
}

impl ObservationStore {
    /// Recent slow holders and exhausted waiters, newest first (at most 256).
    /// Read-only inspection never creates diagnostics or takes the store writer.
    pub fn write_health(&self) -> Result<Vec<serde_json::Value>> {
        let path = path(&self.path);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let connection =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(sqlite_error("open writer diagnostics"))?;
        connection
            .busy_timeout(std::time::Duration::from_millis(750))
            .map_err(sqlite_error("configure writer diagnostics"))?;
        let mut statement = connection
            .prepare("SELECT event FROM writes ORDER BY id DESC LIMIT 256")
            .map_err(sqlite_error("read writer diagnostics"))?;
        let rows = statement
            .query_map([], |row| parse_metadata(row.get(0)?))
            .map_err(sqlite_error("query writer diagnostics"))?;
        collect_rows(rows, "collect writer diagnostics")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_holder_and_losing_writer_survive_reopen_and_are_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.sqlite");
        let store = ObservationStore::open_initialized_at(&path).unwrap();
        unsafe extern "C" fn slow_commit(_: *mut std::ffi::c_void) -> std::ffi::c_int {
            std::thread::sleep(std::time::Duration::from_millis(1010));
            0
        }
        // Exercise the public write interface with a deterministic slow commit.
        // SAFETY: the callback retains no pointer and is removed before closing.
        unsafe {
            rusqlite::ffi::sqlite3_commit_hook(
                store.connection.handle(),
                Some(slow_commit),
                std::ptr::null_mut(),
            );
        }
        store
            .start_run(NewRunRecord::builder("slow-writer").build())
            .unwrap();
        unsafe {
            rusqlite::ffi::sqlite3_commit_hook(
                store.connection.handle(),
                None,
                std::ptr::null_mut(),
            );
        }
        let blocker = Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        store
            .connection
            .busy_timeout(std::time::Duration::ZERO)
            .unwrap();
        assert!(store
            .start_run(NewRunRecord::builder("losing-writer").build())
            .is_err());
        blocker.execute_batch("ROLLBACK").unwrap();
        drop(store);
        let store = ObservationStore::open_readonly_at(&path).unwrap();
        let events = store.write_health().unwrap();
        assert_eq!(events[0]["operation"], "insert run record");
        assert!(events[0]["waited_ms"].is_number());
        assert_eq!(events[1]["operation"], "insert run record");
        for field in ["pid", "duration_ms", "changed_rows", "bytes", "finished_at"] {
            assert!(!events[1][field].is_null(), "{field}");
        }
        for _ in 0..260 {
            append(&path, serde_json::json!({"operation": "bounded"})).unwrap();
        }
        assert_eq!(store.write_health().unwrap().len(), LIMIT as usize);
    }
}
