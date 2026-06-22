use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::path::PathBuf;

use homeboy::core::daemon::{
    self, BrokerConfig, BrokerConfigOptions, DaemonStartResult, DaemonStatus, DaemonStopResult,
    ServiceIdentity,
};
use homeboy::core::http_api::{AnalysisJobRunOutput, AnalysisJobRunner};

use super::CmdResult;

#[derive(Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the local daemon in the background
    Start {
        /// Local bind address. Defaults to an OS-selected loopback port.
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Run the local daemon in the foreground
    Serve {
        /// Local bind address. Defaults to an OS-selected loopback port.
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Stop the background daemon recorded in the state file
    Stop,
    /// Show daemon state and selected local address
    Status,
    /// Render deployable reverse-runner broker service configuration
    BrokerConfig {
        /// Stable loopback address for the VPS service.
        #[arg(long, default_value = "127.0.0.1:7421")]
        listen_addr: String,
        /// Homeboy binary path used by the service unit.
        #[arg(long, default_value = "/usr/local/bin/homeboy")]
        binary_path: String,
        /// System user that runs the broker service.
        #[arg(long, default_value = "homeboy")]
        user: String,
        /// System group that runs the broker service.
        #[arg(long, default_value = "homeboy")]
        group: String,
        /// Optional public hostname to render disabled Nginx/Caddy examples.
        #[arg(long)]
        domain: Option<String>,
    },
    /// Fetch artifact bytes through the local daemon byte endpoint
    ArtifactGet(DaemonArtifactGetArgs),
}

#[derive(Args, Clone)]
pub struct DaemonArtifactGetArgs {
    /// Observation run id that owns the artifact
    pub run_id: String,
    /// Artifact id/path token from daemon artifact metadata
    pub artifact_id: String,
    /// Destination file path. Defaults to the artifact id basename.
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,
    /// Daemon base URL. Defaults to the address from `homeboy daemon status`.
    #[arg(long = "daemon-url")]
    pub daemon_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DaemonOutput {
    Start(DaemonStartResult),
    Serve(DaemonStartResult),
    Stop(DaemonStopResult),
    Status(DaemonStatus),
    BrokerConfig(BrokerConfig),
    ArtifactGet(DaemonArtifactGetOutput),
}

#[derive(Debug, Serialize)]
pub struct DaemonArtifactGetOutput {
    pub command: &'static str,
    pub run_id: String,
    pub artifact_id: String,
    pub daemon_url: String,
    pub content_url: String,
    pub output_path: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
}

pub fn run(args: DaemonArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<DaemonOutput> {
    match args.command {
        DaemonCommand::Start { addr } => {
            Ok((DaemonOutput::Start(daemon::start_background(&addr)?), 0))
        }
        DaemonCommand::Serve { addr } => serve(&addr),
        DaemonCommand::Stop => Ok((DaemonOutput::Stop(daemon::stop()?), 0)),
        DaemonCommand::Status => Ok((DaemonOutput::Status(daemon::read_status()?), 0)),
        DaemonCommand::BrokerConfig {
            listen_addr,
            binary_path,
            user,
            group,
            domain,
        } => Ok((
            DaemonOutput::BrokerConfig(daemon::render_broker_config(BrokerConfigOptions {
                listen_addr,
                binary_path,
                identity: ServiceIdentity {
                    service_user: user,
                    service_group: group,
                },
                domain,
            })?),
            0,
        )),
        DaemonCommand::ArtifactGet(args) => artifact_get(args),
    }
}

fn artifact_get(args: DaemonArtifactGetArgs) -> CmdResult<DaemonOutput> {
    let outcome = daemon::fetch_artifact_to_path(
        &args.run_id,
        &args.artifact_id,
        args.daemon_url,
        args.output,
    )?;

    Ok((
        DaemonOutput::ArtifactGet(DaemonArtifactGetOutput {
            command: "daemon.artifact.get",
            run_id: args.run_id,
            artifact_id: args.artifact_id,
            daemon_url: outcome.daemon_url,
            content_url: outcome.content_url,
            output_path: outcome.output_path.display().to_string(),
            content_type: outcome.content_type,
            size_bytes: Some(outcome.size_bytes),
            sha256: outcome.sha256,
        }),
        0,
    ))
}

fn serve(addr: &str) -> CmdResult<DaemonOutput> {
    let parsed = daemon::parse_bind_addr(addr)?;
    let state = daemon::serve_with_analysis_runner(parsed, CommandAnalysisJobRunner)?;
    Ok((
        DaemonOutput::Serve(DaemonStartResult {
            pid: state.pid,
            address: state.address,
            state_path: state.state_path,
        }),
        0,
    ))
}

#[derive(Debug, Clone, Copy)]
struct CommandAnalysisJobRunner;

impl AnalysisJobRunner for CommandAnalysisJobRunner {
    fn run_analysis_job(&self, argv: Vec<String>) -> homeboy::core::Result<AnalysisJobRunOutput> {
        let cli = crate::cli_surface::Cli::try_parse_from(argv).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "body",
                error.to_string(),
                None,
                Some(vec![
                    "Use the documented JSON request body contract for this endpoint".to_string(),
                ]),
            )
        })?;
        let global = crate::commands::GlobalArgs {};
        let (result, exit_code) = crate::commands::json_output::run(cli.command, &global);
        Ok(AnalysisJobRunOutput {
            exit_code,
            output: result?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use homeboy::test_support::with_isolated_home;

    use super::*;

    #[test]
    fn artifact_get_downloads_daemon_byte_alias() {
        with_isolated_home(|home| {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            let addr = listener.local_addr().expect("addr");
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0; 1024];
                let bytes = stream.read(&mut request).expect("request");
                let request = String::from_utf8_lossy(&request[..bytes]);
                assert!(request.starts_with(
                    "GET /runs/run-1/artifacts/report%2Fsummary.json/content HTTP/1.1"
                ));
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nX-Homeboy-Artifact-Sha256: abc123\r\nConnection: close\r\n\r\n{\"ok\":true}",
                    )
                    .expect("response");
            });
            let output = home.path().join("summary.json");

            let (result, exit_code) = artifact_get(DaemonArtifactGetArgs {
                run_id: "run-1".to_string(),
                artifact_id: "report/summary.json".to_string(),
                output: Some(output.clone()),
                daemon_url: Some(format!("http://{addr}")),
            })
            .expect("artifact get");

            server.join().expect("server");
            assert_eq!(exit_code, 0);
            let DaemonOutput::ArtifactGet(result) = result else {
                panic!("expected artifact get output");
            };
            assert_eq!(result.command, "daemon.artifact.get");
            assert_eq!(result.content_type.as_deref(), Some("application/json"));
            assert_eq!(result.size_bytes, Some(11));
            assert_eq!(result.sha256.as_deref(), Some("abc123"));
            assert_eq!(std::fs::read(&output).expect("output"), br#"{"ok":true}"#);
        });
    }
}
