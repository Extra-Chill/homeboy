use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use clap::Args;
use regex::Regex;
use serde::Serialize;

use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::engine::run_dir::{self, RunDir};
use homeboy::core::extension::trace::{
    ActiveTraceProbes, TraceArtifact, TraceEvent, TraceProbeConfig, TraceResults, TraceStatus,
};
use homeboy::core::git::short_head_revision_at;
use homeboy::core::observation::{NewRunRecord, ObservationStore, RunStatus};
use homeboy::core::Error;

use super::utils::args::PositionalComponentArgs;
use super::{CmdResult, GlobalArgs};

const DEFAULT_DURATION: &str = "30s";
const DEFAULT_PROCESS_WATCH_INTERVAL: &str = "1s";
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const TAIL_LOG_POLL_LIMIT_BYTES: u64 = 256 * 1024;

#[derive(Args, Clone)]
pub struct ObserveArgs {
    /// Component whose live system is being observed (optional when `--path` is supplied).
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    /// Observation duration, e.g. 30s, 5m, 1h.
    #[arg(long, default_value = DEFAULT_DURATION, value_parser = parse_duration)]
    pub duration: Duration,

    /// Log file to tail. Repeatable.
    #[arg(long = "tail-log", value_name = "PATH")]
    pub tail_logs: Vec<PathBuf>,

    /// Only emit tailed log lines matching this regex. Applies to all --tail-log probes.
    #[arg(long, value_name = "REGEX")]
    pub grep: Option<String>,

    /// Regex matched against process command lines during snapshots. Repeatable.
    #[arg(long = "watch-process", value_name = "REGEX")]
    pub watch_processes: Vec<String>,

    /// Poll interval for --watch-process probes, e.g. 500ms, 1s, 5s.
    #[arg(
        long = "watch-process-interval",
        default_value = DEFAULT_PROCESS_WATCH_INTERVAL,
        value_parser = parse_duration
    )]
    pub watch_process_interval: Duration,

    /// Portable trace probe config as JSON. Repeatable.
    #[arg(long = "probe", value_name = "JSON")]
    pub probes: Vec<String>,
}

#[derive(Serialize)]
pub struct ObserveOutput {
    pub command: &'static str,
    pub run_id: String,
    pub component_id: String,
    pub status: String,
    pub duration_ms: u64,
    pub event_count: usize,
    pub artifact_path: String,
    pub hints: Vec<String>,
}

struct TailLogState {
    path: PathBuf,
    offset: u64,
    grep: Option<Regex>,
}

