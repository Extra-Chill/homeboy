use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::core::error::{Error, Result};
use crate::core::rig::{
    TraceNativePublicPreviewSpec, TracePublicPreviewMode, TracePublicPreviewSpec,
};

const DEFAULT_STARTUP_TIMEOUT_SECONDS: u64 = 20;
const DEFAULT_ASSET_CHECK_TIMEOUT_SECONDS: u64 = 10;
const DEFAULT_ASSET_FANOUT_CONCURRENCY: usize = 16;
const DEFAULT_ASSET_FANOUT_REPEAT_COUNT: usize = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TracePreviewAssetCheck {
    pub path: String,
    pub url: String,
    pub status: Option<u16>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TracePreviewAssetFanoutRequest {
    pub path: String,
    pub url: String,
    pub status: Option<u16>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_bucket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TracePreviewAssetFanoutReport {
    pub schema: String,
    pub concurrency: usize,
    pub repeat_count: usize,
    pub asset_path_count: usize,
    pub expected_request_count: usize,
    pub client_request_count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress_request_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_origin_request_count: Option<usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub status_counts: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failure_buckets: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<TracePreviewAssetFanoutRequest>,
}

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_assets: Vec<TracePreviewAssetCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_fanout: Option<TracePreviewAssetFanoutReport>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    pub cleanup_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_paths: Option<TracePreviewLogPaths>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TracePreviewLogPaths {
    pub client_stdout_path: String,
    pub client_stderr_path: String,
}

pub struct TracePublicPreviewSession {
    child: Option<Child>,
    metadata: TracePreviewMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativePreviewClientCommand {
    program: String,
    args: Vec<String>,
}

impl NativePreviewClientCommand {
    fn display(&self) -> String {
        std::iter::once(self.program.clone())
            .chain(self.args.clone())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl TracePublicPreviewSession {
    pub fn start(spec: &TracePublicPreviewSpec) -> Result<Self> {
        Self::start_with_artifact_dir(spec, None)
    }

    pub fn start_with_artifact_dir(
        spec: &TracePublicPreviewSpec,
        artifact_dir: Option<&Path>,
    ) -> Result<Self> {
        match spec.mode {
            TracePublicPreviewMode::External => Self::start_external(spec),
            TracePublicPreviewMode::HomeboyNative => Self::start_homeboy_native(spec, artifact_dir),
        }
    }

    fn start_external(spec: &TracePublicPreviewSpec) -> Result<Self> {
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

        let required_assets = match preflight_required_assets(spec, &public_origin) {
            Ok(checks) => checks,
            Err(error) => {
                stop_child(&mut child);
                return Err(error);
            }
        };

        let asset_fanout = match validate_asset_fanout(spec, &public_origin) {
            Ok(report) => report,
            Err(error) => {
                stop_child(&mut child);
                return Err(error);
            }
        };

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
                required_assets,
                asset_fanout,
                status: "running".to_string(),
                process_id,
                cleanup_status: "pending".to_string(),
                session_id: None,
                public_host: None,
                ingress_url: None,
                client_command: None,
                log_paths: None,
            },
        })
    }

    fn start_homeboy_native(
        spec: &TracePublicPreviewSpec,
        artifact_dir: Option<&Path>,
    ) -> Result<Self> {
        let native = spec.native.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "public_preview.native",
                "homeboy_native public_preview requires native settings",
                Some(spec.local_origin.clone()),
                None,
            )
        })?;
        let public_host = native_public_host(native)?;
        let public_origin = spec
            .public_origin
            .clone()
            .unwrap_or_else(|| format!("https://{public_host}"));

        let is_https = public_origin.starts_with("https://");
        if spec.require_https && !is_https {
            return Err(Error::validation_invalid_argument(
                "public_preview.public_origin",
                "trace public_preview requires an HTTPS public origin",
                Some(public_origin),
                None,
            ));
        }

        let native_command = native_preview_client_command(spec, native, &public_host)?;
        let (mut child, log_paths) = start_native_preview_client(&native_command, artifact_dir)?;
        let ready_child = child.as_mut().ok_or_else(|| {
            Error::internal_io(
                "native trace public_preview client did not start".to_string(),
                Some("trace.preview.native.child".to_string()),
            )
        })?;
        if let Err(error) = read_native_client_ready(
            ready_child,
            &public_origin,
            spec.startup_timeout_seconds
                .unwrap_or(DEFAULT_STARTUP_TIMEOUT_SECONDS),
            log_paths.as_ref(),
        ) {
            stop_child(&mut child);
            return Err(error);
        }

        let required_assets = match preflight_required_assets(spec, &public_origin) {
            Ok(checks) => checks,
            Err(error) => {
                stop_child(&mut child);
                return Err(error);
            }
        };

        let asset_fanout = match validate_asset_fanout(spec, &public_origin) {
            Ok(report) => report,
            Err(error) => {
                stop_child(&mut child);
                return Err(error);
            }
        };

        let process_id = child.as_ref().map(|child| child.id().to_string());
        Ok(Self {
            child,
            metadata: TracePreviewMetadata {
                schema: "homeboy/preview/v1".to_string(),
                requested_mode: "public-https".to_string(),
                provider: spec
                    .provider
                    .clone()
                    .unwrap_or_else(|| "homeboy-native".to_string()),
                local_origin: spec.local_origin.clone(),
                local_url: spec.local_origin.clone(),
                public_origin: public_origin.clone(),
                public_url: public_origin.clone(),
                browser_effective_origin: public_origin.clone(),
                window_location_origin: public_origin,
                window_is_secure_context: is_https,
                require_https: spec.require_https,
                required_assets,
                asset_fanout,
                status: "running".to_string(),
                process_id,
                cleanup_status: "pending".to_string(),
                session_id: native.session_id.clone(),
                public_host: Some(public_host),
                ingress_url: native.ingress_url.clone(),
                client_command: Some(native_command.display()),
                log_paths,
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
            (
                "HOMEBOY_TRACE_PREVIEW_PROVIDER".to_string(),
                self.metadata.provider.clone(),
            ),
        ])
        .map(|mut env| {
            if let Some(session_id) = &self.metadata.session_id {
                env.push((
                    "HOMEBOY_TRACE_PREVIEW_SESSION_ID".to_string(),
                    session_id.clone(),
                ));
            }
            if let Some(public_host) = &self.metadata.public_host {
                env.push((
                    "HOMEBOY_TRACE_PREVIEW_PUBLIC_HOST".to_string(),
                    public_host.clone(),
                ));
            }
            if let Some(ingress_url) = &self.metadata.ingress_url {
                env.push((
                    "HOMEBOY_TRACE_PREVIEW_INGRESS_URL".to_string(),
                    ingress_url.clone(),
                ));
            }
            env
        })
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

fn native_public_host(native: &TraceNativePublicPreviewSpec) -> Result<String> {
    if let Some(host) = native.public_host.as_deref() {
        let host = host
            .trim()
            .trim_start_matches("https://")
            .trim_end_matches('/');
        if !host.is_empty() {
            return Ok(host.to_string());
        }
    }

    let session_id = native.session_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "public_preview.native.session_id",
            "homeboy_native public_preview requires session_id when public_host is omitted",
            None,
            None,
        )
    })?;
    let operator_domain = native.operator_domain.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "public_preview.native.operator_domain",
            "homeboy_native public_preview requires operator_domain when public_host is omitted",
            Some(session_id.to_string()),
            None,
        )
    })?;
    Ok(format!(
        "{}-tunnel.{}",
        session_id.trim().trim_matches('.'),
        operator_domain.trim().trim_start_matches("*."),
    ))
}

