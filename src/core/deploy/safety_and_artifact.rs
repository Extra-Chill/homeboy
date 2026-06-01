use std::collections::HashMap;
use std::path::Path;

use super::permissions;
use crate::core::component;
use crate::core::defaults;
use crate::core::engine::shell;
use crate::core::engine::template::{render_map, TemplateVars};
use crate::core::error::{Error, Result};
use crate::core::extension::DeployVerification;
use crate::core::server::SshClient;

use super::transfer::{upload_directory, upload_file};
use super::types::DeployResult;

/// Framework-neutral shared directory names that typically contain sibling components.
const DANGEROUS_PATH_SUFFIXES: &[&str] = &["/node_modules", "/vendor", "/packages", "/extensions"];

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
    let mut uploaded_artifact_path: Option<String> = None;

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
                     Add one with: homeboy component set <id> --json '{{\"extract_command\": \"unzip -o {{artifact}} && rm {{artifact}}\"}}'",
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
        uploaded_artifact_path = Some(upload_path.clone());

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
                let error_detail = if clean_output.stderr.is_empty() {
                    clean_output.stdout.clone()
                } else {
                    clean_output.stderr.clone()
                };
                return Ok(DeployResult::failure(
                    clean_output.exit_code,
                    format!(
                        "Failed to clean target directory before extraction (exit {}): {}",
                        clean_output.exit_code, error_detail
                    ),
                ));
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
        if let Some(upload_path) = uploaded_artifact_path.as_ref() {
            vars.insert("stagingArtifact".to_string(), upload_path.clone());
        }
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
    let mut result = render_map(template, vars);
    for (key, value) in vars {
        result = result.replace(&format!("{{{}}}", key), value);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{deploy_artifact, render_extract_command, DANGEROUS_PATH_SUFFIXES};
    use crate::core::extension::DeployVerification;
    use crate::core::server::SshClient;
    use std::collections::HashMap;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn local_client() -> SshClient {
        SshClient {
            host: "localhost".to_string(),
            user: "test".to_string(),
            port: 22,
            identity_file: None,
            auth: None,
            is_local: true,
            env: HashMap::new(),
        }
    }

    fn local_client_with_env(env: HashMap<String, String>) -> SshClient {
        SshClient {
            env,
            ..local_client()
        }
    }

    #[test]
    fn test_deploy_artifact_extract_command_template_replaces_vars() {
        let vars = std::collections::HashMap::from([
            ("archive".to_string(), "artifact.zip".to_string()),
            ("target".to_string(), "/srv/site/plugin".to_string()),
        ]);

        assert_eq!(
            render_extract_command("unzip {archive} -d {target}", &vars),
            "unzip artifact.zip -d /srv/site/plugin"
        );
    }

    #[test]
    fn test_deploy_artifact_extract_command_template_replaces_double_brace_vars() {
        let vars = std::collections::HashMap::from([
            (
                "artifact".to_string(),
                ".homeboy-data-machine-events.zip".to_string(),
            ),
            (
                "targetDir".to_string(),
                "/srv/site/wp-content/plugins/data-machine-events".to_string(),
            ),
        ]);

        assert_eq!(
            render_extract_command("unzip -o {{artifact}} && rm {{artifact}}", &vars),
            "unzip -o .homeboy-data-machine-events.zip && rm .homeboy-data-machine-events.zip"
        );
    }

    #[test]
    fn test_deploy_via_git_keeps_framework_safety_policy_external() {
        assert!(DANGEROUS_PATH_SUFFIXES.contains(&"/vendor"));
        assert!(!DANGEROUS_PATH_SUFFIXES.contains(&"/wp-content/plugins"));
    }

    #[test]
    fn test_deploy_artifact_renders_staging_artifact_in_verification_error() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("artifact.txt");
        let target = temp.path().join("target");
        fs::write(&artifact, "artifact bytes").expect("artifact");

        let verification = DeployVerification {
            path_pattern: "/target/".to_string(),
            verify_command: Some("false".to_string()),
            verify_error_message: Some(
                "Deploy verification failed for {{targetDir}} against {{stagingArtifact}}"
                    .to_string(),
            ),
        };

        let result = deploy_artifact(
            &local_client(),
            &artifact,
            target.to_str().expect("target path"),
            None,
            Some(&verification),
            None,
        )
        .expect("deploy result");

        let error = result.error.expect("verification error");
        assert!(!result.success);
        assert!(!error.contains("{{stagingArtifact}}"));
        assert!(error.contains(target.to_str().expect("target path")));
        assert!(error.contains(
            target
                .join("artifact.txt")
                .to_str()
                .expect("uploaded artifact path")
        ));
    }

    #[test]
    #[cfg(unix)]
    fn test_deploy_artifact_fails_when_pre_extract_cleanup_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("artifact.zip");
        let target = temp.path().join("target");
        let fake_bin = temp.path().join("bin");
        let fake_find = fake_bin.join("find");
        fs::create_dir_all(&fake_bin).expect("fake bin");
        fs::write(&artifact, "artifact bytes").expect("artifact");
        fs::write(&fake_find, "#!/bin/sh\necho cleanup denied >&2\nexit 42\n").expect("fake find");
        fs::set_permissions(&fake_find, fs::Permissions::from_mode(0o755))
            .expect("chmod fake find");

        let mut env = HashMap::new();
        env.insert(
            "PATH".to_string(),
            format!(
                "{}:{}",
                fake_bin.to_str().expect("fake bin path"),
                std::env::var("PATH").expect("PATH")
            ),
        );

        let result = deploy_artifact(
            &local_client_with_env(env),
            &artifact,
            target.to_str().expect("target path"),
            Some("true"),
            None,
            None,
        )
        .expect("deploy result");

        let error = result.error.expect("cleanup error");
        assert!(!result.success);
        assert_eq!(42, result.exit_code);
        assert!(error.contains("Failed to clean target directory before extraction"));
        assert!(error.contains("exit 42"));
        assert!(error.contains("cleanup denied"));
    }
}
