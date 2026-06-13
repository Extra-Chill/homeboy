use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::preview_client::{self, PreviewClientReport, PreviewClientStartSpec};
use homeboy::core::preview_ingress::{
    self, PreviewIngressInstallOptions, PreviewIngressInstallPlan, PreviewIngressInstallStatusPlan,
    PreviewIngressRoute, PreviewIngressServeSpec, PreviewIngressStatus,
};
use homeboy::core::tunnel::{
    self, ExposeServiceTunnelSpec, ServiceTunnel, ServiceTunnelAuth, ServiceTunnelAuthMode,
    ServiceTunnelExposure, ServiceTunnelPolicy, ServiceTunnelPreviewPolicy,
    ServiceTunnelPreviewPolicyMode, ServiceTunnelStatus, ServiceTunnelTarget,
    ServiceTunnelTunnelBackend, StartServiceTunnelSpec,
};
use homeboy::core::{EntityCrudOutput, MergeOutput};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use super::{CmdResult, DynamicSetArgs};

#[derive(Debug, Default, Serialize)]
pub struct TunnelExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceTunnelActionOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_client: Option<PreviewClientActionOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_ingress: Option<PreviewIngressActionOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wpcom_edit_page: Option<WpcomEditPageOutput>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ServiceTunnelActionOutput {
    Url {
        service_id: String,
        local_url: String,
    },
    Status(ServiceTunnelStatus),
}

#[derive(Debug, Serialize)]
pub struct PreviewClientActionOutput {
    pub report: PreviewClientReport,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PreviewIngressActionOutput {
    Install(PreviewIngressInstallPlan),
    InstallStatus(PreviewIngressInstallStatusPlan),
    Route(PreviewIngressRoute),
    Routes { routes: Vec<PreviewIngressRoute> },
    Status(PreviewIngressStatus),
}

#[derive(Debug, Serialize)]
pub struct WpcomEditPageOutput {
    pub schema: String,
    pub selected_url: String,
    pub preview_public_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
    pub wpcom_codebox_dir: String,
    pub artifacts_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_routed_editor_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_preview_editor_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_preview_editor_url: Option<String>,
    pub edit_page_exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub artifact_path: String,
}

pub type TunnelOutput = EntityCrudOutput<ServiceTunnel, TunnelExtra>;

#[derive(Args)]
pub struct TunnelArgs {
    #[command(subcommand)]
    command: TunnelCommand,
}

#[derive(Subcommand)]
enum TunnelCommand {
    /// Manage private service tunnel declarations
    Service {
        #[command(subcommand)]
        command: TunnelServiceCommand,
    },
    /// Connect a local preview origin to a Homeboy preview ingress
    #[command(name = "preview-client")]
    PreviewClient {
        #[command(subcommand)]
        command: TunnelPreviewClientCommand,
    },
    /// Run and inspect the VPS-side public preview ingress
    #[command(name = "preview-ingress")]
    PreviewIngress {
        #[command(subcommand)]
        command: TunnelPreviewIngressCommand,
    },
    /// Open a named WordPress.com page in a held Codebox editor through a Homeboy public URL
    #[command(name = "wpcom-edit-page")]
    WpcomEditPage(WpcomEditPageArgs),
}

#[derive(Args)]
struct WpcomEditPageArgs {
    /// WordPress.com or developer.wordpress.com page URL to open
    page_url: String,

    /// Path to the wpcom-codebox checkout/worktree containing scripts/edit-page.mjs
    #[arg(long)]
    wpcom_codebox_dir: PathBuf,

    /// Homeboy service ID whose started tunnel status contains the public preview URL
    #[arg(long, conflicts_with = "preview_public_url")]
    service_id: Option<String>,

    /// Public/tunnel preview origin owned by Homeboy
    #[arg(long, conflicts_with = "service_id")]
    preview_public_url: Option<String>,

    /// Duration passed to wpcom-codebox edit-page so WP Codebox keeps the runtime alive
    #[arg(long, default_value = "30m")]
    preview_hold: String,

    /// Optional fixed preview port forwarded to wpcom-codebox edit-page
    #[arg(long)]
    preview_port: Option<u16>,

    /// Optional preview bind address forwarded to wpcom-codebox edit-page
    #[arg(long)]
    preview_bind: Option<String>,

    /// Make wpcom-codebox hold the preview process in blocking mode
    #[arg(long)]
    preview_hold_blocking: bool,

    /// Node.js binary used to execute scripts/edit-page.mjs
    #[arg(long, default_value = "node")]
    node: String,

    /// Optional wp-codebox CLI path forwarded to wpcom-codebox edit-page
    #[arg(long)]
    cli: Option<PathBuf>,

