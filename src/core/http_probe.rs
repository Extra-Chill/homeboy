use std::time::Duration;

use crate::core::error::Error;

#[derive(Debug, Clone)]
pub(crate) struct HttpProbeError {
    pub message: String,
    pub is_connect: bool,
}

/// Build a blocking reqwest client with a request timeout. Shared by the HTTP
/// probe helpers and other one-off probe call sites (artifact-link reachability,
/// registry publish verification) so the `Client::builder().timeout().build()`
/// boilerplate lives in one place (#5364).
pub(crate) fn blocking_client(timeout: Duration) -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
}

pub(crate) fn get_status(url: &str, timeout: Duration) -> std::result::Result<u16, HttpProbeError> {
    let client = blocking_client(timeout).map_err(|e| HttpProbeError {
        message: Error::internal_unexpected(format!("build http client: {}", e)).message,
        is_connect: false,
    })?;

    let response = client.get(url).send().map_err(|e| HttpProbeError {
        message: format!("HTTP GET {} failed: {}", url, e),
        is_connect: e.is_connect(),
    })?;

    Ok(response.status().as_u16())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn get_status_returns_http_status_code() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 512];
            let _ = stream.read(&mut buffer);
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });

        let status = get_status(&format!("http://{addr}/health"), Duration::from_secs(1)).unwrap();

        assert_eq!(status, 204);
        handle.join().unwrap();
    }

    #[test]
    fn get_status_marks_connection_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let error =
            get_status(&format!("http://{addr}/health"), Duration::from_millis(200)).unwrap_err();

        assert!(error.is_connect);
        assert!(error.message.contains("HTTP GET"));
    }
}