fn native_preview_client_command(
    spec: &TracePublicPreviewSpec,
    native: &TraceNativePublicPreviewSpec,
    public_host: &str,
) -> Result<NativePreviewClientCommand> {
    let program = match native.client_binary.as_deref() {
        Some(path) => path.to_string(),
        None => std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "homeboy".to_string()),
    };
    let mut args = vec![
        "tunnel".to_string(),
        "preview-client".to_string(),
        "start".to_string(),
        "--public-host".to_string(),
        public_host.to_string(),
        "--local-origin".to_string(),
        spec.local_origin.clone(),
    ];
    if let Some(session_id) = native.session_id.as_deref() {
        args.extend(["--session-id".to_string(), session_id.to_string()]);
    }
    if let Some(ingress_url) = native.ingress_url.as_deref() {
        args.extend(["--ingress".to_string(), ingress_url.to_string()]);
    }
    if let Some(token_env) = native.token_env.as_deref() {
        args.extend(["--token-env".to_string(), token_env.to_string()]);
    }
    args.push("--ready-stdout".to_string());
    Ok(NativePreviewClientCommand { program, args })
}

fn start_native_preview_client(
    native_command: &NativePreviewClientCommand,
    artifact_dir: Option<&Path>,
) -> Result<(Option<Child>, Option<TracePreviewLogPaths>)> {
    let (stderr, log_paths) = match artifact_dir {
        Some(artifact_dir) => {
            std::fs::create_dir_all(artifact_dir).map_err(|e| {
                Error::internal_io(
                    format!(
                        "Failed to create trace preview artifact dir {}: {e}",
                        artifact_dir.display()
                    ),
                    Some("trace.preview.native.artifact_dir".to_string()),
                )
            })?;
            let stdout_path = artifact_dir.join("preview-client.stdout.log");
            let stderr_path = artifact_dir.join("preview-client.stderr.log");
            let stderr = std::fs::File::create(&stderr_path).map_err(|e| {
                Error::internal_io(
                    format!(
                        "Failed to create native preview client stderr log {}: {e}",
                        stderr_path.display()
                    ),
                    Some("trace.preview.native.stderr".to_string()),
                )
            })?;
            (
                Stdio::from(stderr),
                Some(TracePreviewLogPaths {
                    client_stdout_path: stdout_path.display().to_string(),
                    client_stderr_path: stderr_path.display().to_string(),
                }),
            )
        }
        None => (Stdio::piped(), None),
    };

    let child = Command::new(&native_command.program)
        .args(&native_command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to start native trace public_preview client: {e}"),
                Some("trace.preview.native.start".to_string()),
            )
        })?;
    Ok((Some(child), log_paths))
}

