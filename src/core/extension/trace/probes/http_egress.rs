use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::{event, push_event, TraceEvent};

const DEFAULT_REDACT_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "cookie",
    "set-cookie",
];

#[derive(Debug, Clone)]
pub(super) struct HttpEgressConfig {
    pub host: String,
    pub path: Option<String>,
    pub capture: String,
    pub max_body_bytes: usize,
    pub redact_headers: Vec<String>,
    pub listen_port: Option<u16>,
}

pub(super) fn default_redact_headers() -> Vec<String> {
    DEFAULT_REDACT_HEADERS
        .iter()
        .map(|header| header.to_string())
        .collect()
}

pub(super) fn run_http_egress(
    config: HttpEgressConfig,
    started_at: Instant,
    events: Arc<Mutex<Vec<TraceEvent>>>,
    stop: mpsc::Receiver<()>,
) {
    let listener = match TcpListener::bind(("127.0.0.1", config.listen_port.unwrap_or(0))) {
        Ok(listener) => listener,
        Err(error) => {
            let mut data = BTreeMap::new();
            data.insert("error".to_string(), serde_json::json!(error.to_string()));
            push_event(
                &events,
                event(started_at, "http.egress", "proxy.error", data),
            );
            return;
        }
    };
    let _ = listener.set_nonblocking(true);
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    push_event(
        &events,
        event(
            started_at,
            "http.egress",
            "proxy.ready",
            BTreeMap::from([
                ("proxy_url".to_string(), serde_json::json!(proxy_url)),
                ("host".to_string(), serde_json::json!(config.host.clone())),
                (
                    "capture".to_string(),
                    serde_json::json!(config.capture.clone()),
                ),
            ]),
        ),
    );

    let mut handles = Vec::new();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let config = config.clone();
                let events = Arc::clone(&events);
                handles.push(thread::spawn(move || {
                    handle_proxy_connection(stream, config, started_at, events)
                }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => {
                let mut data = BTreeMap::new();
                data.insert("error".to_string(), serde_json::json!(error.to_string()));
                push_event(
                    &events,
                    event(started_at, "http.egress", "proxy.error", data),
                );
            }
        }

        if stop.recv_timeout(Duration::from_millis(25)).is_ok() {
            break;
        }
    }

    for handle in handles {
        let _ = handle.join();
    }
}

fn handle_proxy_connection(
    mut client: TcpStream,
    config: HttpEgressConfig,
    started_at: Instant,
    events: Arc<Mutex<Vec<TraceEvent>>>,
) {
    let _ = client.set_read_timeout(Some(Duration::from_secs(5)));
    let request_started = Instant::now();
    let Ok(request) = read_http_message(&mut client) else {
        return;
    };
    if request.method.eq_ignore_ascii_case("CONNECT") {
        handle_connect_tunnel(client, request, &config, started_at, &events);
        return;
    }
    if !matches_filter(&request.host, &request.path, &config) {
        respond_bad_gateway(client, "request did not match http.egress filter");
        return;
    }

    push_event(
        &events,
        event(
            started_at,
            "http.egress",
            "http.request",
            request_event_data(&request, &config),
        ),
    );

    let target_addr = format!("{}:{}", request.host, request.port.unwrap_or(80));
    let mut upstream = match TcpStream::connect(&target_addr) {
        Ok(upstream) => upstream,
        Err(error) => {
            push_http_error(&events, started_at, &request, error.to_string());
            respond_bad_gateway(client, "upstream connection failed");
            return;
        }
    };
    let outbound = request.to_origin_form_bytes();
    if upstream.write_all(&outbound).is_err() {
        push_http_error(
            &events,
            started_at,
            &request,
            "failed to write upstream request",
        );
        return;
    }
    let response = match read_http_message(&mut upstream) {
        Ok(response) => response,
        Err(error) => {
            push_http_error(&events, started_at, &request, error.to_string());
            return;
        }
    };
    let _ = client.write_all(&response.raw);
    let mut data = response_event_data(&response, &config);
    data.insert(
        "latency_ms".to_string(),
        serde_json::json!(request_started.elapsed().as_millis() as u64),
    );
    data.insert("url".to_string(), serde_json::json!(request.url()));
    push_event(
        &events,
        event(started_at, "http.egress", "http.response", data),
    );
}

