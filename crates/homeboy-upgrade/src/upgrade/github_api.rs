//! Authenticated, status-checked requests shared by upgrade release readers.
use homeboy_core::error::{Error, ErrorCode, Result};
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderValue, AUTHORIZATION};
use reqwest::StatusCode;
use std::time::Duration;

use super::constants::VERSION;

pub(super) fn fetch(url: &str, timeout: Duration) -> Result<Response> {
    let client = Client::builder()
        .user_agent(format!("homeboy/{VERSION}"))
        .timeout(timeout)
        .build()
        .map_err(|e| Error::internal_io(e.to_string(), Some("create HTTP client".into())))?;
    let mut request = client.get(url);
    let token = ["GH_TOKEN", "GITHUB_TOKEN"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty());
    if let Some(token) = token {
        let mut header = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
            Error::internal_io(
                "Invalid GitHub authentication header",
                Some("read GH_TOKEN or GITHUB_TOKEN".into()),
            )
        })?;
        header.set_sensitive(true);
        request = request.header(AUTHORIZATION, header);
    }
    let response = request
        .send()
        .map_err(|e| Error::internal_io(e.to_string(), Some("query GitHub releases".into())))?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let headers = response.headers();
    if matches!(
        status,
        StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
    ) && (headers.contains_key("x-ratelimit-remaining")
        || headers.contains_key("x-ratelimit-reset")
        || headers.contains_key("retry-after"))
    {
        let reset = headers
            .get("x-ratelimit-reset")
            .and_then(|value| value.to_str().ok());
        let reset_message = reset
            .map(|value| format!("reset time: {value} (Unix seconds)"))
            .unwrap_or_else(|| "reset time unavailable".into());
        return Err(Error::new(
            ErrorCode::GithubRateLimited,
            format!("GitHub API rate limited ({status}) at {url}; {reset_message}; set GH_TOKEN or GITHUB_TOKEN"),
            serde_json::json!({"url": url, "status": status.as_u16(), "reset": reset}),
        ));
    }
    let mut error = Error::internal_io(
        format!("GitHub API returned {status} for {url}"),
        Some("query GitHub releases".into()),
    );
    error.message = format!("GitHub API returned {status} for {url}");
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Command;

    #[test]
    fn github_request_contract() {
        const URL_ENV: &str = "HOMEBOY_TEST_GITHUB_REQUEST_URL";
        // Isolate real environment-variable lookup from concurrent upgrade tests.
        if let Ok(url) = std::env::var(URL_ENV) {
            let result = fetch(&url, Duration::from_secs(5));
            match std::env::var("HOMEBOY_TEST_GITHUB_STATUS")
                .unwrap()
                .as_str()
            {
                "403" | "429" => {
                    let error = result.unwrap_err();
                    assert_eq!(error.code, ErrorCode::GithubRateLimited);
                    assert!(error.message.contains("1700000000"));
                    assert!(error.message.contains("GH_TOKEN or GITHUB_TOKEN"));
                    assert_eq!(error.details["reset"], "1700000000");
                    assert!(!format!("{error:?}").contains("test-secret"));
                }
                "404" => {
                    let error = result.unwrap_err();
                    assert_ne!(error.code, ErrorCode::GithubRateLimited);
                    assert!(error.message.contains("404"));
                    assert!(error.message.contains(&url));
                }
                _ => assert!(result.is_ok()),
            }
            return;
        }

        for (gh_token, github_token, expected, status) in [
            (None, None, None, "200 OK"),
            (Some(""), Some(""), None, "200 OK"),
            (
                Some("test-secret-gh"),
                None,
                Some("test-secret-gh"),
                "200 OK",
            ),
            (
                None,
                Some("test-secret-github"),
                Some("test-secret-github"),
                "200 OK",
            ),
            (
                Some(""),
                Some("test-secret-fallback"),
                Some("test-secret-fallback"),
                "200 OK",
            ),
            (
                Some("test-secret-first"),
                Some("test-secret-second"),
                Some("test-secret-first"),
                "200 OK",
            ),
            (None, None, None, "403 Forbidden"),
            (None, None, None, "429 Too Many Requests"),
            (None, None, None, "404 Not Found"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/releases", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).unwrap();
                    assert_ne!(count, 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8(request).unwrap();
                let authorization = request.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("authorization")
                        .then(|| value.trim())
                });
                let matches = authorization.map(str::to_owned)
                    == expected.map(|token| format!("Bearer {token}"));
                // Invalid JSON ensures status errors are diagnosed before parsing.
                let response = format!(
                    "HTTP/1.1 {status}\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: 1700000000\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot-json"
                );
                stream.write_all(response.as_bytes()).unwrap();
                matches
            });
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "upgrade::github_api::tests::github_request_contract",
                ])
                .env_remove("GH_TOKEN")
                .env_remove("GITHUB_TOKEN")
                .env(URL_ENV, url)
                .env("HOMEBOY_TEST_GITHUB_STATUS", &status[..3]);
            if let Some(token) = gh_token {
                child.env("GH_TOKEN", token);
            }
            if let Some(token) = github_token {
                child.env("GITHUB_TOKEN", token);
            }
            let output = child.output().unwrap();
            assert!(
                server.join().unwrap(),
                "authorization header contract failed"
            );
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }
}
