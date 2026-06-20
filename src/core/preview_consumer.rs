//! Preview-consumer execution core service.
//!
//! Running a configured preview consumer is orchestration — it parses a config
//! file, resolves the Homeboy-owned public preview URL, spawns the consumer
//! process, and persists a run artifact. The `tunnel` command module stays a
//! thin adapter: it parses CLI arguments into a [`PreviewConsumerRunRequest`]
//! and delegates to [`run`], then adapts the [`PreviewConsumerRunResult`] for
//! output.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::tunnel;

/// Execution mode for a preview consumer run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewConsumerRunMode {
    /// Run the command to completion and record output after it exits.
    Blocking,
    /// Start the command under supervision and return as soon as the preview is
    /// ready (or the readiness wait elapses), leaving the command running.
    NonBlocking,
}

impl Default for PreviewConsumerRunMode {
    fn default() -> Self {
        Self::Blocking
    }
}

/// Lifecycle status reported by a preview consumer run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewConsumerStatus {
    /// The command ran to completion (blocking mode).
    Completed,
    /// The command is still running under supervision (non-blocking mode).
    Running,
}

/// Arguments needed to run a preview consumer, parsed from CLI input.
pub struct PreviewConsumerRunRequest {
    pub config_path: PathBuf,
    pub service_id: Option<String>,
    pub preview_public_url: Option<String>,
    pub artifacts_dir_override: Option<PathBuf>,
    /// Execution mode. Defaults to blocking for simple one-shot consumers.
    pub mode: PreviewConsumerRunMode,
    /// How long to wait for the preview to report ready in non-blocking mode
    /// before returning anyway. Ignored in blocking mode.
    pub ready_timeout: Option<Duration>,
}

/// Structured result of a preview-consumer run, suitable for serialization into
/// command output and persistence as a run artifact.
#[derive(Debug, Serialize)]
pub struct PreviewConsumerRunResult {
    pub schema: String,
    pub consumer_id: String,
    pub preview_public_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
    pub artifacts_dir: String,
    /// Lifecycle status: `completed` for blocking runs, `running` for
    /// non-blocking supervised runs.
    pub status: PreviewConsumerStatus,
    /// Whether a ready preview URL/artifact was detected.
    pub preview_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_result_url: Option<String>,
    /// Process id of the supervised command in non-blocking mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Exit code in blocking mode; `None` while still running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Captured stdout in blocking mode; empty while streaming to log files.
    pub stdout: String,
    /// Captured stderr in blocking mode; empty while streaming to log files.
    pub stderr: String,
    /// Path to the streamed stdout log file in non-blocking mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_log_path: Option<String>,
    /// Path to the streamed stderr log file in non-blocking mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_log_path: Option<String>,
    pub artifact_path: String,
}

