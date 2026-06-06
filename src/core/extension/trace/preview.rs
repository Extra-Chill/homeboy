use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::core::error::{Error, Result};
use crate::core::rig::TracePublicPreviewSpec;

const DEFAULT_STARTUP_TIMEOUT_SECONDS: u64 = 20;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TracePreviewMetadata {
    pub schema: String,
    pub requested_mode: String,
    pub provider: String,
    pub local_origin: String,
    pub local_url: String,
    pub public_origin: String,
    pub public_url: String,
    pub browser_effective_origin: String,
    pub window_location_origin: String,
    pub window_is_secure_context: bool,
    pub require_https: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    pub cleanup_status: String,
}

pub struct TracePublicPreviewSession {
    child: Option<Child>,
    metadata: TracePreviewMetadata,
}

impl TracePublicPreviewSession {
    pub fn start(spec: &TracePublicPreviewSpec) -> Result<Self> {
        let mut child = match spec.command.as_deref() {
            Some(command) if spec.public_origin.is_none() => Some(start_provider_command(command)?),
            _ => None,
        };
        let public_origin = match spec.public_origin.as_deref() {
            Some(origin) => origin.to_string(),
            None => read_public_origin(
                child.as_mut().ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "public_preview",
                        "public_preview requires public_origin or command",
                        Some(spec.local_origin.clone()),
                        None,
                    )
                })?,
                spec.startup_timeout_seconds
                    .unwrap_or(DEFAULT_STARTUP_TIMEOUT_SECONDS),
            )?,
        };

        let is_https = public_origin.starts_with("https://");
        if spec.require_https && !is_https {
            stop_child(&mut child);
            return Err(Error::validation_invalid_argument(
                "public_preview.public_origin",
                "trace public_preview requires an HTTPS public origin",
                Some(public_origin),
                None,
            ));
        }

        let process_id = child.as_ref().map(|child| child.id().to_string());
        Ok(Self {
            child,
            metadata: TracePreviewMetadata {
                schema: "homeboy/preview/v1".to_string(),
                requested_mode: "public-https".to_string(),
                provider: spec
                    .provider
                    .clone()
                    .unwrap_or_else(|| "external-command".to_string()),
                local_origin: spec.local_origin.clone(),
                local_url: spec.local_origin.clone(),
                public_origin: public_origin.clone(),
                public_url: public_origin.clone(),
                browser_effective_origin: public_origin.clone(),
                window_location_origin: public_origin,
                window_is_secure_context: is_https,
                require_https: spec.require_https,
                status: "running".to_string(),
                process_id,
                cleanup_status: "pending".to_string(),
            },
        })
    }

    pub fn metadata(&self) -> &TracePreviewMetadata {
        &self.metadata
    }

    pub fn env_vars(&self) -> Result<Vec<(String, String)>> {
        Ok(vec![
            (
                crate::core::observation::context::PREVIEW_METADATA_ENV.to_string(),
                serde_json::to_string(&self.metadata).map_err(|e| {
                    Error::internal_json(
                        format!("Failed to serialize trace preview metadata: {e}"),
                        Some("trace.preview.serialize".to_string()),
                    )
                })?,
            ),
            (
                crate::core::observation::context::PREVIEW_PUBLIC_URL_ENV.to_string(),
                self.metadata.public_origin.clone(),
            ),
            (
                "HOMEBOY_TRACE_PREVIEW_LOCAL_ORIGIN".to_string(),
                self.metadata.local_origin.clone(),
            ),
            (
                "HOMEBOY_TRACE_PREVIEW_PUBLIC_ORIGIN".to_string(),
                self.metadata.public_origin.clone(),
            ),
            (
                "HOMEBOY_TRACE_BROWSER_EFFECTIVE_ORIGIN".to_string(),
                self.metadata.browser_effective_origin.clone(),
            ),
            (
                "HOMEBOY_TRACE_WINDOW_LOCATION_ORIGIN".to_string(),
                self.metadata.window_location_origin.clone(),
            ),
            (
                "HOMEBOY_TRACE_WINDOW_IS_SECURE_CONTEXT".to_string(),
                self.metadata.window_is_secure_context.to_string(),
            ),
        ])
    }

    pub fn finish(mut self) -> TracePreviewMetadata {
        self.metadata.cleanup_status = if stop_child(&mut self.child) {
            "terminated".to_string()
        } else {
            "not_started".to_string()
        };
        self.metadata.status = "stopped".to_string();
        self.metadata.clone()
    }
}

impl Drop for TracePublicPreviewSession {
    fn drop(&mut self) {
        stop_child(&mut self.child);
    }
}

fn start_provider_command(command: &str) -> Result<Child> {
    Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to start trace public_preview command: {e}"),
                Some("trace.preview.start".to_string()),
            )
        })
}

fn read_public_origin(child: &mut Child, timeout_seconds: u64) -> Result<String> {
    let stdout = child.stdout.take().ok_or_else(|| {
        Error::internal_io(
            "trace public_preview command stdout was not captured".to_string(),
            Some("trace.preview.stdout".to_string()),
        )
    })?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if let Some(origin) = first_https_origin(&line) {
                        let _ = sender.send(Ok(origin));
                        return;
                    }
                }
                Err(e) => {
                    let _ = sender.send(Err(e.to_string()));
                    return;
                }
            }
        }
        let _ = sender.send(Err(
            "provider stdout closed before an HTTPS URL appeared".to_string()
        ));
    });

    match receiver.recv_timeout(Duration::from_secs(timeout_seconds.max(1))) {
        Ok(Ok(origin)) => Ok(origin),
        Ok(Err(message)) => Err(Error::validation_invalid_argument(
            "public_preview.command",
            format!("trace public_preview command failed to provide an HTTPS URL: {message}"),
            None,
            None,
        )),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            Err(Error::validation_invalid_argument(
                "public_preview.command",
                "trace public_preview command did not print an HTTPS URL before timeout",
                None,
                None,
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::validation_invalid_argument(
            "public_preview.command",
            "trace public_preview command stdout reader stopped before an HTTPS URL appeared",
            None,
            None,
        )),
    }
}

fn first_https_origin(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | ')'))
        .unwrap_or(rest.len());
    Some(rest[..end].trim_end_matches('/').to_string())
}

fn stop_child(child: &mut Option<Child>) -> bool {
    let Some(mut child) = child.take() else {
        return false;
    };
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    true
}

#[cfg(test)]
mod tests {
    use super::{first_https_origin, TracePublicPreviewSession};
    use crate::core::rig::TracePublicPreviewSpec;

    #[test]
    fn extracts_first_https_origin_from_provider_output() {
        assert_eq!(
            first_https_origin("ready: https://preview.example.test/run-1\n").as_deref(),
            Some("https://preview.example.test/run-1")
        );
    }

    #[test]
    fn starts_provider_command_and_records_preview_metadata() {
        let session = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            local_origin: "http://127.0.0.1:8080".to_string(),
            public_origin: None,
            command: Some(
                "printf 'ready https://preview.example.test/run-1\\n'; sleep 5".to_string(),
            ),
            require_https: true,
            provider: Some("fixture".to_string()),
            startup_timeout_seconds: Some(2),
        })
        .expect("preview starts");

        assert_eq!(
            session.metadata().public_origin,
            "https://preview.example.test/run-1"
        );
        assert!(session.metadata().window_is_secure_context);

        let metadata = session.finish();
        assert_eq!(metadata.status, "stopped");
        assert_eq!(metadata.cleanup_status, "terminated");
    }
}
