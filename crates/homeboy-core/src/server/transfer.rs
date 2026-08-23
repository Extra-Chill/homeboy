use super::SshClient;
use crate::engine::command::{
    wait_with_bounded_output_supervised_with_progress, ControllerChildGuard,
    SupervisedCommandHeartbeat, DEFAULT_CAPTURE_LIMIT_BYTES,
};
use crate::server::ssh_args::{
    client_option_args, client_ssh_args, shell_join_args, SshArgOptions, SshPortFlag,
};
use serde::Serialize;
use std::path::Path;
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
    /// Whether the transfer backend will recurse after resolving source semantics.
    pub effective_recursive: bool,
    /// Whether recursion was requested by the caller.
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
    effective_recursive: bool,
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
        effective_recursive,
        recursive: config.recursive,
        scope: transfer_scope(config, effective_recursive).to_string(),
        compress: config.compress,
        success,
        error,
        dry_run,
    }
}

fn transfer_scope(config: &TransferConfig, effective_recursive: bool) -> &'static str {
    match (effective_recursive, config.directory_contents) {
        (true, true) => "recursive directory contents",
        (true, false) => "recursive path",
        (false, _) => "single path",
    }
}

fn effective_source(config: &TransferConfig) -> String {
    config.source.clone()
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
    Scp {
        args: Vec<String>,
        prepare: Option<Vec<String>>,
        has_sources: bool,
    },
    Shell {
        command: String,
    },
}

struct TransferPlan {
    method: String,
    direction: String,
    effective_recursive: bool,
    backend: TransferBackend,
}

impl TransferPlan {
    fn dry_run_output(&self, config: &TransferConfig) -> (TransferOutput, i32) {
        (
            transfer_output(
                config,
                &self.method,
                &self.direction,
                self.effective_recursive,
                true,
                None,
                true,
            ),
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
    let effective_local_path = local_path.to_string();
    let effective_recursive = config.recursive || Path::new(local_path).is_dir();

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
            effective_recursive,
            backend: TransferBackend::Scp {
                args: Vec::new(),
                prepare: None,
                has_sources: false,
            },
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

    if effective_recursive {
        scp_args.push("-r".to_string());
    }
    if config.compress {
        scp_args.push("-C".to_string());
    }

    let (prepare, sources) = if config.directory_contents && local.is_dir() {
        let command = format!(
            "mkdir -p -- {}",
            crate::engine::shell::quote_arg(remote_path)
        );
        let prepare = client_ssh_args(
            &client,
            SshArgOptions {
                strict_host_key_checking_no: true,
                batch_mode: true,
                port_flag: Some(SshPortFlag::Lowercase),
                command: Some(&command),
                ..SshArgOptions::default()
            },
        );
        let mut sources = std::fs::read_dir(local)
            .map_err(|error| {
                crate::Error::internal_io(
                    error.to_string(),
                    Some(format!("read directory {}", local.display())),
                )
            })?
            .map(|entry| {
                entry
                    .map(|entry| entry.path().to_string_lossy().into_owned())
                    .map_err(|error| {
                        crate::Error::internal_io(
                            error.to_string(),
                            Some(format!("read directory entry in {}", local.display())),
                        )
                    })
            })
            .collect::<crate::Result<Vec<_>>>()?;
        sources.sort();
        (Some(prepare), sources)
    } else {
        (None, vec![local_path.to_string()])
    };
    let has_sources = !sources.is_empty();
    scp_args.extend(sources);
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
        effective_recursive,
        backend: TransferBackend::Scp {
            args: scp_args,
            prepare,
            has_sources,
        },
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
            effective_recursive: config.recursive,
            backend: TransferBackend::Scp {
                args: Vec::new(),
                prepare: None,
                has_sources: false,
            },
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
        effective_recursive: config.recursive,
        backend: TransferBackend::Scp {
            args: scp_args,
            prepare: None,
            has_sources: true,
        },
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

    let effective_recursive = config.recursive || src_path.ends_with('/');

    if config.dry_run {
        let method = if effective_recursive {
            "tar-pipe"
        } else {
            "cat-pipe"
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
            effective_recursive,
            backend: TransferBackend::Shell {
                command: String::new(),
            },
        });
    }

    let source_ssh_args = ssh_shell_args(&src_client);
    let dest_ssh_args = ssh_shell_args(&dst_client);

    let source_remote = format!("{}@{}", src_client.user, src_client.host);
    let dest_remote = format!("{}@{}", dst_client.user, dst_client.host);

    let (method, command) = if effective_recursive {
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
        effective_recursive,
        backend: TransferBackend::Shell { command },
    })
}

