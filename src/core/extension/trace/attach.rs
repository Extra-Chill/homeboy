use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::core::engine::run_dir::RunDir;
use crate::core::error::{Error, Result};
use crate::core::http_probe;

use super::parsing::{TraceArtifact, TraceEvent, TraceResults};

const TRACE_ATTACHMENTS_ARTIFACT: &str = "trace-attachments.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceAttachment {
    pub kind: String,
    pub target: String,
}

impl TraceAttachment {
    pub fn parse_all(raw_attachments: &[String]) -> Result<Vec<Self>> {
        raw_attachments
            .iter()
            .map(|attachment| Self::parse(attachment))
            .collect()
    }

    pub fn parse(raw: &str) -> Result<Self> {
        if raw.starts_with("http://") || raw.starts_with("https://") {
            return Ok(Self {
                kind: "http".to_string(),
                target: raw.to_string(),
            });
        }
        let Some((kind, target)) = raw.split_once(':') else {
            return Err(invalid_attachment(raw, "expected KIND:TARGET"));
        };
        if target.is_empty() {
            return Err(invalid_attachment(raw, "attachment target cannot be empty"));
        }
        match kind {
            "logfile" | "fswatch" | "systemd" => Ok(Self {
                kind: kind.to_string(),
                target: target.to_string(),
            }),
            "pid" => {
                let pid = target.parse::<u32>().map_err(|_| {
                    invalid_attachment(raw, "pid target must be a positive integer")
                })?;
                if pid == 0 {
                    return Err(invalid_attachment(
                        raw,
                        "pid target must be greater than zero",
                    ));
                }
                Ok(Self {
                    kind: kind.to_string(),
                    target: target.to_string(),
                })
            }
            "port" => {
                let port = target
                    .parse::<u16>()
                    .map_err(|_| invalid_attachment(raw, "port target must be a TCP port"))?;
                if port == 0 {
                    return Err(invalid_attachment(
                        raw,
                        "port target must be greater than zero",
                    ));
                }
                Ok(Self {
                    kind: kind.to_string(),
                    target: port.to_string(),
                })
            }
            "http" => {
                if !(target.starts_with("http://") || target.starts_with("https://")) {
                    return Err(invalid_attachment(
                        raw,
                        "http target must start with http:// or https://",
                    ));
                }
                Ok(Self {
                    kind: kind.to_string(),
                    target: target.to_string(),
                })
            }
            _ => Err(invalid_attachment(
                raw,
                "supported attachment kinds are logfile, fswatch, pid, port, http, and systemd",
            )),
        }
    }
}

fn invalid_attachment(raw: &str, reason: &str) -> Error {
    Error::validation_invalid_argument(
        "trace.attach",
        format!("invalid trace attachment `{raw}`: {reason}"),
        None,
        None,
    )
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct TraceAttachmentObservation {
    phase: &'static str,
    elapsed_ms: u64,
    attachment: TraceAttachment,
    status: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    data: BTreeMap<String, serde_json::Value>,
}

pub(super) fn observe_trace_attachments(
    attachments: &[TraceAttachment],
    phase: &'static str,
    started_at: Instant,
) -> Vec<TraceAttachmentObservation> {
    attachments
        .iter()
        .map(|attachment| {
            let (status, data) = observe_trace_attachment(attachment);
            TraceAttachmentObservation {
                phase,
                elapsed_ms: started_at
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
                attachment: attachment.clone(),
                status,
                data,
            }
        })
        .collect()
}

fn observe_trace_attachment(
    attachment: &TraceAttachment,
) -> (String, BTreeMap<String, serde_json::Value>) {
    match attachment.kind.as_str() {
        "logfile" => observe_logfile(&attachment.target),
        "fswatch" => observe_fswatch(&attachment.target),
        "pid" => observe_pid(&attachment.target),
        "port" => observe_port(&attachment.target),
        "http" => observe_http(&attachment.target),
        "systemd" => observe_systemd(&attachment.target),
        _ => ("unsupported".to_string(), BTreeMap::new()),
    }
}

fn observe_logfile(path: &str) -> (String, BTreeMap<String, serde_json::Value>) {
    observe_file_state(path)
}

fn observe_fswatch(path: &str) -> (String, BTreeMap<String, serde_json::Value>) {
    observe_file_state(path)
}

fn observe_file_state(path: &str) -> (String, BTreeMap<String, serde_json::Value>) {
    let mut data = BTreeMap::new();
    data.insert(
        "path".to_string(),
        serde_json::Value::String(path.to_string()),
    );
    match std::fs::metadata(path) {
        Ok(metadata) => {
            data.insert("bytes".to_string(), serde_json::json!(metadata.len()));
            if let Ok(modified) = metadata.modified() {
                let modified_ms = modified
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_millis())
                    .unwrap_or_default();
                data.insert(
                    "modified_unix_ms".to_string(),
                    serde_json::json!(modified_ms),
                );
            }
            ("present".to_string(), data)
        }
        Err(error) => {
            data.insert(
                "error".to_string(),
                serde_json::Value::String(error.to_string()),
            );
            ("missing".to_string(), data)
        }
    }
}

