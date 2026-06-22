use std::io::{Read, Write};
use std::net::TcpListener;

use super::{artifact_content_url, fetch_artifact_to_path};
use crate::test_support::with_isolated_home;

#[test]
fn artifact_content_url_builds_encoded_daemon_byte_alias() {
    let url = artifact_content_url(
        "http://127.0.0.1:7421/base?ignored=true",
        "run 1",
        "report/summary.json",
    )
    .expect("url");

    assert_eq!(
        url,
        "http://127.0.0.1:7421/runs/run%201/artifacts/report%2Fsummary.json/content"
    );
}

#[test]
fn fetch_artifact_to_path_downloads_daemon_byte_alias() {
    with_isolated_home(|home| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0; 1024];
            let bytes = stream.read(&mut request).expect("request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request
                .starts_with("GET /runs/run-1/artifacts/report%2Fsummary.json/content HTTP/1.1"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nX-Homeboy-Artifact-Sha256: abc123\r\nConnection: close\r\n\r\n{\"ok\":true}",
                )
                .expect("response");
        });
        let output = home.path().join("summary.json");

        let outcome = fetch_artifact_to_path(
            "run-1",
            "report/summary.json",
            Some(format!("http://{addr}")),
            Some(output.clone()),
        )
        .expect("artifact get");

        server.join().expect("server");
        assert_eq!(outcome.content_type.as_deref(), Some("application/json"));
        assert_eq!(outcome.size_bytes, 11);
        assert_eq!(outcome.sha256.as_deref(), Some("abc123"));
        assert_eq!(std::fs::read(&output).expect("output"), br#"{"ok":true}"#);
    });
}
