use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::error::{Error, Result};
use crate::core::paths;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreviewIngressRoute {
    pub session_id: String,
    pub public_host: String,
    pub upstream_origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressStatus {
    pub bind: Option<String>,
    pub domain: Option<String>,
    pub public_host_pattern: Option<String>,
    pub routes: Vec<PreviewIngressRouteStatus>,
    pub recent_failures: Vec<PreviewIngressFailure>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressRouteStatus {
    #[serde(flatten)]
    pub route: PreviewIngressRoute,
    pub lifecycle: PreviewIngressRouteLifecycle,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewIngressRouteLifecycle {
    Active,
    Expired,
    Disconnected,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressFailure {
    pub request_id: String,
    pub host: String,
    pub path: String,
    pub status: u16,
    pub classification: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct PreviewIngressServeSpec {
    pub bind: String,
    pub domain: String,
    pub public_host_pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewIngressInstallOptions {
    pub server_id: String,
    pub domain: String,
    pub public_host_pattern: String,
    pub bind: String,
    pub binary_path: String,
    pub service_name: String,
    pub service_user: String,
    pub service_group: String,
}

impl Default for PreviewIngressInstallOptions {
    fn default() -> Self {
        Self {
            server_id: String::new(),
            domain: String::new(),
            public_host_pattern: String::new(),
            bind: "127.0.0.1:7350".to_string(),
            binary_path: "/usr/local/bin/homeboy".to_string(),
            service_name: "homeboy-preview-ingress".to_string(),
            service_user: "homeboy".to_string(),
            service_group: "homeboy".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressInstallPlan {
    pub command: String,
    pub server_id: String,
    pub domain: String,
    pub public_host_pattern: String,
    pub dns_probe_host: String,
    pub bind: String,
    pub service_name: String,
    pub service_user: String,
    pub service_group: String,
    pub binary_path: String,
    pub local_status_url: String,
    pub public_status_url: String,
    pub dry_run: bool,
    pub applied: bool,
    pub writes: Vec<PreviewIngressWrite>,
    pub systemd_unit: String,
    pub nginx_site: String,
    pub caddy_site: String,
    pub install_commands: Vec<String>,
    pub status_commands: Vec<String>,
    pub rollback_commands: Vec<String>,
    pub smoke_checks: Vec<String>,
    pub required_operator_config: Vec<String>,
    pub secrets_policy: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressWrite {
    pub path: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressInstallStatusPlan {
    pub command: String,
    pub server_id: String,
    pub domain: String,
    pub public_host_pattern: String,
    pub dns_probe_host: String,
    pub bind: String,
    pub service_name: String,
    pub local_status_url: String,
    pub public_status_url: String,
    pub probed: bool,
    pub checks: Vec<PreviewIngressInstallCheck>,
    pub status_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewIngressInstallCheck {
    pub name: String,
    pub command: String,
    pub status: PreviewIngressInstallCheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreviewIngressInstallCheckStatus {
    Planned,
    Passed,
    Failed,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct PreviewIngressLogLine {
    request_id: String,
    host: String,
    path: String,
    status: u16,
    bytes: usize,
    duration_ms: u128,
    classification: String,
}

fn default_true() -> bool {
    true
}

pub fn register_route(route: PreviewIngressRoute) -> Result<PreviewIngressRoute> {
    validate_route(&route)?;
    let path = paths::preview_ingress_route_file(&route.session_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::internal_io(e.to_string(), Some(parent.display().to_string())))?;
    }
    let data = serde_json::to_string_pretty(&route)
        .map_err(|e| Error::internal_json(e.to_string(), Some(route.session_id.clone())))?;
    fs::write(&path, data)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    load_route(&route.session_id)
}

pub fn remove_route(session_id: &str) -> Result<()> {
    let path = paths::preview_ingress_route_file(session_id)?;
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    }
    Ok(())
}

pub fn load_route(session_id: &str) -> Result<PreviewIngressRoute> {
    let path = paths::preview_ingress_route_file(session_id)?;
    let data = fs::read_to_string(&path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&data)
        .map_err(|e| Error::internal_json(e.to_string(), Some(path.display().to_string())))
}

pub fn list_routes() -> Result<Vec<PreviewIngressRoute>> {
    let dir = paths::preview_ingress_routes_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut routes: Vec<PreviewIngressRoute> = Vec::new();
    for entry in fs::read_dir(&dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some(dir.display().to_string())))?
    {
        let entry = entry.map_err(|e| Error::internal_io(e.to_string(), None))?;
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let data = fs::read_to_string(entry.path()).map_err(|e| {
            Error::internal_io(e.to_string(), Some(entry.path().display().to_string()))
        })?;
        routes.push(serde_json::from_str(&data).map_err(|e| {
            Error::internal_json(e.to_string(), Some(entry.path().display().to_string()))
        })?);
    }
    routes.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    Ok(routes)
}

pub fn status(
    bind: Option<String>,
    domain: Option<String>,
    public_host_pattern: Option<String>,
) -> Result<PreviewIngressStatus> {
    status_with_failures(bind, domain, public_host_pattern, Vec::new())
}

pub fn render_install_plan(
    options: PreviewIngressInstallOptions,
) -> Result<PreviewIngressInstallPlan> {
    let normalized = normalize_install_options(options)?;
    let dns_probe_host = dns_probe_host(&normalized.public_host_pattern, &normalized.domain);
    let local_status_url = format!("http://{}/_homeboy/preview-ingress/status", normalized.bind);
    let public_status_url = format!("https://{}/_homeboy/preview-ingress/status", dns_probe_host);

    Ok(PreviewIngressInstallPlan {
        command: "tunnel.preview_ingress.install".to_string(),
        server_id: normalized.server_id.clone(),
        domain: normalized.domain.clone(),
        public_host_pattern: normalized.public_host_pattern.clone(),
        dns_probe_host: dns_probe_host.clone(),
        bind: normalized.bind.clone(),
        service_name: normalized.service_name.clone(),
        service_user: normalized.service_user.clone(),
        service_group: normalized.service_group.clone(),
        binary_path: normalized.binary_path.clone(),
        local_status_url: local_status_url.clone(),
        public_status_url: public_status_url.clone(),
        dry_run: true,
        applied: false,
        writes: vec![
            PreviewIngressWrite {
                path: format!("/etc/systemd/system/{}.service", normalized.service_name),
                description: "systemd unit for the Homeboy preview ingress daemon".to_string(),
            },
            PreviewIngressWrite {
                path: format!("/etc/nginx/sites-available/{}", normalized.service_name),
                description: "optional Nginx reverse proxy snippet for the wildcard preview host"
                    .to_string(),
            },
            PreviewIngressWrite {
                path: format!("/etc/caddy/sites/{}.caddy", normalized.service_name),
                description: "optional Caddy reverse proxy snippet for the wildcard preview host"
                    .to_string(),
            },
        ],
        systemd_unit: render_systemd_unit(&normalized),
        nginx_site: render_nginx_site(&normalized),
        caddy_site: render_caddy_site(&normalized),
        install_commands: install_commands(&normalized),
        status_commands: install_status_commands(&normalized, &dns_probe_host),
        rollback_commands: rollback_commands(&normalized),
        smoke_checks: vec![
            format!("getent hosts {dns_probe_host}"),
            format!("curl -fsS {local_status_url}"),
            format!("curl -fsS {public_status_url}"),
        ],
        required_operator_config: required_operator_config(&normalized),
        secrets_policy: vec![
            "Do not put token material in the systemd unit or proxy snippets.".to_string(),
            "Store ingress pairing/client secrets through Homeboy secret/config surfaces before enabling live routes.".to_string(),
            "This generated plan contains only non-secret operator configuration.".to_string(),
        ],
    })
}

pub fn render_install_status_plan(
    options: PreviewIngressInstallOptions,
) -> Result<PreviewIngressInstallStatusPlan> {
    let normalized = normalize_install_options(options)?;
    let dns_probe_host = dns_probe_host(&normalized.public_host_pattern, &normalized.domain);
    let local_status_url = format!("http://{}/_homeboy/preview-ingress/status", normalized.bind);
    let public_status_url = format!("https://{}/_homeboy/preview-ingress/status", dns_probe_host);
    let commands = install_status_commands(&normalized, &dns_probe_host);

    Ok(PreviewIngressInstallStatusPlan {
        command: "tunnel.preview_ingress.install_status".to_string(),
        server_id: normalized.server_id,
        domain: normalized.domain,
        public_host_pattern: normalized.public_host_pattern,
        dns_probe_host,
        bind: normalized.bind,
        service_name: normalized.service_name,
        local_status_url,
        public_status_url,
        probed: false,
        checks: commands
            .iter()
            .map(|command| PreviewIngressInstallCheck {
                name: install_check_name(command),
                command: command.clone(),
                status: PreviewIngressInstallCheckStatus::Planned,
                exit_code: None,
                stdout: None,
                stderr: None,
            })
            .collect(),
        status_commands: commands,
    })
}

fn status_with_failures(
    bind: Option<String>,
    domain: Option<String>,
    public_host_pattern: Option<String>,
    recent_failures: Vec<PreviewIngressFailure>,
) -> Result<PreviewIngressStatus> {
    Ok(PreviewIngressStatus {
        bind,
        domain,
        public_host_pattern,
        routes: list_routes()?
            .into_iter()
            .map(|route| PreviewIngressRouteStatus {
                lifecycle: classify_route(&route),
                route,
            })
            .collect(),
        recent_failures,
    })
}

pub fn serve(spec: PreviewIngressServeSpec) -> Result<PreviewIngressStatus> {
    validate_serve_spec(&spec)?;
    let listener = TcpListener::bind(&spec.bind)
        .map_err(|e| Error::internal_io(e.to_string(), Some(spec.bind.clone())))?;
    eprintln!(
        "homeboy preview ingress listening on {} for {} ({})",
        spec.bind, spec.domain, spec.public_host_pattern
    );

    let recent_failures = Arc::new(Mutex::new(Vec::<PreviewIngressFailure>::new()));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::internal_unexpected(e.to_string()))?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let client = client.clone();
                let recent_failures = Arc::clone(&recent_failures);
                thread::spawn(move || {
                    if let Err(error) = handle_connection(stream, client, recent_failures) {
                        eprintln!(
                            "homeboy preview ingress connection error: {}",
                            error.message
                        );
                    }
                });
            }
            Err(error) => {
                return Err(Error::internal_io(error.to_string(), Some(spec.bind)));
            }
        }
    }

    status(
        Some(spec.bind),
        Some(spec.domain),
        Some(spec.public_host_pattern),
    )
}

fn handle_connection(
    mut stream: TcpStream,
    client: reqwest::blocking::Client,
    recent_failures: Arc<Mutex<Vec<PreviewIngressFailure>>>,
) -> Result<()> {
    let started = Instant::now();
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("clone preview ingress stream".to_string()),
        )
    })?);
    let request = read_http_request(&mut reader)?;
    let host = request.host.clone().unwrap_or_default();
    let path = request.target.clone();

    if request.target == "/_homeboy/preview-ingress/status" {
        let failures = recent_failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let body = serde_json::to_vec_pretty(&status_with_failures(None, None, None, failures)?)
            .map_err(|e| {
                Error::internal_json(e.to_string(), Some("preview ingress status".to_string()))
            })?;
        write_response(
            &mut stream,
            200,
            "OK",
            &[(&"content-type".to_string(), "application/json".to_string())],
            &body,
        )?;
        log_request(&PreviewIngressLogLine {
            request_id,
            host,
            path,
            status: 200,
            bytes: body.len(),
            duration_ms: started.elapsed().as_millis(),
            classification: "status".to_string(),
        });
        return Ok(());
    }

    let Some(route) = route_for_host(&host)? else {
        let failure = PreviewIngressFailure {
            request_id: request_id.clone(),
            host: host.clone(),
            path: path.clone(),
            status: 404,
            classification: "missing_session".to_string(),
            message: "No active Homeboy preview ingress route matches this host".to_string(),
        };
        record_failure(&recent_failures, failure.clone());
        return write_diagnostic(&mut stream, &failure, started);
    };

    match classify_route(&route) {
        PreviewIngressRouteLifecycle::Expired => {
            let failure = PreviewIngressFailure {
                request_id: request_id.clone(),
                host: host.clone(),
                path: path.clone(),
                status: 410,
                classification: "expired_session".to_string(),
                message: "Homeboy preview ingress route is expired".to_string(),
            };
            record_failure(&recent_failures, failure.clone());
            write_diagnostic(&mut stream, &failure, started)
        }
        PreviewIngressRouteLifecycle::Disconnected => {
            let failure = PreviewIngressFailure {
                request_id: request_id.clone(),
                host: host.clone(),
                path: path.clone(),
                status: 410,
                classification: "disconnected_session".to_string(),
                message: "Homeboy preview ingress route is disconnected".to_string(),
            };
            record_failure(&recent_failures, failure.clone());
            write_diagnostic(&mut stream, &failure, started)
        }
        PreviewIngressRouteLifecycle::Active => proxy_request(
            &mut stream,
            &client,
            &route,
            request,
            request_id,
            host,
            path,
            started,
            recent_failures,
        ),
    }
}

struct IngressHttpRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    host: Option<String>,
    body: Vec<u8>,
}

fn read_http_request(reader: &mut BufReader<TcpStream>) -> Result<IngressHttpRequest> {
    let mut first_line = String::new();
    reader
        .read_line(&mut first_line)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read request line".to_string())))?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    if method.is_empty() {
        return Err(Error::validation_invalid_argument(
            "request",
            "HTTP request line is empty",
            None,
            None,
        ));
    }

    let mut headers = Vec::new();
    let mut host = None;
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read headers".to_string())))?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "host" {
                host = Some(
                    value
                        .split(':')
                        .next()
                        .unwrap_or_default()
                        .to_ascii_lowercase(),
                );
            }
            if name == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }

    let mut body = vec![0; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read body".to_string())))?;
    }

    Ok(IngressHttpRequest {
        method,
        target,
        headers,
        host,
        body,
    })
}

