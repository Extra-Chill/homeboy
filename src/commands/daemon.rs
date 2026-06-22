use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
        DaemonCommand::Start { addr } => start(&addr),
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
    let daemon_url = resolve_daemon_url(args.daemon_url)?;
    let content_url = artifact_content_url(&daemon_url, &args.run_id, &args.artifact_id)?;
    let output_path = args
        .output
        .unwrap_or_else(|| default_artifact_output_path(&args.artifact_id));
    let response = reqwest::blocking::get(&content_url).map_err(reqwest_error)?;
    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(homeboy::core::Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "daemon artifact fetch failed with HTTP {}: {}",
                status.as_u16(),
                body
            ),
            Some(args.artifact_id),
            None,
        ));
    }

    let bytes = response.bytes().map_err(reqwest_error)?;
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            homeboy::core::Error::internal_io(
                e.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    std::fs::write(&output_path, &bytes).map_err(|e| {
        homeboy::core::Error::internal_io(
            e.to_string(),
            Some(format!("write {}", output_path.display())),
        )
    })?;

    Ok((
        DaemonOutput::ArtifactGet(DaemonArtifactGetOutput {
            command: "daemon.artifact.get",
            run_id: args.run_id,
            artifact_id: args.artifact_id,
            daemon_url,
            content_url,
            output_path: output_path.display().to_string(),
            content_type: header_value(&headers, reqwest::header::CONTENT_TYPE.as_str()),
            size_bytes: Some(bytes.len() as u64),
            sha256: header_value(&headers, "x-homeboy-artifact-sha256"),
        }),
        0,
    ))
}

fn resolve_daemon_url(daemon_url: Option<String>) -> homeboy::core::Result<String> {
    if let Some(url) = daemon_url.filter(|url| !url.trim().is_empty()) {
        return Ok(url);
    }
    let status = daemon::read_status()?;
    let Some(state) = status.state.filter(|_| status.running) else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "daemon-url",
            "daemon is not running; pass --daemon-url or start it with `homeboy daemon start`",
            None,
            None,
        ));
    };
    Ok(format!("http://{}", state.address))
}

fn artifact_content_url(
    daemon_url: &str,
    run_id: &str,
    artifact_id: &str,
) -> homeboy::core::Result<String> {
    let mut base = reqwest::Url::parse(daemon_url).map_err(|e| {
        homeboy::core::Error::validation_invalid_argument(
            "daemon-url",
            e.to_string(),
            Some(daemon_url.to_string()),
            None,
        )
    })?;
    base.set_path(&format!(
        "/runs/{}/artifacts/{}/content",
        homeboy::core::execution_contract::encode_uri_component(run_id),
        homeboy::core::execution_contract::encode_uri_component(artifact_id)
    ));
    base.set_query(None);
    Ok(base.to_string())
}

fn default_artifact_output_path(artifact_id: &str) -> PathBuf {
    artifact_id
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("artifact.bin"))
}

fn header_value(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn reqwest_error(error: reqwest::Error) -> homeboy::core::Error {
    homeboy::core::Error::internal_io(error.to_string(), Some("fetch daemon artifact".to_string()))
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

fn start(addr: &str) -> CmdResult<DaemonOutput> {
    daemon::parse_bind_addr(addr)?;

    let exe = std::env::current_exe().map_err(|e| {
        homeboy::core::Error::internal_io(
            e.to_string(),
            Some("resolve current executable".to_string()),
        )
    })?;
    let child = Command::new(exe)
        .args(["daemon", "serve", "--addr", addr])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            homeboy::core::Error::internal_io(e.to_string(), Some("spawn daemon".to_string()))
        })?;
    let pid = child.id();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = daemon::read_status()?;
        if let Some(state) = status.state {
            if state.pid == pid {
                return Ok((
                    DaemonOutput::Start(DaemonStartResult {
                        pid,
                        address: state.address,
                        state_path: state.state_path,
                    }),
                    0,
                ));
            }
        }

        if Instant::now() >= deadline {
            return Err(homeboy::core::Error::internal_unexpected(format!(
                "daemon process {} did not publish state before timeout",
                pid
            )));
        }

        thread::sleep(Duration::from_millis(50));
    }
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
    fn artifact_content_url_builds_encoded_daemon_byte_alias() {
        let url = artifact_content_url(
            "http://127.0.0.1:7421/base?ignored=true",
            "run 1",
            "report/summary.json",
        )
        .expect("url");

        assert_eq!(
            url,
            "http://127.0.0.1:7421/runs/run%201/artifacts/report%2Fsummary.json/content"
        );
    }

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
