//! Static directory-artifact preview + screenshot capture.
//!
//! Boundary: commands resolve which directory artifact to serve and adapt the
//! results for output; this module owns the process/filesystem orchestration —
//! spawning the local static HTTP server, reserving a port, capturing
//! screenshots via the configured provider, and writing the capture manifest.
//! No CLI types cross this boundary.

use std::env;
use std::fs;
use std::net::TcpListener;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use serde::Serialize;

use crate::Error;

/// Viewport dimensions for a screenshot capture.
#[derive(Debug, Clone, Copy)]
pub struct CaptureViewport {
    pub width: u32,
    pub height: u32,
}

/// Resolved browser-capture provider (command + availability).
#[derive(Debug, Clone)]
pub struct CaptureProvider {
    pub command: String,
    pub available: bool,
    pub error: Option<String>,
}

/// Spawn a local `python3 -m http.server` bound to loopback, serving
/// `artifact_path` on `port`. Callers own the returned child's lifecycle.
pub fn start_static_artifact_server(
    artifact_path: &Path,
    port: u16,
    purpose: &str,
) -> crate::Result<Child> {
    let port_arg = port.to_string();
    let directory_arg = artifact_path.display().to_string();
    Command::new("python3")
        .args([
            "-m",
            "http.server",
            &port_arg,
            "--bind",
            "127.0.0.1",
            "--directory",
            &directory_arg,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("start python3 static artifact {purpose} server")),
            )
        })
}

/// Poll the static server until it responds successfully or the retry budget is
/// exhausted. Returns `true` when the server became reachable.
pub fn wait_for_static_server(base_url: &str) -> bool {
    for _ in 0..20 {
        if reqwest::blocking::get(base_url)
            .map(|response| response.status().is_success())
            .unwrap_or(false)
        {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Reserve an available loopback TCP port.
pub fn available_port() -> crate::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|err| {
        Error::internal_io(err.to_string(), Some("reserve preview port".to_string()))
    })?;
    let port = listener.local_addr().map_err(|err| {
        Error::internal_io(err.to_string(), Some("read preview port".to_string()))
    })?;
    Ok(port.port())
}

/// Resolve the browser-capture provider from the environment.
pub fn resolve_capture_provider() -> CaptureProvider {
    match env::var("HOMEBOY_ARTIFACT_CAPTURE_COMMAND") {
        Ok(command) if !command.trim().is_empty() => CaptureProvider {
            command,
            available: true,
            error: None,
        },
        _ => CaptureProvider {
            command: "HOMEBOY_ARTIFACT_CAPTURE_COMMAND".to_string(),
            available: false,
            error: Some(
                "no browser capture provider configured; set HOMEBOY_ARTIFACT_CAPTURE_COMMAND to a command that writes HOMEBOY_ARTIFACT_CAPTURE_OUTPUT"
                    .to_string(),
            ),
        },
    }
}

/// Result of capturing one entrypoint.
#[derive(Debug, Clone)]
pub struct CapturedPage {
    pub entrypoint: String,
    pub page_url: String,
    pub screenshot_path: PathBuf,
    pub status: String,
    pub timing_ms: u128,
    pub error: Option<String>,
}

/// Capture screenshots for each entrypoint of a directory artifact by serving it
/// locally and driving the configured provider. Returns the base URL, the
/// resolved provider, and per-entrypoint results.
pub fn capture_artifact_entrypoints(
    artifact_path: &Path,
    output_dir: &Path,
    entrypoints: &[String],
    viewport: CaptureViewport,
    port: Option<u16>,
) -> crate::Result<(String, CaptureProvider, Vec<CapturedPage>)> {
    fs::create_dir_all(output_dir).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "create artifact capture output directory {}",
                output_dir.display()
            )),
        )
    })?;

    let port = port.map(Ok).unwrap_or_else(available_port)?;
    let base_url = format!("http://127.0.0.1:{port}/");
    let mut child = start_static_artifact_server(artifact_path, port, "capture")?;
    let _ = wait_for_static_server(&base_url);
    let provider = resolve_capture_provider();

    let mut pages = Vec::new();
    for entrypoint in entrypoints {
        let page_start = std::time::Instant::now();
        let screenshot_path = output_dir.join(format!("{}.png", screenshot_basename(entrypoint)));
        let page_url_result = entrypoint_url(&base_url, entrypoint);
        let local_path_result = resolve_artifact_entrypoint(artifact_path, entrypoint);
        let mut status = "captured".to_string();
        let mut error = None;

        let page_url = match page_url_result {
            Ok(page_url) => page_url,
            Err(err) => {
                status = "failed".to_string();
                error = Some(err.to_string());
                base_url.clone()
            }
        };

        if let Err(err) = local_path_result {
            status = "failed".to_string();
            error = Some(err.to_string());
        } else if error.is_none() {
            if let Some(browser_error) = provider.error.as_ref() {
                status = "failed".to_string();
                error = Some(browser_error.clone());
            } else if let Err(err) =
                capture_screenshot(&provider, &page_url, &screenshot_path, viewport)
            {
                status = "failed".to_string();
                error = Some(err.to_string());
            }
        }

        pages.push(CapturedPage {
            entrypoint: entrypoint.clone(),
            page_url,
            screenshot_path,
            status,
            timing_ms: page_start.elapsed().as_millis(),
            error,
        });
    }

    let _ = child.kill();
    let _ = child.wait();

    Ok((base_url, provider, pages))
}