fn execute_plan(
    config: &TransferConfig,
    plan: TransferPlan,
) -> crate::Result<(TransferOutput, i32)> {
    execute_plan_with(config, plan, Path::new("scp"), Path::new("ssh"))
}

fn execute_plan_with(
    config: &TransferConfig,
    plan: TransferPlan,
    scp_program: &Path,
    ssh_program: &Path,
) -> crate::Result<(TransferOutput, i32)> {
    if config.dry_run {
        return Ok(plan.dry_run_output(config));
    }

    if matches!(&plan.backend, TransferBackend::Shell { .. }) {
        log_status!("transfer", "{} -> {}", config.source, config.destination);
        log_status!("transfer", "Method: {}", plan.method);
    }

    let source = bounded_identity(&effective_source(config));
    let destination = bounded_identity(&effective_destination(config));
    let scope = transfer_scope(config, plan.effective_recursive);

    if let TransferBackend::Scp {
        prepare: Some(args),
        ..
    } = &plan.backend
    {
        let mut command = Command::new(ssh_program);
        command.args(args);
        match run_transfer_command(&mut command, |elapsed| {
            eprintln!(
                "[transfer] phase=preparing elapsed={}s source={} destination={} scope={}",
                elapsed.as_secs(),
                source,
                destination,
                scope
            );
        }) {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                return transfer_backend_failure(
                    config,
                    plan.method,
                    plan.direction,
                    plan.effective_recursive,
                    &String::from_utf8_lossy(&output.stderr),
                );
            }
            Err(error) => {
                return transfer_execution_failure(
                    config,
                    plan.method,
                    plan.direction,
                    plan.effective_recursive,
                    "ssh preparation",
                    &error,
                );
            }
        }
    }

    if matches!(
        &plan.backend,
        TransferBackend::Scp {
            has_sources: false,
            ..
        }
    ) {
        log_status!("transfer", "Complete");
        return Ok((
            transfer_output(
                config,
                plan.method,
                plan.direction,
                plan.effective_recursive,
                true,
                None,
                false,
            ),
            0,
        ));
    }

    let mut command = match &plan.backend {
        TransferBackend::Scp { args, .. } => {
            let mut command = Command::new(scp_program);
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
                transfer_output(
                    config,
                    plan.method,
                    plan.direction,
                    plan.effective_recursive,
                    success,
                    error,
                    false,
                ),
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
                    plan.effective_recursive,
                    false,
                    Some(error),
                    false,
                ),
                1,
            ))
        }
    }
}

fn transfer_backend_failure(
    config: &TransferConfig,
    method: String,
    direction: String,
    effective_recursive: bool,
    stderr: &str,
) -> crate::Result<(TransferOutput, i32)> {
    let source = bounded_identity(&effective_source(config));
    let destination = bounded_identity(&effective_destination(config));
    let scope = transfer_scope(config, effective_recursive);
    let detail = bounded_detail(stderr.trim());
    eprintln!(
        "[transfer] phase=failed source={} destination={} scope={} detail={}; destination may be partial; retry: {}",
        source,
        destination,
        scope,
        detail,
        retry_command(config),
    );
    Ok((
        transfer_output(
            config,
            method,
            direction,
            effective_recursive,
            false,
            Some(format!(
                "{}; destination may be partial; retry: {}",
                detail,
                retry_command(config)
            )),
            false,
        ),
        1,
    ))
}

