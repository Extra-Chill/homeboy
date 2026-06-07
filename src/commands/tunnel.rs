use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::core::tunnel::{
    self, ExposeServiceTunnelSpec, ServiceTunnel, ServiceTunnelAuth, ServiceTunnelAuthMode,
    ServiceTunnelExposure, ServiceTunnelPolicy, ServiceTunnelPreviewPolicy,
    ServiceTunnelPreviewPolicyMode, ServiceTunnelStatus, ServiceTunnelTarget,
    ServiceTunnelTunnelBackend, StartServiceTunnelSpec,
};
use homeboy::core::{EntityCrudOutput, MergeOutput};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{CmdResult, DynamicSetArgs};

#[derive(Debug, Default, Serialize)]
pub struct TunnelExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceTunnelActionOutput>,
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
}