#[allow(clippy::too_many_arguments)]
fn proxy_request(
    stream: &mut TcpStream,
    client: &reqwest::blocking::Client,
    route: &PreviewIngressRoute,
    request: IngressHttpRequest,
    request_id: String,
    host: String,
    path: String,
    started: Instant,
    recent_failures: Arc<Mutex<Vec<PreviewIngressFailure>>>,
) -> Result<()> {
    let upstream_url = upstream_url(route, &request.target)?;
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|e| Error::validation_invalid_argument("method", e.to_string(), None, None))?;
    let mut upstream = client.request(method, upstream_url);
    for (name, value) in request.headers {
        if is_hop_by_hop_header(&name) || name == "host" || name == "content-length" {
            continue;
        }
        upstream = upstream.header(&name, value);
    }
    if !request.body.is_empty() {
        upstream = upstream.body(request.body);
    }

    match upstream.send() {
        Ok(mut response) => {
            let status = response.status();
            let headers = response
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    let name = name.as_str().to_ascii_lowercase();
                    if is_hop_by_hop_header(&name) {
                        return None;
                    }
                    value.to_str().ok().map(|value| (name, value.to_string()))
                })
                .collect::<Vec<_>>();
            write_status_and_headers(
                stream,
                status.as_u16(),
                status.canonical_reason().unwrap_or("OK"),
                &headers,
            )?;
            let bytes = response.copy_to(stream).map_err(|e| {
                Error::internal_io(e.to_string(), Some("stream upstream response".to_string()))
            })? as usize;
            log_request(&PreviewIngressLogLine {
                request_id,
                host,
                path,
                status: status.as_u16(),
                bytes,
                duration_ms: started.elapsed().as_millis(),
                classification: "proxied".to_string(),
            });
            Ok(())
        }
        Err(error) => {
            let timeout = error.is_timeout();
            let failure = PreviewIngressFailure {
                request_id: request_id.clone(),
                host: host.clone(),
                path: path.clone(),
                status: if timeout { 504 } else { 502 },
                classification: if timeout {
                    "upstream_timeout"
                } else {
                    "upstream_error"
                }
                .to_string(),
                message: error.to_string(),
            };
            record_failure(&recent_failures, failure.clone());
            write_diagnostic(stream, &failure, started)
        }
    }
}

