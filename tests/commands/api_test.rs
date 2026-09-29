use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use super::{require_apply_for_mutation, run_project, ApiCommand};
use crate::commands::utils::args::MutationArgs;

fn project_request(method: &str, mutation: MutationArgs) -> ApiCommand {
    request_with(method, "site", "/wp/v2/posts", mutation, None, Vec::new())
}

fn request_with(
    method: &str,
    project_id: &str,
    endpoint: &str,
    mutation: MutationArgs,
    body: Option<String>,
    form: Vec<String>,
) -> ApiCommand {
    ApiCommand::Request {
        method: method.to_string(),
        project_id: project_id.to_string(),
        endpoint: endpoint.to_string(),
        mutation,
        body,
        form,
    }
}

#[test]
fn api_mutating_methods_require_apply() {
    for method in ["POST", "PUT", "PATCH", "DELETE", "post"] {
        let err = require_apply_for_mutation(&project_request(method, MutationArgs::from(false)))
            .expect_err("mutating API method should require --apply");

        assert!(err.message.contains("requires explicit --apply"));
        assert!(err
            .message
            .contains(&format!("homeboy api request {method}")));
        assert!(err.message.contains(" site "));
        assert!(err.message.contains("/wp/v2/posts"));
    }
}

fn serve_one_json(response_body: &'static str) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("local addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = accept_with_deadline(&listener);

        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).expect("read request");
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);

            let Some(header_end) = find_header_end(&buffer) else {
                continue;
            };
            let content_length = content_length(&buffer[..header_end]);
            if buffer.len() >= header_end + 4 + content_length {
                break;
            }
        }

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");

        String::from_utf8_lossy(&buffer).to_string()
    });

    (format!("http://{addr}"), handle)
}

fn accept_with_deadline(listener: &TcpListener) -> (std::net::TcpStream, std::net::SocketAddr) {
    listener
        .set_nonblocking(true)
        .expect("nonblocking test listener");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, addr)) => {
                stream.set_nonblocking(false).expect("blocking test stream");
                return (stream, addr);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    panic!("test server received no request");
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("test server accept failed: {err}"),
        }
    }
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(headers: &[u8]) -> usize {
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

/// Write a minimal project fixture whose API client points at `base_url`.
fn write_api_project(project_id: &str, base_url: &str) {
    let config_root = homeboy::core::paths::homeboy().expect("isolated config root");
    let project_dir = config_root.join("projects").join(project_id);
    std::fs::create_dir_all(&project_dir).expect("create project fixture dir");
    let project = serde_json::json!({
        "api": {
            "enabled": true,
            "base_url": base_url,
        }
    });
    std::fs::write(
        project_dir.join(format!("{project_id}.json")),
        project.to_string(),
    )
    .expect("write project fixture");
}

#[test]
fn api_request_get_reads_a_project_api_without_apply() {
    let _home = homeboy::core::test_support::HomeGuard::new();
    let (base_url, server) = serve_one_json(r#"{"posts":[]}"#);
    write_api_project("api-get-e2e", &base_url);

    let (output, code) = run_project(&request_with(
        "GET",
        "api-get-e2e",
        "/wp/v2/posts",
        MutationArgs::from(false),
        None,
        Vec::new(),
    ))
    .expect("a bare project GET should run without --apply");

    assert_eq!(code, 0);
    assert_eq!(output.project_id, "api-get-e2e");
    assert_eq!(output.method, "GET");
    assert_eq!(output.endpoint, "/wp/v2/posts");
    assert_eq!(output.response["posts"], serde_json::json!([]));

    let request = server.join().expect("GET reached the project API");
    assert!(request.starts_with("GET /wp/v2/posts "));
}

#[test]
fn api_request_applied_mutation_sends_the_authorized_request() {
    let _home = homeboy::core::test_support::HomeGuard::new();
    let (base_url, server) = serve_one_json(r#"{"id":1}"#);
    write_api_project("api-mutate-apply", &base_url);

    let (output, code) = run_project(&request_with(
        "POST",
        "api-mutate-apply",
        "/wp/v2/posts",
        MutationArgs::from(true),
        Some("{\"title\":\"Hello\"}".to_string()),
        Vec::new(),
    ))
    .expect("an applied mutation should run");

    assert_eq!(code, 0);
    assert_eq!(output.method, "POST");
    assert_eq!(output.response["id"], 1);

    let request = server
        .join()
        .expect("applied mutation reached the project API");
    assert!(request.starts_with("POST /wp/v2/posts "));
    assert!(request.contains("\"title\":\"Hello\""));
}
