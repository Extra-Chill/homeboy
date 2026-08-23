use super::SshClient;
use crate::engine::command::{
    wait_with_bounded_output_supervised_with_progress, ControllerChildGuard,
    SupervisedCommandHeartbeat, DEFAULT_CAPTURE_LIMIT_BYTES,
};
use crate::server::ssh_args::{client_option_args, shell_join_args, SshArgOptions, SshPortFlag};
use serde::Serialize;
use std::process::Command;
use std::time::Duration;

const TRANSFER_HEARTBEAT_QUIET_AFTER: Duration = Duration::from_secs(5);
const TRANSFER_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const TRANSFER_HEARTBEAT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Configuration for a file transfer operation.
pub struct TransferConfig {
    /// Source: local path or server_id:/path
    pub source: String,
    /// Destination: local path or server_id:/path
    pub destination: String,
    /// Transfer directories recursively
    pub recursive: bool,
    /// Target a directory's contents rather than the directory itself.
    pub directory_contents: bool,
    /// Compress data during transfer
    pub compress: bool,
    /// Show what would be transferred without doing it
    pub dry_run: bool,
    /// Exclude patterns
    pub exclude: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TransferOutput {
    pub source: String,
    pub destination: String,
    /// Source passed to the transfer backend after directory semantics are resolved.
    pub effective_source: String,
    /// Destination directory receiving the transfer contents.
    pub effective_destination: String,
    pub method: String,
    pub direction: String,
    pub recursive: bool,
    /// Effective recursive scope, including sync's directory-contents behavior.
    pub scope: String,
    pub compress: bool,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub dry_run: bool,
}

fn transfer_output(
    config: &TransferConfig,
    method: impl Into<String>,
    direction: impl Into<String>,
    success: bool,
    error: Option<String>,
    dry_run: bool,
) -> TransferOutput {
    TransferOutput {
        source: config.source.clone(),
        destination: config.destination.clone(),
        effective_source: effective_source(config),
        effective_destination: config.destination.clone(),
        method: method.into(),
        direction: direction.into(),
        recursive: config.recursive,
        scope: transfer_scope(config).to_string(),
        compress: config.compress,
        success,
        error,
        dry_run,
    }
}

fn transfer_scope(config: &TransferConfig) -> &'static str {
    match (config.recursive, config.directory_contents) {
        (true, true) => "recursive directory contents",
        (true, false) => "recursive path",
        (false, _) => "single path",
    }
}

fn effective_source(config: &TransferConfig) -> String {
    if config.directory_contents {
        directory_contents_path(&config.source)
    } else {
        config.source.clone()
    }
}

fn directory_contents_path(path: &str) -> String {
    format!("{}/.", path.trim_end_matches('/'))
}

/// A parsed transfer target: either local or remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferTarget {
    Local(String),
    Remote { server_id: String, path: String },
}

/// Parse a transfer target.
///
/// If the target contains "server_id:/path", it's remote.
/// If it starts with "/", "./", "../", "~", or is "." it's local.
/// Otherwise try to parse as server_id:/path, falling back to local.
pub fn parse_target(target: &str) -> TransferTarget {
    // Explicit local paths
    if target.starts_with('/')
        || target.starts_with("./")
        || target.starts_with("../")
        || target.starts_with('~')
        || target == "."
    {
        return TransferTarget::Local(target.to_string());
    }

    // Try server_id:/path split
    if let Some(colon_pos) = target.find(':') {
        let server_part = &target[..colon_pos];
        let path_part = &target[colon_pos + 1..];

        // Must have a non-empty path after the colon
        // and the server part must look like an ID (no slashes)
        if !path_part.is_empty() && !server_part.contains('/') && !server_part.is_empty() {
            return TransferTarget::Remote {
                server_id: server_part.to_string(),
                path: path_part.to_string(),
            };
        }
    }

    // Default: treat as local path
    TransferTarget::Local(target.to_string())
}

enum TransferBackend {
    Scp { args: Vec<String> },
    Shell { command: String },
}

struct TransferPlan {
    method: String,
    direction: String,
    backend: TransferBackend,
}

impl TransferPlan {
    fn dry_run_output(&self, config: &TransferConfig) -> (TransferOutput, i32) {
        (
            transfer_output(config, &self.method, &self.direction, true, None, true),
            0,
        )
    }
}

