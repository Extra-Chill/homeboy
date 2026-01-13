use crate::module::DeployVerification;
use crate::shell;
use crate::ssh::SshClient;
use crate::template::{render_map, TemplateVars};
use crate::Result;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

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

/// Main entry point - uploads artifact and runs extract command if configured
pub fn deploy_artifact(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
    extract_command: Option<&str>,
    verification: Option<&DeployVerification>,
) -> Result<DeployResult> {
    // Step 1: Upload (directory or file)
    if local_path.is_dir() {
        let result = upload_directory(ssh_client, local_path, remote_path)?;
        if !result.success {
            return Ok(result);
        }
    } else {
        // For archives, upload to temp location in target directory
        let artifact_filename = local_path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!(".homeboy-{}", name))
            .unwrap_or_else(|| ".homeboy-artifact".to_string());

        let upload_path = if extract_command.is_some() {
            format!("{}/{}", remote_path, artifact_filename)
        } else {
            remote_path.to_string()
        };

        // Create target directory
        let mkdir_cmd = format!("mkdir -p {}", shell::quote_path(remote_path));
        let mkdir_output = ssh_client.execute(&mkdir_cmd);
        if !mkdir_output.success {
            return Ok(DeployResult::failure(
                mkdir_output.exit_code,
                format!("Failed to create remote directory: {}", mkdir_output.stderr),
            ));
        }

        let result = upload_file(ssh_client, local_path, &upload_path)?;
        if !result.success {
            return Ok(result);
        }

        // Step 2: Execute extract command if configured
        if let Some(cmd_template) = extract_command {
            let mut vars = HashMap::new();
            vars.insert("artifact".to_string(), artifact_filename);
            vars.insert("targetDir".to_string(), remote_path.to_string());

            let rendered_cmd = render_extract_command(cmd_template, &vars);

            let extract_cmd = format!("cd {} && {}", shell::quote_path(remote_path), rendered_cmd);

            let extract_output = ssh_client.execute(&extract_cmd);
            if !extract_output.success {
                return Ok(DeployResult::failure(
                    extract_output.exit_code,
                    format!("Extract command failed: {}", extract_output.stderr),
                ));
            }
        }
    }

    // Step 3: Run verification if configured
    if let Some(v) = verification {
        if let Some(ref verify_cmd_template) = v.verify_command {
            let mut vars = HashMap::new();
            vars.insert(
                TemplateVars::TARGET_DIR.to_string(),
                remote_path.to_string(),
            );
            let verify_cmd = render_map(verify_cmd_template, &vars);

            let verify_output = ssh_client.execute(&verify_cmd);
            if !verify_output.success || verify_output.stdout.trim().is_empty() {
                let error_msg = v
                    .verify_error_message
                    .as_ref()
                    .map(|msg| render_map(msg, &vars))
                    .unwrap_or_else(|| format!("Deploy verification failed for {}", remote_path));
                return Ok(DeployResult::failure(1, error_msg));
            }
        }
    }

    Ok(DeployResult::success(0))
}

fn render_extract_command(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        result = result.replace(&format!("{{{}}}", key), value);
    }
    result
}

fn upload_directory(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
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

    scp_recursive(ssh_client, local_path, remote_path)
}

fn upload_file(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    scp_file(ssh_client, local_path, remote_path)
}

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