fn handle_connect_tunnel(
    mut client: TcpStream,
    request: HttpMessage,
    config: &HttpEgressConfig,
    started_at: Instant,
    events: &Arc<Mutex<Vec<TraceEvent>>>,
) {
    if !matches_host(&request.host, &config.host) {
        respond_bad_gateway(client, "CONNECT host did not match http.egress filter");
        return;
    }
    push_event(
        events,
        event(
            started_at,
            "http.egress",
            "http.connect",
            BTreeMap::from([
                ("host".to_string(), serde_json::json!(request.host.clone())),
                (
                    "port".to_string(),
                    serde_json::json!(request.port.unwrap_or(443)),
                ),
                (
                    "capture".to_string(),
                    serde_json::json!("metadata-only: TLS tunnel not decrypted"),
                ),
            ]),
        ),
    );
    let target_addr = format!("{}:{}", request.host, request.port.unwrap_or(443));
    let Ok(mut upstream) = TcpStream::connect(target_addr) else {
        respond_bad_gateway(client, "CONNECT upstream failed");
        return;
    };
    let _ = client.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = upstream.set_read_timeout(Some(Duration::from_millis(500)));
    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .is_err()
    {
        return;
    }
    let Ok(mut client_to_upstream) = client.try_clone() else {
        return;
    };
    let Ok(mut upstream_to_client) = upstream.try_clone() else {
        return;
    };
    let a = thread::spawn(move || {
        let _ = std::io::copy(&mut client_to_upstream, &mut upstream);
        let _ = upstream.shutdown(Shutdown::Write);
    });
    let b = thread::spawn(move || {
        let _ = std::io::copy(&mut upstream_to_client, &mut client);
        let _ = client.shutdown(Shutdown::Write);
    });
    let _ = a.join();
    let _ = b.join();
}

#[derive(Debug)]
struct HttpMessage {
    method: String,
    target: String,
    version: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    raw: Vec<u8>,
    host: String,
    port: Option<u16>,
    path: String,
}

impl HttpMessage {
    fn url(&self) -> String {
        if self.target.starts_with("http://") || self.target.starts_with("https://") {
            return self.target.clone();
        }
        format!("http://{}{}", self.host, self.path)
    }

