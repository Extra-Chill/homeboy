use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::preview_client::{PreviewIngressRequest, PreviewIngressResponse};

use crate::core::error::{Error, Result};
use crate::core::paths;
use crate::core::plan::{
    HomeboyPlan, PlanArtifact, PlanKind, PlanStep, PlanStepStatus, PlanValues,
};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inspected_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inspected_state: Option<String>,
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
    pub token_sha256_env: String,
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

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PreviewIngressInstallPlan {
    pub command: String,
    pub plan: HomeboyPlan,
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

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PreviewIngressInstallStatusPlan {
    pub command: String,
    pub plan: HomeboyPlan,
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

#[derive(Debug, Clone)]
struct PreviewClientSession {
    local_origin: String,
    pending: VecDeque<PreviewIngressRequest>,
    responses: HashMap<String, PreviewIngressResponse>,
    active: bool,
}

#[derive(Debug, Default)]
struct PreviewClientSessions {
    sessions: Mutex<HashMap<String, PreviewClientSession>>,
    changed: Condvar,
}

#[derive(Debug, Clone)]
struct PreviewIngressAuth {
    token_sha256_env: String,
    token_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PreviewRegisterRequest {
    public_host: String,
    local_origin: String,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PreviewNextRequest {
    public_host: String,
    #[serde(default)]
    timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
struct PreviewRespondRequest {
    public_host: String,
    response: PreviewIngressResponse,
}

#[derive(Debug, Deserialize)]
struct PreviewCloseRequest {
    public_host: String,
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
    status_for_host(bind, domain, public_host_pattern, None)
}

pub fn status_for_host(
    bind: Option<String>,
    domain: Option<String>,
    public_host_pattern: Option<String>,
    host: Option<String>,
) -> Result<PreviewIngressStatus> {
    let inspected_host = host.map(|host| normalize_public_host(&host));
    let inspected_state = inspected_host.as_ref().map(|host| {
        classify_route_host_state(host).unwrap_or_else(|| "missing_session".to_string())
    });
    status_with_failures(
        bind,
        domain,
        public_host_pattern,
        Vec::new(),
        inspected_host,
        inspected_state,
    )
}

pub fn render_install_plan(
    options: PreviewIngressInstallOptions,
) -> Result<PreviewIngressInstallPlan> {
    let normalized = normalize_install_options(options)?;
    let dns_probe_host = dns_probe_host(&normalized.public_host_pattern, &normalized.domain);
    let local_status_url = format!("http://{}/_homeboy/preview-ingress/status", normalized.bind);
    let public_status_url = format!("https://{}/_homeboy/preview-ingress/status", dns_probe_host);

    let mut install_plan = PreviewIngressInstallPlan {
        command: "tunnel.preview_ingress.install".to_string(),
        plan: HomeboyPlan::default(),
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
    };
    install_plan.plan = install_homeboy_plan(&install_plan);

    Ok(install_plan)
}

pub fn render_install_status_plan(
    options: PreviewIngressInstallOptions,
) -> Result<PreviewIngressInstallStatusPlan> {
    let normalized = normalize_install_options(options)?;
    let dns_probe_host = dns_probe_host(&normalized.public_host_pattern, &normalized.domain);
    let local_status_url = format!("http://{}/_homeboy/preview-ingress/status", normalized.bind);
    let public_status_url = format!("https://{}/_homeboy/preview-ingress/status", dns_probe_host);
    let commands = install_status_commands(&normalized, &dns_probe_host);

    let mut status_plan = PreviewIngressInstallStatusPlan {
        command: "tunnel.preview_ingress.install_status".to_string(),
        plan: HomeboyPlan::default(),
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
    };
    status_plan.plan = install_status_homeboy_plan(&status_plan);

    Ok(status_plan)
}

fn install_homeboy_plan(install: &PreviewIngressInstallPlan) -> HomeboyPlan {
    let mut plan = HomeboyPlan::builder_for_description(
        PlanKind::Custom,
        format!("preview ingress install for {}", install.server_id),
    )
    .mode("preview")
    .inputs(
        PlanValues::new()
            .string("command", &install.command)
            .string("server_id", &install.server_id)
            .string("domain", &install.domain)
            .string("public_host_pattern", &install.public_host_pattern)
            .string("dns_probe_host", &install.dns_probe_host)
            .string("bind", &install.bind)
            .string("service_name", &install.service_name)
            .string("service_user", &install.service_user)
            .string("service_group", &install.service_group)
            .string("binary_path", &install.binary_path),
    )
    .policy_value("would_mutate", serde_json::json!(false))
    .policy_value("dry_run", serde_json::json!(install.dry_run))
    .policy_value("applied", serde_json::json!(install.applied))
    .policy_value("secrets", serde_json::json!(&install.secrets_policy))
    .policy_value(
        "required_operator_config",
        serde_json::json!(&install.required_operator_config),
    )
    .steps(install_plan_steps(install))
    .summarize()
    .build();

    plan.artifacts = install_plan_artifacts(install);
    plan.hints = vec![
        "Preview only: this command renders operator work and does not write remote files."
            .to_string(),
    ];
    plan
}

fn install_status_homeboy_plan(status: &PreviewIngressInstallStatusPlan) -> HomeboyPlan {
    let mut plan = HomeboyPlan::builder_for_description(
        PlanKind::Custom,
        format!("preview ingress install status for {}", status.server_id),
    )
    .mode("preview")
    .inputs(
        PlanValues::new()
            .string("command", &status.command)
            .string("server_id", &status.server_id)
            .string("domain", &status.domain)
            .string("public_host_pattern", &status.public_host_pattern)
            .string("dns_probe_host", &status.dns_probe_host)
            .string("bind", &status.bind)
            .string("service_name", &status.service_name),
    )
    .policy_value("would_mutate", serde_json::json!(false))
    .policy_value("probed", serde_json::json!(status.probed))
    .steps(install_status_plan_steps(status))
    .summarize()
    .build();

    plan.artifacts = vec![plan_artifact(
        "preview_ingress.status_commands",
        None,
        "command_list",
        vec![
            (
                "description",
                serde_json::json!("non-mutating status commands"),
            ),
            ("commands", serde_json::json!(&status.status_commands)),
        ],
    )];
    plan.hints = vec![
        "Status plan only: checks are rendered for operators and are not executed here."
            .to_string(),
    ];
    plan
}

fn install_plan_steps(install: &PreviewIngressInstallPlan) -> Vec<PlanStep> {
    let write_steps = install.writes.iter().enumerate().map(|(index, write)| {
        PlanStep::ready(
            format!("preview_ingress.write.{}", plan_slug(&write.path, index)),
            "preview_ingress.write",
        )
        .label(format!("Plan write: {}", write.description))
        .inputs(
            PlanValues::new()
                .string("path", &write.path)
                .string("description", &write.description),
        )
        .build()
    });

    let grouped_steps = [
        (
            "preview_ingress.install_commands",
            "preview_ingress.command_group",
            "Plan install commands",
            serde_json::json!(&install.install_commands),
        ),
        (
            "preview_ingress.status_commands",
            "preview_ingress.command_group",
            "Plan status commands",
            serde_json::json!(&install.status_commands),
        ),
        (
            "preview_ingress.rollback_commands",
            "preview_ingress.rollback",
            "Plan rollback commands",
            serde_json::json!(&install.rollback_commands),
        ),
        (
            "preview_ingress.smoke_checks",
            "preview_ingress.smoke_check",
            "Plan smoke checks",
            serde_json::json!(&install.smoke_checks),
        ),
        (
            "preview_ingress.operator_config",
            "preview_ingress.operator_config",
            "Plan required operator config",
            serde_json::json!(&install.required_operator_config),
        ),
    ]
    .into_iter()
    .map(|(id, kind, label, values)| {
        PlanStep::ready(id, kind)
            .label(label)
            .inputs(PlanValues::new().json("items", values))
            .build()
    });

    write_steps.chain(grouped_steps).collect()
}

fn install_status_plan_steps(status: &PreviewIngressInstallStatusPlan) -> Vec<PlanStep> {
    status
        .checks
        .iter()
        .enumerate()
        .map(|(index, check)| {
            PlanStep::builder(
                format!(
                    "preview_ingress.status_check.{}",
                    plan_slug(&check.name, index)
                ),
                "preview_ingress.status_check",
                match check.status {
                    PreviewIngressInstallCheckStatus::Planned => PlanStepStatus::Ready,
                    PreviewIngressInstallCheckStatus::Passed => PlanStepStatus::Success,
                    PreviewIngressInstallCheckStatus::Failed => PlanStepStatus::Failed,
                },
            )
            .label(format!("Check {}", check.name))
            .inputs(
                PlanValues::new()
                    .string("name", &check.name)
                    .string("command", &check.command),
            )
            .output_value("exit_code", serde_json::json!(check.exit_code))
            .output_value("stdout", serde_json::json!(check.stdout))
            .output_value("stderr", serde_json::json!(check.stderr))
            .build()
        })
        .collect()
}

fn install_plan_artifacts(install: &PreviewIngressInstallPlan) -> Vec<PlanArtifact> {
    let mut artifacts = vec![
        plan_artifact(
            "preview_ingress.systemd_unit",
            Some(format!(
                "/etc/systemd/system/{}.service",
                install.service_name
            )),
            "systemd_unit",
            vec![
                ("description", serde_json::json!("systemd unit preview")),
                ("content", serde_json::json!(&install.systemd_unit)),
            ],
        ),
        plan_artifact(
            "preview_ingress.nginx_site",
            Some(format!(
                "/etc/nginx/sites-available/{}",
                install.service_name
            )),
            "nginx_site",
            vec![
                ("description", serde_json::json!("Nginx site preview")),
                ("content", serde_json::json!(&install.nginx_site)),
            ],
        ),
        plan_artifact(
            "preview_ingress.caddy_site",
            Some(format!("/etc/caddy/sites/{}.caddy", install.service_name)),
            "caddy_site",
            vec![
                ("description", serde_json::json!("Caddy site preview")),
                ("content", serde_json::json!(&install.caddy_site)),
            ],
        ),
    ];

    artifacts.extend([
        plan_artifact(
            "preview_ingress.install_commands",
            None,
            "command_list",
            vec![("commands", serde_json::json!(&install.install_commands))],
        ),
        plan_artifact(
            "preview_ingress.status_commands",
            None,
            "command_list",
            vec![("commands", serde_json::json!(&install.status_commands))],
        ),
        plan_artifact(
            "preview_ingress.rollback_commands",
            None,
            "command_list",
            vec![("commands", serde_json::json!(&install.rollback_commands))],
        ),
        plan_artifact(
            "preview_ingress.smoke_checks",
            None,
            "smoke_checks",
            vec![("commands", serde_json::json!(&install.smoke_checks))],
        ),
    ]);

    artifacts
}

fn plan_artifact(
    id: impl Into<String>,
    path: Option<String>,
    artifact_type: impl Into<String>,
    data: Vec<(&'static str, serde_json::Value)>,
) -> PlanArtifact {
    PlanArtifact {
        id: id.into(),
        path,
        artifact_type: Some(artifact_type.into()),
        data: data
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    }
}

fn plan_slug(value: &str, fallback: usize) -> String {
    let slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    if slug.is_empty() {
        format!("item-{fallback}")
    } else {
        slug.chars().take(72).collect()
    }
}

fn status_with_failures(
    bind: Option<String>,
    domain: Option<String>,
    public_host_pattern: Option<String>,
    recent_failures: Vec<PreviewIngressFailure>,
    inspected_host: Option<String>,
    inspected_state: Option<String>,
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
        inspected_host,
        inspected_state,
    })
}

pub fn serve(spec: PreviewIngressServeSpec) -> Result<PreviewIngressStatus> {
    validate_serve_spec(&spec)?;
    let listener = TcpListener::bind(&spec.bind)
        .map_err(|e| Error::internal_io(e.to_string(), Some(spec.bind.clone())))?;
    let sessions = Arc::new(PreviewClientSessions::default());
    let auth = Arc::new(PreviewIngressAuth {
        token_sha256_env: spec.token_sha256_env.clone(),
        token_sha256: preview_token_sha256(&spec.token_sha256_env),
    });
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
                let sessions = Arc::clone(&sessions);
                let auth = Arc::clone(&auth);
                thread::spawn(move || {
                    if let Err(error) =
                        handle_connection(stream, client, sessions, auth, recent_failures)
                    {
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
    sessions: Arc<PreviewClientSessions>,
    auth: Arc<PreviewIngressAuth>,
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

    if request.target.split('?').next() == Some("/_homeboy/preview-ingress/status") {
        let failures = recent_failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let inspected_host =
            query_value(&request.target, "host").map(|host| normalize_public_host(&host));
        let inspected_state = inspected_host.as_ref().map(|host| {
            classify_runtime_host_state(host, &sessions, &failures)
                .unwrap_or_else(|| "missing_session".to_string())
        });
        let body = serde_json::to_vec_pretty(&status_with_failures(
            None,
            None,
            None,
            failures,
            inspected_host,
            inspected_state,
        )?)
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

    if request.target.starts_with("/preview/client/") {
        return handle_client_api(&mut stream, request, &sessions, &auth, &recent_failures);
    }

    let Some(route) = route_for_host(&host)? else {
        return proxy_reverse_channel_request(
            &mut stream,
            request,
            request_id,
            host,
            path,
            started,
            sessions,
            recent_failures,
        );
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

fn handle_client_api(
    stream: &mut TcpStream,
    request: IngressHttpRequest,
    sessions: &Arc<PreviewClientSessions>,
    auth: &PreviewIngressAuth,
    recent_failures: &Arc<Mutex<Vec<PreviewIngressFailure>>>,
) -> Result<()> {
    if request.method != "POST" {
        return write_json_response(
            stream,
            405,
            json!({ "error": "method_not_allowed", "message": "preview client endpoints require POST" }),
        );
    }
    if !authorized_preview_client(&request, auth) {
        let failure = PreviewIngressFailure {
            request_id: uuid::Uuid::new_v4().to_string(),
            host: request.host.clone().unwrap_or_default(),
            path: request.target.clone(),
            status: 401,
            classification: "auth_failed_recently".to_string(),
            message: "preview client bearer token is missing or invalid; compare no-newline SHA-256 digests with `homeboy tunnel preview-client diagnose-auth`".to_string(),
        };
        record_failure(recent_failures, failure);
        return write_json_response(
            stream,
            401,
            json!({
                "error": "unauthorized",
                "classification": "auth_failed_recently",
                "message": "preview client bearer token is missing or invalid",
                "hint": "Run `homeboy tunnel preview-client diagnose-auth`; Homeboy hashes exact token bytes (printf %s), never newline-terminated input."
            }),
        );
    }

    match request.target.as_str() {
        "/preview/client/register" => {
            let body: PreviewRegisterRequest = parse_json_body(&request.body, "register")?;
            let public_host = normalize_public_host(&body.public_host);
            validate_client_public_host(&public_host)?;
            validate_client_local_origin(&body.local_origin)?;
            let _session_id = body.session_id.unwrap_or_else(|| public_host.clone());
            let mut sessions_guard = sessions
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sessions_guard.insert(
                public_host,
                PreviewClientSession {
                    local_origin: body.local_origin,
                    pending: VecDeque::new(),
                    responses: HashMap::new(),
                    active: true,
                },
            );
            sessions.changed.notify_all();
            write_json_response(stream, 200, json!({ "registered": true }))
        }
        "/preview/client/next" => {
            let body: PreviewNextRequest = parse_json_body(&request.body, "next")?;
            let public_host = normalize_public_host(&body.public_host);
            let timeout = Duration::from_secs(body.timeout_secs.clamp(1, 60));
            let started = Instant::now();
            let mut sessions_guard = sessions
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if let Some(session) = sessions_guard.get_mut(&public_host) {
                    if !session.active {
                        return write_json_response(
                            stream,
                            410,
                            json!({ "error": "session_closed" }),
                        );
                    }
                    if let Some(request) = session.pending.pop_front() {
                        return write_json_response(stream, 200, json!({ "request": request }));
                    }
                } else {
                    return write_json_response(stream, 404, json!({ "error": "missing_session" }));
                }

                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    return write_json_response(stream, 200, json!({ "request": null }));
                }
                let wait_for = timeout - elapsed;
                let (guard, wait) = sessions
                    .changed
                    .wait_timeout(sessions_guard, wait_for)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                sessions_guard = guard;
                if wait.timed_out() {
                    return write_json_response(stream, 200, json!({ "request": null }));
                }
            }
        }
        "/preview/client/respond" => {
            let body: PreviewRespondRequest = parse_json_body(&request.body, "respond")?;
            let public_host = normalize_public_host(&body.public_host);
            let mut sessions_guard = sessions
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(session) = sessions_guard.get_mut(&public_host) else {
                return write_json_response(stream, 404, json!({ "error": "missing_session" }));
            };
            session
                .responses
                .insert(body.response.request_id.clone(), body.response);
            sessions.changed.notify_all();
            write_json_response(stream, 200, json!({ "accepted": true }))
        }
        "/preview/client/close" => {
            let body: PreviewCloseRequest = parse_json_body(&request.body, "close")?;
            let public_host = normalize_public_host(&body.public_host);
            let mut sessions_guard = sessions
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(session) = sessions_guard.get_mut(&public_host) {
                session.active = false;
            }
            sessions_guard.remove(&public_host);
            sessions.changed.notify_all();
            write_json_response(stream, 200, json!({ "closed": true }))
        }
        _ => write_json_response(stream, 404, json!({ "error": "not_found" })),
    }
}

#[allow(clippy::too_many_arguments)]
fn proxy_reverse_channel_request(
    stream: &mut TcpStream,
    request: IngressHttpRequest,
    request_id: String,
    host: String,
    path: String,
    started: Instant,
    sessions: Arc<PreviewClientSessions>,
    recent_failures: Arc<Mutex<Vec<PreviewIngressFailure>>>,
) -> Result<()> {
    let public_host = normalize_public_host(&host);
    let preview_request = PreviewIngressRequest {
        request_id: request_id.clone(),
        method: request.method,
        path: request.target,
        headers: request.headers.into_iter().collect::<BTreeMap<_, _>>(),
        body_base64: if request.body.is_empty() {
            None
        } else {
            Some(base64::engine::general_purpose::STANDARD.encode(request.body))
        },
    };
    let mut sessions_guard = sessions
        .sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(session) = sessions_guard.get_mut(&public_host) else {
        let failure = PreviewIngressFailure {
            request_id,
            host,
            path,
            status: 404,
            classification: "missing_session".to_string(),
            message: "No active Homeboy preview ingress route matches this host".to_string(),
        };
        record_failure(&recent_failures, failure.clone());
        return write_diagnostic(stream, &failure, started);
    };
    if !session.active {
        let failure = PreviewIngressFailure {
            request_id,
            host,
            path,
            status: 410,
            classification: "disconnected_session".to_string(),
            message: "Homeboy preview client session is disconnected".to_string(),
        };
        record_failure(&recent_failures, failure.clone());
        return write_diagnostic(stream, &failure, started);
    }
    let _local_origin = session.local_origin.clone();
    session.pending.push_back(preview_request);
    sessions.changed.notify_all();

    let timeout = Duration::from_secs(60);
    loop {
        if let Some(session) = sessions_guard.get_mut(&public_host) {
            if let Some(response) = session.responses.remove(&request_id) {
                drop(sessions_guard);
                return write_preview_response(stream, response, &host, &path, started);
            }
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let failure = PreviewIngressFailure {
                request_id,
                host,
                path,
                status: 504,
                classification: "client_timeout".to_string(),
                message: "Homeboy preview client did not respond before timeout".to_string(),
            };
            record_failure(&recent_failures, failure.clone());
            return write_diagnostic(stream, &failure, started);
        }
        let (guard, wait) = sessions
            .changed
            .wait_timeout(sessions_guard, timeout - elapsed)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions_guard = guard;
        if wait.timed_out() {
            let failure = PreviewIngressFailure {
                request_id,
                host,
                path,
                status: 504,
                classification: "client_timeout".to_string(),
                message: "Homeboy preview client did not respond before timeout".to_string(),
            };
            record_failure(&recent_failures, failure.clone());
            return write_diagnostic(stream, &failure, started);
        }
    }
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
    if request.method.eq_ignore_ascii_case("OPTIONS") {
        write_status_and_headers(
            stream,
            204,
            "No Content",
            &artifact_cors_headers(Vec::new(), &path),
        )?;
        log_request(&PreviewIngressLogLine {
            request_id,
            host,
            path,
            status: 204,
            bytes: 0,
            duration_ms: started.elapsed().as_millis(),
            classification: "cors_preflight".to_string(),
        });
        return Ok(());
    }
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
            let headers = artifact_cors_headers(headers, &path);
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

fn preview_token_sha256(env_name: &str) -> Option<String> {
    std::env::var(env_name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn authorized_preview_client(request: &IngressHttpRequest, auth: &PreviewIngressAuth) -> bool {
    let Some(expected) = auth.token_sha256.as_deref() else {
        eprintln!(
            "homeboy preview ingress client auth disabled: {} is not set",
            auth.token_sha256_env
        );
        return false;
    };
    let Some(token) = request.headers.iter().find_map(|(name, value)| {
        if name.eq_ignore_ascii_case("authorization") {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
                .map(str::trim)
                .map(str::to_string)
        } else {
            None
        }
    }) else {
        return false;
    };
    let digest = Sha256::digest(token.as_bytes());
    format!("{digest:x}").eq_ignore_ascii_case(expected)
}

fn parse_json_body<T: for<'de> Deserialize<'de>>(body: &[u8], context: &str) -> Result<T> {
    serde_json::from_slice(body)
        .map_err(|e| Error::internal_json(e.to_string(), Some(context.to_string())))
}

fn normalize_public_host(host: &str) -> String {
    host.trim()
        .trim_end_matches('.')
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn validate_client_public_host(public_host: &str) -> Result<()> {
    if public_host.is_empty() || public_host.contains('*') || public_host.contains('/') {
        return Err(Error::validation_invalid_argument(
            "public_host",
            "preview client must register exactly one public host",
            Some(public_host.to_string()),
            None,
        ));
    }
    Ok(())
}

fn classify_runtime_host_state(
    public_host: &str,
    sessions: &Arc<PreviewClientSessions>,
    recent_failures: &[PreviewIngressFailure],
) -> Option<String> {
    let sessions_guard = sessions
        .sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(session) = sessions_guard.get(public_host) {
        return Some(
            if session.active {
                "registered"
            } else {
                "disconnected"
            }
            .to_string(),
        );
    }
    drop(sessions_guard);
    if recent_failures.iter().rev().any(|failure| {
        normalize_public_host(&failure.host) == public_host
            && failure.classification == "auth_failed_recently"
    }) {
        return Some("auth_failed_recently".to_string());
    }
    route_for_host(public_host)
        .ok()
        .flatten()
        .map(|route| route_state_label(&route))
}

fn classify_route_host_state(public_host: &str) -> Option<String> {
    route_for_host(public_host)
        .ok()
        .flatten()
        .map(|route| route_state_label(&route))
}

fn route_state_label(route: &PreviewIngressRoute) -> String {
    match classify_route(route) {
        PreviewIngressRouteLifecycle::Active => "registered".to_string(),
        PreviewIngressRouteLifecycle::Expired => "missing_session".to_string(),
        PreviewIngressRouteLifecycle::Disconnected => "disconnected".to_string(),
    }
}

fn query_value(target: &str, key: &str) -> Option<String> {
    let query = target.split_once('?')?.1;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name == key {
            return Some(value.replace('+', " "));
        }
    }
    None
}

fn validate_client_local_origin(local_origin: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(local_origin).map_err(|err| {
        Error::validation_invalid_argument(
            "local_origin",
            format!("preview client local origin must be a valid HTTP(S) URL: {err}"),
            Some(local_origin.to_string()),
            None,
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::validation_invalid_argument(
            "local_origin",
            "preview client local origin must use http or https",
            Some(local_origin.to_string()),
            None,
        ));
    }
    Ok(())
}

fn write_json_response(stream: &mut TcpStream, status: u16, body: serde_json::Value) -> Result<()> {
    let body = serde_json::to_vec(&body).map_err(|e| {
        Error::internal_json(e.to_string(), Some("preview ingress json".to_string()))
    })?;
    write_response(
        stream,
        status,
        reason_for_status(status),
        &[(&"content-type".to_string(), "application/json".to_string())],
        &body,
    )
}

fn write_preview_response(
    stream: &mut TcpStream,
    response: PreviewIngressResponse,
    host: &str,
    path: &str,
    started: Instant,
) -> Result<()> {
    let body = base64::engine::general_purpose::STANDARD
        .decode(response.body_base64.as_bytes())
        .map_err(|e| Error::internal_json(e.to_string(), Some(response.request_id.clone())))?;
    let headers = response.headers.into_iter().collect::<Vec<_>>();
    write_status_and_headers(
        stream,
        response.status,
        reason_for_status(response.status),
        &headers,
    )?;
    stream.write_all(&body).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("write preview client response body".to_string()),
        )
    })?;
    log_request(&PreviewIngressLogLine {
        request_id: response.request_id,
        host: host.to_string(),
        path: path.to_string(),
        status: response.status,
        bytes: body.len(),
        duration_ms: started.elapsed().as_millis(),
        classification: "reverse_channel".to_string(),
    });
    Ok(())
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
ExecStart={binary} tunnel preview-ingress serve --bind {bind} --domain {domain} --public-host-pattern '{public_host_pattern}' --token-sha256-env HOMEBOY_PREVIEW_TUNNEL_TOKEN_SHA256
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

fn artifact_cors_headers(mut headers: Vec<(String, String)>, path: &str) -> Vec<(String, String)> {
    push_header_if_missing(&mut headers, "access-control-allow-origin", "*");
    push_header_if_missing(
        &mut headers,
        "access-control-allow-methods",
        "GET, HEAD, OPTIONS",
    );
    push_header_if_missing(&mut headers, "access-control-allow-headers", "*");
    if path.split('?').next().unwrap_or(path).ends_with(".json") {
        push_header_if_missing(&mut headers, "content-type", "application/json");
    }
    headers
}

fn push_header_if_missing(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if !headers
        .iter()
        .any(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
    {
        headers.push((name.to_string(), value.to_string()));
    }
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
