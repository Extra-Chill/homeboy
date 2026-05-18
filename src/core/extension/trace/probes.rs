//! Passive trace probes that emit core timeline events.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::error::{Error, Result};

use super::parsing::TraceEvent;

mod cmd_run;
mod file_watch;
mod http_poll;
mod port_snapshot;

use cmd_run::run_cmd_run;
use file_watch::{file_state, run_file_watch, FileState};
use http_poll::run_http_poll;
use port_snapshot::{ports_for_snapshot, run_port_snapshot};

const DEFAULT_PROCESS_INTERVAL_MS: u64 = 1_000;
const DEFAULT_FILE_INTERVAL_MS: u64 = 250;
const DEFAULT_PORT_INTERVAL_MS: u64 = 500;
const DEFAULT_HTTP_INTERVAL_MS: u64 = 1_000;
const LOG_POLL_INTERVAL_MS: u64 = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TraceProbeConfig {
    #[serde(rename = "log.tail")]
    LogTail {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grep: Option<String>,
        #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
        match_pattern: Option<String>,
    },
    #[serde(rename = "process.snapshot")]
    ProcessSnapshot {
        pattern: String,
        #[serde(default, alias = "interval", skip_serializing_if = "Option::is_none")]
        interval_ms: Option<u64>,
    },
    #[serde(rename = "file.watch")]
    FileWatch {
        path: String,
        #[serde(default, alias = "interval", skip_serializing_if = "Option::is_none")]
        interval_ms: Option<u64>,
    },
    #[serde(rename = "port.snapshot")]
    PortSnapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(
            default,
            rename = "port-range",
            alias = "port_range",
            skip_serializing_if = "Option::is_none"
        )]
        port_range: Option<String>,
        #[serde(default, alias = "interval", skip_serializing_if = "Option::is_none")]
        interval_ms: Option<u64>,
    },
    #[serde(rename = "http.poll")]
    HttpPoll {
        url: String,
        #[serde(default, alias = "interval", skip_serializing_if = "Option::is_none")]
        interval_ms: Option<u64>,
        #[serde(
            default,
            rename = "assert-status",
            alias = "assert_status",
            skip_serializing_if = "Option::is_none"
        )]
        assert_status: Option<u16>,
    },
    #[serde(rename = "cmd.run")]
    CmdRun {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
    },
}

pub struct ActiveTraceProbes {
    events: Arc<Mutex<Vec<TraceEvent>>>,
    stops: Vec<mpsc::Sender<()>>,
    handles: Vec<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone)]
enum RunningTraceProbeConfig {
    LogTail {
        path: String,
        grep: Option<String>,
        match_pattern: Option<String>,
        initial_position: u64,
    },
    ProcessSnapshot {
        pattern: String,
        interval_ms: Option<u64>,
    },
    FileWatch {
        path: String,
        interval_ms: Option<u64>,
        initial_state: FileState,
    },
    PortSnapshot {
        ports: Vec<u16>,
        interval_ms: Option<u64>,
    },
    HttpPoll {
        url: String,
        interval_ms: Option<u64>,
        assert_status: Option<u16>,
    },
    CmdRun {
        command: String,
        args: Vec<String>,
    },
}

impl ActiveTraceProbes {
    pub fn start(configs: &[TraceProbeConfig]) -> Result<Self> {
        let started_at = Instant::now();
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut stops = Vec::new();
        let mut handles = Vec::new();

        for config in configs {
            validate_probe(config)?;
            let (stop_tx, stop_rx) = mpsc::channel();
            let events_for_thread = Arc::clone(&events);
            let config = running_probe_config(config);
            let handle =
                thread::spawn(move || run_probe(config, started_at, events_for_thread, stop_rx));
            stops.push(stop_tx);
            handles.push(handle);
        }

        Ok(Self {
            events,
            stops,
            handles,
        })
    }

    pub fn stop(mut self) -> Vec<TraceEvent> {
        for stop in self.stops.drain(..) {
            let _ = stop.send(());
        }
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
        let mut events = self
            .events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default();
        events.sort_by_key(|event| event.t_ms);
        events
    }
}