struct ProcessWatchState {
    pattern: String,
    regex: Regex,
    seen: BTreeMap<u32, ProcessInfo>,
    initialized: bool,
    last_poll: Option<Instant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessInfo {
    ppid: u32,
    command: String,
}

pub fn run(args: ObserveArgs, _global: &GlobalArgs) -> CmdResult<ObserveOutput> {
    validate_probe_selection(&args)?;
    // `observe` runs only passive probes (--tail-log, --watch-process, --probe).
    // None of them require an extension provider, so we resolve the source
    // context with no capability and gracefully accept unregistered paths via
    // `--path`. Probes that intrinsically need component metadata should
    // surface a clear error in their own validation, not in component lookup.
    let ctx = execution_context::resolve(&ResolveOptions {
        component_id: args.comp.component.clone(),
        path_override: args.comp.path.clone(),
        ..Default::default()
    })?;
    let component_id = ctx.component_id.clone();
    let component_path = ctx.source_path.clone();
    let run_dir = RunDir::create()?;
    let trace_path = run_dir.step_file(run_dir::files::TRACE_RESULTS);
    let store = ObservationStore::open_initialized()?;
    let command = observe_command(&args);
    let initial_metadata = serde_json::json!({
        "duration_ms": duration_millis(args.duration),
        "tail_logs": args.tail_logs,
        "grep": args.grep,
        "watch_processes": args.watch_processes,
        "watch_process_interval_ms": duration_millis(args.watch_process_interval),
        "probes": args.probes,
        "run_dir": run_dir.path(),
    });

    let run = store.start_run(
        NewRunRecord::builder("observe")
            .component_id(component_id.clone())
            .command(command)
            .cwd_path(&component_path)
            .current_homeboy_version()
            .git_sha(short_head_revision_at(&component_path))
            .metadata(initial_metadata)
            .build(),
    )?;

    let mut status = RunStatus::Pass;
    let observe_result = collect_timeline(&args);
    let (timeline, failure) = match observe_result {
        Ok(timeline) => (timeline, None),
        Err(error) => {
            status = RunStatus::Error;
            (
                vec![event(
                    0,
                    "observe",
                    "error",
                    [("message", error.to_string())],
                )],
                Some(error.to_string()),
            )
        }
    };

    let results = TraceResults {
        component_id: component_id.clone(),
        scenario_id: "observe".to_string(),
        status: trace_status_for_run_status(status),
        summary: Some("Passive observation timeline".to_string()),
        failure: failure.clone(),
        rig: None,
        evidence: None,
        timeline,
        span_definitions: Vec::new(),
        span_results: Vec::new(),
        assertions: Vec::new(),
        temporal_assertions: Vec::new(),
        artifacts: vec![TraceArtifact {
            label: "observe timeline".to_string(),
            path: trace_path.to_string_lossy().to_string(),
            kind: None,
        }],
        metrics: Default::default(),
        toolchain: None,
        components: None,
        dependencies: Vec::new(),
        preview: None,
    };

    write_trace_results(&trace_path, &results)?;
    let artifact = store.record_artifact(&run.id, "trace-results", &trace_path)?;
    let artifact_id = artifact.id.clone();
    let artifact_path = artifact.path.clone();
    let finished = store.finish_run(
        &run.id,
        status,
        Some(serde_json::json!({
            "duration_ms": duration_millis(args.duration),
            "event_count": results.timeline.len(),
            "trace_results_artifact_id": artifact_id,
            "trace_results_path": artifact_path.clone(),
            "failure": failure,
        })),
    )?;
    run_dir.cleanup();

    Ok((
        ObserveOutput {
            command: "observe",
            run_id: finished.id.clone(),
            component_id,
            status: finished.status,
            duration_ms: duration_millis(args.duration),
            event_count: results.timeline.len(),
            artifact_path,
            hints: vec![
                format!("View this run: homeboy runs show {}", finished.id),
                "List observe runs: homeboy runs list --kind observe".to_string(),
            ],
        },
        if status == RunStatus::Pass { 0 } else { 1 },
    ))
}

fn validate_probe_selection(args: &ObserveArgs) -> homeboy::core::Result<()> {
    if args.tail_logs.is_empty() && args.watch_processes.is_empty() && args.probes.is_empty() {
        return Err(Error::validation_invalid_argument(
            "probe",
            "observe requires at least one --tail-log, --watch-process, or --probe",
            None,
            Some(vec![
                "homeboy observe my-component --duration 30s --tail-log /path/to/app.log"
                    .to_string(),
                "homeboy observe --path /path/to/checkout --duration 30s --watch-process 'node .*serve'"
                    .to_string(),
                r#"homeboy observe my-component --duration 30s --probe '{"type":"http.poll","url":"http://127.0.0.1:3000/health"}'"#.to_string(),
            ]),
        ));
    }
    Ok(())
}

fn collect_timeline(args: &ObserveArgs) -> homeboy::core::Result<Vec<TraceEvent>> {
    let start = Instant::now();
    let mut tail_logs = build_tail_log_states(args)?;
    let mut process_watches = build_process_watch_states(args)?;
    let active_probes = ActiveTraceProbes::start(&build_standard_probe_configs(args)?)?;
    let mut timeline = vec![empty_event(0, "observe", "started")];

    loop {
        let t_ms = elapsed_ms(start);
        for tail in &mut tail_logs {
            timeline.extend(poll_tail_log(tail, t_ms)?);
        }
        timeline.extend(poll_due_process_watches(
            &mut process_watches,
            start,
            args.watch_process_interval,
            t_ms,
            capture_process_snapshot,
        )?);

        if start.elapsed() >= args.duration {
            break;
        }
        thread::sleep(POLL_INTERVAL.min(args.duration.saturating_sub(start.elapsed())));
    }

    timeline.push(empty_event(elapsed_ms(start), "observe", "finished"));
    timeline.extend(active_probes.stop());
    timeline.sort_by_key(|event| event.t_ms);
    Ok(timeline)
}

fn build_standard_probe_configs(
    args: &ObserveArgs,
) -> homeboy::core::Result<Vec<TraceProbeConfig>> {
    args.probes
        .iter()
        .map(|raw| {
            serde_json::from_str::<TraceProbeConfig>(raw).map_err(|error| {
                Error::validation_invalid_argument(
                    "probe",
                    format!("invalid probe JSON: {error}"),
                    Some(raw.clone()),
                    None,
                )
            })
        })
        .collect()
}

fn build_tail_log_states(args: &ObserveArgs) -> homeboy::core::Result<Vec<TailLogState>> {
    let grep = args
        .grep
        .as_deref()
        .map(|pattern| Regex::new(pattern).map_err(|e| invalid_regex("grep", pattern, e)))
        .transpose()?;

    args.tail_logs
        .iter()
        .map(|path| {
            let offset = File::open(path)
                .and_then(|file| file.metadata())
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            Ok(TailLogState {
                path: path.clone(),
                offset,
                grep: grep.clone(),
            })
        })
        .collect()
}

fn build_process_watch_states(args: &ObserveArgs) -> homeboy::core::Result<Vec<ProcessWatchState>> {
    args.watch_processes
        .iter()
        .map(|pattern| {
            Ok(ProcessWatchState {
                pattern: pattern.clone(),
                regex: Regex::new(pattern)
                    .map_err(|e| invalid_regex("watch-process", pattern, e))?,
                seen: BTreeMap::new(),
                initialized: false,
                last_poll: None,
            })
        })
        .collect()
}

fn watch_process_is_due(state: &mut ProcessWatchState, start: Instant, interval: Duration) -> bool {
    let now = Instant::now();
    if state.last_poll.is_none()
        || now
            .duration_since(state.last_poll.unwrap_or(start))
            .ge(&interval)
    {
        state.last_poll = Some(now);
        return true;
    }
    false
}

fn poll_due_process_watches<F>(
    process_watches: &mut [ProcessWatchState],
    start: Instant,
    interval: Duration,
    t_ms: u64,
    mut capture_snapshot: F,
) -> homeboy::core::Result<Vec<TraceEvent>>
where
    F: FnMut() -> homeboy::core::Result<String>,
{
    let mut process_snapshot = None;
    let mut events = Vec::new();

    for watch in process_watches {
        if watch_process_is_due(watch, start, interval) {
            if process_snapshot.is_none() {
                process_snapshot = Some(capture_snapshot()?);
            }
            events.extend(poll_process_watch_from_snapshot(
                watch,
                t_ms,
                process_snapshot
                    .as_deref()
                    .expect("process snapshot should be captured before polling watches"),
            ));
        }
    }

    Ok(events)
}

fn poll_tail_log(state: &mut TailLogState, t_ms: u64) -> homeboy::core::Result<Vec<TraceEvent>> {
    let mut file = match File::open(&state.path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::internal_io(
                format!("Failed to open log {}: {}", state.path.display(), error),
                Some("observe.tail_log.open".to_string()),
            ))
        }
    };

    let len = file
        .metadata()
        .map_err(|e| {
            Error::internal_io(e.to_string(), Some("observe.tail_log.metadata".to_string()))
        })?
        .len();
    if len < state.offset {
        state.offset = 0;
    }
    if len == state.offset {
        return Ok(Vec::new());
    }

    file.seek(SeekFrom::Start(state.offset)).map_err(|e| {
        Error::internal_io(e.to_string(), Some("observe.tail_log.seek".to_string()))
    })?;
    let mut bytes = Vec::new();
    file.take(TAIL_LOG_POLL_LIMIT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| {
            Error::internal_io(e.to_string(), Some("observe.tail_log.read".to_string()))
        })?;
    let truncated = bytes.len() > TAIL_LOG_POLL_LIMIT_BYTES as usize;
    if truncated {
        let overflow = bytes.len() - TAIL_LOG_POLL_LIMIT_BYTES as usize;
        bytes.drain(..overflow);
    }
    state.offset = len;
    let content = String::from_utf8_lossy(&bytes);

    let mut events = content
        .lines()
        .filter(|line| {
            state
                .grep
                .as_ref()
                .map(|re| re.is_match(line))
                .unwrap_or(true)
        })
        .map(|line| {
            event(
                t_ms,
                "log",
                "line",
                [
                    ("path", state.path.to_string_lossy().to_string()),
                    ("line", line.to_string()),
                ],
            )
        })
        .collect::<Vec<_>>();
    if truncated {
        events.insert(
            0,
            event(
                t_ms,
                "log",
                "truncated",
                [
                    ("path", state.path.to_string_lossy().to_string()),
                    ("byte_limit", TAIL_LOG_POLL_LIMIT_BYTES.to_string()),
                ],
            ),
        );
    }
    Ok(events)
}

