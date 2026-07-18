use std::collections::BTreeMap;
use std::fs::Metadata;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use super::{event, push_event, TraceEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileState {
    exists: bool,
    len: u64,
    modified_ms: Option<u128>,
}

pub(super) fn run_file_watch(
    path: String,
    interval_ms: u64,
    mut previous: FileState,
    started_at: Instant,
    events: Arc<Mutex<Vec<TraceEvent>>>,
    stop: mpsc::Receiver<()>,
) {
    let path_buf = PathBuf::from(&path);
    let interval = Duration::from_millis(interval_ms.max(1));

    loop {
        let current = file_state(&path_buf);
        emit_file_watch_events(&path, &previous, &current, started_at, &events);
        previous = current;
        if stop.recv_timeout(interval).is_ok() {
            let current = file_state(&path_buf);
            emit_file_watch_events(&path, &previous, &current, started_at, &events);
            break;
        }
    }
}

pub(super) fn file_state(path: &PathBuf) -> FileState {
    let Ok(metadata) = std::fs::metadata(path) else {
        return FileState {
            exists: false,
            len: 0,
            modified_ms: None,
        };
    };
    file_state_from_metadata(&metadata)
}

fn file_state_from_metadata(metadata: &Metadata) -> FileState {
    FileState {
        exists: true,
        len: metadata.len(),
        modified_ms: metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|modified| modified.as_millis()),
    }
}

fn emit_file_watch_events(
    path: &str,
    previous: &FileState,
    current: &FileState,
    started_at: Instant,
    events: &Arc<Mutex<Vec<TraceEvent>>>,
) {
    let event_name = if !previous.exists && current.exists {
        Some("fs.create")
    } else if previous.exists && !current.exists {
        Some("fs.delete")
    } else if previous.exists
        && current.exists
        && (previous.len != current.len || previous.modified_ms != current.modified_ms)
    {
        Some("fs.write")
    } else {
        None
    };

    let Some(event_name) = event_name else {
        return;
    };
    let mut data = BTreeMap::new();
    data.insert(
        "path".to_string(),
        serde_json::Value::String(path.to_string()),
    );
    data.insert("exists".to_string(), serde_json::json!(current.exists));
    if current.exists {
        data.insert("len".to_string(), serde_json::json!(current.len));
    }
    push_event(events, event(started_at, "file.watch", event_name, data));
}

#[cfg(test)]
mod tests {
    use std::fs;

    use std::thread;
    use std::time::Duration;

    use super::super::{ActiveTraceProbes, TraceProbeConfig};
    use super::file_state;

    #[test]
    fn test_file_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = file_state(&temp.path().join("missing.txt"));
        assert!(!missing.exists);

        let path = temp.path().join("present.txt");
        fs::write(&path, "content").expect("write file");
        let present = file_state(&path);
        assert!(present.exists);
        assert_eq!(present.len, 7);
        assert!(present.modified_ms.is_some());
    }

    #[test]
    fn test_run_file_watch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("watched.txt");
        let probes = ActiveTraceProbes::start(&[TraceProbeConfig::FileWatch {
            path: path.to_string_lossy().to_string(),
            interval_ms: Some(25),
        }])
        .expect("start probes");

        fs::write(&path, "created").expect("create file");
        thread::sleep(Duration::from_millis(200));
        fs::write(&path, "updated with a different length").expect("update file");
        thread::sleep(Duration::from_millis(200));
        fs::remove_file(&path).expect("delete file");
        thread::sleep(Duration::from_millis(200));

        let events = probes.stop();
        assert!(events
            .iter()
            .any(|event| event.source == "file.watch" && event.event == "fs.create"));
        assert!(events
            .iter()
            .any(|event| event.source == "file.watch" && event.event == "fs.write"));
        assert!(events
            .iter()
            .any(|event| event.source == "file.watch" && event.event == "fs.delete"));
    }
}