#[derive(Debug, Deserialize)]
struct PreviewConsumerConfig {
    pub id: String,
    pub command: PreviewConsumerCommandConfig,
    #[serde(default)]
    pub output: PreviewConsumerOutputConfig,
    #[serde(default)]
    pub artifact_file: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PreviewConsumerCommandConfig {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub artifacts_dir: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct PreviewConsumerOutputConfig {
    #[serde(default)]
    pub public_result_json_file: Option<PathBuf>,
    #[serde(default)]
    pub public_result_json_pointer: Option<String>,
    #[serde(default)]
    pub public_result_stdout_prefix: Option<String>,
}

/// Default time to wait for a non-blocking consumer to report a ready preview
/// before returning while leaving the command running.
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Run a configured preview consumer end to end: resolve the public URL, ensure
/// the artifacts directory exists, execute the consumer process, and persist a
/// run artifact. Returns the structured result and the process exit code.
///
/// In [`PreviewConsumerRunMode::Blocking`] the command runs to completion and
/// the artifact captures its full output and exit code. In
/// [`PreviewConsumerRunMode::NonBlocking`] the command is started under
/// supervision with stdout/stderr streamed to log files, and the function
/// returns as soon as a ready preview URL is detected (or the readiness wait
/// elapses) while leaving the command running for held preview workflows.
pub fn run(
    request: PreviewConsumerRunRequest,
) -> crate::core::Result<(PreviewConsumerRunResult, i32)> {
    let config = read_config(&request.config_path)?;
    let public_url = resolve_public_url(
        request.service_id.as_deref(),
        request.preview_public_url.as_deref(),
    )?;
    let artifacts_dir = request
        .artifacts_dir_override
        .clone()
        .or_else(|| config.command.artifacts_dir.clone())
        .unwrap_or_else(|| {
            crate::core::artifacts::root()
                .unwrap_or_else(|_| std::env::temp_dir().join("homeboy-artifacts"))
                .join("preview-consumer")
                .join(safe_artifact_slug(&config.id))
        });
    std::fs::create_dir_all(&artifacts_dir).map_err(|err| {
        crate::core::Error::internal_io(
            err.to_string(),
            Some(format!("create artifacts dir {}", artifacts_dir.display())),
        )
    })?;

    let mut command = Command::new(render_template(
        &config.command.program,
        &public_url,
        &artifacts_dir,
    ));
    for arg in &config.command.args {
        command.arg(render_template(arg, &public_url, &artifacts_dir));
    }
    for (key, value) in &config.command.env {
        command.env(key, render_template(value, &public_url, &artifacts_dir));
    }
    if let Some(cwd) = &config.command.cwd {
        command.current_dir(cwd);
    }

    match request.mode {
        PreviewConsumerRunMode::Blocking => {
            run_blocking(request, config, command, public_url, artifacts_dir)
        }
        PreviewConsumerRunMode::NonBlocking => {
            run_non_blocking(request, config, command, public_url, artifacts_dir)
        }
    }
}

fn run_blocking(
    request: PreviewConsumerRunRequest,
    config: PreviewConsumerConfig,
    mut command: Command,
    public_url: String,
    artifacts_dir: PathBuf,
) -> crate::core::Result<(PreviewConsumerRunResult, i32)> {
    let output = command.output().map_err(|err| {
        crate::core::Error::internal_io(
            err.to_string(),
            Some(format!("run preview consumer {}", config.id)),
        )
    })?;
    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let public_result_url = extract_public_result_url(&config.output, &artifacts_dir, &stdout);
    let result = PreviewConsumerRunResult {
        schema: "homeboy/preview-consumer-run/v1".to_string(),
        consumer_id: config.id.clone(),
        preview_public_url: public_url,
        service_id: request.service_id,
        artifacts_dir: artifacts_dir.display().to_string(),
        status: PreviewConsumerStatus::Completed,
        preview_ready: public_result_url.is_some(),
        public_result_url,
        pid: None,
        exit_code: Some(exit_code),
        stdout,
        stderr,
        stdout_log_path: None,
        stderr_log_path: None,
        artifact_path: artifact_path(&config, &artifacts_dir),
    };

    write_artifact(&result)?;
    Ok((result, exit_code))
}

fn run_non_blocking(
    request: PreviewConsumerRunRequest,
    config: PreviewConsumerConfig,
    mut command: Command,
    public_url: String,
    artifacts_dir: PathBuf,
) -> crate::core::Result<(PreviewConsumerRunResult, i32)> {
    let stdout_log_path = artifacts_dir.join("homeboy-preview-consumer.stdout.log");
    let stderr_log_path = artifacts_dir.join("homeboy-preview-consumer.stderr.log");

    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|err| {
        crate::core::Error::internal_io(
            err.to_string(),
            Some(format!("spawn preview consumer {}", config.id)),
        )
    })?;
    let pid = child.id();

    // Stream stderr to its log file on a background thread so a chatty child
    // cannot block readiness detection on stdout.
    let stderr_reader = child.stderr.take();
    let stderr_log_for_thread = stderr_log_path.clone();
    let stderr_thread = stderr_reader.map(|stderr| {
        std::thread::spawn(move || {
            stream_to_file(stderr, &stderr_log_for_thread);
        })
    });

    // Read stdout line by line, tee it to the stdout log, and watch for a ready
    // preview URL while the child keeps running.
    let ready_timeout = request.ready_timeout.unwrap_or(DEFAULT_READY_TIMEOUT);
    let started = Instant::now();
    let mut public_result_url: Option<String> = None;

    if let Some(stdout) = child.stdout.take() {
        let mut writer = open_log_writer(&stdout_log_path);
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if let Some(writer) = writer.as_mut() {
                use std::io::Write;
                let _ = writeln!(writer, "{line}");
                let _ = writer.flush();
            }
            if public_result_url.is_none() {
                public_result_url = detect_ready_url(&config.output, &line);
                if public_result_url.is_some() {
                    break;
                }
            }
            if started.elapsed() >= ready_timeout {
                break;
            }
        }
    }

    // A ready preview may also be signalled purely via a result file the child
    // writes; check that path even if no stdout prefix matched.
    if public_result_url.is_none() {
        public_result_url = extract_public_result_url(&config.output, &artifacts_dir, "");
    }

    drop(stderr_thread);

    let result = PreviewConsumerRunResult {
        schema: "homeboy/preview-consumer-run/v1".to_string(),
        consumer_id: config.id.clone(),
        preview_public_url: public_url,
        service_id: request.service_id,
        artifacts_dir: artifacts_dir.display().to_string(),
        status: PreviewConsumerStatus::Running,
        preview_ready: public_result_url.is_some(),
        public_result_url,
        pid: Some(pid),
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        stdout_log_path: Some(stdout_log_path.display().to_string()),
        stderr_log_path: Some(stderr_log_path.display().to_string()),
        artifact_path: artifact_path(&config, &artifacts_dir),
    };

