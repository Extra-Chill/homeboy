//! Authenticated, status-checked requests shared by upgrade release readers.
use homeboy_core::error::{Error, ErrorCode, Result};
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderValue, AUTHORIZATION};
use reqwest::StatusCode;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

struct Authentication {
    token: Option<String>,
    source: &'static str,
}

fn resolve_token(
    env: impl Fn(&str) -> Option<String>,
    command: impl FnOnce(&mut Command, Duration) -> Option<String>,
) -> Authentication {
    for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Some(token) = env(name).filter(|value| !value.trim().is_empty()) {
            return Authentication {
                token: Some(token),
                source: "env",
            };
        }
    }
    let token = command(
        Command::new("gh").args(["auth", "token", "--hostname", "github.com"]),
        Duration::from_secs(5),
    )
    .map(|value| value.trim().to_owned())
    .filter(|value| !value.is_empty());
    let source = if token.is_some() { "gh_cli" } else { "none" };
    Authentication { token, source }
}

fn authentication() -> &'static Authentication {
    static AUTH: OnceLock<Authentication> = OnceLock::new();
    AUTH.get_or_init(|| {
        resolve_token(
            |name| std::env::var(name).ok(),
            |command, timeout| {
                super::execution::command_output_with_timeout(command, timeout, "read GitHub token")
                    .ok()
                    .filter(|output| output.status.success())
                    .and_then(|output| String::from_utf8(output.stdout).ok())
            },
        )
    })
}

pub(super) fn token_source() -> String {
    authentication().source.to_owned()
}

use super::constants::VERSION;

pub(super) fn fetch(url: &str, timeout: Duration) -> Result<Response> {
    let client = Client::builder()
        .user_agent(format!("homeboy/{VERSION}"))
        .timeout(timeout)
        .build()
        .map_err(|e| Error::internal_io(e.to_string(), Some("create HTTP client".into())))?;
    let mut request = client.get(url);
    // Never send credentials to release asset hosts or caller-supplied endpoints.
    let github_api = reqwest::Url::parse(url)
        .is_ok_and(|url| url.scheme() == "https" && url.host_str() == Some("api.github.com"));
    if let Some(token) = github_api
        .then(authentication)
        .and_then(|auth| auth.token.as_ref())
    {
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
            .and_then(|value| value.parse::<i64>().ok())
            .and_then(homeboy_engine_primitives::timestamp::unix_seconds_rfc3339)
            .map(|time| format!("reset time: {time}"))
            .unwrap_or_else(|| "reset time unavailable".into());
        return Err(Error::new(
            ErrorCode::GithubRateLimited,
            format!("GitHub API rate limited ({status}) at {url}; {reset_message}; run `gh auth login` or set GH_TOKEN"),
            serde_json::json!({"url": url, "status": status.as_u16(), "reset": reset}),
        ));
    }
    let message = format!("GitHub API returned {status} for {url}");
    let mut error = Error::internal_io(message.clone(), Some("query GitHub releases".into()));
    // `internal_io` keeps the cause in details and a generic "IO error" message;
    // the operator-facing message must name the HTTP status and URL.
    error.message = message;
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn token_resolution_table() {
        for (gh, github, cli, expected, source) in [
            (
                Some("first"),
                Some("second"),
                Some("cli"),
                Some("first"),
                "env",
            ),
            (None, Some("second"), Some("cli"), Some("second"), "env"),
            (Some(""), Some("second"), Some("cli"), Some("second"), "env"),
            (None, None, Some("cli\n"), Some("cli"), "gh_cli"),
            (Some(""), Some("  "), Some("cli"), Some("cli"), "gh_cli"),
            (None, None, None, None, "none"), // missing, failing or timed-out command
            (None, None, Some("\n"), None, "none"),
        ] {
            let auth = resolve_token(
                |name| {
                    match name {
                        "GH_TOKEN" => gh,
                        _ => github,
                    }
                    .map(str::to_owned)
                },
                |command, timeout| {
                    assert_ne!(source, "env", "environment must bypass gh");
                    assert_eq!(command.get_program(), "gh");
                    assert_eq!(
                        command.get_args().collect::<Vec<_>>(),
                        ["auth", "token", "--hostname", "github.com"]
                    );
                    assert_eq!(timeout, Duration::from_secs(5));
                    cli.map(str::to_owned)
                },
            );
            assert_eq!(auth.token.as_deref(), expected);
            assert_eq!(auth.source, source);
        }
    }

    #[test]
    fn github_request_contract() {
        for status in [
            "200 OK",
            "403 Forbidden",
            "429 Too Many Requests",
            "404 Not Found",
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
                assert!(!request.to_ascii_lowercase().contains("authorization:"));
                // Invalid JSON ensures HTTP errors are diagnosed before parsing.
                let response = format!(
                    "HTTP/1.1 {status}\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: 1700000000\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot-json"
                );
                stream.write_all(response.as_bytes()).unwrap();
            });
            let result = fetch(&url, Duration::from_secs(5));
            server.join().unwrap();
            match &status[..3] {
                "403" | "429" => {
                    let error = result.unwrap_err();
                    assert_eq!(error.code, ErrorCode::GithubRateLimited);
                    assert!(error.message.contains("2023-11-14T22:13:20+00:00"));
                    assert!(error.message.contains("gh auth login"));
                    assert!(error.message.contains("set GH_TOKEN"));
                    assert_eq!(error.details["reset"], "1700000000");
                }
                "404" => {
                    let error = result.unwrap_err();
                    assert_ne!(error.code, ErrorCode::GithubRateLimited);
                    assert!(error.message.contains("404"));
                    assert!(error.message.contains(&url));
                }
                _ => assert!(result.is_ok()),
            }
        }
    }
}
