use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::core::error::{Error, Result};
use crate::core::rig::{
    TraceNativePublicPreviewSpec, TracePublicPreviewMode, TracePublicPreviewSpec,
};

const DEFAULT_STARTUP_TIMEOUT_SECONDS: u64 = 20;
const DEFAULT_ASSET_CHECK_TIMEOUT_SECONDS: u64 = 10;
const WC_STRIPE_ECE_PREVIEW_PORT_SETTING: &str = "woocommerce_stripe_ece_preview_port";
const WC_STRIPE_ECE_PREVIEW_PORT_ENV: &str = "HOMEBOY_WC_STRIPE_ECE_PREVIEW_PORT";

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

        if let Err(error) = validate_preview_port_alignment(spec, &public_origin) {
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

fn validate_preview_port_alignment(
    spec: &TracePublicPreviewSpec,
    public_origin: &str,
) -> Result<()> {
    let Some(expected_port) = expected_local_tunnel_port(public_origin) else {
        return Ok(());
    };
    let local_port = local_origin_port(&spec.local_origin);
    if local_port == Some(expected_port) {
        return Ok(());
    }

    let suggested_setting =
        format!("--setting {WC_STRIPE_ECE_PREVIEW_PORT_SETTING}={expected_port}");
    let suggested_env = format!("{WC_STRIPE_ECE_PREVIEW_PORT_ENV}={expected_port}");
    Err(Error::validation_invalid_argument(
        "public_preview.local_origin",
        format!(
            "public preview URL routes to local port {expected_port}, but runtime local preview origin is `{}`. requested_public_url=`{public_origin}` runtime_local_preview_origin=`{}` expected_local_tunnel_port={expected_port} suggested_setting=`{suggested_setting}` suggested_env=`{suggested_env}`",
            spec.local_origin, spec.local_origin
        ),
        Some(public_origin.to_string()),
        Some(vec![suggested_setting, suggested_env]),
    ))
}

fn expected_local_tunnel_port(public_origin: &str) -> Option<u16> {
    let after_scheme = public_origin.split_once("://")?.1;
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme)
        .split('@')
        .next_back()
        .unwrap_or(after_scheme);
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    let prefix = host.split_once("-tunnel.")?.0;
    let digits = prefix.rsplit('-').next().unwrap_or(prefix);
    digits.parse::<u16>().ok().filter(|port| *port > 0)
}

fn local_origin_port(local_origin: &str) -> Option<u16> {
    let after_scheme = local_origin.split_once("://")?.1;
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme)
        .split('@')
        .next_back()
        .unwrap_or(after_scheme);
    authority.rsplit_once(':')?.1.parse::<u16>().ok()
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
    use super::{expected_local_tunnel_port, first_https_origin, TracePublicPreviewSession};
    use crate::core::rig::{
        TraceNativePublicPreviewSpec, TracePublicPreviewMode, TracePublicPreviewSpec,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
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
    fn extracts_expected_port_from_kimaki_tunnel_origin() {
        assert_eq!(
            expected_local_tunnel_port("https://site-49822-tunnel.kimaki.dev/"),
            Some(49822)
        );
        assert_eq!(
            expected_local_tunnel_port("https://49822-tunnel.kimaki.dev"),
            Some(49822)
        );
        assert_eq!(
            expected_local_tunnel_port("https://preview.example.test/"),
            None
        );
    }

    #[test]
    fn fails_fast_when_public_tunnel_port_does_not_match_local_origin() {
        let result = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: "http://127.0.0.1:20000".to_string(),
            public_origin: Some("https://site-49822-tunnel.kimaki.dev/".to_string()),
            command: None,
            require_https: true,
            provider: Some("fixture".to_string()),
            startup_timeout_seconds: None,
            required_asset_paths: Vec::new(),
            native: None,
        });
        let error = match result {
            Ok(_) => panic!("mismatched preview port should fail before trace starts"),
            Err(error) => error,
        };

        let message = error.to_string();
        assert!(message.contains("requested_public_url=`https://site-49822-tunnel.kimaki.dev/`"));
        assert!(message.contains("runtime_local_preview_origin=`http://127.0.0.1:20000`"));
        assert!(message.contains("expected_local_tunnel_port=49822"));
        assert!(message.contains("--setting woocommerce_stripe_ece_preview_port=49822"));
        assert!(message.contains("HOMEBOY_WC_STRIPE_ECE_PREVIEW_PORT=49822"));
    }

    #[test]
    fn records_successful_required_asset_preflight() {
        let origin = start_asset_fixture_server(vec![(
            "/wp-content/plugins/example/build/frontend.js?ver=1".to_string(),
            200,
        )]);

        let session = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: origin.clone(),
            public_origin: Some(origin),
            command: None,
            require_https: false,
            provider: Some("fixture".to_string()),
            startup_timeout_seconds: None,
            required_asset_paths: vec![
                "/wp-content/plugins/example/build/frontend.js?ver=1".to_string()
            ],
            native: None,
        })
        .expect("preview starts after asset preflight");

        assert_eq!(session.metadata().required_assets.len(), 1);
        assert!(session.metadata().required_assets[0].ok);
        assert_eq!(session.metadata().required_assets[0].status, Some(200));
    }

    #[test]
    fn fails_fast_when_required_asset_cannot_be_fetched() {
        let origin = start_asset_fixture_server(vec![(
            "/wp-content/plugins/example/build/frontend.js?ver=1".to_string(),
            502,
        )]);

        let result = TracePublicPreviewSession::start(&TracePublicPreviewSpec {
            mode: TracePublicPreviewMode::External,
            local_origin: origin.clone(),
            public_origin: Some(origin),
            command: None,
            require_https: false,
            provider: Some("fixture".to_string()),
            startup_timeout_seconds: None,
            required_asset_paths: vec![
                "/wp-content/plugins/example/build/frontend.js?ver=1".to_string()
            ],
            native: None,
        });

        let error = match result {
            Ok(_) => panic!("required asset preflight should fail before trace starts"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains("required asset preflight failed before trace collection"));
        assert!(message.contains("/wp-content/plugins/example/build/frontend.js?ver=1 -> HTTP 502"));
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
}