fn route_for_host(host: &str) -> Result<Option<PreviewIngressRoute>> {
    Ok(list_routes()?.into_iter().find(|route| {
        route.public_host.eq_ignore_ascii_case(host)
            || route
                .public_host
                .split(':')
                .next()
                .is_some_and(|public_host| public_host.eq_ignore_ascii_case(host))
    }))
}

fn classify_route(route: &PreviewIngressRoute) -> PreviewIngressRouteLifecycle {
    if !route.active {
        return PreviewIngressRouteLifecycle::Disconnected;
    }
    if route
        .expires_at
        .as_deref()
        .and_then(parse_rfc3339_utc)
        .is_some_and(|expires_at| chrono::Utc::now() > expires_at)
    {
        return PreviewIngressRouteLifecycle::Expired;
    }
    PreviewIngressRouteLifecycle::Active
}

fn validate_route(route: &PreviewIngressRoute) -> Result<()> {
    if route.session_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "session_id",
            "preview ingress session ID is required",
            None,
            None,
        ));
    }
    if route.public_host.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "public_host",
            "preview ingress public host is required",
            Some(route.session_id.clone()),
            None,
        ));
    }
    if !(route.upstream_origin.starts_with("http://")
        || route.upstream_origin.starts_with("https://"))
    {
        return Err(Error::validation_invalid_argument(
            "upstream_origin",
            "upstream origin must be an http:// or https:// URL",
            Some(route.session_id.clone()),
            None,
        ));
    }
    if route
        .expires_at
        .as_deref()
        .is_some_and(|expires_at| parse_rfc3339_utc(expires_at).is_none())
    {
        return Err(Error::validation_invalid_argument(
            "expires_at",
            "preview ingress expiry must be a valid RFC3339 timestamp",
            Some(route.session_id.clone()),
            None,
        ));
    }
    Ok(())
}

