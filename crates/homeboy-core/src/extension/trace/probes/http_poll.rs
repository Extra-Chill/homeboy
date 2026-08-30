use std::collections::BTreeMap;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use super::{event, push_event, TraceEvent};

pub(super) fn run_http_poll(
    url: String,
    interval_ms: u64,
    assert_status: Option<u16>,
    started_at: Instant,
    events: Arc<Mutex<Vec<TraceEvent>>>,
    stop: mpsc::Receiver<()>,
) {
    let interval = Duration::from_millis(interval_ms.max(1));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok();

    loop {
        emit_http_poll_event(client.as_ref(), &url, assert_status, started_at, &events);
        if stop.recv_timeout(interval).is_ok() {
            emit_http_poll_event(client.as_ref(), &url, assert_status, started_at, &events);
            break;
        }
    }
}

fn emit_http_poll_event(
    client: Option<&reqwest::blocking::Client>,
    url: &str,
    assert_status: Option<u16>,
    started_at: Instant,
    events: &Arc<Mutex<Vec<TraceEvent>>>,
) {
    let Some(client) = client else {
        push_http_error(url, "failed to build HTTP client", started_at, events);
        return;
    };
    let request_started = Instant::now();
    match client.get(url).send() {
        Ok(response) => {
            let status = response.status().as_u16();
            let mut data = BTreeMap::new();
            data.insert(
                "url".to_string(),
                serde_json::Value::String(url.to_string()),
            );
            data.insert("status".to_string(), serde_json::json!(status));
            data.insert(
                "latency_ms".to_string(),
                serde_json::json!(request_started.elapsed().as_millis() as u64),
            );
            if let Some(assert_status) = assert_status {
                data.insert(
                    "assert_status".to_string(),
                    serde_json::json!(assert_status),
                );
                data.insert("ok".to_string(), serde_json::json!(status == assert_status));
            }
            push_event(
                events,
                event(started_at, "http.poll", "http.response", data),
            );
        }
        Err(error) => push_http_error(url, &error.to_string(), started_at, events),
    }
}

fn push_http_error(
    url: &str,
    error: &str,
    started_at: Instant,
    events: &Arc<Mutex<Vec<TraceEvent>>>,
) {
    let mut data = BTreeMap::new();
    data.insert(
        "url".to_string(),
        serde_json::Value::String(url.to_string()),
    );
    data.insert(
        "error".to_string(),
        serde_json::Value::String(error.to_string()),
    );
    push_event(events, event(started_at, "http.poll", "http.error", data));
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use super::super::{ActiveTraceProbes, TraceProbeConfig};

    #[test]
    fn test_run_http_poll() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test server");
        let url = format!("http://{}", listener.local_addr().expect("local addr"));
        let server = thread::spawn(move || {
            for stream in listener.incoming().take(1) {
                let mut stream = stream.expect("accept connection");
                let mut buffer = [0; 512];
                let _ = stream.read(&mut buffer);
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .expect("write response");
            }
        });
        let probes = ActiveTraceProbes::start(&[TraceProbeConfig::HttpPoll {
            url,
            interval_ms: Some(25),
            assert_status: Some(204),
        }])
        .expect("start probes");
        thread::sleep(Duration::from_millis(80));

        let events = probes.stop();
        let _ = server.join();
        assert!(events.iter().any(|event| event.source == "http.poll"
            && event.event == "http.response"
            && event.data.get("status").and_then(|value| value.as_u64()) == Some(204)
            && event.data.get("ok").and_then(|value| value.as_bool()) == Some(true)));
    }
}