fn read_native_client_ready(
    child: &mut Child,
    public_origin: &str,
    timeout_seconds: u64,
    log_paths: Option<&TracePreviewLogPaths>,
) -> Result<()> {
    let stdout = child.stdout.take().ok_or_else(|| {
        Error::internal_io(
            "native trace public_preview client stdout was not captured".to_string(),
            Some("trace.preview.native.stdout".to_string()),
        )
    })?;
    let stdout_log = log_paths
        .map(|paths| PathBuf::from(&paths.client_stdout_path))
        .map(std::fs::File::create)
        .transpose()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to create native preview client stdout log: {e}"),
                Some("trace.preview.native.stdout_log".to_string()),
            )
        })?;
    let expected_origin = public_origin.trim_end_matches('/').to_string();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdout_log = stdout_log;
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if let Some(log) = stdout_log.as_mut() {
                        use std::io::Write;
                        let _ = writeln!(log, "{line}");
                    }
                    if first_https_origin(&line).as_deref() == Some(expected_origin.as_str())
                        || line.contains(&expected_origin)
                    {
                        let _ = sender.send(Ok(()));
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
            "native preview client stdout closed before readiness appeared".to_string(),
        ));
    });

    match receiver.recv_timeout(Duration::from_secs(timeout_seconds.max(1))) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(message)) => Err(Error::validation_invalid_argument(
            "public_preview.native.client",
            format!("native trace public_preview client failed to report readiness: {message}"),
            Some(public_origin.to_string()),
            None,
        )),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::validation_invalid_argument(
            "public_preview.native.client",
            "native trace public_preview client did not report readiness before timeout",
            Some(public_origin.to_string()),
            None,
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::validation_invalid_argument(
            "public_preview.native.client",
            "native trace public_preview client readiness reader stopped before readiness appeared",
            Some(public_origin.to_string()),
            None,
        )),
    }
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