    fn to_origin_form_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(
            format!("{} {} {}\r\n", self.method, self.path, self.version).as_bytes(),
        );
        for (name, value) in &self.headers {
            if name.eq_ignore_ascii_case("proxy-connection") {
                continue;
            }
            out.extend_from_slice(format!("{}: {}\r\n", name, value).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

fn read_http_message(stream: &mut TcpStream) -> std::io::Result<HttpMessage> {
    let mut buffer = Vec::new();
    let header_end;
    loop {
        let mut chunk = [0; 1024];
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before headers",
            ));
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(index) = find_header_end(&buffer) {
            header_end = index;
            break;
        }
    }
    let header_bytes = &buffer[..header_end];
    let header_text = String::from_utf8_lossy(header_bytes);
    let mut lines = header_text.split("\r\n");
    let start_line = lines.next().unwrap_or_default();
    let mut start = start_line.split_whitespace();
    let method = start.next().unwrap_or_default().to_string();
    let target = start.next().unwrap_or_default().to_string();
    let version = start.next().unwrap_or("HTTP/1.1").to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect::<Vec<_>>();
    let content_length = header_value(&headers, "content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0; content_length - body.len()];
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let (host, port, path) =
        parse_target(&target, &headers, method.eq_ignore_ascii_case("CONNECT"));
    let mut raw = Vec::new();
    raw.extend_from_slice(&buffer[..header_end + 4]);
    raw.extend_from_slice(&body);
    Ok(HttpMessage {
        method,
        target,
        version,
        headers,
        body,
        raw,
        host,
        port,
        path,
    })
}

fn parse_target(
    target: &str,
    headers: &[(String, String)],
    is_connect: bool,
) -> (String, Option<u16>, String) {
    if is_connect {
        let (host, port) = split_host_port(target, 443);
        return (host, Some(port), String::new());
    }
    if let Some(rest) = target.strip_prefix("http://") {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = split_host_port(authority, 80);
        return (host, Some(port), format!("/{}", path));
    }
    let authority = header_value(headers, "host").unwrap_or_default();
    let (host, port) = split_host_port(&authority, 80);
    (host, Some(port), target.to_string())
}

fn split_host_port(authority: &str, default_port: u16) -> (String, u16) {
    let authority = authority.trim();
    if let Some((host, port)) = authority.rsplit_once(':') {
        if let Ok(port) = port.parse::<u16>() {
            return (host.to_string(), port);
        }
    }
    (authority.to_string(), default_port)
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn matches_filter(host: &str, path: &str, config: &HttpEgressConfig) -> bool {
    matches_host(host, &config.host)
        && config
            .path
            .as_ref()
            .map(|pattern| path.contains(pattern.trim_matches('*')))
            .unwrap_or(true)
}

fn matches_host(host: &str, pattern: &str) -> bool {
    if pattern == "*" || pattern == host {
        return true;
    }
    pattern
        .strip_prefix("*.")
        .map(|suffix| host.ends_with(suffix))
        .unwrap_or(false)
}

fn request_event_data(
    message: &HttpMessage,
    config: &HttpEgressConfig,
) -> BTreeMap<String, serde_json::Value> {
    let mut data = BTreeMap::from([
        ("method".to_string(), serde_json::json!(message.method)),
        ("url".to_string(), serde_json::json!(message.url())),
        ("host".to_string(), serde_json::json!(message.host)),
        ("path".to_string(), serde_json::json!(message.path)),
        (
            "body_bytes".to_string(),
            serde_json::json!(message.body.len()),
        ),
    ]);
    add_capture_data(&mut data, message, config, true);
    data
}

fn response_event_data(
    message: &HttpMessage,
    config: &HttpEgressConfig,
) -> BTreeMap<String, serde_json::Value> {
    let status = message.target.parse::<u16>().ok();
    let mut data = BTreeMap::from([
        ("status".to_string(), serde_json::json!(status)),
        (
            "body_bytes".to_string(),
            serde_json::json!(message.body.len()),
        ),
    ]);
    add_capture_data(&mut data, message, config, false);
    data
}

fn add_capture_data(
    data: &mut BTreeMap<String, serde_json::Value>,
    message: &HttpMessage,
    config: &HttpEgressConfig,
    request: bool,
) {
    if matches!(config.capture.as_str(), "headers" | "body") {
        let redacted = config
            .redact_headers
            .iter()
            .map(|header| header.to_ascii_lowercase())
            .collect::<Vec<_>>();
        data.insert(
            "headers".to_string(),
            serde_json::Value::Object(
                message
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        let value = if redacted
                            .iter()
                            .any(|header| header == &name.to_ascii_lowercase())
                        {
                            serde_json::json!("<redacted>")
                        } else {
                            serde_json::json!(value)
                        };
                        (name.clone(), value)
                    })
                    .collect(),
            ),
        );
    }
    if config.capture == "body" {
        let body = truncate_body(&message.body, config.max_body_bytes);
        data.insert(
            if request {
                "request_body"
            } else {
                "response_body"
            }
            .to_string(),
            serde_json::json!(body),
        );
        data.insert(
            "body_truncated".to_string(),
            serde_json::json!(message.body.len() > config.max_body_bytes),
        );
    }
}

fn truncate_body(body: &[u8], max: usize) -> String {
    String::from_utf8_lossy(&body[..body.len().min(max)]).to_string()
}

fn push_http_error(
    events: &Arc<Mutex<Vec<TraceEvent>>>,
    started_at: Instant,
    request: &HttpMessage,
    error: impl Into<String>,
) {
    push_event(
        events,
        event(
            started_at,
            "http.egress",
            "http.error",
            BTreeMap::from([
                ("url".to_string(), serde_json::json!(request.url())),
                ("error".to_string(), serde_json::json!(error.into())),
            ]),
        ),
    );
}

fn respond_bad_gateway(mut stream: TcpStream, message: &str) {
    let body = message.as_bytes();
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    let _ = stream.write_all(body);
}

#[cfg(test)]
mod tests {
    use super::super::{ActiveTraceProbes, TraceProbeConfig};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn http_egress_proxy_captures_http_request_and_response_bodies() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).expect("bind upstream");
        let upstream_addr = upstream.local_addr().expect("upstream addr");
        let server = thread::spawn(move || {
            let (mut stream, _) = upstream.accept().expect("accept upstream");
            let mut buffer = [0; 1024];
            let n = stream.read(&mut buffer).expect("read upstream request");
            let request = String::from_utf8_lossy(&buffer[..n]);
            assert!(request.contains("POST /v1/messages HTTP/1.1"));
            assert!(request.contains("hello=world"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nSet-Cookie: secret\r\n\r\nhello back",
                )
                .expect("write upstream response");
        });

        let reserved = TcpListener::bind(("127.0.0.1", 0)).expect("reserve proxy port");
        let proxy_addr = reserved.local_addr().expect("proxy addr");
        drop(reserved);
        let probes = ActiveTraceProbes::start(&[TraceProbeConfig::HttpEgress {
            host: "127.0.0.1".to_string(),
            path: Some("/v1/messages".to_string()),
            capture: "body".to_string(),
            max_body_bytes: Some(1024),
            redact_headers: None,
            listen_port: Some(proxy_addr.port()),
        }])
        .expect("start egress proxy");
        thread::sleep(Duration::from_millis(100));
        let mut client = std::net::TcpStream::connect(proxy_addr).expect("connect proxy");
        write!(
            client,
            "POST http://{}/v1/messages HTTP/1.1\r\nHost: {}\r\nAuthorization: secret\r\nContent-Length: 11\r\n\r\nhello=world",
            upstream_addr, upstream_addr
        )
        .expect("write proxy request");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("read response");
        assert!(response.contains("hello back"));
        drop(client);
        let events = probes.stop();
        let _ = server.join();
        assert!(events.iter().any(|event| event.event == "http.request"
            && event
                .data
                .get("request_body")
                .and_then(|value| value.as_str())
                == Some("hello=world")));
        assert!(events.iter().any(|event| event.event == "http.response"
            && event
                .data
                .get("response_body")
                .and_then(|value| value.as_str())
                == Some("hello back")));
        assert!(events.iter().any(|event| event.event == "http.request"
            && event
                .data
                .get("headers")
                .and_then(|headers| headers.get("Authorization"))
                .and_then(|value| value.as_str())
                == Some("<redacted>")));
    }
}