/// Write the capture manifest JSON to `manifest_path`.
pub fn write_capture_manifest(
    manifest_path: &Path,
    manifest: &impl Serialize,
) -> crate::Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("serialize capture manifest".to_string()),
        )
    })?;
    fs::write(manifest_path, bytes).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "write capture manifest {}",
                manifest_path.display()
            )),
        )
    })
}

fn capture_screenshot(
    provider: &CaptureProvider,
    page_url: &str,
    screenshot_path: &Path,
    viewport: CaptureViewport,
) -> crate::Result<()> {
    let output = Command::new("sh")
        .args(["-c", &provider.command])
        .env("HOMEBOY_ARTIFACT_CAPTURE_URL", page_url)
        .env("HOMEBOY_ARTIFACT_CAPTURE_OUTPUT", screenshot_path)
        .env(
            "HOMEBOY_ARTIFACT_CAPTURE_VIEWPORT_WIDTH",
            viewport.width.to_string(),
        )
        .env(
            "HOMEBOY_ARTIFACT_CAPTURE_VIEWPORT_HEIGHT",
            viewport.height.to_string(),
        )
        .stdin(Stdio::null())
        .output()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("run artifact capture provider".to_string()),
            )
        })?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(Error::validation_invalid_argument(
        "HOMEBOY_ARTIFACT_CAPTURE_COMMAND",
        format!(
            "artifact capture provider failed with status {}{}",
            output.status,
            if stderr.is_empty() {
                "".to_string()
            } else {
                format!(": {stderr}")
            }
        ),
        Some(provider.command.clone()),
        Some(vec![
            "Provider commands receive HOMEBOY_ARTIFACT_CAPTURE_URL, HOMEBOY_ARTIFACT_CAPTURE_OUTPUT, HOMEBOY_ARTIFACT_CAPTURE_VIEWPORT_WIDTH, and HOMEBOY_ARTIFACT_CAPTURE_VIEWPORT_HEIGHT.".to_string(),
        ]),
    ))
}

/// Join an entrypoint against the local preview base URL.
pub fn entrypoint_url(base_url: &str, entrypoint: &str) -> crate::Result<String> {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.join(entrypoint).ok())
        .map(|url| url.to_string())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "entrypoint",
                "entrypoint could not be resolved against the local artifact preview URL",
                Some(entrypoint.to_string()),
                None,
            )
        })
}

fn resolve_artifact_entrypoint(artifact_path: &Path, entrypoint: &str) -> crate::Result<PathBuf> {
    let entrypoint_path = Path::new(entrypoint);
    if entrypoint_path.is_absolute()
        || entrypoint_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Error::validation_invalid_argument(
            "entrypoint",
            "entrypoint must be a relative path inside the directory artifact",
            Some(entrypoint.to_string()),
            None,
        ));
    }
    let resolved = artifact_path.join(entrypoint_path);
    if !resolved.is_file() {
        return Err(Error::validation_invalid_argument(
            "entrypoint",
            format!("entrypoint file does not exist at {}", resolved.display()),
            Some(entrypoint.to_string()),
            None,
        ));
    }
    Ok(resolved)
}

fn screenshot_basename(entrypoint: &str) -> String {
    let mut basename = entrypoint
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if basename.is_empty() {
        basename = "entrypoint".to_string();
    }
    basename
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_entrypoint_helpers_are_path_safe_and_stable() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(root.path().join("nested")).expect("nested");
        fs::write(root.path().join("nested/index.html"), b"<html></html>").expect("html");

        assert!(resolve_artifact_entrypoint(root.path(), "nested/index.html").is_ok());
        assert!(resolve_artifact_entrypoint(root.path(), "../outside.html").is_err());
        assert!(resolve_artifact_entrypoint(root.path(), "/tmp/outside.html").is_err());
        assert_eq!(
            screenshot_basename("fse-pilot-build-theme/index.html"),
            "fse_pilot_build_theme_index_html"
        );
    }
}