fn preflight_required_assets(
    spec: &TracePublicPreviewSpec,
    public_origin: &str,
) -> Result<Vec<TracePreviewAssetCheck>> {
    if spec.required_asset_paths.is_empty() {
        return Ok(Vec::new());
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(DEFAULT_ASSET_CHECK_TIMEOUT_SECONDS))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to create trace public_preview asset preflight client: {e}"),
                Some("trace.preview.asset_client".to_string()),
            )
        })?;

    let mut checks = Vec::with_capacity(spec.required_asset_paths.len());
    for path in &spec.required_asset_paths {
        let url = preview_asset_url(public_origin, path);
        let check = match client.get(&url).send() {
            Ok(response) => {
                let status = response.status();
                TracePreviewAssetCheck {
                    path: path.clone(),
                    url,
                    status: Some(status.as_u16()),
                    ok: status.is_success(),
                    error: None,
                }
            }
            Err(error) => TracePreviewAssetCheck {
                path: path.clone(),
                url,
                status: None,
                ok: false,
                error: Some(error.to_string()),
            },
        };
        checks.push(check);
    }

    let failed: Vec<&TracePreviewAssetCheck> = checks.iter().filter(|check| !check.ok).collect();
    if failed.is_empty() {
        return Ok(checks);
    }

    let details = failed
        .iter()
        .map(|check| match (check.status, check.error.as_deref()) {
            (Some(status), _) => format!("{} -> HTTP {}", check.url, status),
            (None, Some(error)) => format!("{} -> {error}", check.url),
            (None, None) => format!("{} -> request failed", check.url),
        })
        .collect::<Vec<_>>()
        .join("; ");

    Err(Error::validation_invalid_argument(
        "public_preview.required_asset_paths",
        format!(
            "trace public_preview required asset preflight failed before trace collection: {details}"
        ),
        Some(public_origin.to_string()),
        Some(failed.iter().map(|check| check.url.clone()).collect()),
    ))
}

