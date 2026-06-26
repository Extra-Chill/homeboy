use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::core::{artifact_origin, Error, Result};

pub const ARTIFACT_DOM_BOXES_SCHEMA: &str = "homeboy/static-artifact-dom-boxes/v1";
const DEFAULT_TEXT_SAMPLE_LIMIT: usize = 160;

#[derive(Debug, Clone)]
pub struct DomBoxCaptureSpec {
    pub root: PathBuf,
    pub entrypoints: Vec<PathBuf>,
    pub report: Option<PathBuf>,
    pub text_sample_limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomBoxReport {
    pub schema: &'static str,
    pub root: String,
    pub entrypoints: Vec<DomBoxEntrypointReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomBoxEntrypointReport {
    pub page_path: String,
    pub page_url: String,
    pub viewport: DomBoxViewport,
    pub elements: Vec<DomBoxElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomBoxViewport {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomBoxElement {
    pub node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    pub selector: String,
    pub tag: String,
    pub text_sample: String,
    #[serde(rename = "boundingClientRect")]
    pub bounding_client_rect: DomRect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BrowserReport {
    entrypoints: Vec<BrowserEntrypointReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BrowserEntrypointReport {
    page_path: String,
    page_url: String,
    viewport: DomBoxViewport,
    elements: Vec<BrowserElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BrowserElement {
    node_id: String,
    node_name: Option<String>,
    selector: String,
    tag: String,
    text_sample: String,
    #[serde(rename = "boundingClientRect")]
    bounding_client_rect: DomRect,
}

impl Default for DomBoxCaptureSpec {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            entrypoints: Vec::new(),
            report: None,
            text_sample_limit: DEFAULT_TEXT_SAMPLE_LIMIT,
        }
    }
}

pub fn capture(spec: DomBoxCaptureSpec) -> Result<DomBoxReport> {
    let plan = plan_capture(&spec.root, &spec.entrypoints)?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| {
        Error::internal_io(err.to_string(), Some("bind artifact DOM server".into()))
    })?;
    let base_url = format!(
        "http://{}",
        listener.local_addr().map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read artifact DOM server address".into()),
            )
        })?
    );
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = Arc::clone(&stop);
    let server_root = spec.root.clone();
    listener.set_nonblocking(true).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("configure artifact DOM server".into()),
        )
    })?;
    let server = std::thread::spawn(move || {
        while !server_stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let root = server_root.clone();
                    std::thread::spawn(move || {
                        let _ = artifact_origin::handle_stream(stream, &root);
                    });
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let result = run_browser_capture(&base_url, &plan, spec.text_sample_limit)
        .map(|browser| shape_report(&spec.root, browser, spec.text_sample_limit));
    stop.store(true, Ordering::Relaxed);
    let _ = server.join();

    let report = result?;
    if let Some(path) = spec.report {
        write_report(&path, &report)?;
    }
    Ok(report)
}

pub fn plan_capture(root: &Path, entrypoints: &[PathBuf]) -> Result<Vec<String>> {
    if entrypoints.is_empty() {
        return Err(Error::validation_missing_argument(vec![
            "entrypoint".to_string()
        ]));
    }
    entrypoints
        .iter()
        .map(|entrypoint| entrypoint_request_path(root, entrypoint))
        .collect()
}

pub(crate) fn shape_report(
    root: &Path,
    browser: BrowserReport,
    text_sample_limit: usize,
) -> DomBoxReport {
    DomBoxReport {
        schema: ARTIFACT_DOM_BOXES_SCHEMA,
        root: root.display().to_string(),
        entrypoints: browser
            .entrypoints
            .into_iter()
            .map(|entrypoint| DomBoxEntrypointReport {
                page_path: entrypoint.page_path,
                page_url: entrypoint.page_url,
                viewport: entrypoint.viewport,
                elements: entrypoint
                    .elements
                    .into_iter()
                    .map(|element| DomBoxElement {
                        node_id: element.node_id,
                        node_name: element.node_name,
                        selector: element.selector,
                        tag: element.tag,
                        text_sample: truncate_text_sample(&element.text_sample, text_sample_limit),
                        bounding_client_rect: element.bounding_client_rect,
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub fn truncate_text_sample(input: &str, limit: usize) -> String {
    let normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized.chars().take(limit).collect()
}

fn entrypoint_request_path(root: &Path, entrypoint: &Path) -> Result<String> {
    let relative = if entrypoint.is_absolute() {
        entrypoint.strip_prefix(root).map_err(|_| {
            Error::validation_invalid_argument(
                "entrypoint",
                format!(
                    "absolute entrypoint {} is not inside artifact root {}",
                    entrypoint.display(),
                    root.display()
                ),
                None,
                None,
            )
        })?
    } else {
        entrypoint
    };
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(Error::validation_invalid_argument(
            "entrypoint",
            "entrypoint must stay inside the artifact root",
            None,
            None,
        ));
    }
    let path = root.join(relative);
    if !path.is_file() {
        return Err(Error::validation_invalid_argument(
            "entrypoint",
            format!("entrypoint file not found: {}", path.display()),
            None,
            None,
        ));
    }
    let request_path = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Ok(format!("/{}", request_path.trim_start_matches('/')))
}

fn run_browser_capture(
    base_url: &str,
    page_paths: &[String],
    text_sample_limit: usize,
) -> Result<BrowserReport> {
    let output = Command::new("node")
        .arg("-e")
        .arg(browser_script())
        .arg(base_url)
        .arg(serde_json::to_string(page_paths).map_err(|err| {
            Error::internal_json(err.to_string(), Some("serialize DOM box page paths".into()))
        })?)
        .arg(text_sample_limit.to_string())
        .output()
        .map_err(|err| missing_browser_error(format!("failed to start node: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if output.status.code() == Some(42) || stderr.contains("playwright install") {
            return Err(missing_browser_error(stderr.trim().to_string()));
        }
        return Err(Error::internal_unexpected(format!(
            "DOM box browser capture failed: {}",
            stderr.trim()
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some(
                String::from_utf8_lossy(&output.stdout)
                    .chars()
                    .take(500)
                    .collect(),
            ),
        )
    })
}

fn missing_browser_error(problem: String) -> Error {
    Error::validation_invalid_argument(
        "browser",
        format!("browser automation dependency is unavailable: {problem}"),
        None,
        Some(vec![
            "Install Node.js and Playwright for this checkout: npm install -D playwright && npx playwright install chromium".to_string(),
        ]),
    )
    .with_hint("Homeboy DOM box capture runs a local Chromium browser through Playwright.".to_string())
}

fn write_report(path: &Path, report: &DomBoxReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(err.to_string(), Some(parent.display().to_string()))
        })?;
    }
    let json = serde_json::to_string_pretty(report).map_err(|err| {
        Error::internal_json(err.to_string(), Some("serialize DOM box report".into()))
    })?;
    std::fs::write(path, format!("{json}\n"))
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))
}

fn browser_script() -> &'static str {
    r#"
const [baseUrl, pathsJson, limitRaw] = process.argv.slice(1);
let chromium;
try {
  chromium = require('playwright').chromium;
} catch (error) {
  console.error('Playwright is not installed or cannot be loaded: ' + error.message);
  process.exit(42);
}
const paths = JSON.parse(pathsJson);
const limit = Number.parseInt(limitRaw, 10) || 160;
(async () => {
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 });
  const entrypoints = [];
  for (const pagePath of paths) {
    const page = await context.newPage();
    const pageUrl = new URL(pagePath, baseUrl).toString();
    await page.goto(pageUrl, { waitUntil: 'networkidle' });
    const viewport = page.viewportSize() || { width: 1440, height: 900 };
    const elements = await page.evaluate((sampleLimit) => Array.from(document.querySelectorAll('[data-figma-node-id]')).map((element) => {
      const rect = element.getBoundingClientRect();
      const nodeId = element.getAttribute('data-figma-node-id') || '';
      const rawText = (element.innerText || element.textContent || '').replace(/\s+/g, ' ').trim();
      const tag = element.tagName.toLowerCase();
      return {
        node_id: nodeId,
        node_name: element.getAttribute('data-figma-node-name') || null,
        selector: `${tag}[data-figma-node-id="${nodeId.replace(/"/g, '\\"')}"]`,
        tag,
        text_sample: rawText.slice(0, sampleLimit),
        boundingClientRect: {
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
          top: rect.top,
          right: rect.right,
          bottom: rect.bottom,
          left: rect.left,
        },
      };
    }), limit);
    entrypoints.push({
      page_path: pagePath,
      page_url: pageUrl,
      viewport: { width: viewport.width, height: viewport.height, device_scale_factor: 1 },
      elements,
    });
    await page.close();
  }
  await browser.close();
  process.stdout.write(JSON.stringify({ entrypoints }));
})().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> DomRect {
        DomRect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
            top: 2.0,
            right: 4.0,
            bottom: 6.0,
            left: 1.0,
        }
    }

    #[test]
    fn shapes_report_and_truncates_text_samples() {
        let report = shape_report(
            Path::new("/artifact"),
            BrowserReport {
                entrypoints: vec![BrowserEntrypointReport {
                    page_path: "/page.html".to_string(),
                    page_url: "http://127.0.0.1/page.html".to_string(),
                    viewport: DomBoxViewport {
                        width: 1440,
                        height: 900,
                        device_scale_factor: 1,
                    },
                    elements: vec![BrowserElement {
                        node_id: "12:34".to_string(),
                        node_name: Some("Footer text".to_string()),
                        selector: "p[data-figma-node-id=\"12:34\"]".to_string(),
                        tag: "p".to_string(),
                        text_sample: "Footer\ntext with    extra words".to_string(),
                        bounding_client_rect: rect(),
                    }],
                }],
            },
            16,
        );

        assert_eq!(report.schema, ARTIFACT_DOM_BOXES_SCHEMA);
        assert_eq!(
            report.entrypoints[0].elements[0].text_sample,
            "Footer text with"
        );
        assert_eq!(report.entrypoints[0].elements[0].node_id, "12:34");
    }

    #[test]
    fn plans_multiple_entrypoint_paths_from_artifact_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("index.html");
        let nested_dir = dir.path().join("nested");
        std::fs::create_dir_all(&nested_dir).expect("nested");
        let second = nested_dir.join("page.html");
        std::fs::write(&first, "<html></html>").expect("first");
        std::fs::write(&second, "<html></html>").expect("second");

        let planned = plan_capture(
            dir.path(),
            &[
                PathBuf::from("index.html"),
                PathBuf::from("nested/page.html"),
            ],
        )
        .expect("plan");

        assert_eq!(planned, vec!["/index.html", "/nested/page.html"]);
    }
}