fn capture_process_snapshot() -> homeboy::core::Result<String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), Some("observe.process.ps".to_string())))?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "ps failed while observing processes: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn poll_process_watch_from_snapshot(
    state: &mut ProcessWatchState,
    t_ms: u64,
    stdout: &str,
) -> Vec<TraceEvent> {
    let mut current = BTreeMap::new();
    let mut events = Vec::new();

    for (pid, info) in parse_process_snapshot(stdout) {
        let command = info.command.as_str();
        if state.regex.is_match(command) {
            if !state.seen.contains_key(&pid) {
                if state.initialized {
                    events.push(event(
                        t_ms,
                        "process",
                        "spawn",
                        [
                            ("pattern", state.pattern.clone()),
                            ("pid", pid.to_string()),
                            ("ppid", info.ppid.to_string()),
                            ("command", command.to_string()),
                        ],
                    ));
                } else {
                    events.push(event(
                        t_ms,
                        "process",
                        "matched",
                        [
                            ("pattern", state.pattern.clone()),
                            ("pid", pid.to_string()),
                            ("command", command.to_string()),
                        ],
                    ));
                }
            }
            current.insert(pid, info);
        }
    }

    for (pid, info) in &state.seen {
        if current.contains_key(pid) {
            continue;
        }
        events.push(event(
            t_ms,
            "process",
            "exit",
            [
                ("pattern", state.pattern.clone()),
                ("pid", pid.to_string()),
                ("was_command", info.command.clone()),
            ],
        ));
    }

    state.seen = current;
    state.initialized = true;
    events
}