fn validate_asset_fanout(
    spec: &TracePublicPreviewSpec,
    public_origin: &str,
) -> Result<Option<TracePreviewAssetFanoutReport>> {
    let Some(fanout) = spec.asset_fanout.as_ref() else {
        return Ok(None);
    };
    if fanout.asset_paths.is_empty() {
        return Ok(Some(empty_asset_fanout_report(fanout)));
    }

    let concurrency = fanout
        .concurrency
        .unwrap_or(DEFAULT_ASSET_FANOUT_CONCURRENCY)
        .max(1);
    let repeat_count = fanout
        .repeat_count
        .unwrap_or(DEFAULT_ASSET_FANOUT_REPEAT_COUNT)
        .max(1);
    let expected_request_count = fanout.asset_paths.len() * repeat_count;
    let mut jobs = VecDeque::with_capacity(expected_request_count);
    for _ in 0..repeat_count {
        for path in &fanout.asset_paths {
            jobs.push_back((path.clone(), preview_asset_url(public_origin, path)));
        }
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(DEFAULT_ASSET_CHECK_TIMEOUT_SECONDS))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to create trace public_preview asset fanout client: {e}"),
                Some("trace.preview.asset_fanout_client".to_string()),
            )
        })?;
    let jobs = Arc::new(Mutex::new(jobs));
    let requests = Arc::new(Mutex::new(Vec::with_capacity(expected_request_count)));
    let expected_body_contains = fanout.expected_body_contains.clone();

    let worker_count = concurrency.min(expected_request_count);
    let mut workers = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let client = client.clone();
        let jobs = Arc::clone(&jobs);
        let requests = Arc::clone(&requests);
        let expected_body_contains = expected_body_contains.clone();
        workers.push(std::thread::spawn(move || loop {
            let Some((path, url)) = jobs.lock().expect("asset fanout jobs lock").pop_front() else {
                break;
            };
            let request = fetch_fanout_asset(&client, path, url, expected_body_contains.as_deref());
            requests
                .lock()
                .expect("asset fanout requests lock")
                .push(request);
        }));
    }
    for worker in workers {
        worker.join().map_err(|_| {
            Error::internal_io(
                "trace public_preview asset fanout worker panicked".to_string(),
                Some("trace.preview.asset_fanout".to_string()),
            )
        })?;
    }

    let requests = Arc::try_unwrap(requests)
        .expect("asset fanout request handles released")
        .into_inner()
        .expect("asset fanout requests lock");
    let report = asset_fanout_report(
        concurrency,
        repeat_count,
        fanout.asset_paths.len(),
        requests,
    );
    if report.failure_count == 0 && report.client_request_count == expected_request_count {
        return Ok(Some(report));
    }

    let details = report
        .failure_buckets
        .iter()
        .map(|(bucket, count)| format!("{bucket}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(Error::validation_invalid_argument(
        "public_preview.asset_fanout",
        format!(
            "trace public_preview asset fanout failed before trace collection: expected_request_count={} client_request_count={} failure_count={} failure_buckets=[{}]",
            report.expected_request_count,
            report.client_request_count,
            report.failure_count,
            details
        ),
        Some(public_origin.to_string()),
        Some(report.failure_buckets.keys().cloned().collect()),
    ))
}

fn fetch_fanout_asset(
    client: &reqwest::blocking::Client,
    path: String,
    url: String,
    expected_body_contains: Option<&str>,
) -> TracePreviewAssetFanoutRequest {
    match client.get(&url).send() {
        Ok(response) => {
            let status = response.status();
            if !status.is_success() {
                let failure_bucket = format!("http_{}", status.as_u16());
                return TracePreviewAssetFanoutRequest {
                    path,
                    url,
                    status: Some(status.as_u16()),
                    ok: false,
                    failure_bucket: Some(failure_bucket),
                    error: None,
                };
            }
            if let Some(expected) = expected_body_contains {
                match response.text() {
                    Ok(body) if body.contains(expected) => TracePreviewAssetFanoutRequest {
                        path,
                        url,
                        status: Some(status.as_u16()),
                        ok: true,
                        failure_bucket: None,
                        error: None,
                    },
                    Ok(_) => TracePreviewAssetFanoutRequest {
                        path,
                        url,
                        status: Some(status.as_u16()),
                        ok: false,
                        failure_bucket: Some("body_mismatch".to_string()),
                        error: Some("response body did not contain expected text".to_string()),
                    },
                    Err(error) => TracePreviewAssetFanoutRequest {
                        path,
                        url,
                        status: Some(status.as_u16()),
                        ok: false,
                        failure_bucket: Some("body_read_error".to_string()),
                        error: Some(error.to_string()),
                    },
                }
            } else {
                TracePreviewAssetFanoutRequest {
                    path,
                    url,
                    status: Some(status.as_u16()),
                    ok: true,
                    failure_bucket: None,
                    error: None,
                }
            }
        }
        Err(error) => TracePreviewAssetFanoutRequest {
            path,
            url,
            status: None,
            ok: false,
            failure_bucket: Some(request_error_bucket(&error)),
            error: Some(error.to_string()),
        },
    }
}