    write_artifact(&result)?;
    // Non-blocking runs intentionally leave the command alive; report success so
    // callers can surface the live preview URL. Final exit status is reconciled
    // when the hold ends or the service is stopped.
    Ok((result, 0))
}

fn artifact_path(config: &PreviewConsumerConfig, artifacts_dir: &Path) -> String {
    let artifact_file = config
        .artifact_file
        .as_deref()
        .unwrap_or("homeboy-preview-consumer.json");
    artifacts_dir.join(artifact_file).display().to_string()
}

fn write_artifact(result: &PreviewConsumerRunResult) -> crate::core::Result<()> {
    let artifact_json = serde_json::to_string_pretty(result).map_err(|err| {
        crate::core::Error::internal_json(
            err.to_string(),
            Some("serialize preview consumer run artifact".to_string()),
        )
    })?;
    std::fs::write(&result.artifact_path, format!("{artifact_json}\n")).map_err(|err| {
        crate::core::Error::internal_io(
            err.to_string(),
            Some(format!("write {}", result.artifact_path)),
        )
    })
}

fn open_log_writer(path: &Path) -> Option<std::fs::File> {
    std::fs::File::create(path).ok()
}

fn stream_to_file<R: std::io::Read>(reader: R, path: &Path) {
    use std::io::Write;
    let mut writer = match std::fs::File::create(path) {
        Ok(file) => file,
        Err(_) => return,
    };
    let reader = BufReader::new(reader);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let _ = writeln!(writer, "{line}");
    }
    let _ = writer.flush();
}

/// Detect a ready preview URL from a single streamed stdout line using the
/// configured stdout prefix.
fn detect_ready_url(config: &PreviewConsumerOutputConfig, line: &str) -> Option<String> {
    config
        .public_result_stdout_prefix
        .as_deref()
        .and_then(|prefix| parse_prefixed_line(line, prefix))
}

fn read_config(path: &Path) -> crate::core::Result<PreviewConsumerConfig> {
    let raw = std::fs::read_to_string(path).map_err(|err| {
        crate::core::Error::internal_io(
            err.to_string(),
            Some(format!("read preview consumer config {}", path.display())),
        )
    })?;
    serde_json::from_str(&raw).map_err(|err| {
        crate::core::Error::validation_invalid_json(
            err,
            Some(format!("parse preview consumer config {}", path.display())),
            Some(raw),
        )
    })
}

fn resolve_public_url(
    service_id: Option<&str>,
    preview_public_url: Option<&str>,
) -> crate::core::Result<String> {
    if let Some(public_url) = preview_public_url {
        return Ok(public_url.to_string());
    }
    let Some(service_id) = service_id else {
        return Err(crate::core::Error::validation_missing_argument(vec![
            "--service-id or --preview-public-url".to_string(),
        ]));
    };
    let status = tunnel::status(service_id)?;
    status
        .preview_identity
        .public_url
        .or_else(|| status.preview.and_then(|preview| preview.preview_identity.public_url))
        .ok_or_else(|| {
            crate::core::Error::validation_invalid_argument(
                "service-id",
                "service status does not contain a public preview URL; start the service with a public tunnel backend first",
                Some(service_id.to_string()),
                None,
            )
        })
}

fn render_template(value: &str, public_url: &str, artifacts_dir: &Path) -> String {
    value
        .replace("${preview_public_url}", public_url)
        .replace("${artifacts_dir}", &artifacts_dir.to_string_lossy())
}

fn extract_public_result_url(
    config: &PreviewConsumerOutputConfig,
    artifacts_dir: &Path,
    stdout: &str,
) -> Option<String> {
    config
        .public_result_json_file
        .as_ref()
        .and_then(|path| {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                artifacts_dir.join(path)
            };
            let raw = std::fs::read_to_string(path).ok()?;
            serde_json::from_str::<Value>(&raw).ok()
        })
        .and_then(|value| {
            config
                .public_result_json_pointer
                .as_deref()
                .and_then(|pointer| json_pointer_string(&value, pointer))
        })
        .or_else(|| {
            config
                .public_result_stdout_prefix
                .as_deref()
                .and_then(|prefix| parse_prefixed_line(stdout, prefix))
        })
}

fn json_pointer_string(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer)?.as_str().map(str::to_string)
}

fn parse_prefixed_line(output: &str, prefix: &str) -> Option<String> {
    output.lines().find_map(|line| {
        line.strip_prefix(prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

/// Build a filesystem-safe artifact slug from a consumer id.
pub fn safe_artifact_slug(value: &str) -> String {
    let slug: String = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    slug.trim_matches('-').chars().take(96).collect()
}

#[cfg(test)]
#[path = "preview_consumer_tests.rs"]
mod preview_consumer_tests;