/// Execute a file transfer between local and remote paths, or between two servers.
///
/// Returns `(TransferOutput, exit_code)` where exit_code is 0 on success.
pub fn transfer(config: &TransferConfig) -> crate::Result<(TransferOutput, i32)> {
    let source = parse_target(&config.source);
    let dest = parse_target(&config.destination);

    match (&source, &dest) {
        (TransferTarget::Local(_), TransferTarget::Local(_)) => {
            Err(crate::Error::validation_invalid_argument(
                "target",
                "Both source and destination are local paths. At least one must be a remote server",
                None,
                Some(vec![
                    "Upload to server: homeboy file copy ./file server:/path/to/file".to_string(),
                    "Copy from server: homeboy file copy server:/path/to/file ./local-copy"
                        .to_string(),
                ]),
            ))
        }
        (TransferTarget::Local(local_path), TransferTarget::Remote { server_id, path }) => {
            execute_plan(config, plan_push(config, local_path, server_id, path)?)
        }
        (TransferTarget::Remote { server_id, path }, TransferTarget::Local(local_path)) => {
            execute_plan(config, plan_pull(config, server_id, path, local_path)?)
        }
        (
            TransferTarget::Remote {
                server_id: src_id,
                path: src_path,
            },
            TransferTarget::Remote {
                server_id: dst_id,
                path: dst_path,
            },
        ) => execute_plan(
            config,
            plan_server_to_server(config, src_id, src_path, dst_id, dst_path)?,
        ),
    }
}

/// Push a local file/directory to a remote server via scp.
fn plan_push(
    config: &TransferConfig,
    local_path: &str,
    server_id: &str,
    remote_path: &str,
) -> crate::Result<TransferPlan> {
    let srv = super::load(server_id)?;
    let client = SshClient::from_server(&srv, server_id)?;

    let remote_target = format!("{}@{}:{}", client.user, client.host, remote_path);
    let effective_local_path = if config.directory_contents {
        directory_contents_path(local_path)
    } else {
        local_path.to_string()
    };

    if config.dry_run {
        log_status!(
            "dry-run",
            "Would push {} -> {}:{}",
            effective_local_path,
            server_id,
            remote_path
        );
        return Ok(TransferPlan {
            method: "scp".to_string(),
            direction: "push".to_string(),
            backend: TransferBackend::Scp { args: Vec::new() },
        });
    }

    // Validate local path exists
    let local = std::path::Path::new(local_path);
    if !local.exists() {
        return Err(crate::Error::validation_invalid_argument(
            "source",
            format!("Local path does not exist: {}", local_path),
            None,
            None,
        ));
    }

    let mut scp_args = scp_args(&client);

    if config.recursive || local.is_dir() {
        scp_args.push("-r".to_string());
    }
    if config.compress {
        scp_args.push("-C".to_string());
    }

    scp_args.push(effective_local_path.clone());
    scp_args.push(remote_target);

    log_status!(
        "transfer",
        "Pushing {} -> {}:{}",
        effective_local_path,
        server_id,
        remote_path
    );

    Ok(TransferPlan {
        method: "scp".to_string(),
        direction: "push".to_string(),
        backend: TransferBackend::Scp { args: scp_args },
    })
}

/// Pull a remote file/directory to a local path via scp.
fn plan_pull(
    config: &TransferConfig,
    server_id: &str,
    remote_path: &str,
    local_path: &str,
) -> crate::Result<TransferPlan> {
    let srv = super::load(server_id)?;
    let client = SshClient::from_server(&srv, server_id)?;

    let effective_remote_path = if config.directory_contents {
        directory_contents_path(remote_path)
    } else {
        remote_path.to_string()
    };
    let remote_target = format!("{}@{}:{}", client.user, client.host, effective_remote_path);

    if config.dry_run {
        log_status!(
            "dry-run",
            "Would pull {}:{} -> {}",
            server_id,
            effective_remote_path,
            local_path
        );
        return Ok(TransferPlan {
            method: "scp".to_string(),
            direction: "pull".to_string(),
            backend: TransferBackend::Scp { args: Vec::new() },
        });
    }

    // Ensure parent directory exists for local destination
    let local = std::path::Path::new(local_path);
    if let Some(parent) = local.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::Error::internal_io(
                    e.to_string(),
                    Some(format!("create directory {}", parent.display())),
                )
            })?;
        }
    }

    let mut scp_args = scp_args(&client);

    if config.recursive {
        scp_args.push("-r".to_string());
    }
    if config.compress {
        scp_args.push("-C".to_string());
    }

    scp_args.push(remote_target);
    scp_args.push(local_path.to_string());

    log_status!(
        "transfer",
        "Pulling {}:{} -> {}",
        server_id,
        effective_remote_path,
        local_path
    );

    Ok(TransferPlan {
        method: "scp".to_string(),
        direction: "pull".to_string(),
        backend: TransferBackend::Scp { args: scp_args },
    })
}