fn request_error_bucket(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "timeout".to_string();
    }
    if error.is_connect() {
        return "connect_error".to_string();
    }
    if error.is_body() {
        return "body_error".to_string();
    }
    if error.is_decode() {
        return "decode_error".to_string();
    }
    "request_error".to_string()
}

fn asset_fanout_report(
    concurrency: usize,
    repeat_count: usize,
    asset_path_count: usize,
    requests: Vec<TracePreviewAssetFanoutRequest>,
) -> TracePreviewAssetFanoutReport {
    let expected_request_count = asset_path_count * repeat_count;
    let client_request_count = requests.len();
    let success_count = requests.iter().filter(|request| request.ok).count();
    let failure_count = client_request_count.saturating_sub(success_count);
    let mut status_counts = BTreeMap::new();
    let mut failure_buckets = BTreeMap::new();
    for request in &requests {
        let status = request
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "none".to_string());
        *status_counts.entry(status).or_insert(0) += 1;
        if let Some(bucket) = request.failure_bucket.as_ref() {
            *failure_buckets.entry(bucket.clone()).or_insert(0) += 1;
        }
    }

    TracePreviewAssetFanoutReport {
        schema: "homeboy/preview-asset-fanout/v1".to_string(),
        concurrency,
        repeat_count,
        asset_path_count,
        expected_request_count,
        client_request_count,
        success_count,
        failure_count,
        ingress_request_count: None,
        local_origin_request_count: None,
        status_counts,
        failure_buckets,
        requests,
    }
}

fn empty_asset_fanout_report(
    fanout: &crate::core::rig::TracePreviewAssetFanoutSpec,
) -> TracePreviewAssetFanoutReport {
    TracePreviewAssetFanoutReport {
        schema: "homeboy/preview-asset-fanout/v1".to_string(),
        concurrency: fanout
            .concurrency
            .unwrap_or(DEFAULT_ASSET_FANOUT_CONCURRENCY)
            .max(1),
        repeat_count: fanout
            .repeat_count
            .unwrap_or(DEFAULT_ASSET_FANOUT_REPEAT_COUNT)
            .max(1),
        asset_path_count: 0,
        expected_request_count: 0,
        client_request_count: 0,
        success_count: 0,
        failure_count: 0,
        ingress_request_count: None,
        local_origin_request_count: None,
        status_counts: BTreeMap::new(),
        failure_buckets: BTreeMap::new(),
        requests: Vec::new(),
    }
}