fn validate_serve_spec(spec: &PreviewIngressServeSpec) -> Result<()> {
    if spec.bind.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "bind",
            "bind address is required",
            None,
            None,
        ));
    }
    if spec.domain.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "domain",
            "operator domain is required",
            None,
            None,
        ));
    }
    if spec.public_host_pattern.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "public_host_pattern",
            "public host pattern is required",
            None,
            None,
        ));
    }
    Ok(())
}

fn normalize_install_options(
    mut options: PreviewIngressInstallOptions,
) -> Result<PreviewIngressInstallOptions> {
    options.domain = trim_scheme(&options.domain)
        .trim_end_matches('/')
        .to_string();
    options.public_host_pattern = options.public_host_pattern.trim().to_string();

    if options.server_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "server",
            "preview ingress install requires a configured Homeboy server id",
            None,
            Some(vec![
                "Create one with: homeboy server create <id> --host <host> --user <user>"
                    .to_string(),
            ]),
        ));
    }
    if options.domain.is_empty() || options.domain.contains('/') {
        return Err(Error::validation_invalid_argument(
            "domain",
            "domain must be a bare operator domain such as example.com",
            Some(options.domain),
            None,
        ));
    }
    if !options.public_host_pattern.contains(&options.domain) {
        return Err(Error::validation_invalid_argument(
            "public_host_pattern",
            "public host pattern must include the operator domain",
            Some(options.public_host_pattern),
            None,
        ));
    }
    if !options.public_host_pattern.contains('*') {
        return Err(Error::validation_invalid_argument(
            "public_host_pattern",
            "public host pattern must include a wildcard label for preview routes",
            Some(options.public_host_pattern),
            Some(vec![format!(
                "Use a value like '*-tunnel.{}'",
                options.domain
            )]),
        ));
    }
    if !options.bind.starts_with("127.") && !options.bind.starts_with("[::1]") {
        return Err(Error::validation_invalid_argument(
            "bind",
            "preview ingress should bind to loopback and be exposed by the reverse proxy",
            Some(options.bind),
            Some(vec!["Use a value like 127.0.0.1:7350".to_string()]),
        ));
    }
    Ok(options)
}

