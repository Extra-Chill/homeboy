use std::collections::BTreeMap;
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use super::{event, push_event, TraceEvent};

pub(super) fn run_cmd_run(
    command: String,
    args: Vec<String>,
    started_at: Instant,
    events: Arc<Mutex<Vec<TraceEvent>>>,
    stop: mpsc::Receiver<()>,
) {
    push_cmd_event(
        &events,
        started_at,
        "cmd.start",
        command_start_data(&command, &args),
    );

    let command_started = Instant::now();
    match Command::new(&command).args(&args).output() {
        Ok(output) => {
            emit_cmd_lines(
                "cmd.stdout",
                &String::from_utf8_lossy(&output.stdout),
                started_at,
                &events,
            );
            emit_cmd_lines(
                "cmd.stderr",
                &String::from_utf8_lossy(&output.stderr),
                started_at,
                &events,
            );
            push_cmd_event(
                &events,
                started_at,
                "cmd.exit",
                command_finish_data(
                    command_started,
                    output.status.code(),
                    output.status.success(),
                ),
            );
        }
        Err(error) => {
            let mut data = command_duration_data(command_started);
            data.insert(
                "error".to_string(),
                serde_json::Value::String(error.to_string()),
            );
            push_event(&events, event(started_at, "cmd.run", "cmd.error", data));
        }
    }
    let _ = stop.recv_timeout(Duration::from_millis(1));
}

fn push_cmd_event(
    events: &Arc<Mutex<Vec<TraceEvent>>>,
    started_at: Instant,
    event_name: &str,
    data: BTreeMap<String, serde_json::Value>,
) {
    push_event(events, event(started_at, "cmd.run", event_name, data));
}

fn command_start_data(command: &str, args: &[String]) -> BTreeMap<String, serde_json::Value> {
    let mut data = BTreeMap::new();
    data.insert(
        "command".to_string(),
        serde_json::Value::String(command.to_string()),
    );
    data.insert(
        "args".to_string(),
        serde_json::Value::Array(
            args.iter()
                .map(|arg| serde_json::Value::String(arg.clone()))
                .collect(),
        ),
    );
    data
}

fn command_finish_data(
    started_at: Instant,
    exit_code: Option<i32>,
    success: bool,
) -> BTreeMap<String, serde_json::Value> {
    let mut data = command_duration_data(started_at);
    data.insert("exit_code".to_string(), serde_json::json!(exit_code));
    data.insert("success".to_string(), serde_json::json!(success));
    data
}

fn command_duration_data(started_at: Instant) -> BTreeMap<String, serde_json::Value> {
    let mut data = BTreeMap::new();
    data.insert(
        "duration_ms".to_string(),
        serde_json::json!(started_at.elapsed().as_millis() as u64),
    );
    data
}

fn emit_cmd_lines(
    event_name: &str,
    output: &str,
    started_at: Instant,
    events: &Arc<Mutex<Vec<TraceEvent>>>,
) {
    for line in output.lines() {
        let mut data = BTreeMap::new();
        data.insert(
            "line".to_string(),
            serde_json::Value::String(line.to_string()),
        );
        push_cmd_event(events, started_at, event_name, data);
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::super::{ActiveTraceProbes, TraceProbeConfig};

    #[test]
    fn test_run_cmd_run() {
        let probes = ActiveTraceProbes::start(&[TraceProbeConfig::CmdRun {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "printf probe-output".to_string()],
        }])
        .expect("start probes");
        thread::sleep(Duration::from_millis(60));

        let events = probes.stop();
        assert!(events
            .iter()
            .any(|event| event.source == "cmd.run" && event.event == "cmd.start"));
        assert!(events.iter().any(|event| event.source == "cmd.run"
            && event.event == "cmd.stdout"
            && event.data.get("line").and_then(|value| value.as_str()) == Some("probe-output")));
        assert!(events.iter().any(|event| event.source == "cmd.run"
            && event.event == "cmd.exit"
            && event.data.get("success").and_then(|value| value.as_bool()) == Some(true)));
    }
}
