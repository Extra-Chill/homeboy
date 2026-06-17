use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use homeboy::core::artifacts::{
    self, ArtifactOriginInspect, ArtifactOriginServeSpec, ArtifactOriginStatus,
};
use homeboy::core::preview_client::{
    self, PreviewClientAuthDiagnostic, PreviewClientReport, PreviewClientStartSpec,
};
use homeboy::core::preview_ingress::{
    self, PreviewIngressInstallOptions, PreviewIngressInstallPlan, PreviewIngressInstallStatusPlan,
    PreviewIngressRoute, PreviewIngressServeSpec, PreviewIngressStatus,
};
use homeboy::core::tunnel::{
    self, ExposeServiceTunnelSpec, ServiceTunnel, ServiceTunnelAuth, ServiceTunnelAuthMode,
    ServiceTunnelExposure, ServiceTunnelPolicy, ServiceTunnelPreviewPolicy,
    ServiceTunnelPreviewPolicyMode, ServiceTunnelReadinessCheck, ServiceTunnelReadinessKind,
    ServiceTunnelStatus, ServiceTunnelTarget, ServiceTunnelTunnelBackend, StartServiceTunnelSpec,
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
    pub preview_consumer: Option<PreviewConsumerOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_origin: Option<ArtifactOriginActionOutput>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<PreviewClientReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_diagnostic: Option<PreviewClientAuthDiagnostic>,
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
pub struct PreviewConsumerOutput {
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

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ArtifactOriginActionOutput {
    Serve(ArtifactOriginStatus),
    Status(ArtifactOriginStatus),
    Inspect(ArtifactOriginInspect),
}

pub type TunnelOutput = EntityCrudOutput<ServiceTunnel, TunnelExtra>;

#[derive(Args)]
pub struct TunnelArgs {
    #[command(subcommand)]
    command: TunnelCommand,
}

impl TunnelArgs {
    pub(crate) fn is_preview_consumer_run(&self) -> bool {
        matches!(
            self.command,
            TunnelCommand::PreviewConsumer {
                command: TunnelPreviewConsumerCommand::Run { .. }
            }
        )
    }

    pub(crate) fn is_service_start(&self) -> bool {
        matches!(
            self.command,
            TunnelCommand::Service {
                command: TunnelServiceCommand::Start { .. }
            }
        )
    }

    pub(crate) fn is_service_expose(&self) -> bool {
        matches!(
            self.command,
            TunnelCommand::Service {
                command: TunnelServiceCommand::Expose { .. }
            }
        )
    }
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
    /// Run a configured preview consumer with a Homeboy-owned public URL
    #[command(name = "preview-consumer")]
    PreviewConsumer {
        #[command(subcommand)]
        command: TunnelPreviewConsumerCommand,
    },
    /// Serve the artifact root as a browser/reviewer-facing static origin
    #[command(name = "artifact-origin")]
    ArtifactOrigin {
        #[command(subcommand)]
        command: TunnelArtifactOriginCommand,
    },
}

#[derive(Subcommand)]
enum TunnelPreviewConsumerCommand {
    /// Run a command described by a preview-consumer JSON config
    Run {
        /// JSON config containing command, args, env, artifact, and extraction rules
        #[arg(long)]
        config: PathBuf,

        /// Service ID whose started tunnel status contains the public preview URL
        #[arg(long, conflicts_with = "preview_public_url")]
        service_id: Option<String>,

        /// Public/tunnel preview origin owned by Homeboy
        #[arg(long, conflicts_with = "service_id")]
        preview_public_url: Option<String>,

        /// Override the config artifact directory
        #[arg(long)]
        artifacts_dir: Option<PathBuf>,
    },
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
    /// Compare preview-client token digests without printing token material
    DiagnoseAuth {
        /// Environment variable that contains the preview tunnel bearer token
        #[arg(long, default_value = "HOMEBOY_PREVIEW_TUNNEL_TOKEN")]
        token_env: String,

        /// Environment variable containing the allowed client token SHA-256 digest
        #[arg(long, default_value = "HOMEBOY_PREVIEW_TUNNEL_TOKEN_SHA256")]
        token_sha256_env: String,
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

        /// Public host routed by the TLS/proxy layer, e.g. run-123-tunnel.preview.example.test
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

        /// Public host to inspect for preview-client registration state
        #[arg(long)]
        host: Option<String>,
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

#[derive(Subcommand)]
enum TunnelArtifactOriginCommand {
    /// Serve Homeboy artifact-root paths with CORS headers for browser consumers
    Serve {
        /// Loopback bind address for the local static artifact origin
        #[arg(long, default_value = "127.0.0.1:7351")]
        bind: String,

        /// Artifact root to serve. Defaults to Homeboy's configured artifact root.
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Print the artifact origin root and public URL mapping without starting a server
    Status {
        /// Loopback bind address expected by the local static artifact origin
        #[arg(long, default_value = "127.0.0.1:7351")]
        bind: String,

        /// Artifact root to inspect. Defaults to Homeboy's configured artifact root.
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Map an artifact-origin request path or file path to its served file and public URL
    Inspect {
        /// Request path, artifact-root-relative path, or filesystem path to inspect
        path: String,

        /// Artifact root to inspect. Defaults to Homeboy's configured artifact root.
        #[arg(long)]
        root: Option<PathBuf>,

        /// Return a non-zero exit code when the mapped file is missing
        #[arg(long)]
        fail_on_missing: bool,
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
        #[arg(long, required_unless_present = "runner_local")]
        server: Option<String>,

        /// Declare a runner-local service without a separate server declaration.
        /// In a runner-local context the runner itself is the server, so a
        /// duplicate server declaration is not required (#4606).
        #[arg(long)]
        runner_local: bool,

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

        /// Readiness contract label reported in service status
        #[arg(long, value_enum, default_value_t = ServiceTunnelReadinessKindArg::Process)]
        readiness_kind: ServiceTunnelReadinessKindArg,

        /// Require the declared local URL host:port to accept TCP connections
        #[arg(long)]
        require_listener: bool,

        /// Artifact file whose JSON value proves readiness
        #[arg(long)]
        readiness_artifact: Option<PathBuf>,

        /// JSON Pointer inside --readiness-artifact whose value must match
        #[arg(long, requires = "readiness_artifact")]
        readiness_artifact_json_pointer: Option<String>,

        /// Expected string/JSON value for --readiness-artifact-json-pointer
        #[arg(long, requires = "readiness_artifact_json_pointer")]
        readiness_artifact_json_equals: Option<String>,

        /// Regex that must match captured service stdout before readiness is true
        #[arg(long)]
        readiness_stdout_regex: Option<String>,

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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ServiceTunnelReadinessKindArg {
    Process,
    Preview,
    Proof,
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

impl From<ServiceTunnelReadinessKindArg> for ServiceTunnelReadinessKind {
    fn from(value: ServiceTunnelReadinessKindArg) -> Self {
        match value {
            ServiceTunnelReadinessKindArg::Process => ServiceTunnelReadinessKind::Process,
            ServiceTunnelReadinessKindArg::Preview => ServiceTunnelReadinessKind::Preview,
            ServiceTunnelReadinessKindArg::Proof => ServiceTunnelReadinessKind::Proof,
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
        TunnelCommand::PreviewConsumer { command } => run_preview_consumer(command),
        TunnelCommand::ArtifactOrigin { command } => run_artifact_origin(command),
    }
}

fn run_artifact_origin(command: TunnelArtifactOriginCommand) -> CmdResult<TunnelOutput> {
    match command {
        TunnelArtifactOriginCommand::Serve { bind, root } => {
            let status = artifacts::status(
                bind.clone(),
                root.clone().unwrap_or(homeboy::core::artifacts::root()?),
            );
            artifacts::serve(ArtifactOriginServeSpec { bind, root })?;
            Ok((
                TunnelOutput {
                    command: "tunnel.artifact_origin.serve".to_string(),
                    extra: TunnelExtra {
                        artifact_origin: Some(ArtifactOriginActionOutput::Serve(status)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                0,
            ))
        }
        TunnelArtifactOriginCommand::Status { bind, root } => {
            let status = artifacts::status_with_command(
                "tunnel.artifact_origin.status",
                bind,
                root.unwrap_or(homeboy::core::artifacts::root()?),
            );
            Ok((
                TunnelOutput {
                    command: "tunnel.artifact_origin.status".to_string(),
                    extra: TunnelExtra {
                        artifact_origin: Some(ArtifactOriginActionOutput::Status(status)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                0,
            ))
        }
        TunnelArtifactOriginCommand::Inspect {
            path,
            root,
            fail_on_missing,
        } => {
            let output =
                artifacts::inspect(root.unwrap_or(homeboy::core::artifacts::root()?), &path);
            let exit_code = if fail_on_missing && !output.exists {
                1
            } else {
                0
            };
            Ok((
                TunnelOutput {
                    command: "tunnel.artifact_origin.inspect".to_string(),
                    extra: TunnelExtra {
                        artifact_origin: Some(ArtifactOriginActionOutput::Inspect(output)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                exit_code,
            ))
        }
    }
}

fn run_preview_consumer(command: TunnelPreviewConsumerCommand) -> CmdResult<TunnelOutput> {
    match command {
        TunnelPreviewConsumerCommand::Run {
            config,
            service_id,
            preview_public_url,
            artifacts_dir,
        } => run_preview_consumer_config(config, service_id, preview_public_url, artifacts_dir),
    }
}

fn run_preview_consumer_config(
    config_path: PathBuf,
    service_id: Option<String>,
    preview_public_url: Option<String>,
    artifacts_dir_override: Option<PathBuf>,
) -> CmdResult<TunnelOutput> {
    let config = read_preview_consumer_config(&config_path)?;
    let public_url =
        resolve_preview_consumer_public_url(service_id.as_deref(), preview_public_url.as_deref())?;
    let artifacts_dir = artifacts_dir_override
        .or_else(|| config.command.artifacts_dir.clone())
        .unwrap_or_else(|| {
            homeboy::core::artifacts::root()
                .unwrap_or_else(|_| std::env::temp_dir().join("homeboy-artifacts"))
                .join("preview-consumer")
                .join(safe_artifact_slug(&config.id))
        });
    fs::create_dir_all(&artifacts_dir).map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some(format!("create artifacts dir {}", artifacts_dir.display())),
        )
    })?;

    let mut command = Command::new(render_preview_consumer_template(
        &config.command.program,
        &public_url,
        &artifacts_dir,
    ));
    for arg in &config.command.args {
        command.arg(render_preview_consumer_template(
            arg,
            &public_url,
            &artifacts_dir,
        ));
    }
    for (key, value) in &config.command.env {
        command.env(
            key,
            render_preview_consumer_template(value, &public_url, &artifacts_dir),
        );
    }
    if let Some(cwd) = &config.command.cwd {
        command.current_dir(cwd);
    }

    let output = command.output().map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some(format!("run preview consumer {}", config.id)),
        )
    })?;
    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let public_result_url =
        extract_preview_consumer_public_result_url(&config.output, &artifacts_dir, &stdout);
    let artifact_file = config
        .artifact_file
        .as_deref()
        .unwrap_or("homeboy-preview-consumer.json");
    let result = PreviewConsumerOutput {
        schema: "homeboy/preview-consumer-run/v1".to_string(),
        consumer_id: config.id.clone(),
        preview_public_url: public_url,
        service_id,
        artifacts_dir: artifacts_dir.display().to_string(),
        public_result_url,
        exit_code,
        stdout,
        stderr,
        artifact_path: artifacts_dir.join(artifact_file).display().to_string(),
    };

    let artifact_json = serde_json::to_string_pretty(&result).map_err(|err| {
        homeboy::core::Error::internal_json(
            err.to_string(),
            Some("serialize preview consumer run artifact".to_string()),
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
            command: "tunnel.preview_consumer.run".to_string(),
            id: Some(config.id),
            extra: TunnelExtra {
                preview_consumer: Some(result),
                ..Default::default()
            },
            ..Default::default()
        },
        exit_code,
    ))
}

fn read_preview_consumer_config(
    path: &std::path::Path,
) -> homeboy::core::Result<PreviewConsumerConfig> {
    let raw = fs::read_to_string(path).map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some(format!("read preview consumer config {}", path.display())),
        )
    })?;
    serde_json::from_str(&raw).map_err(|err| {
        homeboy::core::Error::validation_invalid_json(
            err,
            Some(format!("parse preview consumer config {}", path.display())),
            Some(raw),
        )
    })
}

fn resolve_preview_consumer_public_url(
    service_id: Option<&str>,
    preview_public_url: Option<&str>,
) -> homeboy::core::Result<String> {
    if let Some(public_url) = preview_public_url {
        return Ok(public_url.to_string());
    }
    let Some(service_id) = service_id else {
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
                "service status does not contain a public preview URL; start the service with a public tunnel backend first",
                Some(service_id.to_string()),
                None,
            )
        })
}

fn render_preview_consumer_template(
    value: &str,
    public_url: &str,
    artifacts_dir: &std::path::Path,
) -> String {
    value
        .replace("${preview_public_url}", public_url)
        .replace("${artifacts_dir}", &artifacts_dir.to_string_lossy())
}

fn extract_preview_consumer_public_result_url(
    config: &PreviewConsumerOutputConfig,
    artifacts_dir: &std::path::Path,
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
            let raw = fs::read_to_string(path).ok()?;
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
                        preview_client: Some(PreviewClientActionOutput {
                            report: Some(report.clone()),
                            auth_diagnostic: None,
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                0,
            ))
        }
        TunnelPreviewClientCommand::DiagnoseAuth {
            token_env,
            token_sha256_env,
        } => {
            let diagnostic = preview_client::diagnose_auth(&token_env, &token_sha256_env)?;
            Ok((
                TunnelOutput {
                    command: "tunnel.preview_client.diagnose_auth".to_string(),
                    id: Some(token_env),
                    extra: TunnelExtra {
                        preview_client: Some(PreviewClientActionOutput {
                            report: None,
                            auth_diagnostic: Some(diagnostic),
                        }),
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
            host,
        } => {
            let status = preview_ingress::status_for_host(bind, domain, public_host_pattern, host)?;
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
            runner_local,
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
            server_id: server.unwrap_or_default(),
            runner_local,
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
            readiness_kind,
            require_listener,
            readiness_artifact,
            readiness_artifact_json_pointer,
            readiness_artifact_json_equals,
            readiness_stdout_regex,
            public_tunnel_backend,
            public_tunnel_command,
            public_tunnel_public_url,
            source_run_id,
            source_workflow_id,
        } => {
            let readiness_checks = build_readiness_checks(
                require_listener,
                readiness_artifact,
                readiness_artifact_json_pointer,
                readiness_artifact_json_equals,
                readiness_stdout_regex,
            )?;
            start_service(StartServiceTunnelSpec {
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
                readiness_kind: readiness_kind.into(),
                readiness_checks,
            })
        }
        TunnelServiceCommand::Stop { id } => stop_service(&id),
    }
}

fn build_readiness_checks(
    require_listener: bool,
    readiness_artifact: Option<PathBuf>,
    readiness_artifact_json_pointer: Option<String>,
    readiness_artifact_json_equals: Option<String>,
    readiness_stdout_regex: Option<String>,
) -> homeboy::core::Result<Vec<ServiceTunnelReadinessCheck>> {
    let mut checks = Vec::new();
    if require_listener {
        checks.push(ServiceTunnelReadinessCheck::TcpListener);
    }
    if let Some(pattern) = readiness_stdout_regex {
        checks.push(ServiceTunnelReadinessCheck::StdoutRegex { pattern });
    }
    if let Some(path) = readiness_artifact {
        let Some(pointer) = readiness_artifact_json_pointer else {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "readiness_artifact_json_pointer",
                "artifact readiness requires a JSON pointer",
                Some(path.display().to_string()),
                None,
            ));
        };
        let equals = readiness_artifact_json_equals.unwrap_or_else(|| "ready".to_string());
        checks.push(ServiceTunnelReadinessCheck::ArtifactJsonPointer {
            path: path.display().to_string(),
            pointer,
            equals,
        });
    }
    Ok(checks)
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
                server: Some("private-host".to_string()),
                runner_local: false,
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
    fn expose_service_command_supports_runner_local_without_server() {
        test_support::with_isolated_home(|_| {
            let (output, exit_code) = run_service(TunnelServiceCommand::Expose {
                id: "runner-local-preview".to_string(),
                server: None,
                runner_local: true,
                remote_host: "127.0.0.1".to_string(),
                remote_port: 7331,
                scheme: "http".to_string(),
                local_port: Some(8831),
                auth_mode: ServiceTunnelAuthModeArg::SshOnly,
                auth_env: None,
                auth_header: None,
                allowed_clients: Vec::new(),
                description: None,
                preview_policy: ServiceTunnelPreviewPolicyArg::None,
                preview_keep_alive_until: None,
            })
            .expect("runner-local expose succeeds without server declaration");

            assert_eq!(exit_code, 0);
            assert_eq!(output.command, "tunnel.service.expose");
            assert_eq!(output.entity.expect("entity").id, "runner-local-preview");
        });
    }

    #[test]
    fn parses_public_result_url_from_configured_stdout_prefix() {
        let stdout = "Consumer ready\nPublic result URL: https://run.example.test/result\n";

        assert_eq!(
            parse_prefixed_line(stdout, "Public result URL:").as_deref(),
            Some("https://run.example.test/result")
        );
    }

    #[test]
    fn safe_artifact_slug_keeps_consumer_id_human_readable() {
        assert_eq!(
            safe_artifact_slug("preview consumer: sample"),
            "preview-consumer--sample"
        );
    }

    #[test]
    fn preview_consumer_run_uses_configured_public_url_and_artifacts() {
        test_support::with_isolated_home(|_| {
            let checkout = tempfile::tempdir().expect("checkout");
            let script = checkout.path().join("consumer.mjs");
            fs::write(
                &script,
                r#"
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
const artifacts = process.argv[process.argv.indexOf('--artifacts') + 1];
const publicUrl = process.argv[process.argv.indexOf('--public-url') + 1];
mkdirSync(artifacts, { recursive: true });
writeFileSync(join(artifacts, 'result.json'), JSON.stringify({ public_result_url: `${publicUrl}/result` }));
console.log(`Public result URL: ${publicUrl}/result`);
"#,
            )
            .expect("script");
            let artifacts = tempfile::tempdir().expect("artifacts");
            let config = tempfile::NamedTempFile::new().expect("config");
            fs::write(
                config.path(),
                serde_json::json!({
                    "id": "sample-consumer",
                    "command": {
                        "program": "node",
                        "args": [
                            script.display().to_string(),
                            "--public-url",
                            "${preview_public_url}",
                            "--artifacts",
                            "${artifacts_dir}"
                        ],
                        "artifacts_dir": artifacts.path()
                    },
                    "output": {
                        "public_result_json_file": "result.json",
                        "public_result_json_pointer": "/public_result_url",
                        "public_result_stdout_prefix": "Public result URL:"
                    }
                })
                .to_string(),
            )
            .expect("config json");

            let (output, exit_code) = run_preview_consumer_config(
                config.path().to_path_buf(),
                None,
                Some("https://run.example.test".to_string()),
                None,
            )
            .expect("preview consumer command succeeds");

            assert_eq!(exit_code, 0);
            let result = output
                .extra
                .preview_consumer
                .expect("preview consumer output");
            assert_eq!(
                result.public_result_url.as_deref(),
                Some("https://run.example.test/result")
            );
            assert!(artifacts
                .path()
                .join("homeboy-preview-consumer.json")
                .exists());
        });
    }
}
