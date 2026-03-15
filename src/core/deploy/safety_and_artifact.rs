use std::collections::HashMap;
use std::path::Path;

use super::permissions;
use crate::component;
use crate::defaults;
use crate::engine::shell;
use crate::engine::template::{render_map, TemplateVars};
use crate::error::{Error, Result};
use crate::extension::DeployVerification;
use crate::server::SshClient;

use super::transfer::{upload_directory, upload_file};
use super::types::DeployResult;

/// Well-known shared directory names that typically contain multiple sibling components.
///
/// Deploy targets ending with one of these are almost certainly misconfigured —
/// the `remote_path` should point to the component's own subdirectory inside
/// these directories, not the directory itself. Deploying directly into a shared
/// directory would destroy sibling components during the pre-extraction clean step.
///
/// This list covers common package manager and framework conventions. Extensions
/// can declare additional protected directories via their deploy configuration.
const DANGEROUS_PATH_SUFFIXES: &[&str] = &[
    "/plugins",
    "/themes",
    "/mu-plugins",
    "/wp-content",
    "/wp-content/uploads",
    "/node_modules",
    "/vendor",
    "/packages",
    "/extensions",
];

/// Validate that a deploy target path is safe for destructive operations.
///
/// Prevents catastrophic data loss (issue #353) by catching cases where
/// `remote_path` resolves to a shared parent directory instead of the
/// component's own subdirectory. Two checks:
///
/// 1. The resolved path must not end with a known shared directory suffix
///    (e.g., `/wp-content/plugins`).
/// 2. The resolved path must not equal the project's `base_path` — deploying
///    directly to the site root would destroy the entire site.
pub(super) fn validate_deploy_target(
    install_dir: &str,
    base_path: &str,
    component_id: &str,
) -> Result<()> {
    let normalized = install_dir.trim_end_matches('/');
    let base_normalized = base_path.trim_end_matches('/');

    // Guard 1: target must not be the base_path itself
    if normalized == base_normalized {
        return Err(Error::validation_invalid_argument(
            "remotePath",
            format!(
                "Deploy target '{}' resolves to the project base_path — this would destroy the entire project. \
                 Set remote_path to the component's own subdirectory within the project",
                install_dir
            ),
            Some(install_dir.to_string()),
            None,
        ));
    }

    // Guard 2: target must not end with a known shared directory
    for suffix in DANGEROUS_PATH_SUFFIXES {
        if normalized.ends_with(suffix) {
            return Err(Error::validation_invalid_argument(
                "remotePath",
                format!(
                    "Deploy target '{}' is a shared parent directory — deploying here would delete \
                     sibling components. Set remote_path to the component's own subdirectory \
                     (e.g., '{}/{}')",
                    install_dir, normalized, component_id
                ),
                Some(install_dir.to_string()),
                None,
            ));
        }
    }

    Ok(())
}

/// Deploy a component via git pull on the remote server.
pub(super) fn deploy_via_git(
    ssh_client: &SshClient,
    remote_path: &str,
    git_config: &component::GitDeployConfig,
    component_version: Option<&str>,
) -> Result<DeployResult> {
    // Determine what to checkout
    let checkout_target = if let Some(ref pattern) = git_config.tag_pattern {
        if let Some(ver) = component_version {
            pattern.replace("{{version}}", ver)
        } else {
            git_config.branch.clone()
        }
    } else {
        git_config.branch.clone()
    };

    // Step 1: Fetch latest
    log_status!(
        "deploy:git",
        "Fetching from {} in {}",
        git_config.remote,
        remote_path
    );
    let fetch_cmd = format!(
        "cd {} && git fetch {} --tags",
        shell::quote_path(remote_path),
        shell::quote_arg(&git_config.remote),
    );
    let fetch_output = ssh_client.execute(&fetch_cmd);
    if !fetch_output.success {
        return Ok(DeployResult::failure(
            fetch_output.exit_code,
            format!("git fetch failed: {}", fetch_output.stderr),
        ));
    }

    // Step 2: Checkout target (tag or branch)
    let is_tag = git_config.tag_pattern.is_some() && component_version.is_some();
    let checkout_cmd = if is_tag {
        format!(
            "cd {} && git checkout {}",
            shell::quote_path(remote_path),
            shell::quote_arg(&checkout_target),
        )
    } else {
        format!(
            "cd {} && git checkout {} && git pull {} {}",
            shell::quote_path(remote_path),
            shell::quote_arg(&checkout_target),
            shell::quote_arg(&git_config.remote),
            shell::quote_arg(&checkout_target),
        )
    };
    log_status!("deploy:git", "Checking out {}", checkout_target);
    let checkout_output = ssh_client.execute(&checkout_cmd);
    if !checkout_output.success {
        return Ok(DeployResult::failure(
            checkout_output.exit_code,
            format!("git checkout/pull failed: {}", checkout_output.stderr),
        ));
    }

    // Step 3: Run post-pull commands
    for cmd in &git_config.post_pull {
        log_status!("deploy:git", "Running: {}", cmd);
        let full_cmd = format!("cd {} && {}", shell::quote_path(remote_path), cmd);
        let output = ssh_client.execute(&full_cmd);
        if !output.success {
            return Ok(DeployResult::failure(
                output.exit_code,
                format!("post-pull command failed ({}): {}", cmd, output.stderr),
            ));
        }
    }

    log_status!("deploy:git", "Deploy complete for {}", remote_path);
    Ok(DeployResult::success(0))
}

