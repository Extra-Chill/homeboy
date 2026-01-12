use crate::shell;
use crate::ssh::SshClient;
use crate::Result;
use std::path::Path;
use std::process::Command;

/// Result of a deployment operation
pub struct DeployResult {
    pub success: bool,
    pub exit_code: i32,
    pub error: Option<String>,
}

impl DeployResult {
    fn success(exit_code: i32) -> Self {
        Self {
            success: true,
            exit_code,
            error: None,
        }
    }

    fn failure(exit_code: i32, error: String) -> Self {
        Self {
            success: false,
            exit_code,
            error: Some(error),
        }
    }
}

/// Main entry point - inspects artifact path and deploys appropriately
pub fn deploy_artifact(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    if local_path.is_dir() {
        deploy_directory(ssh_client, local_path, remote_path)
    } else if local_path.extension().is_some_and(|e| e == "zip") {
        deploy_zip(ssh_client, local_path, remote_path)
    } else if is_tarball(local_path, &[".tar.gz", ".tgz"]) {
        deploy_tarball(ssh_client, local_path, remote_path, "xzf")
    } else if is_tarball(local_path, &[".tar.bz2", ".tbz2"]) {
        deploy_tarball(ssh_client, local_path, remote_path, "xjf")
    } else if is_tarball(local_path, &[".tar"]) {
        deploy_tarball(ssh_client, local_path, remote_path, "xf")
    } else {
        deploy_file(ssh_client, local_path, remote_path)
    }
}

fn is_tarball(path: &Path, extensions: &[&str]) -> bool {
    path.to_str()
        .is_some_and(|p| extensions.iter().any(|ext| p.ends_with(ext)))
}

/// Deploy a directory recursively via scp -r
pub fn deploy_directory(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    // Ensure parent directory exists on remote
    let parent = Path::new(remote_path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or(remote_path);

    let mkdir_cmd = format!("mkdir -p {}", shell::quote_path(parent));
    let mkdir_output = ssh_client.execute(&mkdir_cmd);
    if !mkdir_output.success {
        return Ok(DeployResult::failure(
            mkdir_output.exit_code,
            format!("Failed to create remote directory: {}", mkdir_output.stderr),
        ));
    }

    // Use scp -r for recursive directory copy
    scp_recursive(ssh_client, local_path, remote_path)
}

/// Deploy a ZIP archive (upload, extract, cleanup temp file)
pub fn deploy_zip(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    let zip_filename = local_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!(".homeboy-{}", name))
        .unwrap_or_else(|| ".homeboy-archive.zip".to_string());

    // Ensure target directory exists
    let mkdir_cmd = format!("mkdir -p {}", shell::quote_path(remote_path));
    let mkdir_output = ssh_client.execute(&mkdir_cmd);
    if !mkdir_output.success {
        return Ok(DeployResult::failure(
            mkdir_output.exit_code,
            format!("Failed to create remote directory: {}", mkdir_output.stderr),
        ));
    }

    // Upload zip to temp location
    let upload_path = format!("{}/{}", remote_path, zip_filename);
    let upload_result = scp_file(ssh_client, local_path, &upload_path)?;
    if !upload_result.success {
        return Ok(upload_result);
    }

    // Extract and cleanup
    let extract_cmd = format!(
        "cd {} && unzip -o {} && rm {}",
        shell::quote_path(remote_path),
        shell::quote_path(&zip_filename),
        shell::quote_path(&zip_filename)
    );

    let extract_output = ssh_client.execute(&extract_cmd);
    if !extract_output.success {
        return Ok(DeployResult::failure(
            extract_output.exit_code,
            format!("Failed to extract ZIP: {}", extract_output.stderr),
        ));
    }

    Ok(DeployResult::success(0))
}