fn parse_process_snapshot(stdout: &str) -> Vec<(u32, ProcessInfo)> {
    stdout
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let mut parts = trimmed.split_whitespace();
            let pid = parts.next()?.trim().parse::<u32>().ok()?;
            let ppid = parts.next()?.trim().parse::<u32>().ok()?;
            let command = parts.collect::<Vec<_>>().join(" ");
            if command.is_empty() {
                return None;
            }
            Some((pid, ProcessInfo { ppid, command }))
        })
        .collect()
}

fn write_trace_results(path: &Path, results: &TraceResults) -> homeboy::core::Result<()> {
    let content = serde_json::to_string_pretty(results).map_err(|e| {
        Error::internal_unexpected(format!("Failed to serialize observe timeline: {e}"))
    })?;
    std::fs::write(path, content).map_err(|e| {
        Error::internal_io(
            format!("Failed to write observe timeline {}: {}", path.display(), e),
            Some("observe.write_trace_results".to_string()),
        )
    })
}

fn trace_status_for_run_status(status: RunStatus) -> TraceStatus {
    match status {
        RunStatus::Pass => TraceStatus::Pass,
        RunStatus::Fail => TraceStatus::Fail,
        _ => TraceStatus::Error,
    }
}

fn observe_command(args: &ObserveArgs) -> String {
    let mut parts = vec!["homeboy".to_string(), "observe".to_string()];
    if let Some(component) = &args.comp.component {
        parts.push(component.clone());
    }
    if let Some(path) = &args.comp.path {
        parts.push("--path".to_string());
        parts.push(path.clone());
    }
    parts.push("--duration".to_string());
    parts.push(format_duration(args.duration));
    for path in &args.tail_logs {
        parts.push("--tail-log".to_string());
        parts.push(path.to_string_lossy().to_string());
    }
    if let Some(grep) = &args.grep {
        parts.push("--grep".to_string());
        parts.push(grep.clone());
    }
    for pattern in &args.watch_processes {
        parts.push("--watch-process".to_string());
        parts.push(pattern.clone());
    }
    if !args.watch_processes.is_empty() {
        parts.push("--watch-process-interval".to_string());
        parts.push(format_duration(args.watch_process_interval));
    }
    for probe in &args.probes {
        parts.push("--probe".to_string());
        parts.push(probe.clone());
    }
    parts.join(" ")
}

