use super::SshClient;
use serde::Serialize;
use std::process::{Command, Stdio};

/// Configuration for a file transfer operation.
pub struct TransferConfig {
    /// Source: local path or server_id:/path
    pub source: String,
    /// Destination: local path or server_id:/path
    pub destination: String,
    /// Transfer directories recursively
    pub recursive: bool,
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
    pub method: String,
    pub direction: String,
    pub recursive: bool,
    pub compress: bool,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub dry_run: bool,
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

/// Build scp-compatible SSH args for a server connection.
fn build_scp_args(client: &SshClient) -> Vec<String> {
    let mut args = vec![
        "-O".to_string(), // Use legacy SCP protocol (not SFTP)
        "-o".to_string(),
        "StrictHostKeyChecking=no".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
    ];

    if let Some(identity_file) = &client.identity_file {
        args.push("-i".to_string());
        args.push(identity_file.clone());
    }

    if client.port != 22 {
        args.push("-P".to_string()); // scp uses -P (uppercase) for port
        args.push(client.port.to_string());
    }

    args
}

/// Build SSH connection args for server-to-server tar pipe.
fn build_ssh_args(client: &SshClient) -> String {
    let mut args = Vec::new();
    args.push("-o StrictHostKeyChecking=no".to_string());
    args.push("-o BatchMode=yes".to_string());

    if let Some(identity_file) = &client.identity_file {
        args.push(format!("-i {}", identity_file));
    }

    if client.port != 22 {
        args.push(format!("-p {}", client.port));
    }

    args.join(" ")
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
                    "Upload to server: homeboy file upload server ./file /path/to/file".to_string(),
                    "Copy from server: homeboy file copy server:/path/to/file ./local-copy"
                        .to_string(),
                ]),
            ))
        }
        (TransferTarget::Local(local_path), TransferTarget::Remote { server_id, path }) => {
            run_push(config, local_path, server_id, path)
        }
        (TransferTarget::Remote { server_id, path }, TransferTarget::Local(local_path)) => {
            run_pull(config, server_id, path, local_path)
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
        ) => run_server_to_server(config, src_id, src_path, dst_id, dst_path),
    }
}