/// Transfer between two remote servers via SSH tar pipe.
fn plan_server_to_server(
    config: &TransferConfig,
    src_id: &str,
    src_path: &str,
    dst_id: &str,
    dst_path: &str,
) -> crate::Result<TransferPlan> {
    let src_server = super::load(src_id)?;
    let dst_server = super::load(dst_id)?;

    let src_client = SshClient::from_server(&src_server, src_id)?;
    let dst_client = SshClient::from_server(&dst_server, dst_id)?;

    if config.dry_run {
        let method = if config.recursive {
            "tar-pipe"
        } else {
            "scp-pipe"
        };
        log_status!(
            "dry-run",
            "Would transfer {}:{} -> {}:{}",
            src_id,
            src_path,
            dst_id,
            dst_path
        );
        log_status!("dry-run", "Method: {}", method);
        return Ok(TransferPlan {
            method: method.to_string(),
            direction: "server-to-server".to_string(),
            backend: TransferBackend::Shell {
                command: String::new(),
            },
        });
    }

    let source_ssh_args = ssh_shell_args(&src_client);
    let dest_ssh_args = ssh_shell_args(&dst_client);

    let source_remote = format!("{}@{}", src_client.user, src_client.host);
    let dest_remote = format!("{}@{}", dst_client.user, dst_client.host);

    let (method, command) = if config.recursive || src_path.ends_with('/') {
        let tar_compress_flag = if config.compress { "z" } else { "" };

        let exclude_args: String = config
            .exclude
            .iter()
            .map(|e| format!(" --exclude='{}'", e))
            .collect();

        let cmd = format!(
            "ssh {} {} 'tar c{}f - -C \"{}\" .{}' | ssh {} {} 'mkdir -p \"{}\" && tar x{}f - -C \"{}\"'",
            source_ssh_args,
            source_remote,
            tar_compress_flag,
            src_path.trim_end_matches('/'),
            exclude_args,
            dest_ssh_args,
            dest_remote,
            dst_path.trim_end_matches('/'),
            tar_compress_flag,
            dst_path.trim_end_matches('/'),
        );

        ("tar-pipe".to_string(), cmd)
    } else {
        let cmd = format!(
            "ssh {} {} 'cat \"{}\"' | ssh {} {} 'cat > \"{}\"'",
            source_ssh_args, source_remote, src_path, dest_ssh_args, dest_remote, dst_path,
        );

        ("cat-pipe".to_string(), cmd)
    };

    Ok(TransferPlan {
        method,
        direction: "server-to-server".to_string(),
        backend: TransferBackend::Shell { command },
    })
}