/// Main entry point - uploads artifact and runs extract command if configured
pub(super) fn deploy_artifact(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
    extract_command: Option<&str>,
    verification: Option<&DeployVerification>,
    remote_owner: Option<&str>,
) -> Result<DeployResult> {
    // Step 1: Upload (directory or file)
    if local_path.is_dir() {
        let result = upload_directory(ssh_client, local_path, remote_path)?;
        if !result.success {
            return Ok(result);
        }
    } else {
        // Validate: archive artifacts require an extract command
        let is_archive = local_path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| matches!(ext, "zip" | "tar" | "gz" | "tgz"))
            .unwrap_or(false);

        if is_archive && extract_command.is_none() {
            return Ok(DeployResult::failure(
                1,
                format!(
                    "Archive artifact '{}' requires an extractCommand. \
                     Add one with: homeboy component set <id> '{{\"extractCommand\": \"unzip -o {{artifact}} && rm {{artifact}}\"}}'",
                    local_path.display()
                ),
            ));
        }

        // For archives, upload to temp location in target directory
        let deploy_defaults = defaults::load_defaults().deploy;
        let artifact_prefix = &deploy_defaults.artifact_prefix;
        let artifact_filename = local_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "buildArtifact",
                    "Build artifact path must include a file name",
                    Some(local_path.display().to_string()),
                    None,
                )
            })?
            .to_string();
        let artifact_filename = format!("{}{}", artifact_prefix, artifact_filename);

        let upload_path = if extract_command.is_some() {
            // Archives are uploaded into the target directory (often with a prefix) then extracted.
            format!("{}/{}", remote_path, artifact_filename)
        } else {
            // Non-archives (or archives with no extract) should upload directly to a file path.
            // Using an explicit file path allows atomic replacement via a temp upload + mv.
            let local_filename = local_path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "buildArtifact",
                        "Build artifact path must include a file name",
                        Some(local_path.display().to_string()),
                        None,
                    )
                })?;
            format!("{}/{}", remote_path, local_filename)
        };

        // Create target directory
        let mkdir_cmd = format!("mkdir -p {}", shell::quote_path(remote_path));
        log_status!("deploy", "Creating directory: {}", remote_path);
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
            // Defense-in-depth: refuse to clean known shared parent directories.
            // The upstream validate_deploy_target() should already catch this,
            // but since this executes `rm -rf` we add an extra guard.
            let normalized_remote = remote_path.trim_end_matches('/');
            let is_dangerous = DANGEROUS_PATH_SUFFIXES
                .iter()
                .any(|suffix| normalized_remote.ends_with(suffix));
            if is_dangerous {
                return Ok(DeployResult::failure(
                    1,
                    format!(
                        "Refusing to clean '{}' — it is a shared parent directory. \
                         This would delete sibling components. Fix the component's remote_path.",
                        remote_path
                    ),
                ));
            }

            // Clean the target directory before extraction to prevent stale files.
            // This handles directory renames (e.g. blocks/ → Blocks/) where the old
            // casing would persist because unzip merges into existing directories.
            // We remove everything except the uploaded artifact itself.
            let clean_cmd = format!(
                "cd {} && find . -mindepth 1 -maxdepth 1 ! -name {} -exec rm -rf {{}} +",
                shell::quote_path(remote_path),
                shell::quote_arg(&artifact_filename),
            );
            log_status!("deploy", "Cleaning target directory before extraction");
            let clean_output = ssh_client.execute(&clean_cmd);
            if !clean_output.success {
                log_status!(
                    "deploy",
                    "Warning: failed to clean target directory: {}",
                    clean_output.stderr
                );
                // Non-fatal — proceed with extraction anyway
            }

            let mut vars = HashMap::new();
            vars.insert("artifact".to_string(), artifact_filename);
            vars.insert("targetDir".to_string(), remote_path.to_string());

            let rendered_cmd = render_extract_command(cmd_template, &vars);

            let extract_cmd = format!("cd {} && {}", shell::quote_path(remote_path), rendered_cmd);
            log_status!("deploy", "Extracting: {}", rendered_cmd);

            let extract_output = ssh_client.execute(&extract_cmd);
            if !extract_output.success {
                let error_detail = if extract_output.stderr.is_empty() {
                    extract_output.stdout.clone()
                } else {
                    extract_output.stderr.clone()
                };
                return Ok(DeployResult::failure(
                    extract_output.exit_code,
                    format!(
                        "Extract command failed (exit {}): {}",
                        extract_output.exit_code, error_detail
                    ),
                ));
            }

            // Fix file permissions after extraction
            log_status!("deploy", "Fixing file permissions");
            permissions::fix_deployed_permissions(ssh_client, remote_path, remote_owner)?;
        }
    }

    // Step 3: Run verification if configured
    if let Some((v, verify_cmd_template)) = verification
        .as_ref()
        .and_then(|v| v.verify_command.as_ref().map(|cmd| (v, cmd)))
    {
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

    Ok(DeployResult::success(0))
}

fn render_extract_command(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        result = result.replace(&format!("{{{}}}", key), value);
    }
    result
}