fn trim_scheme(value: &str) -> &str {
    value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .unwrap_or(value)
}

fn dns_probe_host(public_host_pattern: &str, domain: &str) -> String {
    let host = public_host_pattern
        .replace('*', "homeboy-health")
        .replace("..", ".")
        .trim_end_matches('.')
        .trim_start_matches('.')
        .to_string();
    if host.is_empty() {
        format!("homeboy-health.{domain}")
    } else {
        host
    }
}

fn render_systemd_unit(options: &PreviewIngressInstallOptions) -> String {
    format!(
        r#"[Unit]
Description=Homeboy preview ingress
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User={user}
Group={group}
Environment=HOME=/var/lib/homeboy
Environment=XDG_DATA_HOME=/var/lib/homeboy/.local/share
ExecStart={binary} tunnel preview-ingress serve --bind {bind} --domain {domain} --public-host-pattern '{public_host_pattern}'
Restart=on-failure
RestartSec=5s
StateDirectory=homeboy
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=read-only

[Install]
WantedBy=multi-user.target
"#,
        user = options.service_user,
        group = options.service_group,
        binary = options.binary_path,
        bind = options.bind,
        domain = options.domain,
        public_host_pattern = options.public_host_pattern,
    )
}

fn render_nginx_site(options: &PreviewIngressInstallOptions) -> String {
    format!(
        r#"server {{
    listen 443 ssl http2;
    server_name {public_host_pattern};

    location / {{
        proxy_pass http://{bind};
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }}
}}
"#,
        public_host_pattern = options.public_host_pattern,
        bind = options.bind,
    )
}