fn running_probe_config(config: &TraceProbeConfig) -> RunningTraceProbeConfig {
    match config {
        TraceProbeConfig::LogTail {
            path,
            grep,
            match_pattern,
        } => RunningTraceProbeConfig::LogTail {
            path: path.clone(),
            grep: grep.clone(),
            match_pattern: match_pattern.clone(),
            initial_position: std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0),
        },
        TraceProbeConfig::ProcessSnapshot {
            pattern,
            interval_ms,
        } => RunningTraceProbeConfig::ProcessSnapshot {
            pattern: pattern.clone(),
            interval_ms: *interval_ms,
        },
        TraceProbeConfig::FileWatch { path, interval_ms } => RunningTraceProbeConfig::FileWatch {
            path: path.clone(),
            interval_ms: *interval_ms,
            initial_state: file_state(&PathBuf::from(path)),
        },
        TraceProbeConfig::PortSnapshot {
            port,
            port_range,
            interval_ms,
        } => RunningTraceProbeConfig::PortSnapshot {
            ports: ports_for_snapshot(*port, port_range.as_deref()).unwrap_or_default(),
            interval_ms: *interval_ms,
        },
        TraceProbeConfig::HttpPoll {
            url,
            interval_ms,
            assert_status,
        } => RunningTraceProbeConfig::HttpPoll {
            url: url.clone(),
            interval_ms: *interval_ms,
            assert_status: *assert_status,
        },
        TraceProbeConfig::CmdRun { command, args } => RunningTraceProbeConfig::CmdRun {
            command: command.clone(),
            args: args.clone(),
        },
    }
}

fn validate_probe(config: &TraceProbeConfig) -> Result<()> {
    match config {
        TraceProbeConfig::LogTail {
            grep,
            match_pattern,
            ..
        } => {
            if let Some(pattern) = grep.as_ref().or(match_pattern.as_ref()) {
                Regex::new(pattern).map_err(|e| {
                    Error::validation_invalid_argument(
                        "trace_probes.grep",
                        format!("invalid log.tail regex: {}", e),
                        None,
                        None,
                    )
                })?;
            }
        }
        TraceProbeConfig::ProcessSnapshot { pattern, .. } => {
            Regex::new(pattern).map_err(|e| {
                Error::validation_invalid_argument(
                    "trace_probes.pattern",
                    format!("invalid process.snapshot regex: {}", e),
                    None,
                    None,
                )
            })?;
        }
        TraceProbeConfig::FileWatch { path, .. } => {
            if path.trim().is_empty() {
                return Err(Error::validation_invalid_argument(
                    "trace_probes.path",
                    "file.watch path cannot be empty".to_string(),
                    None,
                    None,
                ));
            }
        }
        TraceProbeConfig::PortSnapshot {
            port, port_range, ..
        } => {
            ports_for_snapshot(*port, port_range.as_deref())?;
        }
        TraceProbeConfig::HttpPoll { url, .. } => {
            reqwest::Url::parse(url).map_err(|e| {
                Error::validation_invalid_argument(
                    "trace_probes.url",
                    format!("invalid http.poll url: {}", e),
                    None,
                    None,
                )
            })?;
        }
        TraceProbeConfig::CmdRun { command, .. } => {
            if command.trim().is_empty() {
                return Err(Error::validation_invalid_argument(
                    "trace_probes.command",
                    "cmd.run command cannot be empty".to_string(),
                    None,
                    None,
                ));
            }
        }
    }
    Ok(())
}