fn preview_asset_url(public_origin: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    format!(
        "{}/{}",
        public_origin.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
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
    use crate::core::rig::{
        TraceNativePublicPreviewSpec, TracePreviewAssetFanoutSpec, TracePublicPreviewMode,
        TracePublicPreviewSpec,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

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
            mode: TracePublicPreviewMode::External,
            local_origin: "http://127.0.0.1:8080".to_string(),
            public_origin: None,
            command: Some(
                "printf 'ready https://preview.example.test/run-1\\n'; sleep 5".to_string(),
            ),
            require_https: true,
            provider: Some("fixture".to_string()),
            startup_timeout_seconds: Some(2),
            required_asset_paths: Vec::new(),
            asset_fanout: None,
            native: None,
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

    #[test]
    fn records_successful_required_asset_preflight() {
        let origin =
            start_asset_fixture_server(vec![("/assets/frontend.js?ver=1".to_string(), 200)]);

        let session = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: origin.clone(),
            public_origin: Some(origin),
            command: None,
            require_https: false,
            provider: Some("fixture".to_string()),
            startup_timeout_seconds: None,
            required_asset_paths: vec!["/assets/frontend.js?ver=1".to_string()],
            asset_fanout: None,
            native: None,
        })
        .expect("preview starts after asset preflight");

        assert_eq!(session.metadata().required_assets.len(), 1);
        assert!(session.metadata().required_assets[0].ok);
        assert_eq!(session.metadata().required_assets[0].status, Some(200));
    }

    #[test]
    fn fails_fast_when_required_asset_cannot_be_fetched() {
        let origin =
            start_asset_fixture_server(vec![("/assets/frontend.js?ver=1".to_string(), 502)]);

        let result = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: origin.clone(),
            public_origin: Some(origin),
            command: None,
            require_https: false,
            provider: Some("fixture".to_string()),
            startup_timeout_seconds: None,
            required_asset_paths: vec!["/assets/frontend.js?ver=1".to_string()],
            asset_fanout: None,
            native: None,
        });

        let error = match result {
            Ok(_) => panic!("required asset preflight should fail before trace starts"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains("required asset preflight failed before trace collection"));
        assert!(message.contains("/assets/frontend.js?ver=1 -> HTTP 502"));
    }

    #[test]
    fn starts_homeboy_native_client_and_records_lifecycle_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let client = temp.path().join("homeboy-preview-client-fixture.sh");
        std::fs::write(
            &client,
            "#!/bin/sh\nprintf 'ready https://run-42-tunnel.chubes.net\\n'\nsleep 5\n",
        )
        .expect("write native client fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&client).expect("metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&client, permissions).expect("chmod fixture");
        }

        let artifact_dir = temp.path().join("artifacts");
        let session = TracePublicPreviewSession::start_with_artifact_dir(
            &TracePublicPreviewSpec {
                mode: TracePublicPreviewMode::HomeboyNative,
                local_origin: "http://127.0.0.1:49823".to_string(),
                public_origin: None,
                command: None,
                require_https: true,
                provider: None,
                startup_timeout_seconds: Some(5),
                required_asset_paths: Vec::new(),
                asset_fanout: None,
                native: Some(TraceNativePublicPreviewSpec {
                    public_host: None,
                    operator_domain: Some("chubes.net".to_string()),
                    session_id: Some("run-42".to_string()),
                    ingress_url: Some("https://preview-broker.chubes.net".to_string()),
                    token_env: Some("HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()),
                    client_binary: Some(client.display().to_string()),
                }),
            },
            Some(&artifact_dir),
        )
        .expect("native preview starts");

        assert_eq!(session.metadata().provider, "homeboy-native");
        assert_eq!(
            session.metadata().public_origin,
            "https://run-42-tunnel.chubes.net"
        );
        assert_eq!(
            session.metadata().public_host.as_deref(),
            Some("run-42-tunnel.chubes.net")
        );
        assert_eq!(session.metadata().session_id.as_deref(), Some("run-42"));
        assert!(session
            .metadata()
            .client_command
            .as_deref()
            .expect("client command")
            .contains("tunnel preview-client start"));
        assert!(session.metadata().log_paths.is_some());
        let env = session.env_vars().expect("env vars");
        assert!(env.contains(&(
            "HOMEBOY_TRACE_PREVIEW_SESSION_ID".to_string(),
            "run-42".to_string()
        )));
        assert!(env.contains(&(
            "HOMEBOY_TRACE_PREVIEW_PUBLIC_HOST".to_string(),
            "run-42-tunnel.chubes.net".to_string()
        )));

        let metadata = session.finish();
        assert_eq!(metadata.cleanup_status, "terminated");
    }

    #[test]
    fn records_successful_concurrent_asset_fanout() {
        let paths = vec![
            "/assets/app.js?ver=1".to_string(),
            "/assets/app.css?ver=1".to_string(),
            "/assets/vendor.js?ver=1".to_string(),
            "/assets/runtime.css?ver=1".to_string(),
        ];
        let expected_requests = paths.len() * 3;
        let (origin, local_origin_count) =
            start_fanout_fixture_server(paths.clone(), expected_requests, 200);

        let session = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: origin.clone(),
            public_origin: Some(origin),
            command: None,
            require_https: false,
            provider: Some("homeboy-native-fixture".to_string()),
            startup_timeout_seconds: None,
            required_asset_paths: Vec::new(),
            asset_fanout: Some(TracePreviewAssetFanoutSpec {
                asset_paths: paths,
                concurrency: Some(4),
                repeat_count: Some(3),
                expected_body_contains: Some("homeboy-fanout-ok".to_string()),
            }),
            native: None,
        })
        .expect("preview starts after asset fanout");

        let report = session
            .metadata()
            .asset_fanout
            .as_ref()
            .expect("fanout report");
        assert_eq!(report.schema, "homeboy/preview-asset-fanout/v1");
        assert_eq!(report.concurrency, 4);
        assert_eq!(report.repeat_count, 3);
        assert_eq!(report.expected_request_count, expected_requests);
        assert_eq!(report.client_request_count, expected_requests);
        assert_eq!(report.success_count, expected_requests);
        assert_eq!(report.failure_count, 0);
        assert_eq!(report.status_counts.get("200"), Some(&expected_requests));
        assert!(report.failure_buckets.is_empty());
        assert_eq!(local_origin_count.load(Ordering::SeqCst), expected_requests);
    }

    #[test]
    fn fails_fast_when_concurrent_asset_fanout_sees_bad_gateway() {
        let paths = vec![
            "/assets/app.js?ver=1".to_string(),
            "/assets/app.css?ver=1".to_string(),
        ];
        let (origin, local_origin_count) =
            start_fanout_fixture_server(paths.clone(), paths.len(), 502);

        let result = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: origin.clone(),
            public_origin: Some(origin),
            command: None,
            require_https: false,
            provider: Some("homeboy-native-fixture".to_string()),
            startup_timeout_seconds: None,
            required_asset_paths: Vec::new(),
            asset_fanout: Some(TracePreviewAssetFanoutSpec {
                asset_paths: paths,
                concurrency: Some(2),
                repeat_count: Some(1),
                expected_body_contains: None,
            }),
            native: None,
        });

        let error = match result {
            Ok(_) => panic!("asset fanout should fail before trace starts"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains("asset fanout failed before trace collection"));
        assert!(message.contains("failure_buckets=[http_502=2]"));
        assert_eq!(local_origin_count.load(Ordering::SeqCst), 2);
    }

    fn start_asset_fixture_server(routes: Vec<(String, u16)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        let addr = listener.local_addr().expect("fixture server addr");
        thread::spawn(move || {
            for stream in listener.incoming().take(routes.len()) {
                let mut stream = stream.expect("fixture connection");
                let mut buffer = [0; 1024];
                let len = stream.read(&mut buffer).expect("read request");
                let request = String::from_utf8_lossy(&buffer[..len]);
                let target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let status = routes
                    .iter()
                    .find(|(path, _)| path == target)
                    .map(|(_, status)| *status)
                    .unwrap_or(404);
                let reason = if status == 200 { "OK" } else { "Bad Gateway" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
        });
        format!("http://{addr}")
    }

    fn start_fanout_fixture_server(
        paths: Vec<String>,
        expected_requests: usize,
        status: u16,
    ) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fanout fixture server");
        let addr = listener.local_addr().expect("fanout fixture server addr");
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_request_count = Arc::clone(&request_count);
        thread::spawn(move || {
            for stream in listener.incoming().take(expected_requests) {
                let mut stream = stream.expect("fixture connection");
                server_request_count.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0; 2048];
                let len = stream.read(&mut buffer).expect("read request");
                let request = String::from_utf8_lossy(&buffer[..len]);
                let target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let route_status = if paths.iter().any(|path| path == target) {
                    status
                } else {
                    404
                };
                let reason = if route_status == 200 {
                    "OK"
                } else {
                    "Bad Gateway"
                };
                let body = "homeboy-fanout-ok";
                let response = format!(
                    "HTTP/1.1 {route_status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
        });
        (format!("http://{addr}"), request_count)
    }
}
