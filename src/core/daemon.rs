use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::paths;

pub const DEFAULT_ADDR: &str = "127.0.0.1:0";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DaemonState {
    pub address: String,
    pub pid: u32,
    pub state_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<DaemonState>,
    pub state_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonStartResult {
    pub pid: u32,
    pub address: String,
    pub state_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonStopResult {
    pub stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub state_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status_code: u16,
    pub body: serde_json::Value,
}

pub fn parse_bind_addr(addr: &str) -> Result<SocketAddr> {
    let parsed: SocketAddr = addr.parse().map_err(|e| {
        Error::validation_invalid_argument(
            "addr",
            format!("Invalid daemon bind address `{}`: {}", addr, e),
            Some(addr.to_string()),
            Some(vec!["Use a host:port value like 127.0.0.1:0".to_string()]),
        )
    })?;

    if !parsed.ip().is_loopback() {
        return Err(Error::validation_invalid_argument(
            "addr",
            "Daemon MVP only accepts loopback bind addresses",
            Some(addr.to_string()),
            Some(vec!["Use 127.0.0.1:<port> or [::1]:<port>".to_string()]),
        ));
    }

    Ok(parsed)
}

pub fn state_path() -> Result<PathBuf> {
    paths::daemon_state_file()
}

pub fn read_status() -> Result<DaemonStatus> {
    let path = state_path()?;
    let state_path = path.display().to_string();

    if !path.exists() {
        return Ok(DaemonStatus {
            running: false,
            state: None,
            state_path,
        });
    }

    let content = fs::read_to_string(&path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("read {}", path.display()))))?;
    let state: DaemonState = serde_json::from_str(&content)
        .map_err(|e| Error::config_invalid_json(path.display().to_string(), e))?;

    Ok(DaemonStatus {
        running: pid_is_running(state.pid),
        state: Some(state),
        state_path,
    })
}

pub fn stop() -> Result<DaemonStopResult> {
    let status = read_status()?;
    let Some(state) = status.state else {
        return Ok(DaemonStopResult {
            stopped: false,
            pid: None,
            state_path: status.state_path,
        });
    };

    let stopped = if pid_is_running(state.pid) {
        terminate_pid(state.pid)?;
        true
    } else {
        false
    };

    let path = state_path()?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("delete {}", path.display())))
        })?;
    }

    Ok(DaemonStopResult {
        stopped,
        pid: Some(state.pid),
        state_path: status.state_path,
    })
}

pub fn serve(addr: SocketAddr) -> Result<DaemonState> {
    let listener = TcpListener::bind(addr)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("bind daemon to {}", addr))))?;
    let local_addr = listener.local_addr().map_err(|e| {
        Error::internal_io(e.to_string(), Some("read daemon local address".to_string()))
    })?;
    let state = write_state(local_addr)?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let _ = handle_connection(stream);
            }
            Err(err) => {
                return Err(Error::internal_io(
                    err.to_string(),
                    Some("accept daemon connection".to_string()),
                ));
            }
        }
    }

    Ok(state)
}

pub fn route(method: &str, path: &str) -> HttpResponse {
    if method != "GET" {
        return HttpResponse {
            status_code: 405,
            body: json!({ "error": "method_not_allowed" }),
        };
    }

    match path {
        "/health" => HttpResponse {
            status_code: 200,
            body: json!({
                "status": "ok",
                "version": env!("CARGO_PKG_VERSION"),
            }),
        },
        "/version" => HttpResponse {
            status_code: 200,
            body: json!({
                "version": env!("CARGO_PKG_VERSION"),
            }),
        },
        "/config/paths" => match config_paths_body() {
            Ok(body) => HttpResponse {
                status_code: 200,
                body,
            },
            Err(err) => HttpResponse {
                status_code: 500,
                body: json!({ "error": err.message }),
            },
        },
        _ => HttpResponse {
            status_code: 404,
            body: json!({ "error": "not_found" }),
        },
    }
}

fn config_paths_body() -> Result<serde_json::Value> {
    Ok(json!({
        "homeboy": paths::homeboy()?.display().to_string(),
        "homeboy_json": paths::homeboy_json()?.display().to_string(),
        "projects": paths::projects()?.display().to_string(),
        "servers": paths::servers()?.display().to_string(),
        "components": paths::components()?.display().to_string(),
        "extensions": paths::extensions()?.display().to_string(),
        "rigs": paths::rigs()?.display().to_string(),
        "stacks": paths::stacks()?.display().to_string(),
        "daemon_state": paths::daemon_state_file()?.display().to_string(),
    }))
}

fn write_state(addr: SocketAddr) -> Result<DaemonState> {
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }

    let state = DaemonState {
        address: addr.to_string(),
        pid: std::process::id(),
        state_path: path.display().to_string(),
    };
    let body = serde_json::to_string_pretty(&state).map_err(|e| {
        Error::internal_json(e.to_string(), Some("serialize daemon state".to_string()))
    })?;
    fs::write(&path, body).map_err(|e| {
        Error::internal_io(e.to_string(), Some(format!("write {}", path.display())))
    })?;
    Ok(state)
}

fn handle_connection(mut stream: TcpStream) -> std::io::Result<()> {
    let mut buffer = [0; 2048];
    let bytes = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..bytes]);
    let mut parts = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let response = route(method, path);
    let body = serde_json::to_string_pretty(&json!({
        "success": (200..300).contains(&response.status_code),
        "data": response.body,
    }))
    .unwrap_or_else(|_| "{\"success\":false}".to_string());
    let status_text = match response.status_code {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status_code,
        status_text,
        body.len(),
        body
    )
}

fn pid_is_running(pid: u32) -> bool {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
    }

    #[cfg(not(unix))]
    {
        pid == std::process::id()
    }
}

fn terminate_pid(pid: u32) -> Result<()> {
    #[cfg(unix)]
    unsafe {
        if libc::kill(pid as libc::pid_t, libc::SIGTERM) != 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some(format!("stop daemon pid {}", pid)),
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(Error::internal_unexpected(
            "daemon stop is not implemented on this platform",
        ))
    }
}

#[cfg(test)]
#[path = "../../tests/core/daemon_test.rs"]
mod daemon_test;
