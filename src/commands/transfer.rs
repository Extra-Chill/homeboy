use clap::Args;
use homeboy::server;
use homeboy::ssh::SshClient;
use serde::Serialize;
use std::process::{Command, Stdio};

use super::CmdResult;

#[derive(Args)]
pub struct TransferArgs {
    /// Source in format server_id:/path/to/file_or_dir
    pub source: String,

    /// Destination in format server_id:/path/to/file_or_dir
    pub destination: String,

    /// Transfer directories recursively
    #[arg(short, long)]
    pub recursive: bool,

    /// Compress data during transfer
    #[arg(short, long)]
    pub compress: bool,

    /// Show what would be transferred without doing it
    #[arg(long)]
    pub dry_run: bool,

    /// Exclude patterns (can be specified multiple times)
    #[arg(long)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TransferOutput {
    pub source_server: String,
    pub source_path: String,
    pub dest_server: String,
    pub dest_path: String,
    pub method: String,
    pub recursive: bool,
    pub compress: bool,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_transferred: Option<String>,
    pub dry_run: bool,
}

/// Parse a transfer target in format "server_id:/path"
fn parse_target(target: &str) -> Result<(String, String), homeboy::Error> {
    let parts: Vec<&str> = target.splitn(2, ':').collect();
    if parts.len() != 2 || parts[1].is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "target",
            "Must be in format server_id:/path/to/file",
            Some(target.to_string()),
            Some(vec![
                "sarai:/var/www/site/backup.sql".to_string(),
                "command:/tmp/data/".to_string(),
            ]),
        ));
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

/// Build SSH connection args for a server (for use in shell commands)
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

pub fn run(args: TransferArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<TransferOutput> {
    // Parse source and destination
    let (source_server_id, source_path) = parse_target(&args.source)?;
    let (dest_server_id, dest_path) = parse_target(&args.destination)?;

    // Load server configurations
    let source_server = server::load(&source_server_id)?;
    let dest_server = server::load(&dest_server_id)?;

    let source_client = SshClient::from_server(&source_server, &source_server_id)?;
    let dest_client = SshClient::from_server(&dest_server, &dest_server_id)?;

    if args.dry_run {
        let method = if args.recursive { "tar-pipe" } else { "scp-pipe" };
        eprintln!(
            "[dry-run] Would transfer {} -> {}",
            args.source, args.destination
        );
        eprintln!("[dry-run] Method: {}", method);
        if args.compress {
            eprintln!("[dry-run] Compression: enabled");
        }
        if !args.exclude.is_empty() {
            eprintln!("[dry-run] Excludes: {:?}", args.exclude);
        }

        return Ok((
            TransferOutput {
                source_server: source_server_id,
                source_path,
                dest_server: dest_server_id,
                dest_path,
                method: method.to_string(),
                recursive: args.recursive,
                compress: args.compress,
                success: true,
                error: None,
                bytes_transferred: None,
                dry_run: true,
            },
            0,
        ));
    }

    // Build the transfer command
    // Strategy: SSH pipe — ssh source "tar cf - /path" | ssh dest "tar xf - -C /dest"
    let source_ssh_args = build_ssh_args(&source_client);
    let dest_ssh_args = build_ssh_args(&dest_client);

    let source_remote = format!("{}@{}", source_client.user, source_client.host);
    let dest_remote = format!("{}@{}", dest_client.user, dest_client.host);

    let (method, command) = if args.recursive || source_path.ends_with('/') {
        // Directory transfer via tar pipe
        let tar_compress_flag = if args.compress { "z" } else { "" };

        // Build exclude args for tar
        let exclude_args: String = args
            .exclude
            .iter()
            .map(|e| format!(" --exclude='{}'", e))
            .collect();

        // For directory transfers, we tar from the parent and extract to dest
        // This preserves the directory structure correctly
        let cmd = format!(
            "ssh {} {} 'tar c{}f - -C \"{}\" .{}' | ssh {} {} 'mkdir -p \"{}\" && tar x{}f - -C \"{}\"'",
            source_ssh_args,
            source_remote,
            tar_compress_flag,
            source_path.trim_end_matches('/'),
            exclude_args,
            dest_ssh_args,
            dest_remote,
            dest_path.trim_end_matches('/'),
            tar_compress_flag,
            dest_path.trim_end_matches('/'),
        );

        ("tar-pipe".to_string(), cmd)
    } else {
        // Single file transfer via cat pipe
        let cmd = format!(
            "ssh {} {} 'cat \"{}\"' | ssh {} {} 'cat > \"{}\"'",
            source_ssh_args,
            source_remote,
            source_path,
            dest_ssh_args,
            dest_remote,
            dest_path,
        );

        ("cat-pipe".to_string(), cmd)
    };

    eprintln!("Transferring {} -> {}", args.source, args.destination);
    eprintln!("Method: {}", method);

    // Execute the transfer
    let output = Command::new("sh")
        .args(["-c", &command])
        .stdin(Stdio::null())
        .output();

    match output {
        Ok(out) => {
            let success = out.status.success();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();

            if !success {
                eprintln!("Transfer failed: {}", stderr);
            } else {
                eprintln!("Transfer complete");
            }

            Ok((
                TransferOutput {
                    source_server: source_server_id,
                    source_path,
                    dest_server: dest_server_id,
                    dest_path,
                    method,
                    recursive: args.recursive,
                    compress: args.compress,
                    success,
                    error: if success { None } else { Some(stderr) },
                    bytes_transferred: None,
                    dry_run: false,
                },
                if success { 0 } else { 1 },
            ))
        }
        Err(e) => Ok((
            TransferOutput {
                source_server: source_server_id,
                source_path,
                dest_server: dest_server_id,
                dest_path,
                method,
                recursive: args.recursive,
                compress: args.compress,
                success: false,
                error: Some(format!("Failed to execute transfer: {}", e)),
                bytes_transferred: None,
                dry_run: false,
            },
            1,
        )),
    }
}