fn transfer_execution_failure(
    config: &TransferConfig,
    method: String,
    direction: String,
    effective_recursive: bool,
    backend_label: &str,
    error: &std::io::Error,
) -> crate::Result<(TransferOutput, i32)> {
    let source = bounded_identity(&effective_source(config));
    let destination = bounded_identity(&effective_destination(config));
    let scope = transfer_scope(config, effective_recursive);
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
            method,
            direction,
            effective_recursive,
            false,
            Some(format!(
                "Failed to execute {backend_label}: {error}; destination may be partial; retry: {}",
                retry_command(config)
            )),
            false,
        ),
        1,
    ))
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::time::Duration;

    use crate::server::{self, Server};
    use crate::test_support::with_isolated_home;

    use super::{
        bounded_detail, bounded_identity, execute_plan, execute_plan_with, parse_target, plan_push,
        retry_command, run_transfer_command_with_policy, scp_args, transfer, transfer_scope,
        TransferBackend, TransferConfig, TransferOutput, TransferPlan, TransferTarget,
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
            assert!(!out.recursive);
            assert!(!out.effective_recursive);
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
            assert!(out.effective_recursive);
            assert!(out.compress);
            assert!(out.dry_run);
        });
    }

    #[test]
    fn transfer_serialization_preserves_requested_and_effective_recursion_across_directions() {
        let cases = [
            ("push", "scp", false, true),
            ("push", "scp", true, true),
            ("pull", "scp", true, true),
            ("server-to-server", "tar-pipe", false, true),
        ];

        for (direction, method, recursive, effective_recursive) in cases {
            let output = TransferOutput {
                source: "source".to_string(),
                destination: "destination".to_string(),
                effective_source: "source".to_string(),
                effective_destination: "destination".to_string(),
                method: method.to_string(),
                direction: direction.to_string(),
                effective_recursive,
                recursive,
                scope: "recursive path".to_string(),
                compress: false,
                success: true,
                error: None,
                dry_run: true,
            };

            assert_eq!(
                serde_json::to_string(&output).expect("serialize transfer output"),
                format!(
                    "{{\"source\":\"source\",\"destination\":\"destination\",\"effective_source\":\"source\",\"effective_destination\":\"destination\",\"method\":\"{method}\",\"direction\":\"{direction}\",\"effective_recursive\":{effective_recursive},\"recursive\":{recursive},\"scope\":\"recursive path\",\"compress\":false,\"success\":true,\"dry_run\":true}}"
                )
            );
        }
    }

    #[test]
    fn local_directory_push_implicitly_reports_effective_recursion() {
        with_isolated_home(|_| {
            save_server("prod");
            let fixture = tempfile::tempdir().expect("transfer fixture");
            let source = fixture.path().join("directory");
            std::fs::create_dir(&source).expect("source directory");

            let (out, code) = transfer(&TransferConfig {
                source: source.to_string_lossy().into_owned(),
                destination: "prod:/tmp/directory".to_string(),
                recursive: false,
                directory_contents: false,
                compress: false,
                dry_run: true,
                exclude: Vec::new(),
            })
            .expect("dry run directory push");

            assert_eq!(code, 0);
            assert!(!out.recursive);
            assert!(out.effective_recursive);
            assert_eq!(out.scope, "recursive path");
        });
    }

    #[test]
    fn pull_preserves_explicit_recursion_in_dry_run() {
        with_isolated_home(|_| {
            save_server("prod");

            let (out, code) = transfer(&TransferConfig {
                source: "prod:/var/www/uploads".to_string(),
                destination: "./uploads".to_string(),
                recursive: true,
                directory_contents: false,
                compress: false,
                dry_run: true,
                exclude: Vec::new(),
            })
            .expect("dry run pull");

            assert_eq!(code, 0);
            assert_eq!(out.direction, "pull");
            assert!(out.recursive);
            assert!(out.effective_recursive);
        });
    }

    #[test]
    fn non_recursive_server_transfer_dry_run_uses_execution_method() {
        with_isolated_home(|_| {
            save_server("old");
            save_server("new");

            let (out, code) = transfer(&TransferConfig {
                source: "old:/tmp/source.txt".to_string(),
                destination: "new:/tmp/destination.txt".to_string(),
                recursive: false,
                directory_contents: false,
                compress: false,
                dry_run: true,
                exclude: Vec::new(),
            })
            .expect("dry run server transfer");

            assert_eq!(code, 0);
            assert_eq!(out.method, "cat-pipe");
            assert!(!out.effective_recursive);
        });
    }

    #[test]
    fn trailing_slash_server_transfer_reports_effective_recursion_in_dry_run() {
        with_isolated_home(|_| {
            save_server("old");
            save_server("new");

            let (out, code) = transfer(&TransferConfig {
                source: "old:/var/www/uploads/".to_string(),
                destination: "new:/var/www/uploads".to_string(),
                recursive: false,
                directory_contents: false,
                compress: false,
                dry_run: true,
                exclude: Vec::new(),
            })
            .expect("dry run server transfer");

            assert_eq!(code, 0);
            assert_eq!(out.method, "tar-pipe");
            assert!(!out.recursive);
            assert!(out.effective_recursive);
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
            assert_eq!(out.effective_source, "local/existing-dir");
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
            assert_eq!(out.effective_source, "local/existing-dir");
            assert_eq!(out.effective_destination, "sandbox:/missing/same-dir");
        });
    }

    #[cfg(unix)]
    #[test]
    fn local_sync_executes_scp_without_a_terminal_dot_source() {
        with_isolated_home(|_| {
            save_server("sandbox");
            let fixture = tempfile::tempdir().expect("transfer fixture");
            let source = fixture.path().join("source with spaces");
            std::fs::create_dir(&source).expect("source directory");
            std::fs::write(source.join("visible file"), "visible").expect("visible fixture");
            std::fs::write(source.join(".hidden"), "hidden").expect("hidden fixture");

            let remote_destination = fixture.path().join("remote destination");
            std::fs::create_dir(&remote_destination).expect("remote destination");
            let scp_transport = fixture.path().join("scp-transport");
            std::fs::write(
                &scp_transport,
                format!(
                    "#!/bin/sh\nexec scp -t {}\n",
                    crate::engine::shell::quote_arg(&remote_destination.to_string_lossy())
                ),
            )
            .expect("scp transport");
            std::fs::set_permissions(&scp_transport, std::fs::Permissions::from_mode(0o755))
                .expect("executable scp transport");

            let ssh_log = fixture.path().join("ssh-args");
            let ssh = fixture.path().join("ssh-double");
            std::fs::write(
                &ssh,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                    ssh_log.display()
                ),
            )
            .expect("ssh double");
            std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))
                .expect("executable ssh double");

            let config = TransferConfig {
                source: source.to_string_lossy().into_owned(),
                destination: "sandbox:/remote path/destination".to_string(),
                recursive: true,
                directory_contents: true,
                compress: false,
                dry_run: false,
                exclude: Vec::new(),
            };
            let mut plan = plan_push(
                &config,
                &config.source,
                "sandbox",
                "/remote path/destination",
            )
            .expect("plan local sync");
            let TransferBackend::Scp { args, .. } = &mut plan.backend else {
                panic!("local sync should use scp");
            };
            assert!(args
                .iter()
                .any(|arg| *arg == source.join(".hidden").to_string_lossy()));
            assert!(args
                .iter()
                .any(|arg| *arg == source.join("visible file").to_string_lossy()));
            assert!(!args.iter().any(|arg| arg.ends_with("/.")));
            args.splice(
                0..0,
                [
                    "-S".to_string(),
                    scp_transport.to_string_lossy().into_owned(),
                ],
            );
            let (out, code) = execute_plan_with(&config, plan, std::path::Path::new("scp"), &ssh)
                .expect("execute local sync through installed scp");

            assert_eq!(code, 0);
            assert!(out.success);
            assert!(out.effective_recursive);
            assert_eq!(out.effective_source, source.to_string_lossy());
            let (dry_run, dry_run_code) = transfer(&TransferConfig {
                source: source.to_string_lossy().into_owned(),
                destination: "sandbox:/remote path/destination".to_string(),
                recursive: true,
                directory_contents: true,
                compress: false,
                dry_run: true,
                exclude: Vec::new(),
            })
            .expect("plan local sync");
            assert_eq!(dry_run_code, 0);
            assert_eq!(dry_run.effective_source, out.effective_source);
            assert_eq!(dry_run.effective_destination, out.effective_destination);
            assert_eq!(dry_run.direction, out.direction);
            assert_eq!(
                std::fs::read_to_string(remote_destination.join(".hidden"))
                    .expect("copied hidden file"),
                "hidden"
            );
            assert_eq!(
                std::fs::read_to_string(remote_destination.join("visible file"))
                    .expect("copied visible file"),
                "visible"
            );
            let ssh_args = std::fs::read_to_string(ssh_log).expect("recorded ssh args");
            assert!(ssh_args.contains("mkdir -p -- '/remote path/destination'"));
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

        assert_eq!(
            transfer_scope(&config, true),
            "recursive directory contents"
        );
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
            effective_recursive: false,
            backend: TransferBackend::Shell {
                command: "printf '\\377backend failure' >&2; exit 7".to_string(),
            },
        };

        let (output, code) = execute_plan(&config, plan).expect("execute transfer plan");

        assert_eq!(code, 1);
        assert!(!output.success);
        assert!(!output.effective_recursive);
        let error = output.error.expect("failure diagnostic");
        assert!(error.contains('\u{fffd}'));
        assert!(error.contains("backend failure"));
        assert!(error.contains("retry: homeboy file copy"));
    }
}
