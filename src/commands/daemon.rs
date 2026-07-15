use clap::{Args, CommandFactory, Subcommand};
use serde::Serialize;
use std::path::PathBuf;
use uuid::Uuid;

use homeboy::core::daemon::{
    self, BrokerConfig, BrokerConfigOptions, DaemonLeaselessRecoveryResult, DaemonOrphanAdoptionResult, DaemonStartResult, DaemonStateLossRecoveryResult, DaemonStatus, DaemonStopResult,
    ServiceIdentity,
};
use homeboy::core::Error;
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
    /// Return the current live daemon or start one when no live daemon exists
    EnsureRunning {
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Explicitly replace one proven-dead daemon lease and reconcile its durable jobs
    AdoptOrphan {
        /// Exact lease ID reported by `homeboy daemon status`
        #[arg(long)]
        lease_id: String,
        /// Confirm the recorded PID was inspected and is dead
        #[arg(long)]
        confirm_pid_dead: bool,
        /// Accepted migration alias for legacy child recovery. It never mutates jobs.
        #[arg(long)]
        recover_missing_child_identity: bool,
        /// Accepted migration alias for one legacy untracked child. It never mutates jobs.
        #[arg(long = "confirm-untracked-child-dead")]
        confirm_untracked_child_dead: Vec<Uuid>,
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Recover one legacy job with exact PID and Linux start-tick evidence.
    RecoverMissingChildIdentity {
        #[arg(long)]
        lease_id: String,
        #[arg(long)]
        recorded_daemon_pid: u32,
        #[arg(long)]
        recorded_daemon_endpoint: String,
        #[arg(long)]
        job_id: Uuid,
        #[arg(long)]
        child_pid: u32,
        #[arg(long)]
        child_starttime_ticks: u64,
    },
    /// Explicitly reconcile active jobs after proving a missing-lease store has no daemon owner
    ReconcileLeaselessOrphans {
        #[arg(long)]
        confirm_no_daemon_owner: bool,
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Recover one exact lease after its daemon state record was lost
    RecoverMissingLeaseState {
        /// Exact lease ID captured before the daemon state record was lost
        #[arg(long)]
        lease_id: String,
        /// Recorded daemon PID captured with the lease ID
        #[arg(long)]
        recorded_pid: u32,
        /// Recorded concrete loopback endpoint captured with the lease ID
        #[arg(long)]
        recorded_endpoint: String,
        /// Confirm the recorded daemon PID was inspected and is dead
        #[arg(long)]
        confirm_pid_dead: bool,
        /// Confirm the daemon state record and endpoint are unavailable
        #[arg(long)]
        confirm_control_plane_lost: bool,
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Run the local daemon in the foreground
    Serve {
        /// Local bind address. Defaults to an OS-selected loopback port.
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Supervise one daemon child and persist its termination evidence.
    #[command(hide = true)]
    Supervise {
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
        #[arg(long)]
        startup_token: String,
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
    EnsureRunning(DaemonStartResult),
    AdoptOrphan(DaemonOrphanAdoptionResult),
    RecoverMissingChildIdentity(homeboy::core::api_jobs::Job),
    ReconcileLeaselessOrphans(DaemonLeaselessRecoveryResult),
    RecoverMissingLeaseState(DaemonStateLossRecoveryResult),
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
        DaemonCommand::EnsureRunning { addr } => Ok((
            DaemonOutput::EnsureRunning(daemon::ensure_running(&addr)?),
            0,
        )),
        DaemonCommand::AdoptOrphan {
            lease_id,
            confirm_pid_dead,
            recover_missing_child_identity,
            confirm_untracked_child_dead,
            addr,
        } => {
            if recover_missing_child_identity || !confirm_untracked_child_dead.is_empty() {
                return Err(legacy_child_recovery_migration_error(
                    recover_missing_child_identity,
                    &confirm_untracked_child_dead,
                ));
            }
            Ok((
                DaemonOutput::AdoptOrphan(daemon::adopt_orphaned_lease(
                    &lease_id,
                    confirm_pid_dead,
                    &addr,
                )?),
                0,
            ))
        }
        DaemonCommand::RecoverMissingChildIdentity { lease_id, recorded_daemon_pid, recorded_daemon_endpoint, job_id, child_pid, child_starttime_ticks } => Ok((
            DaemonOutput::RecoverMissingChildIdentity(daemon::recover_missing_child_identity(&lease_id, recorded_daemon_pid, &recorded_daemon_endpoint, job_id, child_pid, child_starttime_ticks)?),
            0,
        )),
        DaemonCommand::ReconcileLeaselessOrphans { confirm_no_daemon_owner, addr } => Ok((
            DaemonOutput::ReconcileLeaselessOrphans(daemon::reconcile_leaseless_orphans(confirm_no_daemon_owner, &addr)?),
            0,
        )),
        DaemonCommand::RecoverMissingLeaseState { lease_id, recorded_pid, recorded_endpoint, confirm_pid_dead, confirm_control_plane_lost, addr } => Ok((
            DaemonOutput::RecoverMissingLeaseState(daemon::recover_missing_lease_state(&lease_id, recorded_pid, &recorded_endpoint, confirm_pid_dead, confirm_control_plane_lost, &addr)?),
            0,
        )),
        DaemonCommand::Serve { addr } => serve(&addr),
        DaemonCommand::Supervise { addr, startup_token } => {
            daemon::supervise(&addr, &startup_token)?;
            Ok((DaemonOutput::Serve(DaemonStartResult { pid: std::process::id(), address: addr, state_path: String::new(), lease_id: String::new() }), 0))
        }
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

fn legacy_child_recovery_migration_error(
    recover_missing_child_identity: bool,
    confirm_untracked_child_dead: &[Uuid],
) -> Error {
    let pairing = if recover_missing_child_identity && !confirm_untracked_child_dead.is_empty() {
        "The released legacy flags were supplied together."
    } else {
        "The released legacy flags must be supplied together, but either form is now migration-only."
    };
    Error::validation_invalid_argument(
        "recover_missing_child_identity",
        format!(
            "{pairing} Homeboy will not bulk-terminalize legacy jobs; recover each job with exact persisted evidence instead"
        ),
        None,
        Some(vec![
            "Use `homeboy daemon recover-missing-child-identity --lease-id <expected-lease> --recorded-daemon-pid <recorded-daemon-pid> --recorded-daemon-endpoint <recorded-daemon-endpoint> --job-id <job-id> --child-pid <child-pid> --child-starttime-ticks <child-starttime-ticks>`.".to_string(),
            "Required exact evidence: expected lease, recorded daemon PID, recorded daemon endpoint, job ID, child PID, and child starttime ticks.".to_string(),
        ]),
    )
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
            lease_id: state.lease_id,
        }),
        0,
    ))
}

#[derive(Debug, Clone, Copy)]
struct CommandAnalysisJobRunner;

impl AnalysisJobRunner for CommandAnalysisJobRunner {
    fn run_analysis_job(&self, argv: Vec<String>) -> homeboy::core::Result<AnalysisJobRunOutput> {
        let matches = crate::cli_surface::Cli::command()
            .try_get_matches_from(argv)
            .map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "body",
                    error.to_string(),
                    None,
                    Some(vec![
                        "Use the documented JSON request body contract for this endpoint"
                            .to_string(),
                    ]),
                )
            })?;
        let (cli, spec) = crate::cli_surface::Cli::from_registered_arg_matches(&matches)
            .expect("validated arguments should produce a typed CLI");
        let global = crate::commands::GlobalArgs {};
        let (result, exit_code) = crate::commands::json_output::run(cli.command, spec, &global);
        Ok(AnalysisJobRunOutput {
            exit_code,
            output: result?,
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use homeboy::test_support::with_isolated_home;

    use super::*;
    use crate::cli_surface::{Cli, Commands};

    #[test]
    fn legacy_child_recovery_parser_requires_exact_evidence() {
        assert!(Cli::try_parse_from(["homeboy", "daemon", "recover-missing-child-identity"])
            .is_err());
        assert!(Cli::try_parse_from([
            "homeboy", "daemon", "recover-missing-child-identity", "--lease-id", "lease", "--recorded-daemon-pid", "nope",
        ]).is_err());
        assert!(Cli::try_parse_from([
            "homeboy", "daemon", "recover-missing-child-identity", "--lease-id", "lease", "--recorded-daemon-pid", "42", "--recorded-daemon-endpoint", "127.0.0.1:1", "--job-id", "not-a-uuid", "--child-pid", "43", "--child-starttime-ticks", "1",
        ]).is_err());
        assert!(Cli::try_parse_from([
            "homeboy", "daemon", "recover-missing-child-identity", "--lease-id", "lease", "--recorded-daemon-pid", "42", "--recorded-daemon-endpoint", "127.0.0.1:1", "--job-id", "00000000-0000-0000-0000-000000000001", "--child-pid", "43", "--child-starttime-ticks", "1",
        ]).is_ok());
    }

    #[test]
    fn legacy_adoption_flags_parse_but_return_exact_non_mutating_migration() {
        with_isolated_home(|_| {
            let jobs_path = homeboy::core::paths::daemon_jobs_file().expect("jobs path");
            let store = homeboy::core::api_jobs::JobStore::open_without_reconciliation(&jobs_path)
                .expect("store");
            let job = store.create("runner.exec");
            store.start(job.id).expect("start job");
            let before = std::fs::read(&jobs_path).expect("store bytes");
            let cli = Cli::try_parse_from([
                "homeboy",
                "daemon",
                "adopt-orphan",
                "--lease-id",
                "lease-dead",
                "--confirm-pid-dead",
                "--recover-missing-child-identity",
                "--confirm-untracked-child-dead",
                &job.id.to_string(),
            ])
            .expect("released aliases still parse");
            let Commands::Daemon(args) = cli.command else {
                panic!("expected daemon command");
            };

            let error = run(args, &crate::commands::GlobalArgs {})
                .expect_err("legacy aliases must not mutate");
            assert!(error.message.contains("will not bulk-terminalize"));
            let rendered = format!("{error:?}");
            for field in [
                "recover-missing-child-identity",
                "expected-lease",
                "recorded-daemon-pid",
                "recorded-daemon-endpoint",
                "job-id",
                "child-pid",
                "child-starttime-ticks",
            ] {
                assert!(rendered.contains(field), "missing remediation field {field}");
            }
            assert_eq!(std::fs::read(&jobs_path).expect("store bytes"), before);
        });
    }

    #[test]
    fn unpaired_legacy_adoption_alias_returns_the_same_migration_contract() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "daemon",
            "adopt-orphan",
            "--lease-id",
            "lease-dead",
            "--recover-missing-child-identity",
        ])
        .expect("one released alias still parses");
        let Commands::Daemon(args) = cli.command else {
            panic!("expected daemon command");
        };

        let error = run(args, &crate::commands::GlobalArgs {})
            .expect_err("unpaired legacy alias is migration-only");
        assert!(error.message.contains("must be supplied together"));
        assert!(format!("{error:?}").contains("recover-missing-child-identity"));
    }

    #[test]
    fn leaseless_recovery_subcommand_reaches_address_validation() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "daemon",
            "reconcile-leaseless-orphans",
            "--confirm-no-daemon-owner",
            "--addr",
            "not-an-address",
        ])
        .expect("the recovery subcommand and its required confirmation should parse");
        let Commands::Daemon(args) = cli.command else {
            panic!("expected daemon command");
        };

        let error = run(args, &crate::commands::GlobalArgs {})
            .expect_err("invalid daemon address should reach handler validation");

        assert!(error.message.contains("Invalid daemon bind address"));
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