fn run_probe(
    config: RunningTraceProbeConfig,
    started_at: Instant,
    events: Arc<Mutex<Vec<TraceEvent>>>,
    stop: mpsc::Receiver<()>,
) {
    match config {
        RunningTraceProbeConfig::LogTail {
            path,
            grep,
            match_pattern,
            initial_position,
        } => run_log_tail(
            path,
            grep.or(match_pattern),
            initial_position,
            started_at,
            events,
            stop,
        ),
        RunningTraceProbeConfig::ProcessSnapshot {
            pattern,
            interval_ms,
        } => run_process_snapshot(
            pattern,
            interval_ms.unwrap_or(DEFAULT_PROCESS_INTERVAL_MS),
            started_at,
            events,
            stop,
        ),
        RunningTraceProbeConfig::FileWatch {
            path,
            interval_ms,
            initial_state,
        } => run_file_watch(
            path,
            interval_ms.unwrap_or(DEFAULT_FILE_INTERVAL_MS),
            initial_state,
            started_at,
            events,
            stop,
        ),
        RunningTraceProbeConfig::PortSnapshot { ports, interval_ms } => run_port_snapshot(
            ports,
            interval_ms.unwrap_or(DEFAULT_PORT_INTERVAL_MS),
            started_at,
            events,
            stop,
        ),
        RunningTraceProbeConfig::HttpPoll {
            url,
            interval_ms,
            assert_status,
        } => run_http_poll(
            url,
            interval_ms.unwrap_or(DEFAULT_HTTP_INTERVAL_MS),
            assert_status,
            started_at,
            events,
            stop,
        ),
        RunningTraceProbeConfig::CmdRun { command, args } => {
            run_cmd_run(command, args, started_at, events, stop)
        }
    }
}

fn run_log_tail(
    path: String,
    pattern: Option<String>,
    initial_position: u64,
    started_at: Instant,
    events: Arc<Mutex<Vec<TraceEvent>>>,
    stop: mpsc::Receiver<()>,
) {
    let matcher = pattern
        .as_deref()
        .and_then(|pattern| Regex::new(pattern).ok());
    let path_buf = PathBuf::from(&path);
    let mut position = initial_position;
    let mut partial = String::new();
    let drain = LogTailDrain {
        path_buf: &path_buf,
        path: &path,
        matcher: matcher.as_ref(),
        started_at,
        events: &events,
    };

    loop {
        drain_log_tail_once(drain, &mut position, &mut partial, false);
        if stop
            .recv_timeout(Duration::from_millis(LOG_POLL_INTERVAL_MS))
            .is_ok()
        {
            drain_log_tail_once(drain, &mut position, &mut partial, true);
            break;
        }
    }
}

#[derive(Clone, Copy)]
struct LogTailDrain<'a> {
    path_buf: &'a PathBuf,
    path: &'a str,
    matcher: Option<&'a Regex>,
    started_at: Instant,
    events: &'a Arc<Mutex<Vec<TraceEvent>>>,
}

fn drain_log_tail_once(
    drain: LogTailDrain<'_>,
    position: &mut u64,
    partial: &mut String,
    flush_partial: bool,
) {
    read_new_log_lines(
        drain.path_buf,
        drain.path,
        position,
        partial,
        drain.matcher,
        drain.started_at,
        drain.events,
    );
    if flush_partial && !partial.is_empty() {
        emit_log_line(
            drain.path,
            std::mem::take(partial),
            drain.matcher,
            drain.started_at,
            drain.events,
        );
    }
}

fn read_new_log_lines(
    path_buf: &PathBuf,
    path: &str,
    position: &mut u64,
    partial: &mut String,
    matcher: Option<&Regex>,
    started_at: Instant,
    events: &Arc<Mutex<Vec<TraceEvent>>>,
) {
    let Ok(mut file) = File::open(path_buf) else {
        *position = 0;
        return;
    };
    let Ok(metadata) = file.metadata() else {
        return;
    };
    if metadata.len() < *position {
        *position = 0;
        partial.clear();
    }
    if file.seek(SeekFrom::Start(*position)).is_err() {
        return;
    }
    let mut chunk = String::new();
    if file.read_to_string(&mut chunk).is_err() || chunk.is_empty() {
        return;
    }
    *position = position.saturating_add(chunk.len() as u64);
    partial.push_str(&chunk);
    while let Some(index) = partial.find('\n') {
        let mut line = partial.drain(..=index).collect::<String>();
        while line.ends_with(['\n', '\r']) {
            line.pop();
        }
        emit_log_line(path, line, matcher, started_at, events);
    }
}