fn execute_plan(
    config: &TransferConfig,
    plan: TransferPlan,
) -> crate::Result<(TransferOutput, i32)> {
    if config.dry_run {
        return Ok(plan.dry_run_output(config));
    }

    if matches!(&plan.backend, TransferBackend::Shell { .. }) {
        log_status!("transfer", "{} -> {}", config.source, config.destination);
        log_status!("transfer", "Method: {}", plan.method);
    }

    let mut command = match &plan.backend {
        TransferBackend::Scp { args } => {
            let mut command = Command::new("scp");
            command.args(args);
            command
        }
        TransferBackend::Shell { command } => {
            let mut process = Command::new("sh");
            process.args(["-c", command]);
            process
        }
    };

    let backend_label = match plan.backend {
        TransferBackend::Scp { .. } => "scp",
        TransferBackend::Shell { .. } => "transfer",
    };

    let source = bounded_identity(&effective_source(config));
    let destination = bounded_identity(&effective_destination(config));
    let scope = transfer_scope(config);
    match run_transfer_command(&mut command, |elapsed| {
        eprintln!(
            "[transfer] phase=transferring elapsed={}s source={} destination={} scope={}",
            elapsed.as_secs(),
            source,
            destination,
            scope
        );
    }) {
        Ok(out) => {
            let success = out.status.success();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();

            if !success {
                let detail = bounded_detail(stderr.trim());
                eprintln!(
                    "[transfer] phase=failed source={} destination={} scope={} detail={}; destination may be partial; retry: {}",
                    source,
                    destination,
                    scope,
                    detail,
                    retry_command(config),
                );
            } else {
                log_status!("transfer", "Complete");
            }

            let error = (!success).then(|| {
                format!(
                    "{}; destination may be partial; retry: {}",
                    bounded_detail(stderr.trim()),
                    retry_command(config)
                )
            });

            Ok((
                transfer_output(config, plan.method, plan.direction, success, error, false),
                if success { 0 } else { 1 },
            ))
        }
        Err(e) => {
            let error = format!(
                "Failed to execute {backend_label}: {e}; destination may be partial; retry: {}",
                retry_command(config)
            );
            eprintln!(
                "[transfer] phase=failed source={} destination={} scope={}; destination may be partial; retry: {}",
                source,
                destination,
                scope,
                retry_command(config),
            );
            Ok((
                transfer_output(
                    config,
                    plan.method,
                    plan.direction,
                    false,
                    Some(error),
                    false,
                ),
                1,
            ))
        }
    }
}

fn effective_destination(config: &TransferConfig) -> String {
    config.destination.clone()
}

fn bounded_identity(value: &str) -> String {
    bounded_text(value, 160)
}

fn bounded_detail(value: &str) -> String {
    bounded_text(value, 1024)
}

fn bounded_text(value: &str, limit: usize) -> String {
    let mut value = value.replace(['\n', '\r'], " ");
    if value.len() > limit {
        let mut end = limit - 3;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        value.push_str("...");
    }
    value
}

fn run_transfer_command(
    command: &mut Command,
    heartbeat: impl FnMut(Duration),
) -> std::io::Result<crate::engine::command::BoundedCommandOutput> {
    run_transfer_command_with_policy(
        command,
        TRANSFER_HEARTBEAT_QUIET_AFTER,
        TRANSFER_HEARTBEAT_INTERVAL,
        TRANSFER_HEARTBEAT_POLL_INTERVAL,
        heartbeat,
    )
}

fn run_transfer_command_with_policy(
    command: &mut Command,
    quiet_after: Duration,
    interval: Duration,
    poll_interval: Duration,
    mut heartbeat: impl FnMut(Duration),
) -> std::io::Result<crate::engine::command::BoundedCommandOutput> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let guard = ControllerChildGuard::prepare(command)?;
    let mut child = command.spawn()?;
    guard.attach(&child)?;
    let mut schedule = TransferHeartbeatSchedule::new(quiet_after);
    let output = wait_with_bounded_output_supervised_with_progress(
        &mut child,
        DEFAULT_CAPTURE_LIMIT_BYTES,
        Duration::MAX,
        None,
        poll_interval,
        || false,
        |progress| {
            if schedule.due(&progress, interval) {
                heartbeat(progress.elapsed);
            }
            Ok(())
        },
    )?;
    Ok(output.output)
}

#[derive(Debug)]
struct TransferHeartbeatSchedule {
    next: Duration,
}

impl TransferHeartbeatSchedule {
    fn new(quiet_after: Duration) -> Self {
        Self { next: quiet_after }
    }

    fn due(&mut self, heartbeat: &SupervisedCommandHeartbeat, interval: Duration) -> bool {
        if heartbeat.elapsed < self.next {
            return false;
        }
        self.next = heartbeat.elapsed.saturating_add(interval);
        true
    }
}