fn render_caddy_site(options: &PreviewIngressInstallOptions) -> String {
    format!(
        r#"{public_host_pattern} {{
    reverse_proxy http://{bind}
}}
"#,
        public_host_pattern = options.public_host_pattern,
        bind = options.bind,
    )
}

fn install_commands(options: &PreviewIngressInstallOptions) -> Vec<String> {
    vec![
        "sudo install -d -m 0755 /etc/systemd/system".to_string(),
        format!(
            "sudo install -m 0644 <rendered-unit> /etc/systemd/system/{}.service",
            options.service_name
        ),
        "sudo systemctl daemon-reload".to_string(),
        format!("sudo systemctl enable --now {}", options.service_name),
        "install either the rendered Nginx or Caddy reverse proxy snippet, then reload that proxy"
            .to_string(),
    ]
}

fn install_status_commands(
    options: &PreviewIngressInstallOptions,
    dns_probe_host: &str,
) -> Vec<String> {
    vec![
        format!("systemctl is-active {}", options.service_name),
        format!("systemctl status {} --no-pager", options.service_name),
        format!(
            "curl -fsS http://{}/_homeboy/preview-ingress/status",
            options.bind
        ),
        format!("getent hosts {dns_probe_host}"),
        format!(
            "curl -fsS https://{}/_homeboy/preview-ingress/status",
            dns_probe_host
        ),
    ]
}

fn rollback_commands(options: &PreviewIngressInstallOptions) -> Vec<String> {
    vec![
        format!("sudo systemctl disable --now {}", options.service_name),
        format!(
            "sudo rm -f /etc/systemd/system/{}.service",
            options.service_name
        ),
        "sudo systemctl daemon-reload".to_string(),
        "remove the installed Nginx/Caddy site snippet and reload the proxy".to_string(),
    ]
}