fn emit_log_line(
    path: &str,
    line: String,
    matcher: Option<&Regex>,
    started_at: Instant,
    events: &Arc<Mutex<Vec<TraceEvent>>>,
) {
    push_log_event(
        events,
        started_at,
        "log.line",
        log_line_data(path, &line, None),
    );

    if let Some(matcher) = matcher.filter(|matcher| matcher.is_match(&line)) {
        push_log_event(
            events,
            started_at,
            "log.match",
            log_line_data(path, &line, Some(matcher.as_str())),
        );
    }
}

fn push_log_event(
    events: &Arc<Mutex<Vec<TraceEvent>>>,
    started_at: Instant,
    event_name: &str,
    data: BTreeMap<String, serde_json::Value>,
) {
    push_event(events, event(started_at, "log.tail", event_name, data));
}

fn log_line_data(
    path: &str,
    line: &str,
    pattern: Option<&str>,
) -> BTreeMap<String, serde_json::Value> {
    let mut data = BTreeMap::new();
    data.insert(
        "path".to_string(),
        serde_json::Value::String(path.to_string()),
    );
    data.insert(
        "line".to_string(),
        serde_json::Value::String(line.to_string()),
    );
    if let Some(pattern) = pattern {
        data.insert(
            "pattern".to_string(),
            serde_json::Value::String(pattern.to_string()),
        );
    }
    data
}

fn run_process_snapshot(
    pattern: String,
    interval_ms: u64,
    started_at: Instant,
    events: Arc<Mutex<Vec<TraceEvent>>>,
    stop: mpsc::Receiver<()>,
) {
    let Ok(matcher) = Regex::new(&pattern) else {
        return;
    };
    let interval = Duration::from_millis(interval_ms.max(1));
    let mut previous: Option<BTreeMap<u32, String>> = None;

    loop {
        let current = matching_processes(&matcher);
        emit_process_events(&pattern, previous.as_ref(), &current, started_at, &events);
        previous = Some(current);
        if stop.recv_timeout(interval).is_ok() {
            let current = matching_processes(&matcher);
            emit_process_events(&pattern, previous.as_ref(), &current, started_at, &events);
            break;
        }
    }
}

fn matching_processes(matcher: &Regex) -> BTreeMap<u32, String> {
    process_snapshot()
        .into_iter()
        .filter(|(_, command)| matcher.is_match(command))
        .collect()
}

fn process_snapshot() -> BTreeMap<u32, String> {
    let output = Command::new("ps").args(["-eo", "pid=,command="]).output();
    let Ok(output) = output else {
        return BTreeMap::new();
    };
    if !output.status.success() {
        return BTreeMap::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_ps_line)
        .collect()
}

fn parse_ps_line(line: &str) -> Option<(u32, String)> {
    let trimmed = line.trim_start();
    let split_at = trimmed.find(char::is_whitespace)?;
    let (pid, command) = trimmed.split_at(split_at);
    Some((pid.parse().ok()?, command.trim().to_string()))
}

fn emit_process_events(
    pattern: &str,
    previous: Option<&BTreeMap<u32, String>>,
    current: &BTreeMap<u32, String>,
    started_at: Instant,
    events: &Arc<Mutex<Vec<TraceEvent>>>,
) {
    let mut data = BTreeMap::new();
    data.insert(
        "pattern".to_string(),
        serde_json::Value::String(pattern.to_string()),
    );
    data.insert(
        "processes".to_string(),
        serde_json::Value::Array(
            current
                .iter()
                .map(|(pid, command)| serde_json::json!({ "pid": pid, "command": command }))
                .collect(),
        ),
    );
    push_event(
        events,
        event(started_at, "process.snapshot", "proc.list", data),
    );

    let Some(previous) = previous else {
        return;
    };
    let previous_pids = previous.keys().copied().collect::<BTreeSet<_>>();
    let current_pids = current.keys().copied().collect::<BTreeSet<_>>();

    for pid in current_pids.difference(&previous_pids) {
        if let Some(command) = current.get(pid) {
            push_event(
                events,
                process_delta_event(started_at, "proc.spawn", *pid, command),
            );
        }
    }
    for pid in previous_pids.difference(&current_pids) {
        if let Some(command) = previous.get(pid) {
            push_event(
                events,
                process_delta_event(started_at, "proc.exit", *pid, command),
            );
        }
    }
}

