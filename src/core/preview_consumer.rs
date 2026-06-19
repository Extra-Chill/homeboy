//! Preview-consumer execution core service.
//!
//! Running a configured preview consumer is orchestration — it parses a config
//! file, resolves the Homeboy-owned public preview URL, spawns the consumer
//! process, and persists a run artifact. The `tunnel` command module stays a
//! thin adapter: it parses CLI arguments into a [`PreviewConsumerRunRequest`]
//! and delegates to [`run`], then adapts the [`PreviewConsumerRunResult`] for
//! output.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::tunnel;

/// Arguments needed to run a preview consumer, parsed from CLI input.
pub struct PreviewConsumerRunRequest {
    pub config_path: PathBuf,
    pub service_id: Option<String>,
    pub preview_public_url: Option<String>,
    pub artifacts_dir_override: Option<PathBuf>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_result_url: Option<String>,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
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

/// Run a configured preview consumer end to end: resolve the public URL, ensure
/// the artifacts directory exists, execute the consumer process, and persist a
/// run artifact. Returns the structured result and the process exit code.
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
    let artifact_file = config
        .artifact_file
        .as_deref()
        .unwrap_or("homeboy-preview-consumer.json");
    let result = PreviewConsumerRunResult {
        schema: "homeboy/preview-consumer-run/v1".to_string(),
        consumer_id: config.id.clone(),
        preview_public_url: public_url,
        service_id: request.service_id,
        artifacts_dir: artifacts_dir.display().to_string(),
        public_result_url,
        exit_code,
        stdout,
        stderr,
        artifact_path: artifacts_dir.join(artifact_file).display().to_string(),
    };

    let artifact_json = serde_json::to_string_pretty(&result).map_err(|err| {
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
    })?;

    Ok((result, exit_code))
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