    /// Optional wpcom checkout path forwarded to wpcom-codebox edit-page
    #[arg(long)]
    wpcom: Option<PathBuf>,

    /// Optional exported page source directory forwarded to wpcom-codebox edit-page
    #[arg(long)]
    source_dir: Option<PathBuf>,

    /// Directory for wpcom-codebox edit-page artifacts. Defaults under Homeboy artifact root.
    #[arg(long)]
    artifacts: Option<PathBuf>,
}

#[derive(Subcommand)]
enum TunnelPreviewClientCommand {
    /// Start an outbound authenticated reverse channel for one public host
    Start {
        /// Preview ingress/broker base URL
        #[arg(long)]
        ingress: String,

        /// Exact public host to register. Wildcards are rejected.
        #[arg(long)]
        public_host: String,

        /// Local HTTP(S) origin to forward requests to
        #[arg(long)]
        local_origin: String,

        /// Preview session ID claimed by this client
        #[arg(long)]
        session_id: Option<String>,

        /// Environment variable that contains the preview tunnel bearer token
        #[arg(long, default_value = "HOMEBOY_PREVIEW_TUNNEL_TOKEN")]
        token_env: String,

        /// Long-poll timeout in seconds for ingress request claims
        #[arg(long, default_value_t = 30)]
        poll_timeout: u64,

        /// Print the public preview origin to stdout after successful registration
        #[arg(long)]
        ready_stdout: bool,
    },
}

#[derive(Subcommand)]
enum TunnelPreviewIngressCommand {
    /// Render a non-destructive operator install plan for a VPS preview ingress domain
    Install(PreviewIngressInstallArgs),
    /// Render machine-readable operator install status checks without probing a live VPS
    InstallStatus(PreviewIngressInstallArgs),
    /// Register or replace one active public-host route
    Route {
        /// Preview session ID
        session_id: String,

        /// Public host routed by the TLS/proxy layer, e.g. run-123-tunnel.chubes.net
        #[arg(long)]
        public_host: String,

        /// Local/reverse-channel HTTP origin for this session
        #[arg(long)]
        upstream_origin: String,

        /// RFC3339 expiry after which ingress returns 410
        #[arg(long)]
        expires_at: Option<String>,

        /// Mark the route disconnected while preserving diagnostics
        #[arg(long)]
        inactive: bool,
    },
    /// Remove one preview ingress route
    Unroute {
        /// Preview session ID
        session_id: String,
    },
    /// List registered preview ingress routes
    List,
    /// Report route lifecycle and recent server failure metadata
    Status {
        /// Bind address to include in the status output
        #[arg(long)]
        bind: Option<String>,

        /// Operator-owned preview domain
        #[arg(long)]
        domain: Option<String>,

        /// Public host pattern routed to this ingress
        #[arg(long)]
        public_host_pattern: Option<String>,
    },
    /// Run the blocking HTTP ingress server behind a TLS terminator
    Serve {
        /// Loopback bind address for Nginx/Caddy/Cloudflare to proxy to
        #[arg(long, default_value = "127.0.0.1:7350")]
        bind: String,

        /// Operator-owned preview domain
        #[arg(long)]
        domain: String,

        /// Public host pattern routed to this ingress
        #[arg(long, default_value = "*-tunnel.{domain}")]
        public_host_pattern: String,

        /// Environment variable containing the allowed client token SHA-256 digest
        #[arg(long, default_value = "HOMEBOY_PREVIEW_TUNNEL_TOKEN_SHA256")]
        token_sha256_env: String,
    },
}

#[derive(Args)]
struct PreviewIngressInstallArgs {
    /// Configured Homeboy server ID for the VPS
    #[arg(long)]
    server: String,

    /// Operator-owned domain, e.g. chubes.net
    #[arg(long)]
    domain: String,

    /// Wildcard host pattern routed to the ingress, e.g. *-tunnel.chubes.net
    #[arg(long)]
    public_host_pattern: String,

    /// Stable loopback bind address for the ingress daemon
    #[arg(long, default_value = "127.0.0.1:7350")]
    bind: String,

    /// Homeboy binary path used by the service unit
    #[arg(long, default_value = "/usr/local/bin/homeboy")]
    binary_path: String,

    /// systemd service name
    #[arg(long, default_value = "homeboy-preview-ingress")]
    service_name: String,

    /// System user that runs the ingress service
    #[arg(long, default_value = "homeboy")]
    user: String,