/// Push a local file/directory to a remote server via scp.
fn run_push(
    config: &TransferConfig,
    local_path: &str,
    server_id: &str,
    remote_path: &str,
) -> crate::Result<(TransferOutput, i32)> {
    let srv = super::load(server_id)?;
    let client = SshClient::from_server(&srv, server_id)?;

    let remote_target = format!("{}@{}:{}", client.user, client.host, remote_path);

    if config.dry_run {
        log_status!(
            "dry-run",
            "Would push {} -> {}:{}",
            local_path,
            server_id,
            remote_path
        );
        return Ok((
            TransferOutput {
                source: config.source.clone(),
                destination: config.destination.clone(),
                method: "scp".to_string(),
                direction: "push".to_string(),
                recursive: config.recursive,
                compress: config.compress,
                success: true,
                error: None,
                dry_run: true,
            },
            0,
        ));
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

    let mut scp_args = build_scp_args(&client);

    if config.recursive || local.is_dir() {
        scp_args.push("-r".to_string());
    }
    if config.compress {
        scp_args.push("-C".to_string());
    }

    scp_args.push(local_path.to_string());
    scp_args.push(remote_target);

    log_status!(
        "transfer",
        "Pushing {} -> {}:{}",
        local_path,
        server_id,
        remote_path
    );

    execute_scp(&scp_args, config)
}

/// Pull a remote file/directory to a local path via scp.
fn run_pull(
    config: &TransferConfig,
    server_id: &str,
    remote_path: &str,
    local_path: &str,
) -> crate::Result<(TransferOutput, i32)> {
    let srv = super::load(server_id)?;
    let client = SshClient::from_server(&srv, server_id)?;

    let remote_target = format!("{}@{}:{}", client.user, client.host, remote_path);

    if config.dry_run {
        log_status!(
            "dry-run",
            "Would pull {}:{} -> {}",
            server_id,
            remote_path,
            local_path
        );
        return Ok((
            TransferOutput {
                source: config.source.clone(),
                destination: config.destination.clone(),
                method: "scp".to_string(),
                direction: "pull".to_string(),
                recursive: config.recursive,
                compress: config.compress,
                success: true,
                error: None,
                dry_run: true,
            },
            0,
        ));
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

    let mut scp_args = build_scp_args(&client);

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
        remote_path,
        local_path
    );

    execute_scp(&scp_args, config)
}

/// Transfer between two remote servers via SSH tar pipe.
fn run_server_to_server(
    config: &TransferConfig,
    src_id: &str,
    src_path: &str,
    dst_id: &str,
    dst_path: &str,
) -> crate::Result<(TransferOutput, i32)> {
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
        return Ok((
            TransferOutput {
                source: config.source.clone(),
                destination: config.destination.clone(),
                method: method.to_string(),
                direction: "server-to-server".to_string(),
                recursive: config.recursive,
                compress: config.compress,
                success: true,
                error: None,
                dry_run: true,
            },
            0,
        ));
    }

    let source_ssh_args = build_ssh_args(&src_client);
    let dest_ssh_args = build_ssh_args(&dst_client);

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

    log_status!("transfer", "{} -> {}", config.source, config.destination);
    log_status!("transfer", "Method: {}", method);

    let output = Command::new("sh")
        .args(["-c", &command])
        .stdin(Stdio::null())
        .output();

    match output {
        Ok(out) => {
            let success = out.status.success();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();

            if !success {
                eprintln!("[transfer] Failed: {}", stderr);
            } else {
                log_status!("transfer", "Complete");
            }

            Ok((
                TransferOutput {
                    source: config.source.clone(),
                    destination: config.destination.clone(),
                    method,
                    direction: "server-to-server".to_string(),
                    recursive: config.recursive,
                    compress: config.compress,
                    success,
                    error: if success { None } else { Some(stderr) },
                    dry_run: false,
                },
                if success { 0 } else { 1 },
            ))
        }
        Err(e) => Ok((
            TransferOutput {
                source: config.source.clone(),
                destination: config.destination.clone(),
                method,
                direction: "server-to-server".to_string(),
                recursive: config.recursive,
                compress: config.compress,
                success: false,
                error: Some(format!("Failed to execute transfer: {}", e)),
                dry_run: false,
            },
            1,
        )),
    }
}

/// Execute an scp command and return structured output.
fn execute_scp(
    scp_args: &[String],
    config: &TransferConfig,
) -> crate::Result<(TransferOutput, i32)> {
    let output = Command::new("scp")
        .args(scp_args)
        .stdin(Stdio::null())
        .output();

    match output {
        Ok(out) => {
            let success = out.status.success();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();

            if !success {
                eprintln!("[transfer] Failed: {}", stderr);
            } else {
                log_status!("transfer", "Complete");
            }

            Ok((
                TransferOutput {
                    source: config.source.clone(),
                    destination: config.destination.clone(),
                    method: "scp".to_string(),
                    direction: if config.source.contains(':') {
                        "pull".to_string()
                    } else {
                        "push".to_string()
                    },
                    recursive: config.recursive,
                    compress: config.compress,
                    success,
                    error: if success { None } else { Some(stderr) },
                    dry_run: false,
                },
                if success { 0 } else { 1 },
            ))
        }
        Err(e) => Ok((
            TransferOutput {
                source: config.source.clone(),
                destination: config.destination.clone(),
                method: "scp".to_string(),
                direction: "unknown".to_string(),
                recursive: config.recursive,
                compress: config.compress,
                success: false,
                error: Some(format!("Failed to execute scp: {}", e)),
                dry_run: false,
            },
            1,
        )),
    }
}