fn event<K, V, I>(t_ms: u64, source: &str, name: &str, data: I) -> TraceEvent
where
    K: Into<String>,
    V: Into<serde_json::Value>,
    I: IntoIterator<Item = (K, V)>,
{
    TraceEvent {
        t_ms,
        source: source.to_string(),
        event: name.to_string(),
        data: data
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn empty_event(t_ms: u64, source: &str, name: &str) -> TraceEvent {
    TraceEvent {
        t_ms,
        source: source.to_string(),
        event: name.to_string(),
        data: BTreeMap::new(),
    }
}

fn parse_duration(raw: &str) -> Result<Duration, String> {
    let raw = raw.trim();
    let split = raw
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "expected duration like 500ms, 30s, 5m, or 1h".to_string())?;
    let (amount_raw, unit) = raw.split_at(split);
    let amount = amount_raw
        .parse::<u64>()
        .map_err(|_| "duration amount must be a positive integer".to_string())?;
    if amount == 0 {
        return Err("duration amount must be greater than zero".to_string());
    }

    match unit {
        "ms" => Ok(Duration::from_millis(amount)),
        "s" => Ok(Duration::from_secs(amount)),
        "m" => Ok(Duration::from_secs(amount * 60)),
        "h" => Ok(Duration::from_secs(amount * 60 * 60)),
        _ => Err("duration unit must be one of ms, s, m, or h".to_string()),
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_millis() < 1000 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{}s", duration.as_secs())
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn elapsed_ms(start: Instant) -> u64 {
    duration_millis(start.elapsed())
}

fn invalid_regex(field: &str, pattern: &str, error: regex::Error) -> Error {
    Error::validation_invalid_argument(
        field,
        format!("invalid regex `{pattern}`: {error}"),
        Some(pattern.to_string()),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::test_support::with_isolated_home;

    #[test]
    fn parse_duration_accepts_supported_units() {
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("3m").unwrap(), Duration::from_secs(180));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn tail_log_emits_matching_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        std::fs::write(&path, "already here\n").unwrap();
        let mut state = TailLogState {
            path: path.clone(),
            offset: std::fs::metadata(&path).unwrap().len(),
            grep: Some(Regex::new("invalid_grant").unwrap()),
        };

        std::fs::write(
            &path,
            "already here\nignore me\nHTTP 400 invalid_grant from oauth\n",
        )
        .unwrap();
        let events = poll_tail_log(&mut state, 42).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].t_ms, 42);
        assert_eq!(events[0].source, "log");
        assert_eq!(events[0].event, "line");
        assert_eq!(
            events[0].data.get("line").and_then(|value| value.as_str()),
            Some("HTTP 400 invalid_grant from oauth")
        );
    }

    #[test]
    fn process_watch_emits_initial_matches_then_spawn_and_exit_deltas() {
        let mut state = ProcessWatchState {
            pattern: "sleep".to_string(),
            regex: Regex::new("sleep").unwrap(),
            seen: BTreeMap::new(),
            initialized: false,
            last_poll: None,
        };

        let initial = poll_process_watch_from_snapshot(&mut state, 0, " 10 1 sleep 30\n");
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].event, "matched");
        assert_eq!(
            initial[0].data.get("pid").and_then(|value| value.as_str()),
            Some("10")
        );

        let spawned = poll_process_watch_from_snapshot(
            &mut state,
            1000,
            " 10 1 sleep 30\n 11 10 /bin/sleep 5\n",
        );
        assert_eq!(spawned.len(), 1);
        assert_eq!(spawned[0].event, "spawn");
        assert_eq!(
            spawned[0].data.get("pid").and_then(|value| value.as_str()),
            Some("11")
        );
        assert_eq!(
            spawned[0].data.get("ppid").and_then(|value| value.as_str()),
            Some("10")
        );

        let exited = poll_process_watch_from_snapshot(&mut state, 2000, " 10 1 sleep 30\n");
        assert_eq!(exited.len(), 1);
        assert_eq!(exited[0].event, "exit");
        assert_eq!(
            exited[0].data.get("pid").and_then(|value| value.as_str()),
            Some("11")
        );
        assert_eq!(
            exited[0]
                .data
                .get("was_command")
                .and_then(|value| value.as_str()),
            Some("/bin/sleep 5")
        );
    }

    #[test]
    fn due_process_watchers_share_a_single_snapshot() {
        let snapshot = " 10 1 sleep 30\n 11 1 node server.js\n";
        let mut watches = vec![
            ProcessWatchState {
                pattern: "sleep".to_string(),
                regex: Regex::new("sleep").unwrap(),
                seen: BTreeMap::new(),
                initialized: false,
                last_poll: None,
            },
            ProcessWatchState {
                pattern: "node".to_string(),
                regex: Regex::new("node").unwrap(),
                seen: BTreeMap::new(),
                initialized: false,
                last_poll: None,
            },
        ];
        let mut captures = 0;

        let events = poll_due_process_watches(
            &mut watches,
            Instant::now(),
            Duration::from_secs(1),
            0,
            || {
                captures += 1;
                Ok(snapshot.to_string())
            },
        )
        .unwrap();

        assert_eq!(captures, 1);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].data.get("pid").and_then(|value| value.as_str()),
            Some("10")
        );
        assert_eq!(
            events[1].data.get("pid").and_then(|value| value.as_str()),
            Some("11")
        );
    }

    #[test]
    fn process_watch_interval_defaults_to_one_second() {
        let command = <ObserveArgs as clap::Args>::augment_args(clap::Command::new("homeboy"));
        let matches = command
            .try_get_matches_from(["homeboy", "demo", "--watch-process", "sleep"])
            .unwrap();
        assert_eq!(
            <ObserveArgs as clap::FromArgMatches>::from_arg_matches(&matches)
                .unwrap()
                .watch_process_interval,
            Duration::from_secs(1)
        );

        let command = <ObserveArgs as clap::Args>::augment_args(clap::Command::new("homeboy"));
        let matches = command
            .try_get_matches_from([
                "homeboy",
                "demo",
                "--watch-process",
                "sleep",
                "--watch-process-interval",
                "250ms",
            ])
            .unwrap();
        assert_eq!(
            <ObserveArgs as clap::FromArgMatches>::from_arg_matches(&matches)
                .unwrap()
                .watch_process_interval,
            Duration::from_millis(250)
        );
    }

    fn args_with_probes(component: Option<&str>, probes: Vec<String>) -> ObserveArgs {
        ObserveArgs {
            comp: PositionalComponentArgs {
                component: component.map(str::to_string),
                path: None,
            },
            duration: Duration::from_millis(50),
            tail_logs: Vec::new(),
            grep: None,
            watch_processes: Vec::new(),
            watch_process_interval: Duration::from_secs(1),
            probes,
        }
    }

    #[test]
    fn standard_probe_json_parses_http_poll_and_process_snapshot() {
        let args = args_with_probes(
            Some("demo"),
            vec![
                r#"{"type":"http.poll","url":"http://127.0.0.1:1234/health","interval_ms":25,"assert-status":200}"#.to_string(),
                r#"{"type":"process.snapshot","pattern":"homeboy","interval_ms":25}"#.to_string(),
            ],
        );

        let probes = build_standard_probe_configs(&args).unwrap();

        assert!(matches!(probes[0], TraceProbeConfig::HttpPoll { .. }));
        assert!(matches!(
            probes[1],
            TraceProbeConfig::ProcessSnapshot { .. }
        ));
    }

    #[test]
    fn observe_accepts_standard_probe_without_legacy_flags() {
        let args = args_with_probes(
            Some("demo"),
            vec![r#"{"type":"cmd.run","command":"true"}"#.to_string()],
        );

        validate_probe_selection(&args).unwrap();
    }

    #[test]
    fn collect_timeline_consumes_standard_http_poll_and_process_snapshot() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0; 512];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let process_pattern = std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "homeboy".to_string());
        let mut args = args_with_probes(
            Some("demo"),
            vec![
                format!(
                    r#"{{"type":"http.poll","url":"{url}","interval_ms":25,"assert-status":204}}"#
                ),
                format!(
                    r#"{{"type":"process.snapshot","pattern":"{}","interval_ms":25}}"#,
                    process_pattern
                ),
            ],
        );
        args.duration = Duration::from_millis(80);

        let timeline = collect_timeline(&args).unwrap();
        let _ = server.join();

        assert!(timeline.iter().any(|event| event.source == "http.poll"
            && event.event == "http.response"
            && event.data.get("status").and_then(|value| value.as_u64()) == Some(204)));
        assert!(timeline
            .iter()
            .any(|event| event.source == "process.snapshot" && event.event == "proc.list"));
    }

    /// Issue #2366: `homeboy observe --path <DIR>` parses without a positional
    /// component, mirroring `lint --path` and `trace --path`. The path is
    /// captured in `comp.path` so the runner can degrade component-level
    /// lookups gracefully.
    #[test]
    fn observe_accepts_path_without_component() {
        let command = <ObserveArgs as clap::Args>::augment_args(clap::Command::new("homeboy"));
        let matches = command
            .try_get_matches_from([
                "homeboy",
                "--path",
                "/Users/user/Developer/opencode",
                "--watch-process",
                ".opencode serve",
            ])
            .expect("observe should parse --path without a component arg");

        let parsed = <ObserveArgs as clap::FromArgMatches>::from_arg_matches(&matches).unwrap();
        assert!(parsed.comp.component.is_none());
        assert_eq!(
            parsed.comp.path.as_deref(),
            Some("/Users/user/Developer/opencode")
        );
        assert_eq!(parsed.watch_processes, vec![".opencode serve".to_string()]);
    }

    /// Issue #2366: registered components keep working unchanged — positional
    /// component IDs still parse without `--path`, preserving the legacy UX.
    #[test]
    fn observe_accepts_registered_component_unchanged() {
        let command = <ObserveArgs as clap::Args>::augment_args(clap::Command::new("homeboy"));
        let matches = command
            .try_get_matches_from(["homeboy", "demo", "--watch-process", "sleep"])
            .expect("observe should parse a positional component without --path");

        let parsed = <ObserveArgs as clap::FromArgMatches>::from_arg_matches(&matches).unwrap();
        assert_eq!(parsed.comp.component.as_deref(), Some("demo"));
        assert!(parsed.comp.path.is_none());
    }

    #[test]
    fn observe_reports_persisted_artifact_and_cleans_run_dir() {
        with_isolated_home(|home| {
            let runtime = tempfile::tempdir().expect("runtime tmpdir");
            std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", runtime.path());
            let target = home.path().join("target");
            std::fs::create_dir_all(&target).expect("target dir");

            let args = ObserveArgs {
                comp: PositionalComponentArgs {
                    component: None,
                    path: Some(target.to_string_lossy().to_string()),
                },
                duration: Duration::from_millis(1),
                tail_logs: Vec::new(),
                grep: None,
                watch_processes: vec!["unlikely-homeboy-observe-test-process".to_string()],
                watch_process_interval: Duration::from_millis(1),
                probes: Vec::new(),
            };

            let (output, exit_code) = run(args, &GlobalArgs {}).expect("observe run");

            assert_eq!(exit_code, 0);
            assert!(std::path::Path::new(&output.artifact_path).is_file());
            assert!(!std::path::Path::new(&output.artifact_path).starts_with(runtime.path()));
            assert!(
                std::fs::read_dir(runtime.path())
                    .expect("runtime entries")
                    .next()
                    .is_none(),
                "observe should clean completed run dirs"
            );
        });
    }

    /// Issue #2366: `--path` pointed at an unregistered directory resolves to
    /// a synthetic component so the rest of the observe pipeline (run record,
    /// trace results, observation store) sees a well-formed component id and
    /// source path. Mirrors `lint --path` behavior introduced for #2361.
    #[test]
    fn observe_path_override_synthesizes_component_from_unregistered_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("ad-hoc-target");
        std::fs::create_dir_all(&repo).expect("create repo dir");

        let ctx = execution_context::resolve(&ResolveOptions {
            component_id: None,
            path_override: Some(repo.to_string_lossy().to_string()),
            ..Default::default()
        })
        .expect("path-only override should resolve");

        assert_eq!(ctx.component_id, "ad-hoc-target");
        assert_eq!(ctx.source_path, repo);
    }

    /// Issue #2366: probe selection validation hint mentions the `--path`
    /// shape so unregistered probes are discoverable from the error.
    #[test]
    fn validate_probe_selection_hints_at_path_usage() {
        let args = ObserveArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: Some("/tmp/anywhere".to_string()),
            },
            duration: Duration::from_secs(30),
            tail_logs: Vec::new(),
            grep: None,
            watch_processes: Vec::new(),
            watch_process_interval: Duration::from_secs(1),
            probes: Vec::new(),
        };

        let err = validate_probe_selection(&args).expect_err("missing probes should error");
        let rendered = err.to_string();
        assert!(
            rendered.contains("--tail-log")
                || rendered.contains("--watch-process")
                || rendered.contains("--probe"),
            "expected probe-selection error, got: {rendered}"
        );
    }
}