fn retry_command(config: &TransferConfig) -> String {
    let command = if config.directory_contents {
        "sync"
    } else {
        "copy"
    };
    let mut parts = vec![
        "homeboy".to_string(),
        "file".to_string(),
        command.to_string(),
        shell_quote(&config.source),
        shell_quote(&config.destination),
    ];
    if config.recursive && !config.directory_contents {
        parts.push("--recursive".to_string());
    }
    if config.compress {
        parts.push("--compress".to_string());
    }
    for exclude in &config.exclude {
        parts.push("--exclude".to_string());
        parts.push(shell_quote(exclude));
    }
    parts.join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn scp_args(client: &SshClient) -> Vec<String> {
    client_option_args(
        client,
        SshArgOptions {
            strict_host_key_checking_no: true,
            batch_mode: true,
            legacy_scp: true,
            port_flag: Some(SshPortFlag::Uppercase),
            ..SshArgOptions::default()
        },
    )
}

fn ssh_shell_args(client: &SshClient) -> String {
    shell_join_args(&client_option_args(
        client,
        SshArgOptions {
            strict_host_key_checking_no: true,
            batch_mode: true,
            port_flag: Some(SshPortFlag::Lowercase),
            ..SshArgOptions::default()
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::process::Command;
    use std::time::Duration;

    use crate::server::{self, Server};
    use crate::test_support::with_isolated_home;

    use super::{
        bounded_detail, bounded_identity, execute_plan, parse_target, retry_command,
        run_transfer_command_with_policy, scp_args, transfer, transfer_scope, TransferBackend,
        TransferConfig, TransferPlan, TransferTarget,
    };
    use crate::server::{ManagedSshSession, SshClient};

    fn save_server(id: &str) {
        server::save(&Server {
            id: id.to_string(),
            aliases: Vec::new(),
            host: "example.test".to_string(),
            user: "deploy".to_string(),
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
    fn test_parse_target() {
        assert_eq!(
            parse_target("prod:/var/www"),
            TransferTarget::Remote {
                server_id: "prod".to_string(),
                path: "/var/www".to_string(),
            }
        );
        assert_eq!(
            parse_target("./artifact.zip"),
            TransferTarget::Local("./artifact.zip".to_string())
        );
        assert_eq!(
            parse_target("relative/artifact.zip"),
            TransferTarget::Local("relative/artifact.zip".to_string())
        );
    }

    #[test]
    fn test_transfer() {
        with_isolated_home(|_| {
            save_server("prod");

            let (out, code) = transfer(&TransferConfig {
                source: "./missing-artifact.zip".to_string(),
                destination: "prod:/tmp/artifact.zip".to_string(),
                recursive: false,
                directory_contents: false,
                compress: true,
                dry_run: true,
                exclude: Vec::new(),
            })
            .expect("dry run transfer");

            assert_eq!(code, 0);
            assert_eq!(out.direction, "push");
            assert_eq!(out.method, "scp");
            assert!(out.compress);
            assert!(out.dry_run);
            assert!(out.success);
        });
    }

    #[test]
    fn dry_run_remote_to_remote_preserves_recursive_options() {
        with_isolated_home(|_| {
            save_server("old");
            save_server("new");

            let (out, code) = transfer(&TransferConfig {
                source: "old:/var/www/uploads".to_string(),
                destination: "new:/var/www/uploads".to_string(),
                recursive: true,
                directory_contents: false,
                compress: true,
                dry_run: true,
                exclude: vec!["cache".to_string()],
            })
            .expect("dry run server transfer");

            assert_eq!(code, 0);
            assert_eq!(out.direction, "server-to-server");
            assert_eq!(out.method, "tar-pipe");
            assert!(out.recursive);
            assert!(out.compress);
            assert!(out.dry_run);
        });
    }

    #[test]
    fn sync_to_existing_destination_plans_directory_contents() {
        with_isolated_home(|_| {
            save_server("sandbox");

            let (out, code) = transfer(&TransferConfig {
                source: "local/existing-dir".to_string(),
                destination: "sandbox:/existing/same-dir".to_string(),
                recursive: true,
                directory_contents: true,
                compress: false,
                dry_run: true,
                exclude: Vec::new(),
            })
            .expect("dry run sync");

            assert_eq!(code, 0);
            assert_eq!(out.effective_source, "local/existing-dir/.");
            assert_eq!(out.effective_destination, "sandbox:/existing/same-dir");
        });
    }

    #[test]
    fn sync_to_missing_destination_plans_directory_contents() {
        with_isolated_home(|_| {
            save_server("sandbox");

            let (out, code) = transfer(&TransferConfig {
                source: "local/existing-dir".to_string(),
                destination: "sandbox:/missing/same-dir".to_string(),
                recursive: true,
                directory_contents: true,
                compress: false,
                dry_run: true,
                exclude: Vec::new(),
            })
            .expect("dry run sync");

            assert_eq!(code, 0);
            assert_eq!(out.effective_source, "local/existing-dir/.");
            assert_eq!(out.effective_destination, "sandbox:/missing/same-dir");
        });
    }

    #[test]
    fn managed_scp_uses_controlpath_option_not_ssh_executable_flag() {
        let client = SshClient {
            host: "sandbox-alias".to_string(),
            user: "deploy".to_string(),
            port: 22,
            identity_file: None,
            auth: Some(ManagedSshSession {
                control_path: "/tmp/homeboy-control".to_string(),
                persist: "4h".to_string(),
                persist_source: crate::server::ManagedSshSessionPersistSource::Configured,
            }),
            is_local: false,
            env: HashMap::new(),
        };

        let args = scp_args(&client);

        assert!(args.windows(2).any(|pair| {
            pair == [
                "-o".to_string(),
                "ControlPath=/tmp/homeboy-control".to_string(),
            ]
        }));
        assert!(args.contains(&"ControlMaster=no".to_string()));
        assert!(!args.contains(&"-S".to_string()));
    }

    #[test]
    fn recursive_sync_scope_and_retry_command_preserve_effective_intent() {
        let config = TransferConfig {
            source: "local dir's contents".to_string(),
            destination: "prod:/var/www/site".to_string(),
            recursive: true,
            directory_contents: true,
            compress: true,
            dry_run: false,
            exclude: vec!["cache files".to_string()],
        };

        assert_eq!(transfer_scope(&config), "recursive directory contents");
        assert_eq!(
            retry_command(&config),
            "homeboy file sync 'local dir'\\''s contents' 'prod:/var/www/site' --compress --exclude 'cache files'"
        );
    }

    #[test]
    fn transfer_identity_is_single_line_and_bounded() {
        let identity = bounded_identity(&format!("source\n{}", "x".repeat(200)));

        assert!(!identity.contains('\n'));
        assert!(identity.ends_with("..."));
        assert!(identity.len() <= 160);
    }

    #[test]
    fn transfer_failure_detail_is_single_line_and_bounded() {
        let detail = super::bounded_detail(&format!("failed\r\n{}", "x".repeat(2000)));

        assert!(!detail.contains(['\r', '\n']));
        assert!(detail.ends_with("..."));
        assert!(detail.len() <= 1024);
    }

    #[test]
    fn bounded_transfer_text_preserves_utf8_boundaries() {
        let identity = bounded_identity(&format!("{}ézzz", "a".repeat(156)));
        let detail = bounded_detail(&format!("{}ézzz", "a".repeat(1020)));

        assert!(identity.is_char_boundary(identity.len()));
        assert!(identity.ends_with("..."));
        assert!(identity.len() <= 160);
        assert!(detail.is_char_boundary(detail.len()));
        assert!(detail.ends_with("..."));
        assert!(detail.len() <= 1024);
    }

    #[cfg(unix)]
    #[test]
    fn transfer_runner_heartbeats_and_bounds_noisy_failure_output() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "sleep 0.04; yes noisy-transfer-output | head -c 5000000 >&2; exit 7",
        ]);
        let mut heartbeats = Vec::new();

        let output = run_transfer_command_with_policy(
            &mut command,
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(5),
            |elapsed| heartbeats.push(elapsed),
        )
        .expect("supervise noisy transfer command");

        assert!(!output.status.success());
        assert!(!heartbeats.is_empty());
        assert!(heartbeats[0] >= Duration::from_millis(10));
        assert!(output.capture.stderr.truncated);
        assert!(output.stderr.len() <= super::DEFAULT_CAPTURE_LIMIT_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn transfer_plan_preserves_lossy_non_utf8_failure_diagnostics() {
        let config = TransferConfig {
            source: "local/source".to_string(),
            destination: "prod:/remote/destination".to_string(),
            recursive: false,
            directory_contents: false,
            compress: false,
            dry_run: false,
            exclude: Vec::new(),
        };
        let plan = TransferPlan {
            method: "fixture".to_string(),
            direction: "push".to_string(),
            backend: TransferBackend::Shell {
                command: "printf '\\377backend failure' >&2; exit 7".to_string(),
            },
        };

        let (output, code) = execute_plan(&config, plan).expect("execute transfer plan");

        assert_eq!(code, 1);
        assert!(!output.success);
        let error = output.error.expect("failure diagnostic");
        assert!(error.contains('\u{fffd}'));
        assert!(error.contains("backend failure"));
        assert!(error.contains("retry: homeboy file copy"));
    }
}