fn observe_pid(raw_pid: &str) -> (String, BTreeMap<String, serde_json::Value>) {
    let mut data = BTreeMap::new();
    data.insert("pid".to_string(), serde_json::json!(raw_pid));
    let Ok(pid) = raw_pid.parse::<u32>() else {
        return ("error".to_string(), data);
    };
    let running = process_exists(pid);
    data.insert("running".to_string(), serde_json::json!(running));
    if running {
        ("running".to_string(), data)
    } else {
        ("missing".to_string(), data)
    }
}

fn process_exists(pid: u32) -> bool {
    crate::core::process::pid_is_running(pid)
}

fn observe_port(raw_port: &str) -> (String, BTreeMap<String, serde_json::Value>) {
    let mut data = BTreeMap::new();
    data.insert("port".to_string(), serde_json::json!(raw_port));
    let Ok(port) = raw_port.parse::<u16>() else {
        return ("error".to_string(), data);
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listening = TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok();
    data.insert("listening".to_string(), serde_json::json!(listening));
    if listening {
        ("listening".to_string(), data)
    } else {
        ("closed".to_string(), data)
    }
}

fn observe_http(url: &str) -> (String, BTreeMap<String, serde_json::Value>) {
    let mut data = BTreeMap::new();
    data.insert(
        "url".to_string(),
        serde_json::Value::String(url.to_string()),
    );
    match http_probe::get_status(url, Duration::from_secs(1)) {
        Ok(status) => {
            data.insert("status_code".to_string(), serde_json::json!(status));
            ("reachable".to_string(), data)
        }
        Err(error) => {
            data.insert(
                "error".to_string(),
                serde_json::Value::String(error.message),
            );
            ("unreachable".to_string(), data)
        }
    }
}

fn observe_systemd(unit: &str) -> (String, BTreeMap<String, serde_json::Value>) {
    let mut data = BTreeMap::new();
    data.insert("unit".to_string(), serde_json::json!(unit));

    let output = Command::new("systemctl")
        .args([
            "show",
            unit,
            "--no-pager",
            "--property=Id,LoadState,ActiveState,SubState,MainPID,ExecMainStatus,Result,FragmentPath",
        ])
        .output();

    let output = match output {
        Ok(output) => output,
        Err(error) => {
            data.insert("error".to_string(), serde_json::json!(error.to_string()));
            return ("unavailable".to_string(), data);
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !stderr.is_empty() {
            data.insert("error".to_string(), serde_json::json!(stderr));
        }
        data.insert(
            "exit_code".to_string(),
            serde_json::json!(output.status.code()),
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let fields = parse_systemctl_show(&stdout);
    for (key, value) in &fields {
        data.insert(key.to_string(), serde_json::json!(value));
    }

    let main_pid = fields
        .get("MainPID")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    data.insert("main_pid".to_string(), serde_json::json!(main_pid));
    if main_pid > 0 {
        data.insert(
            "main_pid_running".to_string(),
            serde_json::json!(process_exists(main_pid)),
        );
    }

    let load_state = fields.get("LoadState").map(String::as_str).unwrap_or("");
    let active_state = fields.get("ActiveState").map(String::as_str).unwrap_or("");
    let status = if load_state == "not-found" {
        "missing".to_string()
    } else if active_state == "active" {
        "active".to_string()
    } else if !active_state.is_empty() {
        active_state.to_string()
    } else if output.status.success() {
        "observed".to_string()
    } else {
        "error".to_string()
    };

    (status, data)
}

fn parse_systemctl_show(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

pub(super) fn append_attach_observations(
    results: &mut TraceResults,
    run_dir: &RunDir,
    observations: &[TraceAttachmentObservation],
) -> Result<()> {
    if observations.is_empty() {
        return Ok(());
    }

    for observation in observations {
        let mut data = observation.data.clone();
        data.insert(
            "kind".to_string(),
            serde_json::Value::String(observation.attachment.kind.clone()),
        );
        data.insert(
            "target".to_string(),
            serde_json::Value::String(observation.attachment.target.clone()),
        );
        data.insert(
            "status".to_string(),
            serde_json::Value::String(observation.status.clone()),
        );
        results.timeline.push(TraceEvent {
            t_ms: observation.elapsed_ms,
            source: format!("attach.{}", observation.attachment.kind),
            event: format!("{}.{}", observation.phase, observation.status),
            data,
        });
    }

    let artifact_path = run_dir
        .path()
        .join("artifacts")
        .join(TRACE_ATTACHMENTS_ARTIFACT);
    std::fs::write(
        &artifact_path,
        serde_json::to_string_pretty(observations).map_err(|e| {
            Error::internal_json(
                format!("Failed to serialize trace attachment observations: {e}"),
                Some("trace.attach.observations.serialize".to_string()),
            )
        })?,
    )
    .map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write trace attachment artifact {}: {}",
                artifact_path.display(),
                e
            ),
            Some("trace.attach.observations.write".to_string()),
        )
    })?;
    results.artifacts.push(TraceArtifact {
        label: "Trace attachments".to_string(),
        path: format!("artifacts/{TRACE_ATTACHMENTS_ARTIFACT}"),
        kind: None,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::parsing::TraceStatus;
    use super::*;

    #[test]
    fn test_parse_all() {
        let attachments = TraceAttachment::parse_all(&[
            "logfile:/tmp/service.log".to_string(),
            "fswatch:/tmp/auth.json".to_string(),
            "pid:1234".to_string(),
            "port:8080".to_string(),
            "http://127.0.0.1:8080/health".to_string(),
            "systemd:kimaki.service".to_string(),
        ])
        .unwrap();

        assert_eq!(attachments.len(), 6);
        assert_eq!(attachments[0].kind, "logfile");
        assert_eq!(attachments[1].kind, "fswatch");
        assert_eq!(attachments[2].target, "1234");
        assert_eq!(attachments[3].target, "8080");
        assert_eq!(attachments[4].kind, "http");
        assert_eq!(attachments[5].kind, "systemd");
        assert_eq!(attachments[5].target, "kimaki.service");
    }

    #[test]
    fn trace_attachment_parse_supports_v1_kinds() {
        assert_eq!(
            TraceAttachment::parse("logfile:/tmp/service.log").unwrap(),
            TraceAttachment {
                kind: "logfile".to_string(),
                target: "/tmp/service.log".to_string(),
            }
        );
        assert_eq!(
            TraceAttachment::parse("fswatch:/tmp/auth.json").unwrap(),
            TraceAttachment {
                kind: "fswatch".to_string(),
                target: "/tmp/auth.json".to_string(),
            }
        );
        assert_eq!(TraceAttachment::parse("pid:1234").unwrap().target, "1234");
        assert_eq!(TraceAttachment::parse("port:8080").unwrap().target, "8080");
        assert_eq!(
            TraceAttachment::parse("http:http://127.0.0.1:8080/health")
                .unwrap()
                .target,
            "http://127.0.0.1:8080/health"
        );
        assert_eq!(
            TraceAttachment::parse("http://127.0.0.1:8080/health")
                .unwrap()
                .target,
            "http://127.0.0.1:8080/health"
        );
        assert_eq!(
            TraceAttachment::parse("systemd:kimaki.service").unwrap(),
            TraceAttachment {
                kind: "systemd".to_string(),
                target: "kimaki.service".to_string(),
            }
        );
    }

    #[test]
    fn systemctl_show_parser_extracts_unit_state_fields() {
        let fields = parse_systemctl_show(
            "Id=kimaki.service\nLoadState=loaded\nActiveState=active\nSubState=running\nMainPID=1234\n",
        );

        assert_eq!(fields.get("Id"), Some(&"kimaki.service".to_string()));
        assert_eq!(fields.get("ActiveState"), Some(&"active".to_string()));
        assert_eq!(fields.get("MainPID"), Some(&"1234".to_string()));
    }

    #[test]
    fn test_observe_trace_attachments() {
        let temp = tempfile::tempdir().unwrap();
        let log_path = temp.path().join("service.log");
        std::fs::write(&log_path, "before\n").unwrap();
        let attachment =
            TraceAttachment::parse(&format!("logfile:{}", log_path.display())).unwrap();

        let observations = observe_trace_attachments(&[attachment], "before", Instant::now());

        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].phase, "before");
        assert_eq!(observations[0].attachment.kind, "logfile");
        assert_eq!(observations[0].status, "present");
    }

    #[test]
    fn test_observe_fswatch_attachment_snapshots_file_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let watched_path = temp.path().join("auth.json");
        std::fs::write(&watched_path, "before\n").unwrap();
        let attachment =
            TraceAttachment::parse(&format!("fswatch:{}", watched_path.display())).unwrap();

        let observations = observe_trace_attachments(&[attachment], "before", Instant::now());

        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].phase, "before");
        assert_eq!(observations[0].attachment.kind, "fswatch");
        assert_eq!(observations[0].status, "present");
        assert_eq!(
            observations[0].data.get("bytes"),
            Some(&serde_json::json!(7))
        );
        assert!(observations[0].data.contains_key("modified_unix_ms"));
    }

    #[test]
    fn test_append_attach_observations() {
        let run_dir = RunDir::create().unwrap();
        std::fs::create_dir_all(run_dir.path().join("artifacts")).unwrap();
        let attachment = TraceAttachment::parse("logfile:/tmp/service.log").unwrap();
        let observations = vec![TraceAttachmentObservation {
            phase: "after",
            elapsed_ms: 7,
            attachment,
            status: "present".to_string(),
            data: BTreeMap::new(),
        }];
        let mut results = TraceResults {
            component_id: "example".to_string(),
            scenario_id: "attach".to_string(),
            status: TraceStatus::Pass,
            summary: None,
            failure: None,
            rig: None,
            evidence: None,
            timeline: Vec::new(),
            span_definitions: Vec::new(),
            span_results: Vec::new(),
            assertions: Vec::new(),
            temporal_assertions: Vec::new(),
            artifacts: Vec::new(),
            metrics: Default::default(),
            toolchain: None,
            components: None,
            dependencies: Vec::new(),
            preview: None,
        };

        append_attach_observations(&mut results, &run_dir, &observations).unwrap();

        assert_eq!(results.timeline.len(), 1);
        assert_eq!(results.timeline[0].source, "attach.logfile");
        assert_eq!(results.timeline[0].event, "after.present");
        assert!(results
            .artifacts
            .iter()
            .any(|artifact| artifact.path == "artifacts/trace-attachments.json"));
        assert!(run_dir
            .path()
            .join("artifacts/trace-attachments.json")
            .exists());
        run_dir.cleanup();
    }
}