    /// System group that runs the ingress service
    #[arg(long, default_value = "homeboy")]
    group: String,
}

impl PreviewIngressInstallArgs {
    fn into_options(self) -> PreviewIngressInstallOptions {
        PreviewIngressInstallOptions {
            server_id: self.server,
            domain: self.domain,
            public_host_pattern: self.public_host_pattern,
            bind: self.bind,
            binary_path: self.binary_path,
            service_name: self.service_name,
            service_user: self.user,
            service_group: self.group,
        }
    }
}

#[derive(Subcommand)]
enum TunnelServiceCommand {
    /// Declare a private service tunnel without opening a public listener
    Expose {
        /// Service tunnel ID
        id: String,

        /// SSH server that can reach the private service
        #[arg(long)]
        server: String,

        /// Hostname or IP of the service as seen from the SSH server
        #[arg(long)]
        remote_host: String,

        /// Port of the service as seen from the SSH server
        #[arg(long)]
        remote_port: u16,

        /// URL scheme for the local service URL
        #[arg(long, default_value = "http")]
        scheme: String,

        /// Fixed local loopback port to reserve for this service later
        #[arg(long)]
        local_port: Option<u16>,

        /// Required auth mode for clients that use the private service
        #[arg(long, value_enum)]
        auth_mode: ServiceTunnelAuthModeArg,

        /// Environment variable that supplies auth material for env-backed modes
        #[arg(long)]
        auth_env: Option<String>,

        /// Header name for header/bearer auth modes
        #[arg(long)]
        auth_header: Option<String>,

        /// Allowed client label. Repeat for multiple expected clients.
        #[arg(long = "allow-client")]
        allowed_clients: Vec<String>,

        /// Human-readable description
        #[arg(long)]
        description: Option<String>,

        /// Workflow preview URL policy for this managed service
        #[arg(long, value_enum, default_value_t = ServiceTunnelPreviewPolicyArg::None)]
        preview_policy: ServiceTunnelPreviewPolicyArg,

        /// RFC3339 expiry for --preview-policy keep-alive-until
        #[arg(long)]
        preview_keep_alive_until: Option<String>,
    },
    /// List private service tunnel declarations
    List,
    /// Show a private service tunnel declaration
    Show {
        /// Service tunnel ID
        id: String,
    },
    /// Modify a private service tunnel declaration
    Set {
        #[command(flatten)]
        args: DynamicSetArgs,
    },
    /// Remove a private service tunnel declaration
    Remove {
        /// Service tunnel ID
        id: String,
    },
    /// Print the declared private local URL for a service tunnel
    Url {
        /// Service tunnel ID
        id: String,
    },
    /// Show declaration, process, health, backend, and evidence status
    Status {
        /// Service tunnel ID
        id: String,
    },
    /// Start and supervise a declared local service command
    Start {
        /// Service tunnel ID
        id: String,

        /// Long-running service command to execute through the platform shell
        #[arg(long)]
        command: String,

        /// Working directory for the service command
        #[arg(long)]
        cwd: Option<PathBuf>,

        /// Environment assignment passed to the service command. Repeat for multiple values.
        #[arg(long = "env")]
        env: Vec<String>,

        /// Local loopback host declared for this service
        #[arg(long)]
        host: Option<String>,

        /// Local port declared for this service
        #[arg(long)]
        port: Option<u16>,

        /// Local URL scheme
        #[arg(long)]
        scheme: Option<String>,

        /// Full health-check URL to poll before reporting the service ready
        #[arg(long)]
        health_url: Option<String>,

        /// Health-check path appended to the declared local URL
        #[arg(long)]
        health_path: Option<String>,

        /// Seconds to wait for the service health check
        #[arg(long, default_value_t = 30)]
        readiness_timeout: u64,

        /// Public tunnel backend adapter.
        #[arg(long, value_enum, default_value_t = ServiceTunnelBackendArg::None)]
        public_tunnel_backend: ServiceTunnelBackendArg,

        /// Provider-neutral backend command to supervise when using the command backend
        #[arg(long)]
        public_tunnel_command: Option<String>,

        /// Public URL exposed by the backend command
        #[arg(long)]
        public_tunnel_public_url: Option<String>,

        /// Owning workflow run ID to attach to preview artifacts
        #[arg(long)]
        source_run_id: Option<String>,

        /// Owning workflow ID to attach to preview artifacts
        #[arg(long)]
        source_workflow_id: Option<String>,
    },
    /// Stop a running managed local service and cleanup runtime state
    Stop {
        /// Service tunnel ID
        id: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ServiceTunnelAuthModeArg {
    BearerEnv,
    HeaderEnv,
    BasicEnv,
    MutualTls,
    SshOnly,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ServiceTunnelBackendArg {
    None,
    Command,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ServiceTunnelPreviewPolicyArg {
    None,
    Always,
    OnFailure,
    ManualApproval,
    KeepAliveUntil,
}

impl From<ServiceTunnelPreviewPolicyArg> for ServiceTunnelPreviewPolicyMode {
    fn from(value: ServiceTunnelPreviewPolicyArg) -> Self {
        match value {
            ServiceTunnelPreviewPolicyArg::None => ServiceTunnelPreviewPolicyMode::None,
            ServiceTunnelPreviewPolicyArg::Always => ServiceTunnelPreviewPolicyMode::Always,
            ServiceTunnelPreviewPolicyArg::OnFailure => ServiceTunnelPreviewPolicyMode::OnFailure,
            ServiceTunnelPreviewPolicyArg::ManualApproval => {
                ServiceTunnelPreviewPolicyMode::ManualApproval
            }
            ServiceTunnelPreviewPolicyArg::KeepAliveUntil => {
                ServiceTunnelPreviewPolicyMode::KeepAliveUntil
            }
        }
    }
}

impl std::fmt::Display for ServiceTunnelBackendArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceTunnelBackendArg::None => write!(f, "none"),
            ServiceTunnelBackendArg::Command => write!(f, "command"),
        }
    }
}

impl From<ServiceTunnelBackendArg> for ServiceTunnelTunnelBackend {
    fn from(value: ServiceTunnelBackendArg) -> Self {
        match value {
            ServiceTunnelBackendArg::None => ServiceTunnelTunnelBackend::None,
            ServiceTunnelBackendArg::Command => ServiceTunnelTunnelBackend::Command,
        }
    }
}

impl From<ServiceTunnelAuthModeArg> for ServiceTunnelAuthMode {
    fn from(value: ServiceTunnelAuthModeArg) -> Self {
        match value {
            ServiceTunnelAuthModeArg::BearerEnv => ServiceTunnelAuthMode::BearerEnv,
            ServiceTunnelAuthModeArg::HeaderEnv => ServiceTunnelAuthMode::HeaderEnv,
            ServiceTunnelAuthModeArg::BasicEnv => ServiceTunnelAuthMode::BasicEnv,
            ServiceTunnelAuthModeArg::MutualTls => ServiceTunnelAuthMode::MutualTls,
            ServiceTunnelAuthModeArg::SshOnly => ServiceTunnelAuthMode::SshOnly,
        }
    }
}

pub fn run(args: TunnelArgs, _global: &super::GlobalArgs) -> CmdResult<TunnelOutput> {
    match args.command {
        TunnelCommand::Service { command } => run_service(command),
        TunnelCommand::PreviewClient { command } => run_preview_client(command),
        TunnelCommand::PreviewIngress { command } => run_preview_ingress(command),
        TunnelCommand::WpcomEditPage(args) => run_wpcom_edit_page(args),
    }
}

fn run_wpcom_edit_page(args: WpcomEditPageArgs) -> CmdResult<TunnelOutput> {
    let public_url = resolve_wpcom_edit_page_public_url(&args)?;
    let artifacts_dir = args.artifacts.clone().unwrap_or_else(|| {
        homeboy::core::artifact_root()
            .unwrap_or_else(|_| std::env::temp_dir().join("homeboy-artifacts"))
            .join("wpcom-edit-page")
            .join(safe_artifact_slug(&args.page_url))
    });
    fs::create_dir_all(&artifacts_dir).map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some(format!("create artifacts dir {}", artifacts_dir.display())),
        )
    })?;

    let script = args.wpcom_codebox_dir.join("scripts/edit-page.mjs");
    if !script.exists() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "wpcom-codebox-dir",
            "scripts/edit-page.mjs was not found",
            None,
            Some(vec![script.display().to_string()]),
        ));
    }

    let mut command = Command::new(&args.node);
    command
        .arg(&script)
        .arg(&args.page_url)
        .arg("--preview-hold")
        .arg(&args.preview_hold)
        .arg("--preview-public-url")
        .arg(&public_url)
        .arg("--artifacts")
        .arg(&artifacts_dir)
        .current_dir(&args.wpcom_codebox_dir);
    if let Some(port) = args.preview_port {
        command.arg("--preview-port").arg(port.to_string());
    }
    if let Some(bind) = &args.preview_bind {
        command.arg("--preview-bind").arg(bind);
    }
    if args.preview_hold_blocking {
        command.arg("--preview-hold-blocking").arg("true");
    }
    if let Some(cli) = &args.cli {
        command.arg("--cli").arg(cli);
    }
    if let Some(wpcom) = &args.wpcom {
        command.arg("--wpcom").arg(wpcom);
    }
    if let Some(source_dir) = &args.source_dir {
        command.arg("--source-dir").arg(source_dir);
    }

    let output = command.output().map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some(format!("run {}", script.display())),
        )
    })?;
    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let summary = read_wpcom_edit_page_summary(&artifacts_dir);
    let result = WpcomEditPageOutput {
        schema: "homeboy/wpcom-edit-page-orchestration/v1".to_string(),
        selected_url: args.page_url.clone(),
        preview_public_url: public_url,
        service_id: args.service_id.clone(),
        wpcom_codebox_dir: args.wpcom_codebox_dir.display().to_string(),
        artifacts_dir: artifacts_dir.display().to_string(),
        logical_routed_editor_url: summary
            .as_ref()
            .and_then(|value| json_pointer_string(value, "/editor/logical_routed_editor_url")),
        local_preview_editor_url: summary
            .as_ref()
            .and_then(|value| json_pointer_string(value, "/editor/local_preview_editor_url"))
            .or_else(|| parse_prefixed_line(&stdout, "Local preview editor URL:")),
        public_preview_editor_url: summary
            .as_ref()
            .and_then(|value| json_pointer_string(value, "/editor/public_preview_editor_url"))
            .or_else(|| parse_prefixed_line(&stdout, "Public preview editor URL:")),
        edit_page_exit_code: exit_code,
        stdout,
        stderr,
        artifact_path: artifacts_dir
            .join("homeboy-wpcom-edit-page.json")
            .display()
            .to_string(),
    };

    let artifact_json = serde_json::to_string_pretty(&result).map_err(|err| {
        homeboy::core::Error::internal_json(
            err.to_string(),
            Some("serialize wpcom edit-page orchestration artifact".to_string()),
        )
    })?;
    fs::write(&result.artifact_path, format!("{artifact_json}\n")).map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some(format!("write {}", result.artifact_path)),
        )
    })?;

    Ok((
        TunnelOutput {
            command: "tunnel.wpcom_edit_page".to_string(),
            id: Some(args.page_url),
            extra: TunnelExtra {
                wpcom_edit_page: Some(result),
                ..Default::default()
            },
            ..Default::default()
        },
        exit_code,
    ))
}