fn process_delta_event(
    started_at: Instant,
    event_name: &str,
    pid: u32,
    command: &str,
) -> TraceEvent {
    let mut data = BTreeMap::new();
    data.insert("pid".to_string(), serde_json::json!(pid));
    data.insert(
        "command".to_string(),
        serde_json::Value::String(command.to_string()),
    );
    event(started_at, "process.snapshot", event_name, data)
}

fn event(
    started_at: Instant,
    source: &str,
    event: &str,
    data: BTreeMap<String, serde_json::Value>,
) -> TraceEvent {
    TraceEvent {
        t_ms: started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        source: source.to_string(),
        event: event.to_string(),
        data,
    }
}

fn push_event(events: &Arc<Mutex<Vec<TraceEvent>>>, event: TraceEvent) {
    if let Ok(mut events) = events.lock() {
        events.push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn active_trace_probes_start_rejects_invalid_regex() {
        let result = ActiveTraceProbes::start(&[TraceProbeConfig::ProcessSnapshot {
            pattern: "(".to_string(),
            interval_ms: Some(25),
        }]);
        let error = result.err().expect("invalid regex should fail start");

        assert!(error.to_string().contains("invalid process.snapshot regex"));
    }

    #[test]
    fn active_trace_probes_stop_returns_sorted_events() {
        let events = Arc::new(Mutex::new(vec![
            TraceEvent {
                t_ms: 20,
                source: "test".to_string(),
                event: "later".to_string(),
                data: BTreeMap::new(),
            },
            TraceEvent {
                t_ms: 10,
                source: "test".to_string(),
                event: "earlier".to_string(),
                data: BTreeMap::new(),
            },
        ]));
        let probes = ActiveTraceProbes {
            events,
            stops: Vec::new(),
            handles: Vec::new(),
        };

        let events = probes.stop();

        assert_eq!(
            events.iter().map(|event| event.t_ms).collect::<Vec<_>>(),
            vec![10, 20]
        );
    }

    #[test]
    fn log_tail_emits_line_and_match_events() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log_path = temp.path().join("app.log");
        fs::write(&log_path, "old line\n").expect("write initial log");

        let probes = ActiveTraceProbes::start(&[TraceProbeConfig::LogTail {
            path: log_path.to_string_lossy().to_string(),
            grep: Some("needle".to_string()),
            match_pattern: None,
        }])
        .expect("start probes");

        fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .and_then(|mut file| {
                use std::io::Write;
                writeln!(file, "new needle line")
            })
            .expect("append log");
        thread::sleep(Duration::from_millis(150));

        let events = probes.stop();
        assert!(events.iter().any(|event| event.event == "log.line"
            && event.data.get("line").and_then(|value| value.as_str()) == Some("new needle line")));
        assert!(events.iter().any(|event| event.event == "log.match"));
        assert!(!events.iter().any(|event| event
            .data
            .get("line")
            .and_then(|value| value.as_str())
            == Some("old line")));
    }

    #[test]
    fn process_snapshot_emits_list_events() {
        let pattern = std::env::current_exe()
            .expect("current exe")
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("homeboy")
            .to_string();
        let probes = ActiveTraceProbes::start(&[TraceProbeConfig::ProcessSnapshot {
            pattern,
            interval_ms: Some(25),
        }])
        .expect("start probes");
        thread::sleep(Duration::from_millis(80));

        let events = probes.stop();
        assert!(events
            .iter()
            .any(|event| event.source == "process.snapshot" && event.event == "proc.list"));
    }
}
