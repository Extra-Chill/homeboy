use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::core::artifacts::{
    self, ArtifactOriginInspect, ArtifactOriginServeSpec, ArtifactOriginStatus,
};
use homeboy::core::preview_client::{
    self, PreviewClientAuthDiagnostic, PreviewClientReport, PreviewClientStartSpec,
};
use homeboy::core::preview_consumer;
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
use std::path::PathBuf;

use super::{CmdResult, DynamicSetArgs};
use crate::command_contract::{
    CommandPortabilityContract, LabCommandContract, TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL,
    TUNNEL_SERVICE_EXPOSE_LAB_LABEL, TUNNEL_SERVICE_START_LAB_LABEL,
};

mod service;
use service::TunnelServiceCommand;

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

/// Preview-consumer run output, owned by the core preview-consumer service.
pub use homeboy::core::preview_consumer::PreviewConsumerRunResult as PreviewConsumerOutput;

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

    pub(crate) fn portability_contract(&self) -> CommandPortabilityContract {
        if self.is_preview_consumer_run() {
            return CommandPortabilityContract::lab(LabCommandContract::explicit_runner_simple(
                TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL,
            ));
        }
        if self.is_service_start() {
            return CommandPortabilityContract::lab(LabCommandContract::runner_resident(
                TUNNEL_SERVICE_START_LAB_LABEL,
            ));
        }
        if self.is_service_expose() {
            return CommandPortabilityContract::lab(LabCommandContract::runner_resident(
                TUNNEL_SERVICE_EXPOSE_LAB_LABEL,
            ));
        }
        CommandPortabilityContract::none()
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

        /// Start the command under supervision and return as soon as the
        /// preview is ready, leaving the command running (held preview flows).
        #[arg(long)]
        non_blocking: bool,

        /// Seconds to wait for the preview to report ready in non-blocking mode
        /// before returning while leaving the command running.
        #[arg(long, requires = "non_blocking")]
        ready_timeout: Option<u64>,
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

    /// Operator-owned domain, e.g. example.com
    #[arg(long)]
    domain: String,

    /// Wildcard host pattern routed to the ingress, e.g. *-tunnel.example.com
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
            identity: homeboy::core::daemon::ServiceIdentity {
                service_user: self.user,
                service_group: self.group,
            },
        }
    }
}

pub fn run(args: TunnelArgs, _global: &super::GlobalArgs) -> CmdResult<TunnelOutput> {
    match args.command {
        TunnelCommand::Service { command } => service::run_service(command),
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
            non_blocking,
            ready_timeout,
        } => run_preview_consumer_config(
            config,
            service_id,
            preview_public_url,
            artifacts_dir,
            non_blocking,
            ready_timeout,
        ),
    }
}

fn run_preview_consumer_config(
    config_path: PathBuf,
    service_id: Option<String>,
    preview_public_url: Option<String>,
    artifacts_dir_override: Option<PathBuf>,
    non_blocking: bool,
    ready_timeout: Option<u64>,
) -> CmdResult<TunnelOutput> {
    let mode = if non_blocking {
        preview_consumer::PreviewConsumerRunMode::NonBlocking
    } else {
        preview_consumer::PreviewConsumerRunMode::Blocking
    };
    let (result, exit_code) = preview_consumer::run(preview_consumer::PreviewConsumerRunRequest {
        config_path,
        service_id,
        preview_public_url,
        artifacts_dir_override,
        mode,
        ready_timeout: ready_timeout.map(std::time::Duration::from_secs),
    })?;

    Ok((
        TunnelOutput {
            command: "tunnel.preview_consumer.run".to_string(),
            id: Some(result.consumer_id.clone()),
            extra: TunnelExtra {
                preview_consumer: Some(result),
                ..Default::default()
            },
            ..Default::default()
        },
        exit_code,
    ))
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

#[cfg(test)]
#[path = "../../tests/commands/tunnel_test.rs"]
mod tests;