fn resolve_wpcom_edit_page_public_url(args: &WpcomEditPageArgs) -> homeboy::core::Result<String> {
    if let Some(public_url) = &args.preview_public_url {
        return Ok(public_url.clone());
    }
    let Some(service_id) = &args.service_id else {
        return Err(homeboy::core::Error::validation_missing_argument(vec![
            "--service-id or --preview-public-url".to_string(),
        ]));
    };
    let status = tunnel::status(service_id)?;
    status
        .preview_identity
        .public_url
        .or_else(|| status.preview.and_then(|preview| preview.preview_identity.public_url))
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "service-id",
                "service status does not contain a public preview URL; start the service with a Homeboy public tunnel backend first",
                Some(service_id.clone()),
                None,
            )
        })
}

fn read_wpcom_edit_page_summary(artifacts_dir: &std::path::Path) -> Option<Value> {
    let path = artifacts_dir.join("summary.json");
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
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

fn safe_artifact_slug(value: &str) -> String {
    let slug: String = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    slug.trim_matches('-').chars().take(96).collect()
}

fn run_preview_client(command: TunnelPreviewClientCommand) -> CmdResult<TunnelOutput> {
    match command {
        TunnelPreviewClientCommand::Start {
            ingress,
            public_host,
            local_origin,
            session_id,
            token_env,
            poll_timeout,
            ready_stdout,
        } => {
            let report = preview_client::start(PreviewClientStartSpec {
                ingress,
                public_host,
                local_origin,
                session_id,
                token_env,
                poll_timeout_secs: poll_timeout,
                ready_stdout,
            })?;
            Ok((
                TunnelOutput {
                    command: "tunnel.preview_client.start".to_string(),
                    id: Some(report.public_host.clone()),
                    extra: TunnelExtra {
                        preview_client: Some(PreviewClientActionOutput { report }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                0,
            ))
        }
    }
}

fn run_preview_ingress(command: TunnelPreviewIngressCommand) -> CmdResult<TunnelOutput> {
    match command {
        TunnelPreviewIngressCommand::Install(args) => {
            let server_id = args.server.clone();
            let plan = preview_ingress::render_install_plan(args.into_options())?;
            Ok((
                TunnelOutput {
                    command: "tunnel.preview_ingress.install".to_string(),
                    id: Some(server_id),
                    extra: TunnelExtra {
                        preview_ingress: Some(PreviewIngressActionOutput::Install(plan)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                0,
            ))
        }
        TunnelPreviewIngressCommand::InstallStatus(args) => {
            let server_id = args.server.clone();
            let status = preview_ingress::render_install_status_plan(args.into_options())?;
            Ok((
                TunnelOutput {
                    command: "tunnel.preview_ingress.install_status".to_string(),
                    id: Some(server_id),
                    extra: TunnelExtra {
                        preview_ingress: Some(PreviewIngressActionOutput::InstallStatus(status)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                0,
            ))
        }
        TunnelPreviewIngressCommand::Route {
            session_id,
            public_host,
            upstream_origin,
            expires_at,
            inactive,
        } => {
            let route = preview_ingress::register_route(PreviewIngressRoute {
                session_id: session_id.clone(),
                public_host,
                upstream_origin,
                expires_at,
                active: !inactive,
            })?;
            Ok((
                TunnelOutput {
                    command: "tunnel.preview_ingress.route".to_string(),
                    id: Some(session_id),
                    extra: TunnelExtra {
                        preview_ingress: Some(PreviewIngressActionOutput::Route(route)),
                        ..Default::default()
                    },
                    updated_fields: vec!["route".to_string()],
                    ..Default::default()
                },
                0,
            ))
        }
        TunnelPreviewIngressCommand::Unroute { session_id } => {
            preview_ingress::remove_route(&session_id)?;
            Ok((
                TunnelOutput {
                    command: "tunnel.preview_ingress.unroute".to_string(),
                    id: Some(session_id.clone()),
                    deleted: vec![session_id],
                    ..Default::default()
                },
                0,
            ))
        }
        TunnelPreviewIngressCommand::List => {
            let routes = preview_ingress::list_routes()?;
            Ok((
                TunnelOutput {
                    command: "tunnel.preview_ingress.list".to_string(),
                    extra: TunnelExtra {
                        preview_ingress: Some(PreviewIngressActionOutput::Routes { routes }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                0,
            ))
        }
        TunnelPreviewIngressCommand::Status {
            bind,
            domain,
            public_host_pattern,
        } => {
            let status = preview_ingress::status(bind, domain, public_host_pattern)?;
            Ok((
                TunnelOutput {
                    command: "tunnel.preview_ingress.status".to_string(),
                    extra: TunnelExtra {
                        preview_ingress: Some(PreviewIngressActionOutput::Status(status)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                0,
            ))
        }
        TunnelPreviewIngressCommand::Serve {
            bind,
            domain,
            public_host_pattern,
            token_sha256_env,
        } => {
            let pattern = public_host_pattern.replace("{domain}", &domain);
            let status = preview_ingress::serve(PreviewIngressServeSpec {
                bind,
                domain,
                public_host_pattern: pattern,
                token_sha256_env,
            })?;
            Ok((
                TunnelOutput {
                    command: "tunnel.preview_ingress.serve".to_string(),
                    extra: TunnelExtra {
                        preview_ingress: Some(PreviewIngressActionOutput::Status(status)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                0,
            ))
        }
    }
}

fn run_service(command: TunnelServiceCommand) -> CmdResult<TunnelOutput> {
    match command {
        TunnelServiceCommand::Expose {
            id,
            server,
            remote_host,
            remote_port,
            scheme,
            local_port,
            auth_mode,
            auth_env,
            auth_header,
            allowed_clients,
            description,
            preview_policy,
            preview_keep_alive_until,
        } => expose_service(ExposeServiceTunnelSpec {
            id,
            server_id: server,
            scheme,
            local_port,
            auth: ServiceTunnelAuth {
                mode: auth_mode.into(),
                env_var: auth_env,
                header: auth_header,
            },
            target: ServiceTunnelTarget {
                host: remote_host,
                port: remote_port,
            },
            policy: ServiceTunnelPolicy {
                exposure: ServiceTunnelExposure::PrivateLoopback,
                require_auth: true,
                allowed_clients,
                preview: ServiceTunnelPreviewPolicy {
                    mode: preview_policy.into(),
                    keep_alive_until: preview_keep_alive_until,
                },
                native_preview_auth: Default::default(),
            },
            description,
        }),
        TunnelServiceCommand::List => list_services(),
        TunnelServiceCommand::Show { id } => show_service(&id),
        TunnelServiceCommand::Set { args } => set_service(args),
        TunnelServiceCommand::Remove { id } => remove_service(&id),
        TunnelServiceCommand::Url { id } => url_service(&id),
        TunnelServiceCommand::Status { id } => status_service(&id),
        TunnelServiceCommand::Start {
            id,
            command,
            cwd,
            env,
            host,
            port,
            scheme,
            health_url,
            health_path,
            readiness_timeout,
            public_tunnel_backend,
            public_tunnel_command,
            public_tunnel_public_url,
            source_run_id,
            source_workflow_id,
        } => start_service(StartServiceTunnelSpec {
            id,
            command,
            cwd,
            env: parse_env_assignments(env)?,
            host,
            port,
            scheme,
            health_url,
            health_path,
            readiness_timeout_secs: readiness_timeout,
            backend: public_tunnel_backend.into(),
            backend_command: public_tunnel_command,
            backend_public_url: public_tunnel_public_url,
            source_run_id,
            source_workflow_id,
        }),
        TunnelServiceCommand::Stop { id } => stop_service(&id),
    }
}

fn expose_service(spec: ExposeServiceTunnelSpec) -> CmdResult<TunnelOutput> {
    let tunnel = tunnel::expose(spec)?;
    Ok((
        TunnelOutput {
            command: "tunnel.service.expose".to_string(),
            id: Some(tunnel.id.clone()),
            entity: Some(tunnel),
            updated_fields: vec!["declared".to_string()],
            ..Default::default()
        },
        0,
    ))
}

fn list_services() -> CmdResult<TunnelOutput> {
    Ok((
        TunnelOutput {
            command: "tunnel.service.list".to_string(),
            entities: tunnel::list()?,
            ..Default::default()
        },
        0,
    ))
}

fn show_service(id: &str) -> CmdResult<TunnelOutput> {
    Ok((
        TunnelOutput {
            command: "tunnel.service.show".to_string(),
            id: Some(id.to_string()),
            entity: Some(tunnel::load(id)?),
            ..Default::default()
        },
        0,
    ))
}

fn set_service(args: DynamicSetArgs) -> CmdResult<TunnelOutput> {
    let merged = super::merge_dynamic_args(&args)?.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "spec",
            "Provide JSON spec, --json flag, --base64 flag, or --key value flags",
            None,
            None,
        )
    })?;
    let (json_string, replace_fields) = super::finalize_set_spec(&merged, &args.replace)?;

    match tunnel::merge(args.id.as_deref(), &json_string, &replace_fields)? {
        MergeOutput::Single(result) => {
            let entity = tunnel::load(&result.id)?;
            Ok((
                TunnelOutput {
                    command: "tunnel.service.set".to_string(),
                    id: Some(result.id),
                    entity: Some(entity),
                    updated_fields: result.updated_fields,
                    ..Default::default()
                },
                0,
            ))
        }
        MergeOutput::Bulk(summary) => {
            let exit_code = summary.exit_code();
            Ok((
                TunnelOutput {
                    command: "tunnel.service.set".to_string(),
                    batch: Some(summary),
                    ..Default::default()
                },
                exit_code,
            ))
        }
    }
}

fn remove_service(id: &str) -> CmdResult<TunnelOutput> {
    tunnel::delete(id)?;
    Ok((
        TunnelOutput {
            command: "tunnel.service.remove".to_string(),
            id: Some(id.to_string()),
            deleted: vec![id.to_string()],
            ..Default::default()
        },
        0,
    ))
}

fn url_service(id: &str) -> CmdResult<TunnelOutput> {
    let local_url = tunnel::local_url(id)?;
    Ok((
        TunnelOutput {
            command: "tunnel.service.url".to_string(),
            id: Some(id.to_string()),
            extra: TunnelExtra {
                service: Some(ServiceTunnelActionOutput::Url {
                    service_id: id.to_string(),
                    local_url,
                }),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn status_service(id: &str) -> CmdResult<TunnelOutput> {
    let report = tunnel::status(id)?;
    Ok((
        TunnelOutput {
            command: "tunnel.service.status".to_string(),
            id: Some(id.to_string()),
            extra: TunnelExtra {
                service: Some(ServiceTunnelActionOutput::Status(report)),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn start_service(spec: StartServiceTunnelSpec) -> CmdResult<TunnelOutput> {
    let id = spec.id.clone();
    let report = tunnel::start(spec)?;
    Ok((
        TunnelOutput {
            command: "tunnel.service.start".to_string(),
            id: Some(id),
            extra: TunnelExtra {
                service: Some(ServiceTunnelActionOutput::Status(report)),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn stop_service(id: &str) -> CmdResult<TunnelOutput> {
    let report = tunnel::stop(id)?;
    Ok((
        TunnelOutput {
            command: "tunnel.service.stop".to_string(),
            id: Some(id.to_string()),
            extra: TunnelExtra {
                service: Some(ServiceTunnelActionOutput::Status(report)),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn parse_env_assignments(
    assignments: Vec<String>,
) -> homeboy::core::Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();
    for assignment in assignments {
        let Some((key, value)) = assignment.split_once('=') else {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "env",
                "environment values must use KEY=VALUE syntax",
                None,
                Some(vec![assignment]),
            ));
        };
        if key.trim().is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "env",
                "environment variable name is required",
                None,
                None,
            ));
        }
        env.insert(key.to_string(), value.to_string());
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;
    use homeboy::core::server::Server;
    use std::collections::HashMap;
    use std::fs;

    fn create_server() {
        homeboy::core::server::save(&Server {
            id: "private-host".to_string(),
            aliases: Vec::new(),
            host: "private.example.test".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            kind: None,
            auth: None,
            env: HashMap::new(),
            runner: None,
        })
        .expect("save server");
    }

    #[test]
    fn expose_service_command_records_declaration() {
        test_support::with_isolated_home(|_| {
            create_server();
            let (output, exit_code) = run_service(TunnelServiceCommand::Expose {
                id: "site-preview".to_string(),
                server: "private-host".to_string(),
                remote_host: "127.0.0.1".to_string(),
                remote_port: 7331,
                scheme: "http".to_string(),
                local_port: Some(8831),
                auth_mode: ServiceTunnelAuthModeArg::BearerEnv,
                auth_env: Some("SITE_PREVIEW_TOKEN".to_string()),
                auth_header: Some("Authorization".to_string()),
                allowed_clients: vec!["app-runtime".to_string()],
                description: None,
                preview_policy: ServiceTunnelPreviewPolicyArg::None,
                preview_keep_alive_until: None,
            })
            .expect("command succeeds");

            assert_eq!(exit_code, 0);
            assert_eq!(output.command, "tunnel.service.expose");
            assert_eq!(output.entity.expect("entity").id, "site-preview");
        });
    }

    #[test]
    fn parses_public_editor_url_from_edit_page_stdout() {
        let stdout = "Selected page: https://wordpress.com/ai\nPublic preview editor URL: https://run.example.test/wp-admin/post.php?post=24117035&action=edit\n";

        assert_eq!(
            parse_prefixed_line(stdout, "Public preview editor URL:").as_deref(),
            Some("https://run.example.test/wp-admin/post.php?post=24117035&action=edit")
        );
    }

    #[test]
    fn safe_artifact_slug_keeps_named_page_url_human_readable() {
        assert_eq!(
            safe_artifact_slug("https://wordpress.com/ai"),
            "https---wordpress-com-ai"
        );
    }

    #[test]
    fn wpcom_edit_page_command_returns_public_editor_url_and_artifact() {
        test_support::with_isolated_home(|_| {
            let checkout = tempfile::tempdir().expect("checkout");
            let scripts_dir = checkout.path().join("scripts");
            fs::create_dir_all(&scripts_dir).expect("scripts dir");
            fs::write(
                scripts_dir.join("edit-page.mjs"),
                r#"
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
const artifacts = process.argv[process.argv.indexOf('--artifacts') + 1];
mkdirSync(artifacts, { recursive: true });
writeFileSync(join(artifacts, 'summary.json'), JSON.stringify({
  editor: {
    logical_routed_editor_url: 'https://wordpress.com/wp-admin/post.php?post=24117035&action=edit',
    public_preview_editor_url: 'https://run.example.test/wp-admin/post.php?post=24117035&action=edit'
  }
}));
console.log('Public preview editor URL: https://run.example.test/wp-admin/post.php?post=24117035&action=edit');
"#,
            )
            .expect("script");
            let artifacts = tempfile::tempdir().expect("artifacts");

            let (output, exit_code) = run_wpcom_edit_page(WpcomEditPageArgs {
                page_url: "https://wordpress.com/ai".to_string(),
                wpcom_codebox_dir: checkout.path().to_path_buf(),
                service_id: None,
                preview_public_url: Some("https://run.example.test".to_string()),
                preview_hold: "5m".to_string(),
                preview_port: None,
                preview_bind: None,
                preview_hold_blocking: false,
                node: "node".to_string(),
                cli: None,
                wpcom: None,
                source_dir: None,
                artifacts: Some(artifacts.path().to_path_buf()),
            })
            .expect("wpcom edit-page command succeeds");

            assert_eq!(exit_code, 0);
            let result = output
                .extra
                .wpcom_edit_page
                .expect("wpcom edit-page output");
            assert_eq!(
                result.public_preview_editor_url.as_deref(),
                Some("https://run.example.test/wp-admin/post.php?post=24117035&action=edit")
            );
            assert!(artifacts
                .path()
                .join("homeboy-wpcom-edit-page.json")
                .exists());
        });
    }
}
