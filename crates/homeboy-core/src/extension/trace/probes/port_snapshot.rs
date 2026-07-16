use std::collections::{BTreeMap, BTreeSet};
use std::net::TcpListener;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

use super::{event, push_event, TraceEvent};

pub(super) fn run_port_snapshot(
    ports: Vec<u16>,
    interval_ms: u64,
    started_at: Instant,
    events: Arc<Mutex<Vec<TraceEvent>>>,
    stop: mpsc::Receiver<()>,
) {
    let interval = Duration::from_millis(interval_ms.max(1));
    let mut previous: Option<BTreeSet<u16>> = None;

    loop {
        let current = listening_ports(&ports);
        emit_port_events(previous.as_ref(), &current, started_at, &events);
        previous = Some(current);
        if stop.recv_timeout(interval).is_ok() {
            let current = listening_ports(&ports);
            emit_port_events(previous.as_ref(), &current, started_at, &events);
            break;
        }
    }
}

fn listening_ports(ports: &[u16]) -> BTreeSet<u16> {
    ports
        .iter()
        .copied()
        .filter(|port| TcpListener::bind(("127.0.0.1", *port)).is_err())
        .collect()
}

fn emit_port_events(
    previous: Option<&BTreeSet<u16>>,
    current: &BTreeSet<u16>,
    started_at: Instant,
    events: &Arc<Mutex<Vec<TraceEvent>>>,
) {
    let mut data = BTreeMap::new();
    data.insert(
        "ports".to_string(),
        serde_json::Value::Array(current.iter().map(|port| serde_json::json!(port)).collect()),
    );
    push_event(
        events,
        event(started_at, "port.snapshot", "net.listening", data),
    );

    let Some(previous) = previous else {
        return;
    };
    for port in current.difference(previous) {
        push_event(events, port_delta_event(started_at, "net.bind", *port));
    }
    for port in previous.difference(current) {
        push_event(events, port_delta_event(started_at, "net.unbind", *port));
    }
}

fn port_delta_event(started_at: Instant, event_name: &str, port: u16) -> TraceEvent {
    let mut data = BTreeMap::new();
    data.insert("port".to_string(), serde_json::json!(port));
    event(started_at, "port.snapshot", event_name, data)
}

pub(super) fn ports_for_snapshot(port: Option<u16>, port_range: Option<&str>) -> Result<Vec<u16>> {
    let mut ports = BTreeSet::new();
    if let Some(port) = port {
        ports.insert(port);
    }
    if let Some(port_range) = port_range {
        let Some((start, end)) = port_range.split_once('-') else {
            return Err(Error::validation_invalid_argument(
                "trace_probes.port-range",
                "port.snapshot port-range must be formatted as start-end".to_string(),
                None,
                None,
            ));
        };
        let start: u16 = start.trim().parse().map_err(|_| {
            Error::validation_invalid_argument(
                "trace_probes.port-range",
                "port.snapshot port-range start must be a port number".to_string(),
                None,
                None,
            )
        })?;
        let end: u16 = end.trim().parse().map_err(|_| {
            Error::validation_invalid_argument(
                "trace_probes.port-range",
                "port.snapshot port-range end must be a port number".to_string(),
                None,
                None,
            )
        })?;
        if start > end {
            return Err(Error::validation_invalid_argument(
                "trace_probes.port-range",
                "port.snapshot port-range start must be <= end".to_string(),
                None,
                None,
            ));
        }
        ports.extend(start..=end);
    }
    if ports.is_empty() {
        return Err(Error::validation_invalid_argument(
            "trace_probes.port",
            "port.snapshot requires port or port-range".to_string(),
            None,
            None,
        ));
    }
    Ok(ports.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use super::super::{ActiveTraceProbes, TraceProbeConfig};
    use super::ports_for_snapshot;

    #[test]
    fn test_run_port_snapshot() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
        let port = listener.local_addr().expect("local addr").port();
        let probes = ActiveTraceProbes::start(&[TraceProbeConfig::PortSnapshot {
            port: Some(port),
            port_range: None,
            interval_ms: Some(25),
        }])
        .expect("start probes");
        thread::sleep(Duration::from_millis(60));

        let events = probes.stop();
        assert!(events.iter().any(|event| event.source == "port.snapshot"
            && event.event == "net.listening"
            && event
                .data
                .get("ports")
                .and_then(|value| value.as_array())
                .is_some_and(|ports| ports
                    .iter()
                    .any(|value| value.as_u64() == Some(u64::from(port))))));
    }

    #[test]
    fn test_ports_for_snapshot() {
        assert_eq!(
            ports_for_snapshot(Some(3000), Some("3002-3003")).expect("ports"),
            vec![3000, 3002, 3003]
        );

        let result = ActiveTraceProbes::start(&[TraceProbeConfig::PortSnapshot {
            port: None,
            port_range: Some("9002-9001".to_string()),
            interval_ms: Some(25),
        }]);
        let error = result.err().expect("invalid range should fail start");

        assert!(error
            .to_string()
            .contains("port-range start must be <= end"));
    }
}