/// Deploy a tarball (upload, extract, cleanup temp file)
pub fn deploy_tarball(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
    tar_flags: &str,
) -> Result<DeployResult> {
    let tarball_filename = local_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!(".homeboy-{}", name))
        .unwrap_or_else(|| ".homeboy-archive.tar.gz".to_string());

    // Ensure target directory exists
    let mkdir_cmd = format!("mkdir -p {}", shell::quote_path(remote_path));
    let mkdir_output = ssh_client.execute(&mkdir_cmd);
    if !mkdir_output.success {
        return Ok(DeployResult::failure(
            mkdir_output.exit_code,
            format!("Failed to create remote directory: {}", mkdir_output.stderr),
        ));
    }

    // Upload tarball to temp location
    let upload_path = format!("{}/{}", remote_path, tarball_filename);
    let upload_result = scp_file(ssh_client, local_path, &upload_path)?;
    if !upload_result.success {
        return Ok(upload_result);
    }

    // Extract and cleanup
    let extract_cmd = format!(
        "cd {} && tar {} {} && rm {}",
        shell::quote_path(remote_path),
        tar_flags,
        shell::quote_path(&tarball_filename),
        shell::quote_path(&tarball_filename)
    );

    let extract_output = ssh_client.execute(&extract_cmd);
    if !extract_output.success {
        return Ok(DeployResult::failure(
            extract_output.exit_code,
            format!("Failed to extract tarball: {}", extract_output.stderr),
        ));
    }

    Ok(DeployResult::success(0))
}

/// Deploy a single file via scp
pub fn deploy_file(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    // Ensure parent directory exists on remote
    let parent = Path::new(remote_path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or(remote_path);

    let mkdir_cmd = format!("mkdir -p {}", shell::quote_path(parent));
    let mkdir_output = ssh_client.execute(&mkdir_cmd);
    if !mkdir_output.success {
        return Ok(DeployResult::failure(
            mkdir_output.exit_code,
            format!("Failed to create remote directory: {}", mkdir_output.stderr),
        ));
    }

    scp_file(ssh_client, local_path, remote_path)
}

/// SCP a single file to remote path
fn scp_file(ssh_client: &SshClient, local_path: &Path, remote_path: &str) -> Result<DeployResult> {
    let mut scp_args: Vec<String> = vec![];

    if let Some(identity_file) = &ssh_client.identity_file {
        scp_args.push("-i".to_string());
        scp_args.push(identity_file.clone());
    }

    if ssh_client.port != 22 {
        scp_args.push("-P".to_string());
        scp_args.push(ssh_client.port.to_string());
    }

    scp_args.push(local_path.to_string_lossy().to_string());
    scp_args.push(format!(
        "{}@{}:{}",
        ssh_client.user,
        ssh_client.host,
        shell::quote_path(remote_path)
    ));

    let output = Command::new("scp").args(&scp_args).output();

    match output {
        Ok(output) if output.status.success() => Ok(DeployResult::success(0)),
        Ok(output) => Ok(DeployResult::failure(
            output.status.code().unwrap_or(1),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )),
        Err(err) => Ok(DeployResult::failure(1, err.to_string())),
    }
}

/// SCP a directory recursively to remote path
fn scp_recursive(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    let mut scp_args: Vec<String> = vec!["-r".to_string()];

    if let Some(identity_file) = &ssh_client.identity_file {
        scp_args.push("-i".to_string());
        scp_args.push(identity_file.clone());
    }

    if ssh_client.port != 22 {
        scp_args.push("-P".to_string());
        scp_args.push(ssh_client.port.to_string());
    }

    scp_args.push(local_path.to_string_lossy().to_string());
    scp_args.push(format!(
        "{}@{}:{}",
        ssh_client.user,
        ssh_client.host,
        shell::quote_path(remote_path)
    ));

    let output = Command::new("scp").args(&scp_args).output();

    match output {
        Ok(output) if output.status.success() => Ok(DeployResult::success(0)),
        Ok(output) => Ok(DeployResult::failure(
            output.status.code().unwrap_or(1),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )),
        Err(err) => Ok(DeployResult::failure(1, err.to_string())),
    }
}