fn required_operator_config(options: &PreviewIngressInstallOptions) -> Vec<String> {
    vec![
        format!(
            "Homeboy server `{}` with SSH access to the target VPS",
            options.server_id
        ),
        format!(
            "Wildcard DNS for `{}` pointing at the VPS public address",
            options.public_host_pattern
        ),
        "TLS certificate coverage for the wildcard preview host pattern".to_string(),
        format!(
            "A Homeboy binary installed at `{}` on the VPS",
            options.binary_path
        ),
        format!(
            "System user/group `{}`/`{}` available on the VPS",
            options.service_user, options.service_group
        ),
        "A proxy choice: install the rendered Nginx or Caddy snippet, not both".to_string(),
    ]
}

fn install_check_name(command: &str) -> String {
    if command.starts_with("systemctl is-active") {
        "systemd_active".to_string()
    } else if command.starts_with("systemctl status") {
        "systemd_status".to_string()
    } else if command.starts_with("curl -fsS http://") {
        "local_status".to_string()
    } else if command.starts_with("getent hosts") {
        "dns".to_string()
    } else if command.starts_with("curl -fsS https://") {
        "public_status".to_string()
    } else {
        "command".to_string()
    }
}

fn upstream_url(route: &PreviewIngressRoute, target: &str) -> Result<String> {
    let base = route.upstream_origin.trim_end_matches('/');
    let target = if target.starts_with('/') {
        target.to_string()
    } else {
        format!("/{target}")
    };
    Ok(format!("{base}{target}"))
}

fn parse_rfc3339_utc(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|datetime| datetime.with_timezone(&chrono::Utc))
}

fn write_diagnostic(
    stream: &mut TcpStream,
    failure: &PreviewIngressFailure,
    started: Instant,
) -> Result<()> {
    let body = serde_json::to_vec_pretty(failure).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("preview ingress diagnostic".to_string()),
        )
    })?;
    write_response(
        stream,
        failure.status,
        reason_for_status(failure.status),
        &[(&"content-type".to_string(), "application/json".to_string())],
        &body,
    )?;
    log_request(&PreviewIngressLogLine {
        request_id: failure.request_id.clone(),
        host: failure.host.clone(),
        path: failure.path.clone(),
        status: failure.status,
        bytes: body.len(),
        duration_ms: started.elapsed().as_millis(),
        classification: failure.classification.clone(),
    });
    Ok(())
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&String, String)],
    body: &[u8],
) -> Result<()> {
    let owned_headers = headers
        .iter()
        .map(|(name, value)| ((*name).clone(), value.clone()))
        .collect::<Vec<_>>();
    write_status_and_headers(stream, status, reason, &owned_headers)?;
    stream
        .write_all(body)
        .map_err(|e| Error::internal_io(e.to_string(), Some("write response body".to_string())))
}

fn write_status_and_headers(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(String, String)],
) -> Result<()> {
    write!(stream, "HTTP/1.1 {} {}\r\n", status, reason)
        .map_err(|e| Error::internal_io(e.to_string(), Some("write status".to_string())))?;
    let has_connection = headers.iter().any(|(name, _)| name == "connection");
    for (name, value) in headers {
        write!(stream, "{}: {}\r\n", name, value)
            .map_err(|e| Error::internal_io(e.to_string(), Some("write header".to_string())))?;
    }
    if !has_connection {
        write!(stream, "connection: close\r\n").map_err(|e| {
            Error::internal_io(e.to_string(), Some("write connection header".to_string()))
        })?;
    }
    write!(stream, "\r\n")
        .map_err(|e| Error::internal_io(e.to_string(), Some("write header terminator".to_string())))
}

fn reason_for_status(status: u16) -> &'static str {
    match status {
        404 => "Not Found",
        410 => "Gone",
        502 => "Bad Gateway",
        504 => "Gateway Timeout",
        _ => "OK",
    }
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn record_failure(
    recent_failures: &Arc<Mutex<Vec<PreviewIngressFailure>>>,
    failure: PreviewIngressFailure,
) {
    let mut failures = recent_failures
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    failures.push(failure);
    if failures.len() > 50 {
        failures.remove(0);
    }
}

fn log_request(line: &PreviewIngressLogLine) {
    match serde_json::to_string(line) {
        Ok(line) => eprintln!("{}", line),
        Err(_) => eprintln!("preview ingress request log serialization failed"),
    }
}
